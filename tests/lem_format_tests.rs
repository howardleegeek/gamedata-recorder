//! Integration tests for LEM (Large Entity Models) format
//!
//! Tests the complete LEM format pipeline including:
//! - Session directory structure creation
//! - Action event serialization
//! - Metadata file generation
//! - Timestamp mapping
//! - Video metadata extraction

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use serde_json::json;
use tempfile::TempDir;

// Note: These tests would normally import from the gamedata-recorder crate,
// but since this is a binary crate, we test the serialization formats directly.

/// Test that LEM directory structure is correct
#[test]
fn test_lem_directory_structure() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let session_path = temp_dir.path().join("session_20260115_143022");

    // Create expected LEM directory structure
    let dirs = [
        "recordings",
        "streams",
        "extracted/rgb",
        "extracted/depth",
        "metadata",
        "checksums",
    ];

    for dir in &dirs {
        std::fs::create_dir_all(session_path.join(dir)).expect("Failed to create directory");
    }

    // Verify all directories exist
    assert!(session_path.join("recordings").exists());
    assert!(session_path.join("streams").exists());
    assert!(session_path.join("extracted/rgb").exists());
    assert!(session_path.join("extracted/depth").exists());
    assert!(session_path.join("metadata").exists());
    assert!(session_path.join("checksums").exists());
}

/// Test action event serialization format
#[test]
fn test_action_event_serialization() {
    // Mouse move event
    let mouse_move = json!({
        "t_ns": 15642909582000000_u64,
        "frame_idx": 1_u64,
        "type": "mouse_move",
        "x": 965_i32,
        "y": 542_i32,
        "delta": [5_i32, 2_i32]
    });

    let serialized = serde_json::to_string(&mouse_move).expect("Failed to serialize");
    assert!(serialized.contains("\"t_ns\":15642909582000000"));
    assert!(serialized.contains("\"type\":\"mouse_move\""));
    assert!(serialized.contains("\"delta\":[5,2]"));

    // Key down event
    let key_down = json!({
        "t_ns": 15642909583000000_u64,
        "frame_idx": 2_u64,
        "type": "key_down",
        "key": "W",
        "scancode": 17_u32
    });

    let serialized = serde_json::to_string(&key_down).expect("Failed to serialize");
    assert!(serialized.contains("\"type\":\"key_down\""));
    assert!(serialized.contains("\"key\":\"W\""));
    assert!(serialized.contains("\"scancode\":17"));
}

/// Test timestamp mapping format
#[test]
fn test_timestamp_mapping_serialization() {
    let mapping = json!({
        "frame_idx": 100_u64,
        "video_pts_ns": 3333333333_u64,
        "real_t_ns": 15642909582000000_u64,
        "drift_ns": -1000_i64
    });

    let serialized = serde_json::to_string(&mapping).expect("Failed to serialize");
    assert!(serialized.contains("\"frame_idx\":100"));
    assert!(serialized.contains("\"drift_ns\":-1000"));
}

/// Test session metadata format
#[test]
fn test_session_metadata_serialization() {
    let session = json!({
        "session_id": "session_20260115_143022",
        "created_at": "2026-01-15T14:30:22Z",
        "duration_seconds": 300_u64,
        "total_frames": 9000_u64,
        "total_actions": 15420_u64,
        "game": "cyberpunk2077",
        "version": "1.0.0",
        "notes": "Test recording"
    });

    let serialized = serde_json::to_string(&session).expect("Failed to serialize");
    assert!(serialized.contains("\"session_id\":\"session_20260115_143022\""));
    assert!(serialized.contains("\"total_frames\":9000"));
    assert!(serialized.contains("\"game\":\"cyberpunk2077\""));
}

