//! Layer 2: Trajectory segmentation — groups continuous mouse movements into meaningful strokes.
//!
//! A trajectory is a sequence of mouse movements terminated by:
//! - A click event (left/right/middle button down)
//! - A keyboard event (any key press)
//! - A scroll event
//! - A pause exceeding the threshold (default 250ms)
//!
//! Each trajectory captures:
//! - Start/end timestamps (nanoseconds)
//! - Path coordinates [(x, y), ...]
//! - Total distance (pixels)
//! - Duration (ms)
//! - Average speed (px/ms)
//! - Terminating action (what ended this stroke)
//!
//! This is far more useful for ML models than raw dx/dy events,
//! because it captures *intent* — a mouse drag to a target, a flick shot, etc.

use serde::Serialize;

/// A mouse trajectory (stroke) — continuous movement terminated by a discrete action.
#[derive(Debug, Clone, Serialize)]
pub struct Trajectory {
    /// Index in the session's trajectory sequence
    pub index: u32,
    /// Start timestamp in nanoseconds from session start
    pub start_ns: u64,
    /// End timestamp in nanoseconds from session start
    pub end_ns: u64,
    /// Duration in milliseconds
    pub duration_ms: f64,
    /// Path points: accumulated (x, y) positions
    pub path: Vec<[i32; 2]>,
    /// Total distance traveled in pixels
    pub total_distance_px: f64,
    /// Average speed in pixels per millisecond
    pub avg_speed_px_per_ms: f64,
    /// Number of movement events in this trajectory
    pub event_count: u32,
    /// What terminated this trajectory
    pub terminator: TrajectoryTerminator,
}

/// What caused a trajectory to end.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum TrajectoryTerminator {
    /// Mouse button click (begins a new action)
    Click { button: u16 },
    /// Keyboard key press
    KeyPress { key: u16 },
    /// Scroll wheel
    Scroll { delta: i16 },
    /// Pause — no movement for longer than threshold
    Pause { gap_ms: f64 },
    /// End of session
    SessionEnd,
}

