# Capture Performance Investigation: 286 frames / 286 seconds

> Status: read-only source analysis. No code changed.
> Scope: explain why a real-data bundle reports 286 frames for a 286-second
> recording while WGC was attempting ~30 fps and the OBS encoder reported only
> 34 skipped frames out of 8598 attempts.
> Conclusion: with **HIGH confidence**, the 8312 "missing" frames were never
> dropped. The recorder file `frames.jsonl` and `fps_log.json` count
> `Recorder::poll()` invocations (1 Hz), not encoder frame deliveries. The MP4
> on disk almost certainly contains the full ~30 fps stream.

---

## 1. Symptom

Howard's bundle, summarised:

| Field                          | Value                                |
| ------------------------------ | ------------------------------------ |
| Wall-clock duration            | 286 s                                |
| `frames.jsonl` rows            | 286                                  |
| Apparent fps from those rows   | 1.0 fps                              |
| WGC capture attempts (logged)  | ~8598                                |
| OBS skipped-frames log line    | `34 / 8598` (0.4%)                   |
| Configured target FPS          | 30 (`constants::FPS`)                |
| Configured resolution          | 1920 × 1080                          |
| Encoder                        | HEVC, 10 Mbps, CBR, 2 B-frames       |

The discrepancy in plain numbers:

- WGC delivered ≈ 30 × 286 ≈ 8580 frames.
- OBS reports it skipped 34 of those due to encoder lag.
- Therefore ~ 8546 frames should have reached the muxer and been written to
  `recording.mp4`.
- But `frames.jsonl` only has 286 rows (1 per second).

The hypothesis "WGC capture is inadequate" does not fit the OBS log line —
OBS is telling us 8564 frames *did* arrive at the encoder. The mismatch is
elsewhere.

---

## 2. Pipeline trace (file:line)

```
WGC source (libobs win-capture, method=2)
    obs_embedded_recorder.rs:2061-2143  prepare_source() builds window_capture
                                        with WGC_CAPTURE_METHOD_WGC = 2
        │
        ▼
OBS internal video pipeline (FPS=30, 1920x1080)
    obs_embedded_recorder.rs:1730-1760  video_info() — fps_num=FPS=30, fps_den=1
    crates/constants/src/lib.rs:290     pub const FPS: u32 = 30;
    crates/constants/src/lib.rs:291-292 RECORDING_WIDTH=1920, RECORDING_HEIGHT=1080
        │
        ▼
HEVC encoder (NvEncHevc/AmfHevc/QsvHevc, 10 Mbps CBR)
    crates/constants/src/encoding.rs:94 BITRATE = 10_000
    crates/constants/src/encoding.rs:97 RATE_CONTROL = "CBR"
    crates/constants/src/encoding.rs:100 B_FRAMES = 2
        │
        ▼
ffmpeg_muxer output → recording.mp4
    obs_embedded_recorder.rs:874        OutputInfo::new("ffmpeg_muxer", ...)
    obs_embedded_recorder.rs:1194       self.output.start()
        │
        ▼ (skipped-frames count surfaces via OBS log line)
TracingObsLogger
    obs_embedded_recorder.rs:2206-2224  parses "number of skipped frames due
                                        to encoding lag: X/Y" → SkippedFrames
                                        and signals skipped_frames_notify
        │
        ▼
Phase-2 stop merges SkippedFrames into metadata
    obs_embedded_recorder.rs:1285-1316  stop_recording_phase2(): folds
                                        {skipped, total} into settings JSON
                                        and bails if percentage > 5%
```

A separate, parallel "frame counter" path runs entirely on the **tokio side**,
disconnected from the actual frame pipeline:

```
tokio_thread.rs:151    perform_checks = tokio::time::interval(Duration::from_secs(1))
        │
        ▼ once per second
tokio_thread.rs:848-907   perform_checks.tick() arm of select! → state.tick()
        │
        ▼
tokio_thread.rs:1486      self.recorder.poll(&self.input_capture).await
        │
        ▼
recorder.rs:341-355       Recorder::poll() — calls video_recorder.poll()
                          and forwards `update.active_fps` if Some
        │
        ▼
obs_embedded_recorder.rs:449-452   <ObsEmbeddedRecorder as VideoRecorder>::poll()
                                   returns PollUpdate {
                                       active_fps: Some(obs_get_active_fps()),
                                       ...
                                   }
        │  (active_fps here is OBS's *instantaneous* fps gauge, e.g. 30.0,
        │   sampled once per second — it is NOT a frame-arrival count)
        ▼
recorder.rs:352-354       recording.update_fps(fps)
        │
        ▼
recording.rs:262-271      Recording::update_fps:
                              fps_sample_count += 1                  // ← 1 per second
                              average_fps = running cumulative avg
                              self.fps_logger.on_frame()             // ← 1 per second!
        │
        ▼
fps_logger.rs:82-110      FpsLogger::on_frame:
                              total_frames += 1                      // ← 1 per second
                              frame_timestamps.push(FrameTimestamp { idx, t_ns })
        │
        ▼
fps_logger.rs:161-201     FpsLogger::save() flushes:
                              total_frames → metadata.frame_count    // ← 286 over 286s
                              frame_timestamps → frames.jsonl        // ← 286 rows
                              entries → fps_log.json                 // ← 1 fps per second
```