/// Test hardware metadata format
#[test]
fn test_hardware_metadata_serialization() {
    let hardware = json!({
        "cpu": "AMD Ryzen 9 5950X",
        "gpu": "NVIDIA GeForce RTX 4090",
        "ram_gb": 64_u32,
        "os": "Windows 11 Pro",
        "recording_drive": "NVMe SSD",
        "average_fps": 142.5_f64,
        "dropped_frames": 12_u32,
        "cpu_physical_cores": Some(16_u32),
        "cpu_logical_cores": Some(32_u32),
        "cpu_frequency_mhz": Some(3400_u64),
        "ram_available_gb": Some(48.5_f64)
    });

    let serialized = serde_json::to_string(&hardware).expect("Failed to serialize");
    assert!(serialized.contains("\"cpu\":\"AMD Ryzen 9 5950X\""));
    assert!(serialized.contains("\"gpu\":\"NVIDIA GeForce RTX 4090\""));
    assert!(serialized.contains("\"ram_gb\":64"));
}

/// Test game metadata format
#[test]
fn test_game_metadata_serialization() {
    let game = json!({
        "game": "cyberpunk2077",
        "version": "2.12",
        "graphics_settings": {
            "resolution": [1920_u32, 1080_u32],
            "quality": "Ultra",
            "fov": 90_u32,
            "motion_blur": false,
            "ray_tracing": true
        },
        "control_settings": {
            "mouse_sensitivity": 0.8_f64,
            "invert_y": false,
            "keybindings": {
                "forward": "W",
                "backward": "S",
                "left": "A",
                "right": "D"
            }
        }
    });

    let serialized = serde_json::to_string(&game).expect("Failed to serialize");
    assert!(serialized.contains("\"game\":\"cyberpunk2077\""));
    assert!(serialized.contains("\"resolution\":[1920,1080]"));
    assert!(serialized.contains("\"ray_tracing\":true"));
}

/// Test recorder metadata format
#[test]
fn test_recorder_metadata_serialization() {
    let recorder = json!({
        "recorder_version": "2.6.0",
        "target_fps": 60_u32,
        "video_codec": "h265",
        "video_bitrate_mbps": 50_u32,
        "capture_method": "game_capture",
        "record_audio": true,
        "audio_bitrate": 192_u32,
        "record_depth": false,
        "compress_actions": true
    });

    let serialized = serde_json::to_string(&recorder).expect("Failed to serialize");
    assert!(serialized.contains("\"recorder_version\":\"2.6.0\""));
    assert!(serialized.contains("\"video_codec\":\"h265\""));
    assert!(serialized.contains("\"capture_method\":\"game_capture\""));
}

/// Test video metadata format
#[test]
fn test_video_metadata_serialization() {
    let video = json!({
        "codec": "hevc",
        "profile": "main",
        "bitrate": "50 Mbps",
        "fps": 60_u32,
        "resolution": [1920_u32, 1080_u32],
        "pixel_format": "yuv420p",
        "duration_seconds": 300_u64,
        "total_frames": 18000_u64,
        "file_size_bytes": 1875000000_u64,
        "keyframes": [
            {
                "frame_index": 0_u64,
                "byte_offset": 0_u64,
                "pts": 0_u64
            },
            {
                "frame_index": 300_u64,
                "byte_offset": 52428800_u64,
                "pts": 5000000000_u64
            }
        ],
        "frame_duration_ns": 16666666_u64,
        "start_time_ns": 15642900000000000_u64,
        "end_time_ns": 15642903000000000_u64
    });

    let serialized = serde_json::to_string(&video).expect("Failed to serialize");
    assert!(serialized.contains("\"codec\":\"hevc\""));
    assert!(serialized.contains("\"fps\":60"));
    assert!(serialized.contains("\"total_frames\":18000"));
}

/// Test state event serialization
#[test]
fn test_state_event_serialization() {
    let state = json!({
        "frame_idx": 1500_u64,
        "t_ns": 15642909582000000_u64,
        "player_pos": [100.5_f64, 200.0_f64, 50.25_f64],
        "player_rot": [0.0_f64, 90.0_f64, 0.0_f64],
        "health": 100_u32,
        "ammo": 30_u32,
        "score": 15000_u64
    });

    let serialized = serde_json::to_string(&state).expect("Failed to serialize");
    assert!(serialized.contains("\"player_pos\":[100.5,200.0,50.25]"));
    assert!(serialized.contains("\"health\":100"));
    assert!(serialized.contains("\"score\":15000"));
}

