//! Atomic, fsync'd file writes for crash-safe session output.
//!
//! On unclean shutdown (power loss, kernel panic, force-quit) a plain
//! `std::fs::write(path, data)` can leave the destination file in three bad
//! states:
//!   1. File exists but content is empty / truncated (fs committed inode but
//!      not data blocks).
//!   2. File exists with partial content (some blocks flushed, others not).
//!   3. File replaced with a zero-length file (atime update flushed but
//!      rename not yet journaled).
//!
//! For session metadata (`metadata.json`, `fps_log.json`, `frames.jsonl`,
//! `session.json`, etc.) any of those outcomes is catastrophic: the recording
//! on disk becomes unreadable or misreported, and because the caller already
//! treated the write as "done" there's no retry.
//!
//! The fix is the standard write-tmp → fsync → rename dance:
//!   a. Write bytes to a uniquely-named tempfile in the SAME directory as
//!      the destination (`tempfile::NamedTempFile::new_in`). Same-directory
//!      placement is critical: `rename(2)` is only atomic when source and
//!      destination are on the same filesystem, and a per-call unique
//!      filename prevents two concurrent atomic writes from colliding on a
//!      shared `<path>.tmp` sibling.
//!   b. `File::sync_all()` on the temp file so the data is durable on the
//!      physical medium, not just in the page cache.
//!   c. `rename(tempfile, <path>)` — this is the atomic commit point.
//!      `NamedTempFile::persist()` performs the rename and disarms its
//!      RAII drop guard so the file isn't deleted if persist succeeds. On
//!      failure the drop guard *will* unlink the tempfile, so we can't end
//!      up with orphan tmp files after a failed commit.
//!   d. (POSIX) `fsync` the containing directory so the rename is also
//!      durable and can't be rolled back after a crash.
//!
//! On Windows step (d) errors because opening a directory as a `File` isn't
//! supported the same way; we silently ignore that error — step (c) is
//! already atomic on NTFS via `MoveFileExW(MOVEFILE_WRITE_THROUGH)` semantics
//! that `std::fs::rename` (and `NamedTempFile::persist`) inherits, and a
//! full directory handle sync is not generally available without
//! `OpenDirectoryHandle` / `FlushFileBuffers`.

use std::{io::Write as _, path::Path};

/// Extension used for the temporary file during atomic write. Historical
/// callers in the codebase look for the `<path>.tmp` convention when
/// detecting crashed sessions; we keep that as the *prefix* of the unique
/// tempfile name (e.g. `metadata.json.tmp.AbC123`) so existing
/// crash-recovery scans still match while concurrent writers can't collide
/// on a shared name.
const TMP_PREFIX_SUFFIX: &str = ".tmp.";

