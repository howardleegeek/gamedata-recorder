use std::{
    path::PathBuf,
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use constants::{encoding::VideoEncoderType, unsupported_games::UnsupportedGames};
use egui_wgpu::wgpu;
use tokio::sync::{broadcast, mpsc};

use crate::{
    config::Config,
    play_time::PlayTimeTracker,
    record::LocalRecording,
    upload::{ProgressData, UploadTrigger},
};

pub struct AppState {
    /// holds the current state of recording, recorder <-> overlay
    pub state: RwLock<RecordingStatus>,
    pub config: RwLock<Config>,
    pub async_request_tx: mpsc::Sender<AsyncRequest>,
    pub ui_update_tx: UiUpdateSender,
    pub ui_update_unreliable_tx: broadcast::Sender<UiUpdateUnreliable>,
    pub adapter_infos: Vec<wgpu::AdapterInfo>,
    pub upload_pause_flag: Arc<AtomicBool>,
    /// Sender for upload triggers. The upload worker task owns the receiver and
    /// maintains its own dedup set, so enqueueing is branch-free and race-free.
    /// See [`crate::upload::UploadTrigger`] for the trigger kinds.
    pub upload_trigger_tx: mpsc::UnboundedSender<UploadTrigger>,
    /// Displayed pending-upload count, kept in sync by the upload worker.
    /// Read-only for everything outside the worker; the worker is the single writer.
    pub auto_upload_queue_count: Arc<AtomicUsize>,
    /// Flag indicating an upload is currently in progress
    pub upload_in_progress: Arc<AtomicBool>,
    /// Hotkey-rebind state machine. Atomic / CAS-driven so that concurrent
    /// "begin listening" clicks (UI thread) cannot race past a captured key
    /// (tokio thread). See [`AtomicListeningForNewHotkey`] for the state
    /// transition rules — all reads/writes must go through its methods.
    pub listening_for_new_hotkey: AtomicListeningForNewHotkey,
    pub is_out_of_date: AtomicBool,
    pub play_time_state: RwLock<PlayTimeTracker>,
    pub last_foregrounded_game: RwLock<Option<ForegroundedGame>>,
    /// The exe name (e.g. "game.exe") of the last application that was recognised as recordable.
    /// Used by the games settings UI to offer per-game configuration.
    pub last_recordable_game: RwLock<Option<String>>,
    pub unsupported_games: RwLock<UnsupportedGames>,
    /// Offline mode state
    pub offline: OfflineState,
    /// Upload filters for date range filtering
    pub upload_filters: RwLock<UploadFilters>,
}

/// State for offline mode and backoff retry logic
pub struct OfflineState {
    /// Flag for offline mode - skips API server calls when enabled
    pub mode: AtomicBool,
    /// Whether offline backoff retry is currently active
    pub backoff_active: AtomicBool,
    /// Timestamp (as seconds since UNIX epoch) of when the next offline retry will occur
    pub next_retry_time: AtomicU64,
    /// Current retry count for offline backoff (used to display in UI)
    pub retry_count: AtomicU32,
}

impl Default for OfflineState {
    fn default() -> Self {
        Self {
            mode: AtomicBool::new(false),
            backoff_active: AtomicBool::new(false),
            next_retry_time: AtomicU64::new(0),
            retry_count: AtomicU32::new(0),
        }
    }
}
impl AppState {
    pub fn new(
        async_request_tx: mpsc::Sender<AsyncRequest>,
        ui_update_tx: UiUpdateSender,
        ui_update_unreliable_tx: broadcast::Sender<UiUpdateUnreliable>,
        adapter_infos: Vec<wgpu::AdapterInfo>,
        upload_trigger_tx: mpsc::UnboundedSender<UploadTrigger>,
    ) -> Self {
        tracing::debug!("AppState::new() called");
        tracing::debug!("Loading configuration");
        let state = Self {
            state: RwLock::new(RecordingStatus::Stopped),
            config: RwLock::new(Config::load().expect("failed to init configs")),
            async_request_tx,
            ui_update_tx,
            ui_update_unreliable_tx,
            adapter_infos,
            upload_pause_flag: Arc::new(AtomicBool::new(false)),
            upload_trigger_tx,
            auto_upload_queue_count: Arc::new(AtomicUsize::new(0)),
            upload_in_progress: Arc::new(AtomicBool::new(false)),
            listening_for_new_hotkey: AtomicListeningForNewHotkey::new(),
            is_out_of_date: AtomicBool::new(false),
            play_time_state: RwLock::new(PlayTimeTracker::load()),
            last_foregrounded_game: RwLock::new(None),
            last_recordable_game: RwLock::new(None),
            unsupported_games: RwLock::new(UnsupportedGames::load_from_embedded()),
            offline: OfflineState::default(),
            upload_filters: RwLock::new(UploadFilters::default()),
        };
        tracing::debug!("AppState::new() complete");
        state
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct UploadFilters {
    pub start_date: Option<chrono::NaiveDate>,
    pub end_date: Option<chrono::NaiveDate>,
}

#[derive(Clone, PartialEq)]
pub struct ForegroundedGame {
    pub exe_name: Option<String>,
    pub unsupported_reason: Option<String>,
}
impl ForegroundedGame {
    pub fn is_recordable(&self) -> bool {
        self.unsupported_reason.is_none()
    }
}

/// This is meant to be a read-only reflection of the current recording state that is
/// only updated by the recorder.rs object (not tokio_thread RecordingState), and read by UI and overlay threads.
/// We want the RecordingStatus to reflect ground truth, and its also more accurate to get ::Recording info
/// directly from the recorder object. Desync between RecordingStatus and RecordingState shouldn't occur either way.
#[derive(Clone, PartialEq)]
pub enum RecordingStatus {
    Stopped,
    Recording {
        start_time: Instant,
        game_exe: String,
        current_fps: Option<f64>,
    },
    Paused,
}
impl RecordingStatus {
    pub fn is_recording(&self) -> bool {
        matches!(self, RecordingStatus::Recording { .. })
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ListeningForNewHotkey {
    NotListening,
    Listening {
        target: HotkeyRebindTarget,
    },
    Captured {
        target: HotkeyRebindTarget,
        key: u16,
    },
}
impl ListeningForNewHotkey {
    pub fn listening_hotkey_target(&self) -> Option<HotkeyRebindTarget> {
        match self {
            ListeningForNewHotkey::Listening { target } => Some(*target),
            _ => None,
        }
    }
}

#[derive(PartialEq, Clone, Copy, Eq, Debug)]
pub enum HotkeyRebindTarget {
    /// Listening for start key
    Start,
    /// Listening for stop key
    Stop,
}

/// Lock-free container for [`ListeningForNewHotkey`].
///
/// The previous `RwLock<ListeningForNewHotkey>` was TOCTOU-unsafe: two UI
/// threads (or the UI thread + the tokio raw-input loop) could both observe
/// `NotListening`, both write `Listening{target}`, and the later write
/// silently overrode the earlier one. Under real user-click timing this
/// rarely bit, but the audit (R33 in MEGA_AUDIT.md) flagged it and we want
/// to close it for good.
///
/// The state is packed into a single `AtomicU32`:
/// ```text
///   bits 0..=1  : tag       (0=NotListening, 1=Listening, 2=Captured)
///   bit  2      : target    (0=Start, 1=Stop)   — valid iff tag != 0
///   bits 3..=18 : key (u16) — valid iff tag == 2
/// ```
///
/// All transitions happen via `compare_exchange`, so:
///   * `begin_listening(target)` atomically does `NotListening → Listening{target}`
///     and returns `false` if we lost the race.
///   * `capture_key(key)` atomically does `Listening{target} → Captured{target,key}`
///     and returns `false` if we lost the race (state changed under us).
///   * `stop_listening()` unconditionally returns to `NotListening`.
pub struct AtomicListeningForNewHotkey {
    state: AtomicU32,
}

impl AtomicListeningForNewHotkey {
    const TAG_NOT_LISTENING: u32 = 0;
    const TAG_LISTENING: u32 = 1;
    const TAG_CAPTURED: u32 = 2;
    const TAG_MASK: u32 = 0b11;
    const TARGET_BIT: u32 = 1 << 2;
    const KEY_SHIFT: u32 = 3;
    const KEY_MASK: u32 = 0xFFFF << Self::KEY_SHIFT;

    pub fn new() -> Self {
        Self {
            state: AtomicU32::new(Self::encode(ListeningForNewHotkey::NotListening)),
        }
    }

    fn encode(value: ListeningForNewHotkey) -> u32 {
        match value {
            ListeningForNewHotkey::NotListening => Self::TAG_NOT_LISTENING,
            ListeningForNewHotkey::Listening { target } => {
                Self::TAG_LISTENING | Self::encode_target(target)
            }
            ListeningForNewHotkey::Captured { target, key } => {
                Self::TAG_CAPTURED
                    | Self::encode_target(target)
                    | ((key as u32) << Self::KEY_SHIFT)
            }
        }
    }

    fn encode_target(target: HotkeyRebindTarget) -> u32 {
        match target {
            HotkeyRebindTarget::Start => 0,
            HotkeyRebindTarget::Stop => Self::TARGET_BIT,
        }
    }

    fn decode_target(raw: u32) -> HotkeyRebindTarget {
        if raw & Self::TARGET_BIT == 0 {
            HotkeyRebindTarget::Start
        } else {
            HotkeyRebindTarget::Stop
        }
    }

    fn decode(raw: u32) -> ListeningForNewHotkey {
        match raw & Self::TAG_MASK {
            Self::TAG_LISTENING => ListeningForNewHotkey::Listening {
                target: Self::decode_target(raw),
            },
            Self::TAG_CAPTURED => ListeningForNewHotkey::Captured {
                target: Self::decode_target(raw),
                key: ((raw & Self::KEY_MASK) >> Self::KEY_SHIFT) as u16,
            },
            // fallthrough includes TAG_NOT_LISTENING and any unexpected value
            _ => ListeningForNewHotkey::NotListening,
        }
    }

    /// Load the current state. Use for display/read-only UI.
    pub fn load(&self) -> ListeningForNewHotkey {
        Self::decode(self.state.load(Ordering::Acquire))
    }

    /// Convenience — returns `Some(target)` iff currently in the `Listening` state.
    pub fn listening_hotkey_target(&self) -> Option<HotkeyRebindTarget> {
        self.load().listening_hotkey_target()
    }

    /// Atomically transition `NotListening -> Listening { target }`.
    ///
    /// Returns `true` if *this call* actually began listening. Returns
    /// `false` if another thread already held the listening slot (either
    /// `Listening{...}` for any target, or `Captured{...}` not yet consumed).
    /// The caller is expected to bail out on `false`.
    #[must_use]
    pub fn begin_listening(&self, target: HotkeyRebindTarget) -> bool {
        let next = Self::encode(ListeningForNewHotkey::Listening { target });
        self.state
            .compare_exchange(
                Self::TAG_NOT_LISTENING,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Atomically transition `Listening { target } -> Captured { target, key }`.
    /// Returns `true` iff we were still in `Listening` (so the capture took effect).
    /// Returns `false` if the state was no longer `Listening` (e.g. UI cancelled
    /// the rebind concurrently) — caller should drop the key event.
    #[must_use]
    pub fn capture_key(&self, key: u16) -> bool {
        // We don't know the target up front; load, then CAS with that target.
        loop {
            let current = self.state.load(Ordering::Acquire);
            if current & Self::TAG_MASK != Self::TAG_LISTENING {
                return false;
            }
            let target = Self::decode_target(current);
            let next = Self::encode(ListeningForNewHotkey::Captured { target, key });
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue, // spurious / racy; re-check tag
            }
        }
    }

    /// Unconditionally return to `NotListening`. Returns the prior state —
    /// useful when the UI wants to know what it just cancelled/consumed.
    pub fn stop_listening(&self) -> ListeningForNewHotkey {
        let prev = self
            .state
            .swap(Self::TAG_NOT_LISTENING, Ordering::AcqRel);
        Self::decode(prev)
    }
}

impl Default for AtomicListeningForNewHotkey {
    fn default() -> Self {
        Self::new()
    }
}

pub struct GitHubRelease {
    pub name: String,
    pub release_notes_url: String,
    pub download_url: String,
    pub release_date: Option<chrono::DateTime<chrono::Utc>>,
}

/// Default page size for upload list queries.
pub const UPLOAD_LIST_DEFAULT_LIMIT: u32 = 100;

/// A request for some async action to happen. Response will be delivered via [`UiUpdate`].
pub enum AsyncRequest {
    ValidateApiKey {
        api_key: String,
    },
    UploadData,
    PauseUpload,
    OpenDataDump,
    OpenLog,
    UpdateUnsupportedGames(UnsupportedGames),
    LoadUploadStatistics,
    LoadUploadList {
        limit: u32,
        offset: u32,
    },
    LoadLocalRecordings,
    DeleteAllInvalidRecordings,
    DeleteAllUploadedLocalRecordings,
    DeleteRecording(PathBuf),
    OpenFolder(PathBuf),
    MoveRecordingsFolder {
        from: PathBuf,
        to: PathBuf,
    },
    PickRecordingFolder {
        current_location: PathBuf,
    },
    PlayCue {
        cue: String,
    },
    /// Sent by upload::start() when upload batch completes, with count of recordings processed
    UploadCompleted {
        uploaded_count: usize,
    },
    /// Clear the auto-upload queue (called when unchecking auto-upload preference)
    ClearAutoUploadQueue,
    /// Switch to/from offline mode
    SetOfflineMode {
        enabled: bool,
        offline_reason: Option<String>,
    },
    /// Attempt to go online with backoff - starts backoff if not active, or retries if active
    OfflineBackoffAttempt,
    /// Cancel the offline mode backoff retry loop
    CancelOfflineBackoff,
}

impl AsyncRequest {
    /// Create a [`LoadUploadList`](Self::LoadUploadList) request with the default limit and offset 0.
    pub fn load_upload_list_default() -> Self {
        Self::LoadUploadList {
            limit: UPLOAD_LIST_DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

/// A message sent to the UI thread, usually in response to some action taken in another thread
pub enum UiUpdate {
    /// Dummy update to force the UI to repaint
    ForceUpdate,
    UpdateAvailableVideoEncoders(Vec<VideoEncoderType>),
    UpdateUserId(Result<String, String>),
    UploadFailed(String),
    UpdateRecordingState(bool),
    UpdateNewerReleaseAvailable(GitHubRelease),
    UpdateUserUploadStatistics(crate::api::UserUploadStatistics),
    UpdateUserUploadList {
        uploads: Vec<crate::api::UserUpload>,
        limit: u32,
        offset: u32,
    },
    UpdateLocalRecordings(Vec<LocalRecording>),
    FolderPickerResult {
        old_path: PathBuf,
        new_path: PathBuf,
    },
    /// Update the auto-upload queue count displayed in the UI
    UpdateAutoUploadQueueCount(usize),
}

/// A message sent to the UI thread, usually in response to some action taken in another thread
/// but is not important enough to warrant a force update, or to be queued up.
#[derive(Clone, PartialEq)]
pub enum UiUpdateUnreliable {
    UpdateUploadProgress(Option<ProgressData>),
}

pub type UiUpdateUnreliableSender = broadcast::Sender<UiUpdateUnreliable>;

/// A sender for [`UiUpdate`] messages. Will automatically repaint the UI after sending a message.
#[derive(Clone)]
pub struct UiUpdateSender {
    tx: mpsc::UnboundedSender<UiUpdate>,
    pub ctx: OnceLock<egui::Context>,
}
impl UiUpdateSender {
    pub fn build() -> (Self, tokio::sync::mpsc::UnboundedReceiver<UiUpdate>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                tx,
                ctx: OnceLock::new(),
            },
            rx,
        )
    }

    pub fn send(&self, cmd: UiUpdate) -> Result<(), mpsc::error::SendError<UiUpdate>> {
        let res = self.tx.send(cmd);
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint_after(Duration::from_millis(10))
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn listening_state_roundtrips_through_encode_decode() {
        let samples = [
            ListeningForNewHotkey::NotListening,
            ListeningForNewHotkey::Listening {
                target: HotkeyRebindTarget::Start,
            },
            ListeningForNewHotkey::Listening {
                target: HotkeyRebindTarget::Stop,
            },
            ListeningForNewHotkey::Captured {
                target: HotkeyRebindTarget::Start,
                key: 0,
            },
            ListeningForNewHotkey::Captured {
                target: HotkeyRebindTarget::Stop,
                key: u16::MAX,
            },
            ListeningForNewHotkey::Captured {
                target: HotkeyRebindTarget::Start,
                key: 0x1234,
            },
        ];
        for sample in samples {
            let raw = AtomicListeningForNewHotkey::encode(sample);
            assert_eq!(AtomicListeningForNewHotkey::decode(raw), sample);
        }
    }

    #[test]
    fn begin_listening_is_noop_when_already_listening() {
        let s = AtomicListeningForNewHotkey::new();
        assert!(s.begin_listening(HotkeyRebindTarget::Start));
        // Second call must fail — we're already listening for Start.
        assert!(!s.begin_listening(HotkeyRebindTarget::Start));
        // And must also fail for a different target — no silent overwrite.
        assert!(!s.begin_listening(HotkeyRebindTarget::Stop));
        assert_eq!(
            s.load(),
            ListeningForNewHotkey::Listening {
                target: HotkeyRebindTarget::Start,
            }
        );
    }

    #[test]
    fn capture_key_only_fires_when_listening() {
        let s = AtomicListeningForNewHotkey::new();
        // Not listening — capture should be a no-op.
        assert!(!s.capture_key(0x10));
        assert_eq!(s.load(), ListeningForNewHotkey::NotListening);

        // Begin listening, then capture should succeed exactly once.
        assert!(s.begin_listening(HotkeyRebindTarget::Stop));
        assert!(s.capture_key(0x20));
        assert_eq!(
            s.load(),
            ListeningForNewHotkey::Captured {
                target: HotkeyRebindTarget::Stop,
                key: 0x20,
            }
        );
        // Already captured — another capture is a no-op.
        assert!(!s.capture_key(0x30));
    }

    #[test]
    fn stop_listening_returns_previous_state() {
        let s = AtomicListeningForNewHotkey::new();
        assert_eq!(s.stop_listening(), ListeningForNewHotkey::NotListening);
        assert!(s.begin_listening(HotkeyRebindTarget::Start));
        assert_eq!(
            s.stop_listening(),
            ListeningForNewHotkey::Listening {
                target: HotkeyRebindTarget::Start,
            }
        );
        assert_eq!(s.load(), ListeningForNewHotkey::NotListening);
    }

    /// Fires N threads that all race to call `begin_listening`. Exactly one
    /// of them may succeed; all others must see `false`. This is the
    /// regression check for R33 (TOCTOU on listening_for_new_hotkey).
    #[test]
    fn concurrent_begin_listening_only_lets_one_win() {
        const THREADS: usize = 32;
        let state = Arc::new(AtomicListeningForNewHotkey::new());
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let state = state.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    // Force all threads to line up before calling CAS,
                    // so we maximise the chance of a race.
                    barrier.wait();
                    let target = if i % 2 == 0 {
                        HotkeyRebindTarget::Start
                    } else {
                        HotkeyRebindTarget::Stop
                    };
                    (target, state.begin_listening(target))
                })
            })
            .collect();

        let results: Vec<(HotkeyRebindTarget, bool)> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        let winners: Vec<_> = results.iter().filter(|(_, won)| *won).collect();
        assert_eq!(
            winners.len(),
            1,
            "exactly one thread should win the CAS; got {} winners out of {THREADS}",
            winners.len()
        );

        let (winning_target, _) = winners[0];
        assert_eq!(
            state.load(),
            ListeningForNewHotkey::Listening {
                target: *winning_target,
            },
            "state must reflect the winner's target, not a later writer"
        );
    }

    /// Once a key has been captured, begin_listening must still refuse —
    /// otherwise the UI could overwrite an unread capture and drop the
    /// user's rebind silently.
    #[test]
    fn begin_listening_refuses_over_unconsumed_capture() {
        let s = AtomicListeningForNewHotkey::new();
        assert!(s.begin_listening(HotkeyRebindTarget::Start));
        assert!(s.capture_key(42));
        assert!(!s.begin_listening(HotkeyRebindTarget::Stop));
        assert_eq!(
            s.load(),
            ListeningForNewHotkey::Captured {
                target: HotkeyRebindTarget::Start,
                key: 42,
            }
        );
    }
}