This second path has **no connection** to actual frame delivery. It increments
once per `tokio::time::interval(1s)` tick.

---

## 3. What `skipped_frames` actually counts (file:line evidence)

There are *three* different things called "frames" in this codebase. They are
not the same and the naming has caused confusion:

### 3a. `SkippedFrames { skipped, total }` — encoder-side, authoritative

- Defined: `src/record/obs_embedded_recorder.rs:2143-2157`
- Populated: `src/record/obs_embedded_recorder.rs:2206-2224`
  by parsing the OBS log line
  `"number of skipped frames due to encoding lag: X/TOTAL"`.
- Parser: `parse_skipped_frames` at
  `src/record/obs_embedded_recorder.rs:2232-2261`.
- Emitted by libobs at output stop, where `TOTAL` is the number of frames OBS
  *attempted to push into the encoder* during the recording, and `X` is the
  count it had to drop because the encoder could not keep up.
- This is the **only counter that reflects real WGC → encoder throughput**.
- Howard's bundle: `34 / 8598` — i.e. 8564 frames were successfully encoded
  and muxed into `recording.mp4`.

### 3b. `FrameStats { total_frames, dropped_frames, duration }` — derived from 3a

- Defined: `src/record/metadata_writer.rs:53-77`.
- Populated by reusing the `SkippedFrames` numbers parsed above; this is where
  the post-audit `fps_actual = (total - dropped) / duration` lives
  (`metadata_writer.rs:69-76`).
- Comment at `metadata_writer.rs:18-22` is explicit:
  > `fps_actual` / `average_fps`: was a heartbeat-count approximation;
  > now uses `FrameStats { total_frames, dropped_frames, duration }`
  > parsed from OBS's `"number of skipped frames due to encoding lag:
  > X/TOTAL"` log line by `TracingObsLogger`.
- So `hardware.json::average_fps` is honest: ≈ (8598-34)/286 ≈ 29.95 fps.

### 3c. `FpsLogger::total_frames` / `frame_timestamps` — **NOT a frame counter**

- Defined: `src/record/fps_logger.rs:48-67`.
- Incremented exclusively in `FpsLogger::on_frame()` at
  `src/record/fps_logger.rs:82-110` (`self.total_frames += 1`).
- `on_frame` is called only from
  `src/record/recording.rs:262-271 :: Recording::update_fps`.
- `update_fps` is called only from `src/record/recorder.rs:352-354 ::
  Recorder::poll`.
- `Recorder::poll` is called once per second from
  `src/tokio_thread.rs:848 + 1486` via
  `tokio::time::interval(Duration::from_secs(1))`
  (`src/tokio_thread.rs:151-152`).
- Therefore `total_frames` here is **the count of polling ticks**, not the
  count of frames captured by WGC or written by the encoder.
- This counter is what gets written to:
  - `metadata.json::frame_count` via `local_recording.rs:710`
  - `metadata.json::fps_effective` via `local_recording.rs:646-652`
    (`frame_count / duration` → ~1 fps from a 1 Hz sampler)
  - `frames.jsonl` (one line per tick, `idx` and `t_ns`) via
    `fps_logger.rs:181-198`
  - `fps_log.json` per-second entries via `fps_logger.rs:172-179`.

The variable name `total_frames` and the file name `frames.jsonl` are
**misnomers** — they imply per-frame ground truth but the data underneath is
a 1 Hz heartbeat scaled by whatever OBS's instantaneous fps gauge happened to
report at each tick.

The value passed in (`update.active_fps`, sourced from
`libobs_wrapper::sys::obs_get_active_fps()` at
`obs_embedded_recorder.rs:450`) **is not used** to weight the increment —
`on_frame()` ignores its argument entirely
(`fps_logger.rs:82` takes `&mut self` only). So even if OBS correctly reports
30 fps every tick, the counter still goes up by exactly 1 per call.

---

## 4. Likely root cause (ranked)

### Hypothesis #1 (HIGH confidence): The 8312 frames are NOT lost. The metric is wrong.

Evidence:
- WGC attempted 8598 frames (consistent with 30 fps × 286 s).
- OBS encoder log says only 34 were dropped due to lag.
- `frames.jsonl` has 286 rows because the writer is a 1 Hz polling heartbeat
  (proof in §3c above: every entry is generated by a `tokio::time::interval(1s)`
  tick, not by a frame callback).
