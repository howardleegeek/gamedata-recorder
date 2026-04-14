use std::{
    fs::File,
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr as _,
};

use color_eyre::eyre::{self, Context as _};
use serde::{Deserialize, Serialize};

use crate::output_types::{InputEvent, InputEventType, Metadata};

pub mod gamepad;
pub mod keyboard;
pub mod mouse;
pub mod video;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InputStats {
    #[serde(flatten)]
    pub keyboard_stats: keyboard::KeyboardOutputStats,
    #[serde(flatten)]
    pub mouse_stats: mouse::MouseOutputStats,
    #[serde(flatten)]
    pub gamepad_stats: gamepad::GamepadOutputStats,
}

#[derive(Clone)]
pub struct ValidationResult {
    pub mp4_path: PathBuf,
    pub csv_path: PathBuf,
    pub meta_path: PathBuf,
    pub metadata: Metadata,
}

/// Validates the given recording folder, creating a [`constants::filename::recording::INVALID`] file if validation fails.
pub fn validate_folder(path: &Path) -> eyre::Result<ValidationResult> {
    // Validate path to prevent directory traversal attacks
    // Reject paths that contain components which could escape the intended directory
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        eyre::bail!("Invalid path: contains parent directory references");
    }

    match validate_folder_impl(path) {
        Ok(result) => {
            // Clean up any stale INVALID marker file from previous failed validation
            let invalid_marker = path.join(constants::filename::recording::INVALID);
            if invalid_marker.is_file() {
                if let Err(e) = std::fs::remove_file(&invalid_marker) {
                    tracing::warn!("Failed to remove stale INVALID marker file: {}", e);
                }
            }
            Ok(result)
        }
        Err(e) => {
            // Use atomic write pattern for crash durability: write to temp, sync, rename
            let invalid_path = path.join(constants::filename::recording::INVALID);
            let temp_path = invalid_path.with_extension("tmp");
            if let Err(write_err) = std::fs::write(&temp_path, e.join("\n")) {
                tracing::error!("Failed to write INVALID marker temp file: {write_err}");
            } else if let Err(sync_err) = std::fs::File::open(&temp_path)
                .and_then(|f| f.sync_all())
            {
                tracing::error!("Failed to sync INVALID marker file: {sync_err}");
            } else if let Err(rename_err) = std::fs::rename(&temp_path, &invalid_path) {
                tracing::error!("Failed to rename INVALID marker file: {rename_err}");
                // Clean up temp file on rename failure
                std::fs::remove_file(&temp_path).ok();
            }
            eyre::bail!("Validation failures: {}", e.join("\n"));
        }
    }
}

// Maximum allowed size for input files (50 MB) to prevent memory exhaustion
const MAX_INPUT_FILE_SIZE: u64 = 50 * 1024 * 1024;

// This is a bit messy - I don't love using a Vec of Strings for the errors -
// but I wanted to capture the multi-error nature of the validation process
//
// TODO: Think of a better way to handle this
fn validate_folder_impl(path: &Path) -> Result<ValidationResult, Vec<String>> {
    // This is not guaranteed to be constants::recording::VIDEO_FILENAME if the WebSocket recorder
    // is being used, which is why we search for it
    let Some(mp4_path) = path
        .read_dir()
        .map_err(|e| vec![e.to_string()])?
        .filter_map(|e| match e {
            Ok(entry) => Some(entry.path()),
            Err(e) => {
                tracing::warn!("Error reading directory entry in {}: {}", path.display(), e);
                None
            }
        })
        .find(|e| e.extension().and_then(|e| e.to_str()) == Some("mp4"))
    else {
        return Err(vec![format!("No MP4 file found in {}", path.display())]);
    };
    // Support both new .jsonl and legacy .csv input files
    let input_path = path.join(constants::filename::recording::INPUTS);
    let csv_path = if input_path.is_file() {
        input_path
    } else {
        // Fall back to legacy CSV format for older recordings
        let legacy_path = path.join(constants::filename::recording::INPUTS_LEGACY_CSV);
        if !legacy_path.is_file() {
            return Err(vec![format!(
                "No input file found in {} (expected {} or {})",
                path.display(),
                constants::filename::recording::INPUTS,
                constants::filename::recording::INPUTS_LEGACY_CSV,
            )]);
        }
        legacy_path
    };
    let meta_path = path.join(constants::filename::recording::METADATA);
    if !meta_path.is_file() {
        return Err(vec![format!(
            "No metadata file found in {} (expected {})",
            path.display(),
            meta_path.display()
        )]);
    }

    let metadata = std::fs::read_to_string(&meta_path)
        .map_err(|e| vec![format!("Error reading metadata file: {e:?}")])?;
    let mut metadata = serde_json::from_str::<Metadata>(&metadata)
        .map_err(|e| vec![format!("Error parsing metadata file: {e:?}")])?;

    let (input_stats, mut invalid_reasons) = validate_files(&metadata, &mp4_path, &csv_path)
        .map_err(|e| vec![format!("Error validating recording at {path:?}: {e:?}")])?;

    metadata.input_stats = Some(input_stats);

    match serde_json::to_string_pretty(&metadata) {
        Ok(metadata) => {
            // fs::write will completely overwrite existing metadata file, and if the OS is
            // out of available memory (either due to user skill issue or a bug with owlc),
            // it becomes a nightmare case where the metadata just gets deleted.
            // To be safe, we use atomic write pattern: write to temp file, sync, then rename
            // This prevents corruption if the process crashes, runs out of memory, or loses power
            let temp_path = meta_path.with_extension("tmp");
            if let Err(e) = (|| {
                let file = std::fs::File::create(&temp_path)?;
                std::io::Write::write_all(&file, metadata.as_bytes())?;
                file.sync_all()?;
                std::fs::rename(&temp_path, &meta_path)
            })() {
                invalid_reasons.push(format!("Error writing metadata temp file: {e:?}"));
                // Clean up temp file on failure
                if let Err(e) = std::fs::remove_file(&temp_path) {
                    tracing::warn!("Failed to clean up temp file after rename failure: {}", e);
                }
            }
        }
        Err(e) => invalid_reasons.push(format!("Error generating JSON for metadata file: {e:?}")),
    }

    if invalid_reasons.is_empty() {
        Ok(ValidationResult {
            mp4_path,
            csv_path,
            meta_path,
            metadata,
        })
    } else {
        Err(invalid_reasons)
    }
}