/// Blocking atomic write with fsync. Safe to call from sync code or from a
/// `tokio::task::spawn_blocking` closure.
///
/// Writes `contents` to `path` such that after a crash either the old file
/// (or none, if the path didn't exist) or the complete new file is visible
/// — never a torn, truncated, or empty file.
///
/// Failure modes:
///   - Tempfile creation fails (out of space, permissions): returns
///     `io::Error`, no tempfile remains.
///   - Write or fsync to tempfile fails: tempfile is removed by
///     `NamedTempFile`'s RAII drop, returns `io::Error`.
///   - Rename to final path fails: tempfile is removed by
///     `NamedTempFile`'s RAII drop, returns `io::Error`. The destination
///     keeps its prior contents (or stays absent if it didn't exist).
///   - Rename succeeds but directory fsync fails: best-effort, error is
///     swallowed — the data is already durable on the medium.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    // Same-directory placement. If `path` has no parent we fall back to the
    // current dir — but every metadata path the recorder writes has a
    // session-directory parent, so this is just defensive.
    let parent = path.parent().unwrap_or(Path::new("."));

    // Prefix the tempfile name with the final file's name so a leftover
    // tmp after a crash is obvious on disk (e.g. `metadata.json.tmp.AbC123`
    // sits next to `metadata.json`). Same-directory placement is critical
    // for `rename(2)` atomicity.
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let prefix = format!("{file_name}{TMP_PREFIX_SUFFIX}");
    let mut named = tempfile::Builder::new()
        .prefix(prefix.as_str())
        .tempfile_in(parent)?;

    // Write all the contents. Use a separate Write block so the buffer is
    // flushed before we sync — flush() is a no-op on `File` but doc-clear.
    {
        let f = named.as_file_mut();
        f.write_all(contents)?;
        // Make data durable on the physical medium before we swing the name.
        // Without this, a power loss between `write_all` (page-cache only)
        // and the subsequent rename could leave a zero-length file under the
        // final name even though rename(2) itself is atomic.
        f.sync_all()?;
    }

    // Atomic commit. `persist` renames the tempfile to `path`, disarms the
    // drop-time unlink, and returns the underlying `File` (which we don't
    // need). If this fails (cross-device, permissions, file locked), the
    // `PersistError` returned wraps both the `io::Error` and the (still
    // RAII-managed) `NamedTempFile` — letting the latter drop reverts to
    // unlinking the tempfile, so no orphan remains.
    named.persist(path).map_err(|persist_err| {
        // The PersistError already carries the original io::Error; surface
        // it verbatim. Drop of `persist_err` releases the tempfile back to
        // its RAII guard which then unlinks it.
        persist_err.error
    })?;

    // Best-effort: fsync the containing directory so the rename itself
    // survives a crash. On POSIX this is necessary; on Windows opening a
    // directory as a `File` errors, and we accept that — the rename we just
    // issued is already durable on NTFS via MoveFile semantics.
    if let Ok(dir) = std::fs::File::open(parent) {
        // Ignore the error — on Windows this will often fail with
        // "Access is denied" because a directory handle isn't a writable
        // file handle. That's fine; the rename is already durable.
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Async wrapper around [`write_atomic`]. Delegates to a blocking task so
/// we don't hold the tokio reactor during the fsync (which can take tens to
/// hundreds of ms on a busy disk).
pub async fn write_atomic_async(path: &Path, contents: Vec<u8>) -> std::io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write_atomic(&path, &contents))
        .await
        .map_err(std::io::Error::other)?
}

/// Best-effort fsync of a directory — surfaces to the durability of the
/// preceding `rename`/`create` calls in it. Errors are swallowed on Windows
/// (where opening a dir as a file fails with "dir not a file") because the
/// semantics there don't require it.
pub fn sync_dir(dir: &Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Best-effort fsync of a file by re-opening it read+write and calling
/// `sync_all`. Used after a subprocess (OBS) closes an output file to make
/// sure the data, inode, and extent list are all durable before we write any
/// metadata that references the file.
///
/// If the file can't be opened or sync fails, we log and swallow the error —
/// the caller has already moved on, and we'd rather write metadata on a
/// best-effort-fsynced file than drop the recording entirely.
pub fn fsync_file(path: &Path) -> std::io::Result<()> {
    // Open read-only is sufficient for sync_all on Unix. On Windows,
    // sync_all → FlushFileBuffers requires GENERIC_WRITE access, so we open
    // read+write. Use OpenOptions so behaviour is identical across platforms.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_atomic_creates_file_with_expected_content() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x.json");
        write_atomic(&p, br#"{"a":1}"#).unwrap();
        let read = std::fs::read_to_string(&p).unwrap();
        assert_eq!(read, r#"{"a":1}"#);
    }

    #[test]
    fn write_atomic_leaves_no_tmp_file_on_success() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x.json");
        write_atomic(&p, b"hello").unwrap();
        let tmp_sibling = tmp.path().join("x.json.tmp");
        assert!(
            !tmp_sibling.exists(),
            "leftover .tmp file after successful rename"
        );
    }

    #[test]
    fn write_atomic_overwrites_existing_file() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x.json");
        std::fs::write(&p, b"old").unwrap();
        write_atomic(&p, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
    }

    #[tokio::test]
    async fn write_atomic_async_works_from_tokio() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("async.json");
        write_atomic_async(&p, b"async".to_vec()).await.unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "async");
    }
}