/// Segments raw input events into trajectories.
///
/// Input: chronologically sorted events with timestamps.
/// Output: list of Trajectory structs.
pub fn segment_trajectories(events: &[RawEvent], pause_threshold_ms: f64) -> Vec<Trajectory> {
    // Validate pause_threshold_ms to ensure pause detection works correctly
    let pause_threshold_ms = if pause_threshold_ms <= 0.0 || !pause_threshold_ms.is_finite() {
        tracing::warn!(
            "Invalid pause_threshold_ms: {}, using default 250.0",
            pause_threshold_ms
        );
        250.0
    } else {
        pause_threshold_ms
    };
    let mut trajectories = Vec::new();
    let mut current_path: Vec<[i32; 2]> = Vec::new();
    let mut current_start_ns: Option<u64> = None;
    let mut last_move_ns: u64 = 0;
    let mut cursor_x: i32 = 0;
    let mut cursor_y: i32 = 0;
    let mut total_distance: f64 = 0.0;
    let mut event_count: u32 = 0;
    let mut traj_index: u32 = 0;

    for event in events {
        match &event.kind {
            RawEventKind::MouseMove { dx, dy } => {
                // Check for pause gap
                if let Some(start) = current_start_ns {
                    let gap_ms =
                        event.timestamp_ns.saturating_sub(last_move_ns) as f64 / 1_000_000.0;
                    if gap_ms > pause_threshold_ms && !current_path.is_empty() {
                        // Finalize trajectory due to pause
                        trajectories.push(build_trajectory(
                            traj_index,
                            start,
                            last_move_ns,
                            &current_path,
                            total_distance,
                            event_count,
                            TrajectoryTerminator::Pause { gap_ms },
                        ));
                        traj_index = traj_index.saturating_add(1);
                        current_path.clear();
                        total_distance = 0.0;
                        event_count = 0;
                        current_start_ns = None;
                    }
                }

                // Accumulate movement using saturating_add to prevent i32 overflow
                // in long sessions with extensive mouse movement.
                cursor_x = cursor_x.saturating_add(*dx);
                cursor_y = cursor_y.saturating_add(*dy);
                let dist = ((*dx as f64).powi(2) + (*dy as f64).powi(2)).sqrt();
                total_distance += dist;
                event_count = event_count.saturating_add(1);

                if current_start_ns.is_none() {
                    current_start_ns = Some(event.timestamp_ns);
                }
                current_path.push([cursor_x, cursor_y]);
                last_move_ns = event.timestamp_ns;
            }

            RawEventKind::MouseButton {
                button,
                pressed: true,
            } => {
                // Click terminates the current trajectory
                if let Some(start) = current_start_ns {
                    trajectories.push(build_trajectory(
                        traj_index,
                        start,
                        event.timestamp_ns,
                        &current_path,
                        total_distance,
                        event_count,
                        TrajectoryTerminator::Click { button: *button },
                    ));
                    traj_index = traj_index.saturating_add(1);
                }
                current_path.clear();
                total_distance = 0.0;
                event_count = 0;
                current_start_ns = None;
            }

            RawEventKind::KeyDown { vkey, .. } => {
                // Key press terminates trajectory
                if let Some(start) = current_start_ns {
                    trajectories.push(build_trajectory(
                        traj_index,
                        start,
                        event.timestamp_ns,
                        &current_path,
                        total_distance,
                        event_count,
                        TrajectoryTerminator::KeyPress { key: *vkey },
                    ));
                    traj_index = traj_index.saturating_add(1);
                }
                current_path.clear();
                total_distance = 0.0;
                event_count = 0;
                current_start_ns = None;
            }

            RawEventKind::Scroll { delta } => {
                if let Some(start) = current_start_ns {
                    trajectories.push(build_trajectory(
                        traj_index,
                        start,
                        event.timestamp_ns,
                        &current_path,
                        total_distance,
                        event_count,
                        TrajectoryTerminator::Scroll { delta: *delta },
                    ));
                    traj_index = traj_index.saturating_add(1);
                }
                current_path.clear();
                total_distance = 0.0;
                event_count = 0;
                current_start_ns = None;
            }

            _ => {} // Key up, mouse button up — don't terminate trajectories
        }
    }

    // Finalize any remaining trajectory
    if let Some(start) = current_start_ns
        && !current_path.is_empty()
    {
        trajectories.push(build_trajectory(
            traj_index,
            start,
            last_move_ns,
            &current_path,
            total_distance,
            event_count,
            TrajectoryTerminator::SessionEnd,
        ));
    }

    trajectories
}

fn build_trajectory(
    index: u32,
    start_ns: u64,
    end_ns: u64,
    path: &[[i32; 2]],
    total_distance: f64,
    event_count: u32,
    terminator: TrajectoryTerminator,
) -> Trajectory {
    let duration_ms = end_ns.saturating_sub(start_ns) as f64 / 1_000_000.0;
    // Use a minimum threshold to prevent infinity from extreme values when duration is tiny
    let avg_speed = if duration_ms > 0.001 {
        total_distance / duration_ms
    } else {
        0.0
    };

    Trajectory {
        index,
        start_ns,
        end_ns,
        duration_ms: (duration_ms * 100.0).round() / 100.0,
        path: path.to_vec(),
        total_distance_px: (total_distance * 100.0).round() / 100.0,
        avg_speed_px_per_ms: (avg_speed * 100.0).round() / 100.0,
        event_count,
        terminator,
    }
}

/// Raw event for trajectory processing input.
#[derive(Debug, Clone)]
pub struct RawEvent {
    pub timestamp_ns: u64,
    pub kind: RawEventKind,
}

/// Event types for trajectory segmentation.
#[derive(Debug, Clone)]
pub enum RawEventKind {
    MouseMove { dx: i32, dy: i32 },
    MouseButton { button: u16, pressed: bool },
    KeyDown { vkey: u16, scan_code: u16 },
    KeyUp { vkey: u16, scan_code: u16 },
    Scroll { delta: i16 },
}
