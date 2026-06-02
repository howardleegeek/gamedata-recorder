//! Self-diagnostic measurement of the ACTUALLY-ENCODED video file.
//!
//! WHY THIS EXISTS (read before editing):
//! The recorder's `metadata.json` historically reported the video's
//! resolution/fps/frame-count from OBS's *configured* values
//! (`constants::RECORDING_WIDTH`/`HEIGHT`, `constants::FPS`). Those are what we
//! ASKED OBS to produce — not necessarily what the muxer actually wrote. Two
//! real defects proved the configured values can be a lie:
//!
//!   * a v2.6.14 session rendered a CFR-30 stream that the mp4 muxer coalesced
//!     to ~24 fps (millisecond-timescale rounding — see the `muxer_settings`
//!     comment in `obs_embedded_recorder.rs`);
//!   * a v2.6.15 session whose `metadata.json` claimed 1920x1080 @ 29.97 fps /
//!     9494 frames but whose decoded mp4 was 960x544 @ 24 fps / 7585 frames
//!     (the game_hook captured a 960x544 render surface, OBS down-mapped, and
//!     the encode dropped frames).
//!
//! Each time, we trusted the configured metadata and got fooled. This module
//! is the cure: AFTER the mp4 is finalized we MEASURE the real file and record
//! the measured truth in a `_video_actual` metadata block, with a loud
//! mismatch warning. This module is PURELY additive and DIAGNOSTIC — it never
//! changes capture, encoding, the muxer, fps, or resolution. It only reads the
//! finished file and reports numbers.
//!
//! MEASUREMENT STRATEGY (and why):
//!   1. If a bundled `ffprobe`/`ffprobe.exe` sits next to our executable, shell
//!      out to it. ffprobe is the authoritative decoder and handles every mp4
//!      edge case. We deliberately use the BUNDLED binary next to the exe (the
//!      same place `obs-ffmpeg-mux.exe` ships) rather than a PATH lookup,
//!      because relying on a PATH `ffprobe` was the root cause of a long string
//!      of silent failures (it simply was not present on tester machines —
//!      "WinError 2"). We never fall through to PATH.
//!   2. Otherwise, parse the mp4 `moov` box directly in Rust. This is a small
//!      self-contained, zero-dependency parser that ALWAYS works because the
//!      file is right there on disk — no external binary, no subprocess, no
//!      PATH. It reads the video track's `stsz` `sample_count` (= encoded frame
//!      count) and the `mvhd`/`mdhd` `timescale`+`duration` (= duration), plus
//!      the `tkhd` track width/height (fixed-point 16.16). fps = frames /
//!      duration.
//!
//! The moov parser is intentionally minimal: it understands only the handful of
//! boxes OBS's `ffmpeg_muxer` writes for an H.264/H.265 + AAC mp4. It is robust
//! against truncation and bad sizes (returns an error rather than panicking)
//! and never reads outside the file.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// How the actual video numbers were obtained. The serialized form is governed
/// by [`VideoActual`]'s `serialize_with = "serialize_source"` (which emits
/// [`MeasurementSource::as_str`]); this `Serialize` derive is only here so the
/// enum can also be embedded verbatim elsewhere if needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MeasurementSource {
    /// Measured by shelling out to a bundled `ffprobe` binary.
    Ffprobe,
    /// Measured by parsing the mp4 `moov` box directly in Rust.
    Mp4MoovParse,
}

impl MeasurementSource {
    /// The exact string written to `_video_actual.source`.
    pub fn as_str(self) -> &'static str {
        match self {
            MeasurementSource::Ffprobe => "ffprobe",
            MeasurementSource::Mp4MoovParse => "mp4_moov_parse",
        }
    }
}

/// The measured truth about an encoded video file.
///
/// All fields describe the ACTUAL bytes on disk, not any configured/claimed
/// value. `fps` is `frame_count / duration_s` (0.0 when duration is 0 so the
/// JSON never carries a NaN). This struct is serialized verbatim as the
/// `_video_actual` block alongside (never replacing) the claimed fields.
#[derive(Debug, Clone, Serialize)]
pub struct VideoActual {
    /// Encoded frame width in pixels.
    pub width: u32,
    /// Encoded frame height in pixels.
    pub height: u32,
    /// Effective frames-per-second: `frame_count / duration_s`.
    pub fps: f64,
    /// Number of encoded video samples (frames) in the file.
    pub frame_count: u64,
    /// Media duration of the video track in seconds.
    pub duration_s: f64,
    /// How these numbers were obtained.
    #[serde(serialize_with = "serialize_source")]
    pub source: MeasurementSource,
    /// Whether the measured values match the recorder's claimed values:
    /// width/height EXACT and fps within ±`FPS_MATCH_TOLERANCE`.
    pub matches_claim: bool,
}