struct ValidationInput<'a> {
    pub start_time: f64,
    pub filtered_events: &'a [InputEvent],
    pub duration_minutes: f64,
}
fn validate_files(
    metadata: &Metadata,
    mp4_path: &Path,
    csv_path: &Path,
) -> eyre::Result<(InputStats, Vec<String>)> {
    let file_content = std::fs::read_to_string(csv_path)
        .with_context(|| format!("Error reading CSV file at {csv_path:?})"))?;
    // Legacy CSV format has a header row that must be skipped;
    // new JSON Lines format has no header (each line is self-describing).
    let is_legacy_csv = csv_path.extension().and_then(|e| e.to_str()) == Some("csv");
    let events = file_content
        .lines()
        .skip(if is_legacy_csv { 1 } else { 0 })
        .map(InputEvent::from_str)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("Error parsing CSV file at {csv_path:?}"))?;

    // Validate that we have at least some events to process
    if events.is_empty() {
        eyre::bail!("No input events found in recording");
    }

    let start_time = events
        .iter()
        .find(|event| matches!(event.event, InputEventType::Start { .. }))
        .map(|event| event.timestamp)
        .unwrap_or(0.0);

    let end_time = events
        .iter()
        .find(|event| matches!(event.event, InputEventType::End { .. }))
        .or_else(|| events.last())
        .map(|event| event.timestamp)
        .unwrap_or(0.0);

    // Validate timeline values are finite to prevent NaN/Infinity from contaminating calculations
    if !start_time.is_finite() || !end_time.is_finite() {
        eyre::bail!(
            "Invalid timeline: start_time ({}) or end_time ({}) is not finite",
            start_time, end_time
        );
    }

    // Validate timeline consistency to prevent empty filtered events and confusing errors
    if start_time > end_time {
        eyre::bail!(
            "Invalid timeline: start_time ({}) is after end_time ({})",
            start_time, end_time
        );
    }

    let filtered_events: Vec<_> = events
        .iter()
        .filter(|event| event.timestamp >= start_time && event.timestamp <= end_time)
        .cloned()
        .collect();

    let input = ValidationInput {
        start_time,
        filtered_events: &filtered_events,
        duration_minutes: (end_time - start_time) / 60.0,
    };

    let mut invalid_reasons = video::validate(mp4_path, metadata);
    let (keyboard_stats, keyboard_invalid_reasons) = keyboard::validate(&input);
    let (mouse_stats, mouse_invalid_reasons) = mouse::validate(&input);
    let (gamepad_stats, gamepad_invalid_reasons) = gamepad::validate(&input);

    // Invalidate if any input type has validation errors
    if !keyboard_invalid_reasons.is_empty()
        || !mouse_invalid_reasons.is_empty()
        || !gamepad_invalid_reasons.is_empty()
    {
        invalid_reasons.extend(keyboard_invalid_reasons);
        invalid_reasons.extend(mouse_invalid_reasons);
        invalid_reasons.extend(gamepad_invalid_reasons);
    }

    Ok((
        InputStats {
            keyboard_stats,
            mouse_stats,
            gamepad_stats,
        },
        invalid_reasons,
    ))
}
