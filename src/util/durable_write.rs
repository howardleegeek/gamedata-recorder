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
//!   a. Write bytes to `<path>.tmp`.
//!   b. `File::sync_all()` on the temp file so the data is durable on the
//!      physical medium, not just in the page cache.
//!   c. `rename(<path>.tmp, <path>)` — this is the atomic commit point.
//!   d. (POSIX) `fsync` the containing directory so the rename is also
//!      durable and can't be rolled back after a crash.
//!
//! On Windows step (d) errors because opening a directory as a `File` isn't
//! supported the same way; we silently ignore that error — step (c) is
//! already atomic on NTFS via `MoveFileExW(MOVEFILE_WRITE_THROUGH)` semantics
//! that `std::fs::rename` inherits, and a full directory handle sync is not
//! generally available without `OpenDirectoryHandle` / `FlushFileBuffers`.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

/// Extension used for the temporary file during atomic write. Chosen so that
/// leftover temp files from a crash are obvious on disk and don't collide
/// with normal output files. We append `.tmp` to whatever extension the
/// final path has (so `metadata.json` → `metadata.json.tmp`). This preserves
/// the existing convention already used by
/// [`crate::record::local_recording::write_metadata_and_validate`].
fn temp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Blocking atomic write with fsync. Safe to call from sync code or from a
/// `tokio::task::spawn_blocking` closure.
///
/// Writes `contents` to `path` such that after a crash either the old file
/// (or none, if the path didn't exist) or the complete new file is visible
/// — never a torn, truncated, or empty file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = temp_path_for(path);

    // Scope the file handle so it's closed before we rename; some platforms
    // (notably older Windows filesystems) refuse `rename` over an open handle.
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents)?;
        // Make data durable on the physical medium before we swing the name.
        // Without this, a power loss between `write_all` (page-cache only)
        // and the subsequent rename could leave a zero-length file under the
        // final name even though rename(2) itself is atomic.
        f.sync_all()?;
    }

    // Atomic commit. If this fails (permissions, cross-device, file locked),
    // remove the orphan tmp file so a retry starts clean.
    if let Err(e) = std::fs::rename(&tmp, path) {
        std::fs::remove_file(&tmp).ok();
        return Err(e);
    }

    // Best-effort: fsync the containing directory so the rename itself
    // survives a crash. On POSIX this is necessary; on Windows opening a
    // directory as a `File` errors, and we accept that — the rename we just
    // issued is already durable on NTFS via MoveFile semantics.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            // Ignore the error — on Windows this will often fail with
            // "Access is denied" because a directory handle isn't a writable
            // file handle. That's fine; the rename is already durable.
            let _ = dir.sync_all();
        }
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
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
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