fn serialize_source<S>(source: &MeasurementSource, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    s.serialize_str(source.as_str())
}

/// FPS tolerance for `matches_claim`. ±1.0 fps absorbs the legitimate slop
/// between a nominal target (e.g. 30) and a measured rate (e.g. 29.97) while
/// still flagging the 30→24 defect class loudly.
pub const FPS_MATCH_TOLERANCE: f64 = 1.0;

impl VideoActual {
    /// Compute `matches_claim` against claimed dimensions + fps.
    ///
    /// Resolution must match EXACTLY (a 1080p claim for a 544p encode is never
    /// "close enough"); fps must be within [`FPS_MATCH_TOLERANCE`].
    fn evaluate_match(&self, claimed_width: u32, claimed_height: u32, claimed_fps: f64) -> bool {
        self.width == claimed_width
            && self.height == claimed_height
            && (self.fps - claimed_fps).abs() <= FPS_MATCH_TOLERANCE
    }
}

/// Raw measured numbers before the claim comparison is applied. Internal to
/// keep each measurement backend free of claim logic.
#[derive(Debug, Clone, Copy)]
struct RawMeasurement {
    width: u32,
    height: u32,
    frame_count: u64,
    duration_s: f64,
}

impl RawMeasurement {
    fn fps(&self) -> f64 {
        if self.duration_s > 0.0 && self.frame_count > 0 {
            self.frame_count as f64 / self.duration_s
        } else {
            0.0
        }
    }
}

/// Measure the encoded video at `mp4_path` and build a [`VideoActual`] with
/// `matches_claim` evaluated against the supplied claimed values.
///
/// Tries the bundled-ffprobe backend first, then the in-Rust moov parser.
/// Returns `Err` only if BOTH backends fail (e.g. the file is missing or
/// truncated before the `moov` box was written). The caller treats an error as
/// "could not self-measure" and simply omits the `_video_actual` block —
/// measurement is diagnostic and never blocks finalizing a recording.
pub fn measure_encoded_video(
    mp4_path: &Path,
    claimed_width: u32,
    claimed_height: u32,
    claimed_fps: f64,
) -> color_eyre::Result<VideoActual> {
    let (raw, source) = match measure_with_bundled_ffprobe(mp4_path) {
        Ok(Some(raw)) => (raw, MeasurementSource::Ffprobe),
        Ok(None) => {
            // No bundled ffprobe present — expected on builds that ship only
            // the moov parser. Fall straight through to the parser.
            let raw = parse_moov(mp4_path)?;
            (raw, MeasurementSource::Mp4MoovParse)
        }
        Err(e) => {
            // ffprobe was present but failed (bad exit, unparseable output).
            // The moov parser is the reliable floor, so try it before giving up.
            tracing::warn!(
                "Bundled ffprobe measurement failed ({e}); falling back to mp4 moov parser"
            );
            let raw = parse_moov(mp4_path)?;
            (raw, MeasurementSource::Mp4MoovParse)
        }
    };

    let mut actual = VideoActual {
        width: raw.width,
        height: raw.height,
        fps: raw.fps(),
        frame_count: raw.frame_count,
        duration_s: raw.duration_s,
        source,
        matches_claim: false,
    };
    actual.matches_claim = actual.evaluate_match(claimed_width, claimed_height, claimed_fps);
    Ok(actual)
}

