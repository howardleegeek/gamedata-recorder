mod action_camera_writer;
pub(crate) mod fps_logger;
mod input_recorder;
mod local_recording;
mod obs_embedded_recorder;
mod obs_socket_recorder;
mod recorder;
mod recording;

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
