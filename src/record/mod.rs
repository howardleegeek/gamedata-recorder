mod action_camera_writer;
pub(crate) mod adaptive_capture;
pub(crate) mod fps_logger;
mod input_recorder;
mod local_recording;
mod obs_embedded_recorder;
mod obs_socket_recorder;
mod recorder;
mod recording;
/// Stream BN (rc17.2): post-session lint v3 self-validation hook.
/// Re-runs the 32-criteria PRD lint after `Recording::stop()` and
/// writes `lint_result.json` + toast on FAIL. See module docs.
pub mod validation;

/// Stream BJ (rc17.2.2): gameinfo.xlsx writer. Generates per-session
/// Excel workbook with Session / GameEvents / BlockStats / BiomeVisits
/// sheets via shell-out to Python openpyxl. Called from
/// `Recording::stop()` finalize after metadata flush.
pub mod gameinfo_writer;

/// Stream BJ (rc17.2.2): depth EXR writer. Generates per-frame
/// 32-bit float depth maps at 1080×1080 via DepthAnything V2 +
/// onnxruntime-directml shell-out. 1 Hz cadence matching
/// frames.jsonl entries. Called from `Recording::stop()` after
/// metadata flush.
pub mod depth_exr_writer;

// LEM format modules
pub mod lem_input_recorder;
pub mod metadata_writer;
pub mod session_manager;
pub mod video_metadata;

pub use local_recording::{
    LocalRecording, LocalRecordingInfo, LocalRecordingPaused, UploadProgressState,
};
pub use recorder::{Recorder, get_foregrounded_game};
pub use recording::get_recording_base_resolution;

// LEM format re-exports
pub use lem_input_recorder::{LemInputRecorder, LemInputStream};
pub use metadata_writer::MetadataWriter;
pub use session_manager::SessionManager;
pub use video_metadata::VideoMetadataExtractor;