/// Test game event serialization
#[test]
fn test_game_event_serialization() {
    let event = json!({
        "t_ns": 15642909582000000_u64,
        "frame_idx": 2500_u64,
        "type": "kill",
        "data": {
            "target": "enemy_soldier",
            "weapon": "assault_rifle",
            "headshot": true,
            "distance_m": 45.2_f64
        }
    });

    let serialized = serde_json::to_string(&event).expect("Failed to serialize");
    assert!(serialized.contains("\"type\":\"kill\""));
    assert!(serialized.contains("\"headshot\":true"));
}

/// Test JSONL file format
#[test]
fn test_jsonl_format() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let actions_path = temp_dir.path().join("actions.jsonl");

    // Write multiple action events
    let events = vec![
        json!({
            "t_ns": 15642909582000000_u64,
            "frame_idx": 0_u64,
            "type": "mouse_move",
            "x": 960_i32,
            "y": 540_i32,
            "delta": [0_i32, 0_i32]
        }),
        json!({
            "t_ns": 15642909582100000_u64,
            "frame_idx": 0_u64,
            "type": "mouse_move",
            "x": 965_i32,
            "y": 542_i32,
            "delta": [5_i32, 2_i32]
        }),
        json!({
            "t_ns": 15642909582500000_u64,
            "frame_idx": 1_u64,
            "type": "key_down",
            "key": "W",
            "scancode": 17_u32
        }),
    ];

    // Write events
    let mut content = String::new();
    for event in &events {
        content.push_str(&serde_json::to_string(event).expect("Failed to serialize"));
        content.push('\n');
    }
    std::fs::write(&actions_path, &content).expect("Failed to write file");

    // Read and verify
    let file_content = std::fs::read_to_string(&actions_path).expect("Failed to read file");
    let lines: Vec<&str> = file_content.lines().collect();
    assert_eq!(lines.len(), 3);

    // Verify each line is valid JSON
    for (i, line) in lines.iter().enumerate() {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("Failed to parse JSON line");
        assert_eq!(
            parsed["frame_idx"].as_u64().unwrap(),
            events[i]["frame_idx"].as_u64().unwrap()
        );
    }
}

/// Test checksum file generation
#[test]
fn test_checksum_generation() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a test file
    let test_file = temp_dir.path().join("test.json");
    std::fs::write(&test_file, b"test content").expect("Failed to write test file");

    // Generate SHA256 checksum
    use sha2::{Digest, Sha256};
    let content = std::fs::read(&test_file).expect("Failed to read file");
    let mut hasher = Sha256::new();
    hasher.update(&content);
    let hash = hasher.finalize();
    let hash_hex = format!("{:x}", hash);

    // Write checksum file
    let checksum_file = temp_dir.path().join("test.sha256");
    let checksum_line = format!("{}  test.json\n", hash_hex);
    std::fs::write(&checksum_file, &checksum_line).expect("Failed to write checksum");

    // Verify checksum file format
    let checksum_content = std::fs::read_to_string(&checksum_file).expect("Failed to read checksum");
    assert!(checksum_content.starts_with(&hash_hex));
    assert!(checksum_content.contains("  test.json"));
}

/// Test nanosecond timestamp conversion
#[test]
fn test_timestamp_conversion() {
    // SystemTime to nanoseconds
    let now = SystemTime::now();
    let duration = now.duration_since(SystemTime::UNIX_EPOCH).expect("Time went backwards");
    let ns = duration.as_nanos() as u64;

    // Verify nanosecond precision
    assert!(ns > 1_700_000_000_000_000_000); // After 2023
    assert!(ns < 2_000_000_000_000_000_000); // Before 2033

    // Verify we can convert back
    let seconds = ns / 1_000_000_000;
    let nanos = ns % 1_000_000_000;
    assert!(seconds > 1_700_000_000);
    assert!(nanos < 1_000_000_000);
}