/// Locate a bundled `ffprobe` binary next to the running executable.
///
/// We ONLY look next to the exe (where `obs-ffmpeg-mux.exe` is bundled), never
/// on `PATH` — a missing PATH ffprobe was the historical silent-failure root
/// cause. Returns `None` if no bundled binary is found, which is a normal,
/// non-error condition (the moov parser handles it).
fn bundled_ffprobe_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Windows ships `ffprobe.exe`; keep the bare name too for non-Windows dev.
    for name in ["ffprobe.exe", "ffprobe"] {
        let candidate = exe_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Measure via a bundled `ffprobe`, if one exists next to the exe.
///
/// Returns:
///   * `Ok(Some(raw))` — ffprobe ran and produced usable numbers.
///   * `Ok(None)`      — no bundled ffprobe found (normal; use the parser).
///   * `Err(_)`        — ffprobe was found but failed or returned junk.
///
/// We ask ffprobe for the first video stream's coded width/height,
/// `nb_frames`, the stream/format duration, and `avg_frame_rate`. We compute
/// our own fps from `frame_count / duration` so it matches the moov-parser
/// definition exactly (rather than trusting `avg_frame_rate`, which ffprobe
/// rounds). `avg_frame_rate` is used only as a fallback frame-count source when
/// `nb_frames` is absent (some muxers omit it).
fn measure_with_bundled_ffprobe(mp4_path: &Path) -> color_eyre::Result<Option<RawMeasurement>> {
    use color_eyre::eyre::eyre;

    let Some(ffprobe) = bundled_ffprobe_path() else {
        return Ok(None);
    };

    let output = std::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,nb_frames,duration,avg_frame_rate:format=duration",
            "-print_format",
            "json",
        ])
        .arg(mp4_path)
        .output()
        .map_err(|e| {
            eyre!(
                "failed to run bundled ffprobe at {}: {e}",
                ffprobe.display()
            )
        })?;

    if !output.status.success() {
        return Err(eyre!(
            "bundled ffprobe exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| eyre!("failed to parse ffprobe json: {e}"))?;

    parse_ffprobe_json(&json).map(Some)
}

/// Parse the JSON ffprobe emits into a [`RawMeasurement`]. Split out so it can
/// be unit-tested against captured ffprobe output without a binary.
fn parse_ffprobe_json(json: &serde_json::Value) -> color_eyre::Result<RawMeasurement> {
    use color_eyre::eyre::eyre;

    let stream = json
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| eyre!("ffprobe json had no video stream"))?;

    // ffprobe emits numbers as JSON strings under -show_entries.
    let as_u32 = |v: &serde_json::Value| -> Option<u32> {
        v.as_u64()
            .map(|n| n as u32)
            .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
    };
    let as_f64 = |v: &serde_json::Value| -> Option<f64> {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
    };

    let width = stream.get("width").and_then(as_u32).unwrap_or(0);
    let height = stream.get("height").and_then(as_u32).unwrap_or(0);

    // Duration: prefer the stream duration, fall back to the container format
    // duration (some muxers only populate one).
    let duration_s = stream
        .get("duration")
        .and_then(as_f64)
        .or_else(|| {
            json.get("format")
                .and_then(|f| f.get("duration"))
                .and_then(as_f64)
        })
        .unwrap_or(0.0);

    // Frame count: prefer nb_frames; if absent, derive from avg_frame_rate ×
    // duration (e.g. "30000/1001" → 29.97).
    let frame_count = stream
        .get("nb_frames")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .filter(|&n| n > 0)
        .or_else(|| {
            let afr = stream.get("avg_frame_rate").and_then(|v| v.as_str())?;
            let fps = parse_rational(afr)?;
            if fps > 0.0 && duration_s > 0.0 {
                Some((fps * duration_s).round() as u64)
            } else {
                None
            }
        })
        .unwrap_or(0);

    Ok(RawMeasurement {
        width,
        height,
        frame_count,
        duration_s,
    })
}

/// Parse an ffprobe rational like `"30000/1001"` or `"24/1"` into an f64.
/// Returns `None` for `"0/0"` or malformed input.
fn parse_rational(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if den == 0.0 { None } else { Some(num / den) }
}

// ── Minimal mp4 `moov` parser ───────────────────────────────────────────────
//
// mp4 is a tree of length-prefixed "boxes" (a.k.a. atoms). Each box is:
//   [4 bytes big-endian size][4 bytes type] ... payload ...
// A size of 1 means a 64-bit size follows the type (8 bytes). A size of 0 means
// "to end of file". We only descend into container boxes we care about and read
// scalar fields from the leaf boxes. Layout we rely on (OBS ffmpeg_muxer
// output):
//   moov
//     mvhd                      -> movie timescale + duration (fallback)
//     trak (one per stream)
//       tkhd                    -> track width/height (16.16 fixed point)
//       mdia
//         mdhd                  -> media timescale + duration (authoritative)
//         hdlr                  -> handler type ('vide' = video track)
//         minf/stbl/stsz        -> sample_count (= frame count)
//
// We select the trak whose hdlr is 'vide'. Everything is bounds-checked; a
// short/garbled file yields an Err, never a panic or out-of-bounds read.

/// A parsed box header: payload bounds within the file and the 4-cc type.
struct BoxHeader {
    /// 4-character box type, e.g. *b"moov".
    kind: [u8; 4],
    /// Absolute file offset where this box's payload (after the header) begins.
    payload_start: u64,
    /// Absolute file offset one past the end of this box.
    end: u64,
}

/// Read the entire mp4 into memory and parse it.
///
/// Recording mp4s are large (hundreds of MB), but we only ever index a handful
/// of header boxes. To avoid pulling the whole file into RAM we read it with a
/// seeking reader and only materialize the small leaf-box payloads we need.
fn parse_moov(mp4_path: &Path) -> color_eyre::Result<RawMeasurement> {
    use color_eyre::eyre::eyre;

    let mut file = std::fs::File::open(mp4_path)
        .map_err(|e| eyre!("failed to open {} for moov parse: {e}", mp4_path.display()))?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file_len == 0 {
        return Err(eyre!("video file {} is empty", mp4_path.display()));
    }

    // Find the top-level `moov` box.
    let moov = find_box(&mut file, &[*b"moov"], 0, file_len)?
        .ok_or_else(|| eyre!("no moov box found in {}", mp4_path.display()))?;

    // Movie-level timescale/duration as a fallback when the track's mdhd is
    // missing or zero.
    let (movie_timescale, movie_duration) =
        match find_box(&mut file, &[*b"mvhd"], moov.payload_start, moov.end)? {
            Some(mvhd) => read_mvhd_timescale_duration(&mut file, &mvhd).unwrap_or((0, 0)),
            None => (0, 0),
        };

    // Walk each trak; keep the first one whose handler is 'vide'.
    let mut cursor = moov.payload_start;
    let mut video: Option<VideoTrack> = None;
    while cursor < moov.end {
        let Some(b) = read_box_header(&mut file, cursor, moov.end)? else {
            break;
        };
        if &b.kind == b"trak" {
            if let Some(track) = parse_trak(&mut file, &b)? {
                if track.is_video {
                    video = Some(track);
                    break;
                }
            }
        }
        cursor = b.end;
    }

    let track = video.ok_or_else(|| eyre!("no video track found in {}", mp4_path.display()))?;

    // Resolve duration in seconds: prefer the media (mdhd) timescale/duration;
    // fall back to the movie (mvhd) header.
    let (timescale, duration_units) = if track.timescale > 0 && track.duration > 0 {
        (track.timescale, track.duration)
    } else {
        (movie_timescale, movie_duration)
    };
    let duration_s = if timescale > 0 {
        duration_units as f64 / timescale as f64
    } else {
        0.0
    };

    Ok(RawMeasurement {
        width: track.width,
        height: track.height,
        frame_count: track.sample_count,
        duration_s,
    })
}

/// Fields extracted from a single `trak`.
struct VideoTrack {
    is_video: bool,
    width: u32,
    height: u32,
    sample_count: u64,
    timescale: u32,
    duration: u64,
}

/// Parse a `trak`: read tkhd dimensions, descend mdia→{mdhd, hdlr, stbl/stsz}.
fn parse_trak(
    file: &mut std::fs::File,
    trak: &BoxHeader,
) -> color_eyre::Result<Option<VideoTrack>> {
    // tkhd: track display width/height (16.16 fixed point) live at the tail.
    let (mut width, mut height) = (0u32, 0u32);
    if let Some(tkhd) = find_box(file, &[*b"tkhd"], trak.payload_start, trak.end)? {
        if let Some((w, h)) = read_tkhd_dimensions(file, &tkhd) {
            width = w;
            height = h;
        }
    }

    let Some(mdia) = find_box(file, &[*b"mdia"], trak.payload_start, trak.end)? else {
        return Ok(None);
    };

    // hdlr handler_type tells us whether this is the video track.
    let is_video = match find_box(file, &[*b"hdlr"], mdia.payload_start, mdia.end)? {
        Some(hdlr) => read_hdlr_is_video(file, &hdlr).unwrap_or(false),
        None => false,
    };

    // mdhd: media timescale + duration (authoritative for fps).
    let (timescale, duration) = match find_box(file, &[*b"mdhd"], mdia.payload_start, mdia.end)? {
        Some(mdhd) => read_mvhd_timescale_duration(file, &mdhd).unwrap_or((0, 0)),
        None => (0, 0),
    };

    // stsz sample_count: nested mdia→minf→stbl→stsz. We search the whole mdia
    // subtree for stsz (find_box only scans one level, so descend explicitly).
    let sample_count = match find_box(file, &[*b"minf"], mdia.payload_start, mdia.end)? {
        Some(minf) => match find_box(file, &[*b"stbl"], minf.payload_start, minf.end)? {
            Some(stbl) => match find_box(file, &[*b"stsz"], stbl.payload_start, stbl.end)? {
                Some(stsz) => read_stsz_sample_count(file, &stsz).unwrap_or(0),
                None => 0,
            },
            None => 0,
        },
        None => 0,
    };

    Ok(Some(VideoTrack {
        is_video,
        width,
        height,
        sample_count,
        timescale,
        duration,
    }))
}

/// Read a box header at `offset`, bounded by `limit` (one past the parent
/// payload). Returns `Ok(None)` when there isn't room for another header.
fn read_box_header(
    file: &mut std::fs::File,
    offset: u64,
    limit: u64,
) -> color_eyre::Result<Option<BoxHeader>> {
    use color_eyre::eyre::eyre;
    use std::io::{Read, Seek, SeekFrom};

    if offset + 8 > limit {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut hdr = [0u8; 8];
    if file.read_exact(&mut hdr).is_err() {
        return Ok(None);
    }
    let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
    let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];

    let (payload_start, end) = if size32 == 1 {
        // 64-bit largesize follows the type.
        if offset + 16 > limit {
            return Ok(None);
        }
        let mut large = [0u8; 8];
        file.read_exact(&mut large)?;
        let size64 = u64::from_be_bytes(large);
        let end = offset
            .checked_add(size64)
            .ok_or_else(|| eyre!("box size overflow"))?;
        (offset + 16, end.min(limit))
    } else if size32 == 0 {
        // Extends to end of parent.
        (offset + 8, limit)
    } else {
        let end = offset
            .checked_add(size32)
            .ok_or_else(|| eyre!("box size overflow"))?;
        (offset + 8, end.min(limit))
    };

    if payload_start > end {
        return Err(eyre!("malformed box: payload starts past its end"));
    }
    Ok(Some(BoxHeader {
        kind,
        payload_start,
        end,
    }))
}

/// Scan ONE level of boxes between `[start, limit)` for the first whose type is
/// in `wanted`. Does not descend into children.
fn find_box(
    file: &mut std::fs::File,
    wanted: &[[u8; 4]],
    start: u64,
    limit: u64,
) -> color_eyre::Result<Option<BoxHeader>> {
    let mut cursor = start;
    while cursor < limit {
        let Some(b) = read_box_header(file, cursor, limit)? else {
            break;
        };
        if wanted.iter().any(|w| *w == b.kind) {
            return Ok(Some(b));
        }
        if b.end <= cursor {
            // Zero-length / non-advancing box — stop rather than spin forever.
            break;
        }
        cursor = b.end;
    }
    Ok(None)
}

/// Read `[timescale: u32, duration: u32|u64]` from an `mvhd` or `mdhd` box
/// (identical layout for these fields). Version 0 uses 32-bit times; version 1
/// uses 64-bit. Layout after the 8-byte box header:
///   [1 version][3 flags][... times ...]
/// v0: creation(4) modification(4) timescale(4) duration(4)
/// v1: creation(8) modification(8) timescale(4) duration(8)
fn read_mvhd_timescale_duration(file: &mut std::fs::File, b: &BoxHeader) -> Option<(u32, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let payload_len = b.end.saturating_sub(b.payload_start);
    if payload_len < 4 {
        return None;
    }
    file.seek(SeekFrom::Start(b.payload_start)).ok()?;
    let mut version = [0u8; 4]; // version + 3 flags
    file.read_exact(&mut version).ok()?;
    if version[0] == 1 {
        // Skip creation(8) + modification(8) = 16 bytes.
        let mut buf = [0u8; 16 + 4 + 8];
        if payload_len < 4 + buf.len() as u64 {
            return None;
        }
        file.read_exact(&mut buf).ok()?;
        let timescale = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let duration = u64::from_be_bytes([
            buf[20], buf[21], buf[22], buf[23], buf[24], buf[25], buf[26], buf[27],
        ]);
        Some((timescale, duration))
    } else {
        // v0: skip creation(4) + modification(4) = 8 bytes.
        let mut buf = [0u8; 8 + 4 + 4];
        if payload_len < 4 + buf.len() as u64 {
            return None;
        }
        file.read_exact(&mut buf).ok()?;
        let timescale = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let duration = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]) as u64;
        Some((timescale, duration))
    }
}