- The MP4 on disk was muxed by libobs with all 8564 successful frames; we have
  no source-side mechanism that would drop frames between encoder and muxer
  silently. OBS's `ffmpeg_muxer` (`obs_embedded_recorder.rs:874`) writes
  every encoder packet it receives.

What "needs runtime evidence":
- Confirmation that `recording.mp4` actually contains ~8564 frames.
  Anyone with the bundle can run
  `ffprobe -v error -count_frames -select_streams v:0 -show_entries
  stream=nb_read_frames recording.mp4` to verify.
- If `nb_read_frames ≈ 8500`, hypothesis #1 is proven and the work is purely
  to fix the metric, not the pipeline.
- If `nb_read_frames ≈ 286`, hypothesis #1 is wrong — drop down to #2/#3.

### Hypothesis #2 (LOW confidence): Encoder is silently dropping after libobs's accounting

Evidence: the OBS log line is libobs's own accounting; if the GPU encoder's
internal queue stalls, OBS would report it as `skipped_frames_due_to_lag`,
not silently drop. There is no code in this repo that interposes between OBS
and the muxer.

This hypothesis would only hold if:
- A driver-level GPU fault truncated the encoded bitstream.
- The MP4 muxer encountered a write error and silently truncated.
- `durable_write::fsync_file` (called at `recording.rs:432-458`) is run on
  the *closed* MP4 well after libobs has released it, but if the file were
  truncated by then, fsync wouldn't notice.

Cannot be proven from source-reading alone — needs `ffprobe` on the produced
MP4.

### Hypothesis #3 (LOW confidence): WGC is throttled to 1 fps by configuration

Evidence against:
- `video_info()` at `obs_embedded_recorder.rs:1750-1759` hardcodes
  `fps_num=FPS=30`, `fps_den=1`. There is no per-game or runtime override.
- The "method=2 forces WGC" path at `obs_embedded_recorder.rs:2090,2109` does
  not set any frame-rate property on the source (WGC takes the OBS video
  pipeline's fps).
- WGC's reported attempts are 8598 — internally consistent with 30 fps.
  If WGC were running at 1 fps we'd see ~286 attempts in the OBS log, not
  8598.

So the WGC source itself is delivering at 30 fps. Throttling would have to
come from a property we don't set, which contradicts the libobs default
behaviour.

---

## 5. Remediation options

Effort × impact ranking. Top-of-list = lowest effort × highest payoff.

### Option A (recommended): Fix the metric, not the pipeline.

The cheapest, most correct fix.

**Files to touch:**
- `src/record/recorder.rs:341-355` — split `Recorder::poll`'s two
  responsibilities (gauge-reporting for the UI, and "advance the frame log").
  The gauge stays. The frame-log call is removed.
- `src/record/recording.rs:262-271` — `Recording::update_fps` should stop
  calling `self.fps_logger.on_frame()`. The running-average code can stay
  (it's actually correct — it averages the OBS gauge readings, which is what
  `metadata.json::average_fps` is for).
- `src/record/fps_logger.rs:82-110` — repurpose `on_frame` so that it is
  driven by the **encoder's frame callback**, not the tokio heartbeat. Two
  realistic options:
  1. **Best:** subscribe to OBS's encoder frame callback (the same callback
     that increments libobs's internal counter that emits the
     `"number of skipped frames due to encoding lag: X/Y"` log line at stop).
     This gives ground-truth per-frame `t_ns` synced to encoder time.
  2. **Simpler:** at stop, derive `frames.jsonl` synthetically from the OBS
     `total = 8598` figure: emit `total - dropped` rows at `t_ns =
     idx * (1e9 / FPS)`. This loses real per-frame jitter info but is honest
     about the count.
- `src/record/local_recording.rs:646-652,710` — rename
  `metadata.json::frame_count` to its real semantics or repoint it at the
  fixed counter above. Treat the legacy name as deprecated rather than
  dropping it (per CLAUDE.md "Never remove event types").

**Effort:** small (≈ 100-150 LOC, ≈ 1 day with tests). No new deps.
**Impact:** the metric stops lying. Downstream training tooling that reads
`frames.jsonl` to align inputs to frames stops being off by a factor of 30.

### Option B: Prove the MP4 is intact, then publish a sanity-check tool.

Add a stop-time validator that runs `ffprobe -count_frames` on the produced
MP4 and writes the *real* frame count to a new metadata field (e.g.
`metadata.json::mp4_frame_count`). This is a paranoia belt to put alongside
Option A.

**Files:** add a stage in
`src/record/recording.rs:432-487 :: Recording::stop` after the existing
`fsync_file` call (`recording.rs:438`); reuse
`src/record/video_metadata.rs:43-58 :: extract_keyframes_with_ffprobe`'s
ffprobe-spawning pattern.