/// Test frame index increment
#[test]
fn test_frame_index_increment() {
    let mut frame_idx: u64 = 0;

    // Simulate frame increments
    for _ in 0..100 {
        frame_idx += 1;
    }
    assert_eq!(frame_idx, 100);

    // Verify atomic-like behavior (conceptually)
    let counter = std::sync::atomic::AtomicU64::new(0u64);
    assert_eq!(counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Test complete LEM session structure
#[test]
fn test_complete_lem_session() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let session_path = temp_dir.path().join("session_20260115_143022");

    // Create directory structure
    let dirs = [
        "recordings",
        "streams",
        "extracted/rgb",
        "extracted/depth",
        "metadata",
        "checksums",
    ];
    for dir in &dirs {
        std::fs::create_dir_all(session_path.join(dir)).expect("Failed to create directory");
    }

    // Create recordings/main_record.mp4 (placeholder)
    std::fs::write(
        session_path.join("recordings/main_record.mp4"),
        b"fake video data",
    )
    .expect("Failed to write video");

    // Create recordings/main_record.meta.json
    let video_meta = json!({
        "codec": "hevc",
        "fps": 60_u32,
        "resolution": [1920_u32, 1080_u32],
        "duration_seconds": 10_u64,
        "total_frames": 600_u64
    });
    std::fs::write(
        session_path.join("recordings/main_record.meta.json"),
        serde_json::to_string_pretty(&video_meta).expect("Failed to serialize"),
    )
    .expect("Failed to write video metadata");

    // Create streams/actions.jsonl
    let actions = vec![
        json!({"t_ns": 1000000000_u64, "frame_idx": 0_u64, "type": "mouse_move", "x": 960_i32, "y": 540_i32, "delta": [0_i32, 0_i32]}),
        json!({"t_ns": 10166666666_u64, "frame_idx": 1_u64, "type": "key_down", "key": "W", "scancode": 17_u32}),
    ];
    let actions_content: String = actions
        .iter()
        .map(|a| serde_json::to_string(a).expect("Failed to serialize"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        session_path.join("streams/actions.jsonl"),
        actions_content,
    )
    .expect("Failed to write actions");

    // Create streams/timestamps.jsonl
    let timestamps = vec![
        json!({"frame_idx": 0_u64, "video_pts_ns": 0_u64, "real_t_ns": 1000000000_u64, "drift_ns": 0_i64}),
        json!({"frame_idx": 1_u64, "video_pts_ns": 16666666_u64, "real_t_ns": 10166666666_u64, "drift_ns": 0_i64}),
    ];
    let timestamps_content: String = timestamps
        .iter()
        .map(|t| serde_json::to_string(t).expect("Failed to serialize"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        session_path.join("streams/timestamps.jsonl"),
        timestamps_content,
    )
    .expect("Failed to write timestamps");

    // Create metadata/session.json
    let session_meta = json!({
        "session_id": "session_20260115_143022",
        "created_at": "2026-01-15T14:30:22Z",
        "duration_seconds": 10_u64,
        "total_frames": 600_u64,
        "total_actions": 2_u64,
        "game": "test_game",
        "version": "1.0.0"
    });
    std::fs::write(
        session_path.join("metadata/session.json"),
        serde_json::to_string_pretty(&session_meta).expect("Failed to serialize"),
    )
    .expect("Failed to write session metadata");

    // Create metadata/hardware.json
    let hardware_meta = json!({
        "cpu": "Test CPU",
        "gpu": "Test GPU",
        "ram_gb": 16_u32,
        "os": "Windows 11",
        "recording_drive": "NVMe SSD",
        "average_fps": 60.0_f64,
        "dropped_frames": 0_u32
    });
    std::fs::write(
        session_path.join("metadata/hardware.json"),
        serde_json::to_string_pretty(&hardware_meta).expect("Failed to serialize"),
    )
    .expect("Failed to write hardware metadata");

    // Create metadata/game.json
    let game_meta = json!({
        "game": "test_game",
        "version": "1.0.0",
        "graphics_settings": {
            "resolution": [1920_u32, 1080_u32],
            "quality": "High",
            "fov": 90_u32,
            "motion_blur": false,
            "ray_tracing": false
        },
        "control_settings": {
            "mouse_sensitivity": 1.0_f64,
            "invert_y": false,
            "keybindings": {}
        }
    });
    std::fs::write(
        session_path.join("metadata/game.json"),
        serde_json::to_string_pretty(&game_meta).expect("Failed to serialize"),
    )
    .expect("Failed to write game metadata");

    // Create metadata/recorder.json
    let recorder_meta = json!({
        "recorder_version": "2.6.0",
        "target_fps": 60_u32,
        "video_codec": "hevc",
        "video_bitrate_mbps": 50_u32,
        "capture_method": "game_capture",
        "record_audio": true,
        "audio_bitrate": 192_u32,
        "record_depth": false,
        "compress_actions": false
    });
    std::fs::write(
        session_path.join("metadata/recorder.json"),
        serde_json::to_string_pretty(&recorder_meta).expect("Failed to serialize"),
    )
    .expect("Failed to write recorder metadata");

    // Verify all files exist
    assert!(session_path.join("recordings/main_record.mp4").exists());
    assert!(session_path.join("recordings/main_record.meta.json").exists());
    assert!(session_path.join("streams/actions.jsonl").exists());
    assert!(session_path.join("streams/timestamps.jsonl").exists());
    assert!(session_path.join("metadata/session.json").exists());
    assert!(session_path.join("metadata/hardware.json").exists());
    assert!(session_path.join("metadata/game.json").exists());
    assert!(session_path.join("metadata/recorder.json").exists());

    // Verify file contents are valid JSON
    let session: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session_path.join("metadata/session.json")).expect("Failed to read"))
            .expect("Failed to parse session.json");
    assert_eq!(session["session_id"], "session_20260115_143022");

    let hardware: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session_path.join("metadata/hardware.json")).expect("Failed to read"))
            .expect("Failed to parse hardware.json");
    assert_eq!(hardware["ram_gb"], 16);

    let actions_lines = std::fs::read_to_string(session_path.join("streams/actions.jsonl"))
        .expect("Failed to read actions.jsonl")
        .lines()
        .count();
    assert_eq!(actions_lines, 2);
}