/// Read track display width/height from a `tkhd` box. They are the last two
/// 32-bit fields, stored as 16.16 fixed point. Layout after the 8-byte header:
///   [1 version][3 flags]
///   v0: creation(4) modification(4) track_id(4) reserved(4) duration(4)
///   v1: creation(8) modification(8) track_id(4) reserved(4) duration(8)
///   then (both versions): reserved(8) layer(2) altgroup(2) volume(2)
///   reserved(2) matrix(36) width(4) height(4)
/// width/height are the final 8 bytes of the box.
fn read_tkhd_dimensions(file: &mut std::fs::File, b: &BoxHeader) -> Option<(u32, u32)> {
    use std::io::{Read, Seek, SeekFrom};
    let payload_len = b.end.saturating_sub(b.payload_start);
    if payload_len < 8 {
        return None;
    }
    // width/height are the last 8 bytes regardless of version, so just read the
    // tail of the box.
    file.seek(SeekFrom::Start(b.end - 8)).ok()?;
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf).ok()?;
    let width_fixed = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let height_fixed = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    // 16.16 fixed point → integer pixels (round to nearest).
    let width = ((width_fixed as f64) / 65536.0).round() as u32;
    let height = ((height_fixed as f64) / 65536.0).round() as u32;
    Some((width, height))
}