**Effort:** small (≈ 50 LOC). Dependency on `ffprobe` is already required by
the project (`video_metadata.rs:45-58`).
**Impact:** future regressions where the MP4 *really is* truncated will be
caught. Today's bundle would have shown `mp4_frame_count ≈ 8564` and
collapsed the entire investigation.

### Option C: Hook the encoder callback directly (highest fidelity, more work).

If the data-team requirement is "real per-frame `t_ns` matching what's in the
MP4," Option A's "synthetic" path is not enough — synthetic `t_ns` discards
real jitter.

Hook a libobs encoder frame callback (analogous to how
`TracingObsLogger` already taps the OBS log stream — see
`obs_embedded_recorder.rs:2160-2230`) and feed each callback into a new
`FpsLogger::on_encoded_frame(pts_ns)` method. This requires a `libobs_wrapper`
API surface that may not exist today; check
`crates/` and `Cargo.toml` for what `libobs-wrapper` exposes.

**Effort:** medium (≈ 1 week, plus possible upstream patch to
`libobs-wrapper`).
**Impact:** the only way to get truly authoritative per-frame timestamps
without re-parsing the MP4 after stop.

### Bonus rip-cord: tighten the validator threshold.

`src/record/recording.rs:355-362` rejects recordings whose
`average_fps < MIN_AVERAGE_FPS`. `MIN_AVERAGE_FPS = 5.0` at
`crates/constants/src/lib.rs:313`. With `average_fps` coming from OBS's
gauge (correct, ~30 fps), this threshold catches nothing. Once the metric is
fixed (Option A), reconsider whether 5.0 is still the right number.
This is a **read-only observation** — do not change without a separate
discussion: today's recordings rely on it being lenient.

---

## 6. Diagnostic next steps (data Howard could collect)

Ranked by information density per minute of effort.

1. **`ffprobe -v error -count_frames -select_streams v:0 -show_entries
    stream=nb_read_frames -of default=nokey=1:noprint_wrappers=1
    /path/to/recording.mp4`**
   Single command. Produces the real frame count. If ≥ 8000, hypothesis #1
   is proven and the rest of this doc is the plan. If ≈ 286, escalate to
   hypothesis #2.

2. **Inspect the bundle's `metadata.json` for `average_fps`.**
   Path: same recording folder. Code path:
   `local_recording.rs:707` writes `average_fps` from
   `recording.rs:265-268` (the running cumulative average of OBS gauge
   readings). If this value is ~30, it confirms the OBS gauge was delivering
   30 fps the whole time and the only thing wrong is the
   `frame_count` / `frames.jsonl` writer. If this value is ~1, then OBS itself
   was reporting 1 fps and we have a real WGC/encoder problem.

3. **Inspect the bundle's `metadata.json` for `recorder_extra.skipped_frames`
    object.**
   Code path: `obs_embedded_recorder.rs:1300-1305` injects
   `{skipped: 34, total: 8598}` into `settings.skipped_frames`, which is
   passed up via `stop_recording`'s return value and ends up under
   `metadata.json::recorder_extra.skipped_frames`. If `total` is ~8598,
   hypothesis #1 is essentially proven without even running ffprobe.

4. **Search the OBS log file** (the bundle should include the application's
   log; tracing target `obs` writes via `TracingObsLogger`) for the literal
   line `"number of skipped frames due to encoding lag:"` — if it's there
   and reads `34 / 8598` (Howard already cited this), and if step 2 shows
   `average_fps ≈ 30`, the case is closed without further data.

5. **Only if 1-4 leave doubt:** `cargo flamegraph --release --bin
    gamedata-recorder` for a representative run on Howard's hardware. The
   tokio loop runs at 1 Hz so it should be invisible in the flamegraph; the
   OBS thread should show steady encoder work at 30 fps. A flamegraph would
   only matter if hypothesis #2 is in play (real encoder stalls).

6. **Optional, lowest priority:** Perfetto trace via `tracing-perfetto` —
   would pin down per-frame latencies if the goal is fidelity tuning rather
   than answering today's question. Not necessary for this investigation.

---

## 7. Top-line recommendation

Run step 1 (`ffprobe -count_frames`) before anyone touches code. Expected
result: ~8564 frames in `recording.mp4`. If confirmed, take **Option A** to
fix the metric and **Option B** to add an MP4-frame-count guard to metadata.
Total expected effort: ≈ 1.5 days for one engineer. No need for Option C
unless the data team specifically asks for real per-frame `t_ns`.

If `ffprobe` shows the MP4 is actually truncated, this doc is wrong and the
investigation needs to pivot to hypotheses #2/#3 with runtime-side
evidence — flamegraph, OBS log scan for muxer/encoder errors, GPU memory
events.