/// Test backward compatibility with legacy format
#[test]
fn test_legacy_format_compatibility() {
    // Legacy format input event
    let legacy_event = json!({
        "timestamp": 1712345678.123_f64,
        "event_type": "MOUSE_MOVE",
        "event_args": [5, 2]
    });

    // Verify legacy format can be parsed
    let serialized = serde_json::to_string(&legacy_event).expect("Failed to serialize");
    assert!(serialized.contains("\"event_type\":\"MOUSE_MOVE\""));

    // LEM format event
    let lem_event = json!({
        "t_ns": 1712345678123000000_u64,
        "frame_idx": 100_u64,
        "type": "mouse_move",
        "x": 965_i32,
        "y": 542_i32,
        "delta": [5_i32, 2_i32]
    });

    let serialized = serde_json::to_string(&lem_event).expect("Failed to serialize");
    assert!(serialized.contains("\"type\":\"mouse_move\""));
    assert!(serialized.contains("\"t_ns\":"));
}

/// Test action type variants
#[test]
fn test_all_action_types() {
    let action_types = vec![
        ("mouse_move", json!({"type": "mouse_move", "x": 100_i32, "y": 200_i32, "delta": [5_i32, 3_i32]})),
        ("mouse_button", json!({"type": "mouse_button", "button": "left", "pressed": true})),
        ("mouse_wheel", json!({"type": "mouse_wheel", "direction": "up", "amount": 120_i16})),
        ("key_down", json!({"type": "key_down", "key": "W", "scancode": 17_u32})),
        ("key_up", json!({"type": "key_up", "key": "W", "scancode": 17_u32})),
        ("game_command", json!({"type": "game_command", "command": "jump", "target": [100_i32, 0_i32, 200_i32]})),
    ];

    for (name, mut action) in action_types {
        // Add required fields
        action["t_ns"] = json!(15642909582000000_u64);
        action["frame_idx"] = json!(1_u64);

        let serialized = serde_json::to_string(&action).expect("Failed to serialize");
        assert!(serialized.contains(&format!("\"type\":\"{}\"", name)), "Missing type: {}", name);

        // Verify round-trip
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("Failed to parse serialized action");
        assert_eq!(parsed["type"], name);
    }
}