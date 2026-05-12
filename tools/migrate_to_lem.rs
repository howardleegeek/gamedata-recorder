//! Migration tool for converting legacy GameData Recorder format to LEM format
//!
//! Usage:
//!   migrate_to_lem <input_dir> <output_dir>
//!
//! Input directory should contain:
//!   - video.mp4 (or .avi, .mkv, etc.)
//!   - inputs.jsonl
//!   - metadata.json
//!
//! Output will be LEM format:
//!   session_YYYYMMDD_HHMMSS/
//!     recordings/main_record.mp4
//!     recordings/main_record.meta.json
//!     streams/actions.jsonl
//!     streams/timestamps.jsonl
//!     metadata/session.json
//!     metadata/hardware.json
//!     metadata/game.json
//!     metadata/recorder.json
//!     checksums/recordings.sha256
//!     checksums/streams.sha256

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use clap::Parser;
use color_eyre::{
    Result,
    eyre::{Context, eyre},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Migrate legacy GameData Recorder format to LEM format
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input directory containing legacy format files
    input_dir: PathBuf,

    /// Output directory for LEM format
    output_dir: PathBuf,

    /// Session ID (default: auto-generate from metadata)
    #[arg(short, long)]
    session_id: Option<String>,

    /// Game name (default: from metadata.json)
    #[arg(short, long)]
    game: Option<String>,

    /// Target FPS for frame calculation
    #[arg(long, default_value = "60")]
    target_fps: u32,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

/// Legacy input event format
#[derive(Debug, Deserialize)]
struct LegacyInputEvent {
    timestamp: f64,
    event_type: String,
    event_args: Vec<serde_json::Value>,
}

/// Legacy metadata format
#[derive(Debug, Deserialize)]
struct LegacyMetadata {
    game_exe: String,
    #[serde(default)]
    window_name: Option<String>,
    session_id: String,
    hardware_id: String,
    start_timestamp: f64,
    end_timestamp: f64,
    duration: f64,
    #[serde(default)]
    average_fps: Option<f64>,
    #[serde(default)]
    frame_count: Option<u64>,
    #[serde(default)]
    recorder_version: Option<String>,
    #[serde(default)]
    hardware_specs: Option<HardwareSpecs>,
}

#[derive(Debug, Deserialize)]
struct HardwareSpecs {
    #[serde(default)]
    cpu: Option<String>,
    #[serde(default)]
    gpu: Option<String>,
    #[serde(default)]
    ram_gb: Option<u32>,
    #[serde(default)]
    os: Option<String>,
}

/// LEM action event
#[derive(Debug, Serialize)]
struct ActionEvent {
    t_ns: u64,
    frame_idx: u64,
    #[serde(flatten)]
    action_type: ActionType,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ActionType {
    MouseMove {
        #[serde(rename = "type")]
        type_name: &'static str,
        x: i32,
        y: i32,
        delta: [i32; 2],
    },
    MouseButton {
        #[serde(rename = "type")]
        type_name: &'static str,
        button: String,
        pressed: bool,
    },
    MouseWheel {
        #[serde(rename = "type")]
        type_name: &'static str,
        direction: String,
        amount: i16,
    },
    KeyDown {
        #[serde(rename = "type")]
        type_name: &'static str,
        key: String,
        scancode: u32,
    },
    KeyUp {
        #[serde(rename = "type")]
        type_name: &'static str,
        key: String,
        scancode: u32,
    },
    GameCommand {
        #[serde(rename = "type")]
        type_name: &'static str,
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<[i32; 3]>,
    },
}

/// LEM timestamp mapping
#[derive(Debug, Serialize)]
struct TimestampMapping {
    frame_idx: u64,
    video_pts_ns: u64,
    real_t_ns: u64,
    drift_ns: i64,
}

/// LEM session metadata
#[derive(Debug, Serialize)]
struct SessionMetadata {
    session_id: String,
    created_at: String,
    duration_seconds: u64,
    total_frames: u64,
    total_actions: u64,
    game: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

/// LEM hardware metadata
#[derive(Debug, Serialize)]
struct HardwareMetadata {
    cpu: String,
    gpu: String,
    ram_gb: u32,
    os: String,
    recording_drive: String,
    average_fps: f64,
    dropped_frames: u32,
}

/// LEM game metadata
#[derive(Debug, Serialize)]
struct GameMetadata {
    game: String,
    version: String,
    graphics_settings: GraphicsSettings,
    control_settings: ControlSettings,
}

#[derive(Debug, Serialize)]
struct GraphicsSettings {
    resolution: [u32; 2],
    quality: String,
    fov: u32,
    motion_blur: bool,
    ray_tracing: bool,
}

#[derive(Debug, Serialize)]
struct ControlSettings {
    mouse_sensitivity: f64,
    invert_y: bool,
    keybindings: HashMap<String, String>,
}

/// LEM recorder metadata
#[derive(Debug, Serialize)]
struct RecorderMetadata {
    recorder_version: String,
    target_fps: u32,
    video_codec: String,
    video_bitrate_mbps: u32,
    capture_method: String,
    record_audio: bool,
    audio_bitrate: u32,
    record_depth: bool,
    compress_actions: bool,
}

/// LEM video metadata
#[derive(Debug, Serialize)]
struct VideoMetadata {
    codec: String,
    profile: String,
    bitrate: String,
    fps: u32,
    resolution: [u32; 2],
    pixel_format: String,
    duration_seconds: u64,
    total_frames: u64,
    file_size_bytes: u64,
    keyframes: Vec<KeyframeInfo>,
    frame_duration_ns: u64,
    start_time_ns: u64,
    end_time_ns: u64,
}

#[derive(Debug, Serialize)]
struct KeyframeInfo {
    frame_index: u64,
    byte_offset: u64,
    pts: u64,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let args = Args::parse();

    migrate_recording(&args.input_dir, &args.output_dir, &args)?;

    Ok(())
}

fn migrate_recording(input_dir: &Path, output_dir: &Path, args: &Args) -> Result<()> {
    println!("Migrating recording from {} to LEM format...", input_dir.display());

    // Find input files
    let video_file = find_video_file(input_dir)?;
    let inputs_file = input_dir.join("inputs.jsonl");
    let metadata_file = input_dir.join("metadata.json");

    if !inputs_file.exists() {
        return Err(eyre!("inputs.jsonl not found in input directory"));
    }
    if !metadata_file.exists() {
        return Err(eyre!("metadata.json not found in input directory"));
    }

    // Parse legacy metadata
    let metadata_content = fs::read_to_string(&metadata_file)
        .with_context(|| format!("Failed to read {}", metadata_file.display()))?;
    let legacy_metadata: LegacyMetadata = serde_json::from_str(&metadata_content)
        .with_context(|| "Failed to parse metadata.json")?;

    // Generate session ID
    let session_id = args.session_id.clone().unwrap_or_else(|| {
        let start_time = DateTime::from_timestamp(
            (legacy_metadata.start_timestamp as i64),
            0,
        )
        .unwrap_or_else(|| Utc::now());
        format!("session_{}", start_time.format("%Y%m%d_%H%M%S"))
    });

    // Create session directory
    let session_path = output_dir.join(&session_id);
    create_lem_directory_structure(&session_path)?;

    // Copy video file
    let video_dest = session_path.join("recordings/main_record.mp4");
    fs::copy(&video_file, &video_dest)
        .with_context(|| format!("Failed to copy video from {} to {}", video_file.display(), video_dest.display()))?;

    // Convert inputs.jsonl to actions.jsonl and timestamps.jsonl
    let (total_actions, total_frames) = convert_input_events(
        &inputs_file,
        &session_path.join("streams/actions.jsonl"),
        &session_path.join("streams/timestamps.jsonl"),
        &legacy_metadata,
        args.target_fps,
        args.verbose,
    )?;

    // Write metadata files
    write_session_metadata(&session_path, &legacy_metadata, &session_id, total_actions, total_frames)?;
    write_hardware_metadata(&session_path, &legacy_metadata)?;
    write_game_metadata(&session_path, &legacy_metadata)?;
    write_recorder_metadata(&session_path, &legacy_metadata, args.target_fps)?;
    write_video_metadata(&session_path, &video_dest, &legacy_metadata, args.target_fps)?;

    // Generate checksums
    generate_checksums(&session_path)?;

    println!("Migration complete!");
    println!("  Session ID: {}", session_id);
    println!("  Total frames: {}", total_frames);
    println!("  Total actions: {}", total_actions);
    println!("  Output: {}", session_path.display());

    Ok(())
}

fn find_video_file(dir: &Path) -> Result<PathBuf> {
    let video_extensions = ["mp4", "avi", "mkv", "mov", "webm"];

    for ext in &video_extensions {
        let video_path = dir.join(format!("video.{}", ext));
        if video_path.exists() {
            return Ok(video_path);
        }
    }

    // Try to find any video file
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some(ext) = path.extension() {
            if video_extensions.contains(&ext.to_string_lossy().to_lowercase().as_str()) {
                return Ok(path);
            }
        }
    }

    Err(eyre!("No video file found in input directory"))
}

fn create_lem_directory_structure(session_path: &Path) -> Result<()> {
    let dirs = [
        "recordings",
        "streams",
        "extracted/rgb",
        "extracted/depth",
        "metadata",
        "checksums",
    ];

    for dir in &dirs {
        fs::create_dir_all(session_path.join(dir))
            .with_context(|| format!("Failed to create directory: {}", dir))?;
    }

    Ok(())
}

fn convert_input_events(
    inputs_path: &Path,
    actions_path: &Path,
    timestamps_path: &Path,
    metadata: &LegacyMetadata,
    target_fps: u32,
    verbose: bool,
) -> Result<(u64, u64)> {
    let inputs_file = File::open(inputs_path)
        .with_context(|| format!("Failed to open {}", inputs_path.display()))?;
    let reader = BufReader::new(inputs_file);

    let actions_file = File::create(actions_path)
        .with_context(|| format!("Failed to create {}", actions_path.display()))?;
    let mut actions_writer = BufWriter::new(actions_file);

    let timestamps_file = File::create(timestamps_path)
        .with_context(|| format!("Failed to create {}", timestamps_path.display()))?;
    let mut timestamps_writer = BufWriter::new(timestamps_file);

    let start_ns = (metadata.start_timestamp * 1_000_000_000.0) as u64;
    let frame_duration_ns = 1_000_000_000 / target_fps as u64;

    let mut total_actions = 0u64;
    let mut current_frame = 0u64;
    let mut last_frame_idx = 0u64;

    for line in reader.lines() {
        let line = line.with_context(|| "Failed to read line from inputs.jsonl")?;
        if line.trim().is_empty() {
            continue;
        }

        let legacy_event: LegacyInputEvent = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse input event: {}", line))?;

        // Convert timestamp to nanoseconds
        let t_ns = (legacy_event.timestamp * 1_000_000_000.0) as u64;

        // Calculate frame index
        let frame_idx = (t_ns.saturating_sub(start_ns)) / frame_duration_ns;

        // Convert event type
        let action_type = convert_event_type(&legacy_event.event_type, &legacy_event.event_args)?;

        let action_event = ActionEvent {
            t_ns,
            frame_idx,
            action_type,
        };

        // Write action
        let action_json = serde_json::to_string(&action_event)
            .with_context(|| "Failed to serialize action event")?;
        writeln!(actions_writer, "{}", action_json)
            .with_context(|| "Failed to write action event")?;

        total_actions += 1;

        // Write timestamp mapping if this is a new frame
        if frame_idx > last_frame_idx {
            for f in last_frame_idx..frame_idx {
                let video_pts_ns = f * frame_duration_ns;
                let real_t_ns = start_ns + video_pts_ns;
                let drift_ns = 0i64; // No drift info in legacy format

                let mapping = TimestampMapping {
                    frame_idx: f,
                    video_pts_ns,
                    real_t_ns,
                    drift_ns,
                };

                let mapping_json = serde_json::to_string(&mapping)
                    .with_context(|| "Failed to serialize timestamp mapping")?;
                writeln!(timestamps_writer, "{}", mapping_json)
                    .with_context(|| "Failed to write timestamp mapping")?;
            }
            last_frame_idx = frame_idx;
        }

        if verbose && total_actions % 1000 == 0 {
            println!("  Processed {} actions...", total_actions);
        }
    }

    // Write final frame timestamp
    let final_frame_idx = last_frame_idx + 1;
    let video_pts_ns = final_frame_idx * frame_duration_ns;
    let real_t_ns = start_ns + video_pts_ns;

    let mapping = TimestampMapping {
        frame_idx: final_frame_idx,
        video_pts_ns,
        real_t_ns,
        drift_ns: 0,
    };
    let mapping_json = serde_json::to_string(&mapping)?;
    writeln!(timestamps_writer, "{}", mapping_json)?;

    actions_writer.flush()?;
    timestamps_writer.flush()?;

    Ok((total_actions, final_frame_idx + 1))
}

fn convert_event_type(event_type: &str, args: &[serde_json::Value]) -> Result<ActionType> {
    match event_type {
        "MOUSE_MOVE" | "MouseMove" => {
            let x = args.get(0)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .unwrap_or(0);
            let y = args.get(1)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .unwrap_or(0);
            let dx = args.get(2)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .unwrap_or(0);
            let dy = args.get(3)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .unwrap_or(0);

            Ok(ActionType::MouseMove {
                type_name: "mouse_move",
                x,
                y,
                delta: [dx, dy],
            })
        }
        "MOUSE_BUTTON" | "MouseButton" => {
            let button = args.get(0)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let pressed = args.get(1)
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            Ok(ActionType::MouseButton {
                type_name: "mouse_button",
                button,
                pressed,
            })
        }
        "MOUSE_WHEEL" | "MouseWheel" => {
            let direction = args.get(0)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "up".to_string());
            let amount = args.get(1)
                .and_then(|v| v.as_i64())
                .map(|v| v as i16)
                .unwrap_or(120);

            Ok(ActionType::MouseWheel {
                type_name: "mouse_wheel",
                direction,
                amount,
            })
        }
        "KEYBOARD" | "KeyDown" => {
            let key = args.get(0)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Unknown".to_string());
            let pressed = args.get(1)
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let scancode = args.get(2)
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(0);

            if pressed {
                Ok(ActionType::KeyDown {
                    type_name: "key_down",
                    key,
                    scancode,
                })
            } else {
                Ok(ActionType::KeyUp {
                    type_name: "key_up",
                    key,
                    scancode,
                })
            }
        }
        "KeyUp" => {
            let key = args.get(0)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Unknown".to_string());
            let scancode = args.get(1)
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(0);

            Ok(ActionType::KeyUp {
                type_name: "key_up",
                key,
                scancode,
            })
        }
        _ => {
            // Unknown event type - store as game command
            let command = event_type.to_string();
            Ok(ActionType::GameCommand {
                type_name: "game_command",
                command,
                target: None,
            })
        }
    }
}

fn write_session_metadata(
    session_path: &Path,
    legacy: &LegacyMetadata,
    session_id: &str,
    total_actions: u64,
    total_frames: u64,
) -> Result<()> {
    let start_time = DateTime::from_timestamp(
        (legacy.start_timestamp as i64),
        0,
    )
    .unwrap_or_else(|| Utc::now());

    let metadata = SessionMetadata {
        session_id: session_id.to_string(),
        created_at: start_time.to_rfc3339(),
        duration_seconds: legacy.duration as u64,
        total_frames,
        total_actions,
        game: legacy.game_exe.clone(),
        version: "1.0.0".to_string(),
        notes: legacy.window_name.clone(),
    };

    let json = serde_json::to_string_pretty(&metadata)
        .with_context(|| "Failed to serialize session metadata")?;
    let path = session_path.join("metadata/session.json");
    fs::write(&path, json)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

fn write_hardware_metadata(session_path: &Path, legacy: &LegacyMetadata) -> Result<()> {
    let (cpu, gpu, ram_gb, os) = if let Some(ref specs) = legacy.hardware_specs {
        (
            specs.cpu.clone().unwrap_or_else(|| "Unknown".to_string()),
            specs.gpu.clone().unwrap_or_else(|| "Unknown".to_string()),
            specs.ram_gb.unwrap_or(16),
            specs.os.clone().unwrap_or_else(|| "Unknown".to_string()),
        )
    } else {
        ("Unknown".to_string(), "Unknown".to_string(), 16, "Unknown".to_string())
    };

    let metadata = HardwareMetadata {
        cpu,
        gpu,
        ram_gb,
        os,
        recording_drive: "Unknown".to_string(),
        average_fps: legacy.average_fps.unwrap_or(60.0),
        dropped_frames: 0,
    };

    let json = serde_json::to_string_pretty(&metadata)
        .with_context(|| "Failed to serialize hardware metadata")?;
    let path = session_path.join("metadata/hardware.json");
    fs::write(&path, json)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

fn write_game_metadata(session_path: &Path, legacy: &LegacyMetadata) -> Result<()> {
    let metadata = GameMetadata {
        game: legacy.game_exe.clone(),
        version: "1.0.0".to_string(),
        graphics_settings: GraphicsSettings {
            resolution: [1920, 1080],
            quality: "Unknown".to_string(),
            fov: 90,
            motion_blur: false,
            ray_tracing: false,
        },
        control_settings: ControlSettings {
            mouse_sensitivity: 1.0,
            invert_y: false,
            keybindings: HashMap::new(),
        },
    };

    let json = serde_json::to_string_pretty(&metadata)
        .with_context(|| "Failed to serialize game metadata")?;
    let path = session_path.join("metadata/game.json");
    fs::write(&path, json)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

fn write_recorder_metadata(
    session_path: &Path,
    legacy: &LegacyMetadata,
    target_fps: u32,
) -> Result<()> {
    let metadata = RecorderMetadata {
        recorder_version: legacy.recorder_version.clone().unwrap_or_else(|| "unknown".to_string()),
        target_fps,
        video_codec: "hevc".to_string(),
        video_bitrate_mbps: 50,
        capture_method: "game_capture".to_string(),
        record_audio: true,
        audio_bitrate: 192,
        record_depth: false,
        compress_actions: false,
    };

    let json = serde_json::to_string_pretty(&metadata)
        .with_context(|| "Failed to serialize recorder metadata")?;
    let path = session_path.join("metadata/recorder.json");
    fs::write(&path, json)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

fn write_video_metadata(
    session_path: &Path,
    video_path: &Path,
    legacy: &LegacyMetadata,
    target_fps: u32,
) -> Result<()> {
    let file_metadata = fs::metadata(video_path)
        .with_context(|| format!("Failed to read video file metadata: {}", video_path.display()))?;
    let file_size = file_metadata.len();

    let start_ns = (legacy.start_timestamp * 1_000_000_000.0) as u64;
    let end_ns = (legacy.end_timestamp * 1_000_000_000.0) as u64;
    let duration_ns = end_ns.saturating_sub(start_ns);
    let total_frames = (duration_ns as f64 / 1_000_000_000.0 * target_fps as f64) as u64;

    let metadata = VideoMetadata {
        codec: "hevc".to_string(),
        profile: "main".to_string(),
        bitrate: "50 Mbps".to_string(),
        fps: target_fps,
        resolution: [1920, 1080],
        pixel_format: "yuv420p".to_string(),
        duration_seconds: legacy.duration as u64,
        total_frames,
        file_size_bytes: file_size,
        keyframes: vec![], // Would need ffprobe to extract
        frame_duration_ns: 1_000_000_000 / target_fps as u64,
        start_time_ns: start_ns,
        end_time_ns: end_ns,
    };

    let json = serde_json::to_string_pretty(&metadata)
        .with_context(|| "Failed to serialize video metadata")?;
    let path = session_path.join("recordings/main_record.meta.json");
    fs::write(&path, json)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

fn generate_checksums(session_path: &Path) -> Result<()> {
    // Generate checksums for recordings
    let recordings_dir = session_path.join("recordings");
    let recordings_checksums = session_path.join("checksums/recordings.sha256");
    generate_dir_checksums(&recordings_dir, &recordings_checksums)?;

    // Generate checksums for streams
    let streams_dir = session_path.join("streams");
    let streams_checksums = session_path.join("checksums/streams.sha256");
    generate_dir_checksums(&streams_dir, &streams_checksums)?;

    Ok(())
}

fn generate_dir_checksums(dir: &Path, output: &Path) -> Result<()> {
    let mut checksum_file = BufWriter::new(File::create(output)?);

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            let content = fs::read(&path)?;
            let mut hasher = Sha256::new();
            hasher.update(&content);
            let hash = hasher.finalize();
            let hash_hex = format!("{:x}", hash);

            let file_name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            writeln!(checksum_file, "{}  {}", hash_hex, file_name)?;
        }
    }

    checksum_file.flush()?;
    Ok(())
}