/// Read whether an `hdlr` box describes a video track (`handler_type == 'vide'`).
/// Layout after the 8-byte header:
///   [1 version][3 flags][4 pre_defined][4 handler_type]...
fn read_hdlr_is_video(file: &mut std::fs::File, b: &BoxHeader) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let payload_len = b.end.saturating_sub(b.payload_start);
    if payload_len < 12 {
        return None;
    }
    file.seek(SeekFrom::Start(b.payload_start)).ok()?;
    let mut buf = [0u8; 12]; // version+flags(4) + pre_defined(4) + handler_type(4)
    file.read_exact(&mut buf).ok()?;
    let handler = &buf[8..12];
    Some(handler == b"vide")
}

/// Read `sample_count` (= encoded frame count) from an `stsz` box. Layout after
/// the 8-byte header:
///   [1 version][3 flags][4 sample_size][4 sample_count]...
fn read_stsz_sample_count(file: &mut std::fs::File, b: &BoxHeader) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let payload_len = b.end.saturating_sub(b.payload_start);
    if payload_len < 12 {
        return None;
    }
    file.seek(SeekFrom::Start(b.payload_start)).ok()?;
    let mut buf = [0u8; 12]; // version+flags(4) + sample_size(4) + sample_count(4)
    file.read_exact(&mut buf).ok()?;
    let sample_count = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    Some(sample_count as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn fps_zero_duration_is_zero_not_nan() {
        let raw = RawMeasurement {
            width: 1920,
            height: 1080,
            frame_count: 100,
            duration_s: 0.0,
        };
        assert_eq!(raw.fps(), 0.0);
    }

    #[test]
    fn fps_zero_frames_is_zero() {
        let raw = RawMeasurement {
            width: 1920,
            height: 1080,
            frame_count: 0,
            duration_s: 10.0,
        };
        assert_eq!(raw.fps(), 0.0);
    }

    #[test]
    fn fps_basic_division() {
        let raw = RawMeasurement {
            width: 1920,
            height: 1080,
            frame_count: 300,
            duration_s: 10.0,
        };
        assert!((raw.fps() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn matches_claim_exact_resolution_and_close_fps() {
        let mut v = VideoActual {
            width: 1920,
            height: 1080,
            fps: 29.97,
            frame_count: 9000,
            duration_s: 300.3,
            source: MeasurementSource::Mp4MoovParse,
            matches_claim: false,
        };
        v.matches_claim = v.evaluate_match(1920, 1080, 30.0);
        assert!(v.matches_claim, "29.97 vs 30 within ±1.0 and res exact");
    }

    #[test]
    fn mismatch_on_resolution_even_if_fps_matches() {
        // The real v2.6.15 defect: claimed 1080p, encoded 544p.
        let mut v = VideoActual {
            width: 960,
            height: 544,
            fps: 30.0,
            frame_count: 9000,
            duration_s: 300.0,
            source: MeasurementSource::Mp4MoovParse,
            matches_claim: true,
        };
        v.matches_claim = v.evaluate_match(1920, 1080, 30.0);
        assert!(!v.matches_claim, "544p must never match a 1080p claim");
    }

    #[test]
    fn mismatch_on_fps_even_if_resolution_matches() {
        // The 30→24 fps defect class.
        let mut v = VideoActual {
            width: 1920,
            height: 1080,
            fps: 24.0,
            frame_count: 7200,
            duration_s: 300.0,
            source: MeasurementSource::Mp4MoovParse,
            matches_claim: true,
        };
        v.matches_claim = v.evaluate_match(1920, 1080, 30.0);
        assert!(!v.matches_claim, "24 fps is outside ±1.0 of 30");
    }

    #[test]
    fn fps_tolerance_boundary() {
        let v = VideoActual {
            width: 1280,
            height: 720,
            fps: 29.0,
            frame_count: 8700,
            duration_s: 300.0,
            source: MeasurementSource::Mp4MoovParse,
            matches_claim: false,
        };
        // exactly 1.0 away → inside (<=)
        assert!(v.evaluate_match(1280, 720, 30.0));
        // 1.01 away → outside
        assert!(!v.evaluate_match(1280, 720, 30.01));
    }

    #[test]
    fn source_strings_are_stable() {
        assert_eq!(MeasurementSource::Ffprobe.as_str(), "ffprobe");
        assert_eq!(MeasurementSource::Mp4MoovParse.as_str(), "mp4_moov_parse");
    }

    #[test]
    fn parse_rational_handles_ntsc_and_garbage() {
        assert!((parse_rational("30000/1001").unwrap() - 29.97002997).abs() < 1e-6);
        assert!((parse_rational("24/1").unwrap() - 24.0).abs() < 1e-9);
        assert_eq!(parse_rational("0/0"), None);
        assert_eq!(parse_rational("notarational"), None);
    }

    #[test]
    fn parse_ffprobe_json_uses_nb_frames_when_present() {
        let json = serde_json::json!({
            "streams": [{
                "width": 1920,
                "height": 1080,
                "nb_frames": "9000",
                "duration": "300.300000",
                "avg_frame_rate": "30000/1001"
            }],
            "format": { "duration": "300.300000" }
        });
        let raw = parse_ffprobe_json(&json).unwrap();
        assert_eq!(raw.width, 1920);
        assert_eq!(raw.height, 1080);
        assert_eq!(raw.frame_count, 9000);
        assert!((raw.duration_s - 300.3).abs() < 1e-6);
    }

    #[test]
    fn parse_ffprobe_json_derives_frames_from_avg_rate_when_nb_frames_absent() {
        let json = serde_json::json!({
            "streams": [{
                "width": 960,
                "height": 544,
                "duration": "300.000000",
                "avg_frame_rate": "24/1"
            }],
            "format": { "duration": "300.000000" }
        });
        let raw = parse_ffprobe_json(&json).unwrap();
        assert_eq!(raw.frame_count, 7200); // 24 * 300
        assert_eq!(raw.width, 960);
        assert_eq!(raw.height, 544);
    }

    #[test]
    fn parse_ffprobe_json_falls_back_to_format_duration() {
        let json = serde_json::json!({
            "streams": [{
                "width": 1280,
                "height": 720,
                "nb_frames": "3000"
            }],
            "format": { "duration": "100.0" }
        });
        let raw = parse_ffprobe_json(&json).unwrap();
        assert!((raw.duration_s - 100.0).abs() < 1e-9);
    }

    // ── moov parser tests on synthetic, minimal mp4 box trees ──────────────

    /// Build a box: 4-byte size + 4-byte type + payload.
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut v = Vec::with_capacity(size as usize);
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    /// A version-0 mvhd/mdhd payload with the given timescale + duration.
    fn mvhd_v0_payload(timescale: u32, duration: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&[0u8; 4]); // version 0 + flags
        p.extend_from_slice(&[0u8; 4]); // creation
        p.extend_from_slice(&[0u8; 4]); // modification
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&duration.to_be_bytes());
        p.extend_from_slice(&[0u8; 4]); // rate (pad so box has room)
        p
    }

    /// A tkhd payload (v0) whose final 8 bytes encode width/height in 16.16.
    fn tkhd_v0_payload(width: u32, height: u32) -> Vec<u8> {
        // Header up to the matrix is irrelevant to our reader (it reads the
        // last 8 bytes), so we just pad to a realistic length then append
        // width/height fixed-point.
        let mut p = vec![0u8; 4 + 4 + 4 + 4 + 4 + 4 + 8 + 2 + 2 + 2 + 2 + 36];
        p[0] = 0; // version 0
        p.extend_from_slice(&(width << 16).to_be_bytes());
        p.extend_from_slice(&(height << 16).to_be_bytes());
        p
    }

    fn hdlr_payload(handler: &[u8; 4]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&[0u8; 4]); // version + flags
        p.extend_from_slice(&[0u8; 4]); // pre_defined
        p.extend_from_slice(handler); // handler_type
        p.extend_from_slice(&[0u8; 12]); // reserved
        p
    }

    fn stsz_payload(sample_count: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&[0u8; 4]); // version + flags
        p.extend_from_slice(&[0u8; 4]); // sample_size (0 = per-sample table)
        p.extend_from_slice(&sample_count.to_be_bytes());
        p
    }

    /// Assemble a minimal but well-formed mp4 with one video trak.
    fn synthetic_mp4(
        width: u32,
        height: u32,
        sample_count: u32,
        timescale: u32,
        duration: u32,
    ) -> Vec<u8> {
        let stsz = boxed(b"stsz", &stsz_payload(sample_count));
        let stbl = boxed(b"stbl", &stsz);
        let minf = boxed(b"minf", &stbl);
        let mdhd = boxed(b"mdhd", &mvhd_v0_payload(timescale, duration));
        let hdlr = boxed(b"hdlr", &hdlr_payload(b"vide"));
        let mut mdia_payload = Vec::new();
        mdia_payload.extend_from_slice(&mdhd);
        mdia_payload.extend_from_slice(&hdlr);
        mdia_payload.extend_from_slice(&minf);
        let mdia = boxed(b"mdia", &mdia_payload);
        let tkhd = boxed(b"tkhd", &tkhd_v0_payload(width, height));
        let mut trak_payload = Vec::new();
        trak_payload.extend_from_slice(&tkhd);
        trak_payload.extend_from_slice(&mdia);
        let trak = boxed(b"trak", &trak_payload);
        let mvhd = boxed(b"mvhd", &mvhd_v0_payload(timescale, duration));
        let mut moov_payload = Vec::new();
        moov_payload.extend_from_slice(&mvhd);
        moov_payload.extend_from_slice(&trak);
        let moov = boxed(b"moov", &moov_payload);
        // Prepend an ftyp box and a (fake) mdat so moov isn't at offset 0,
        // mirroring a real faststart file's general shape.
        let ftyp = boxed(b"ftyp", b"isom\x00\x00\x02\x00isomiso2");
        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&moov);
        file
    }

    fn write_temp(bytes: &[u8]) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("mp4_probe_test_{nanos}.mp4"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        f.sync_all().unwrap();
        path
    }

    #[test]
    fn moov_parser_reads_synthetic_1080p30() {
        let bytes = synthetic_mp4(1920, 1080, 9000, 30000, 9_000_000); // 300s
        let path = write_temp(&bytes);
        let raw = parse_moov(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(raw.width, 1920);
        assert_eq!(raw.height, 1080);
        assert_eq!(raw.frame_count, 9000);
        assert!((raw.duration_s - 300.0).abs() < 1e-6);
        assert!((raw.fps() - 30.0).abs() < 1e-6);
    }

    #[test]
    fn moov_parser_reads_the_real_defect_shape_960x544_24fps() {
        // 7585 frames over 316.04s ≈ 24 fps, 960x544 — the exact v2.6.15 lie.
        let bytes = synthetic_mp4(960, 544, 7585, 24000, 7_585_000);
        let path = write_temp(&bytes);
        let actual = measure_encoded_video(&path, 1920, 1080, 30.0).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(actual.width, 960);
        assert_eq!(actual.height, 544);
        assert_eq!(actual.frame_count, 7585);
        assert!((actual.fps - 24.0).abs() < 0.01);
        assert!(
            !actual.matches_claim,
            "must flag the 960x544@24 encode as NOT matching the 1080p@30 claim"
        );
        // With no bundled ffprobe in the test environment, the parser is used.
        assert_eq!(actual.source, MeasurementSource::Mp4MoovParse);
    }

    #[test]
    fn moov_parser_matches_a_truthful_recording() {
        let bytes = synthetic_mp4(1920, 1080, 9009, 30000, 9_018_009); // ~300.6s → ~29.97
        let path = write_temp(&bytes);
        let actual = measure_encoded_video(&path, 1920, 1080, 30.0).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            actual.matches_claim,
            "1920x1080 @ ~29.97 must match a 1080p@30 claim (±1.0 fps)"
        );
    }

    #[test]
    fn empty_file_errors_cleanly() {
        let path = write_temp(&[]);
        let err = parse_moov(&path);
        let _ = std::fs::remove_file(&path);
        assert!(err.is_err(), "empty file must error, not panic");
    }

    #[test]
    fn truncated_before_moov_errors_cleanly() {
        // Just an ftyp, no moov.
        let ftyp = boxed(b"ftyp", b"isom\x00\x00\x02\x00isomiso2");
        let path = write_temp(&ftyp);
        let err = parse_moov(&path);
        let _ = std::fs::remove_file(&path);
        assert!(err.is_err(), "missing moov must error, not panic");
    }

    #[test]
    fn serialized_video_actual_has_expected_shape() {
        let v = VideoActual {
            width: 960,
            height: 544,
            fps: 24.0,
            frame_count: 7585,
            duration_s: 316.04,
            source: MeasurementSource::Mp4MoovParse,
            matches_claim: false,
        };
        let value = serde_json::to_value(&v).unwrap();
        assert_eq!(value["width"], 960);
        assert_eq!(value["height"], 544);
        assert_eq!(value["frame_count"], 7585);
        assert_eq!(value["source"], "mp4_moov_parse");
        assert_eq!(value["matches_claim"], false);
        assert!(value["fps"].is_number());
        assert!(value["duration_s"].is_number());
    }
}
