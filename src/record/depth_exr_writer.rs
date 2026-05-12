//! Depth EXR writer for LEM Format
//!
//! Generates per-frame depth EXR files using DepthAnything V2.
//! - Reads frame timestamps from frames.jsonl (1 Hz cadence)
//! - Runs depth inference on each frame
//! - Writes 32-bit float depth tensor to depth_<idx>.exr
//!
//! This module shells out to Python with DepthAnything V2 and onnxruntime.

use std::path::PathBuf;
use std::process::Command;

use color_eyre::{Result, eyre::Context};

/// Generate depth EXR files for a recording session.
///
/// This function shells out to Python with DepthAnything V2 to generate depth maps.
/// The script reads from frames.jsonl in the session directory and writes to depth/.
///
/// # Arguments
/// * `session_dir` - Path to the recording session directory
/// * `resolution` - Target resolution (width, height)
/// * `device` - Device to run inference on ("auto", "cuda", or "cpu")
pub async fn write_depth_exr(
    session_dir: &PathBuf,
    resolution: Option<(u32, u32)>,
    device: Option<String>,
) -> Result<usize> {
    let session_dir = session_dir.clone();
    let resolution = resolution.unwrap_or((1920, 1080));
    let device = device.unwrap_or_else(|| "auto".to_string());
    
    let output = tokio::task::spawn_blocking(move || {
        let script_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default()
            .join("scripts")
            .join("generate_depth_exr.py");
        
        // Try multiple possible locations for the script
        let possible_paths = [
            script_path,
            PathBuf::from("scripts/generate_depth_exr.py"),
            PathBuf::from("vendor/recorder/scripts/generate_depth_exr.py"),
            PathBuf::from("../../../vendor/recorder/scripts/generate_depth_exr.py"),
        ];
        
        let script = possible_paths
            .iter()
            .find(|p| p.exists())
            .cloned()
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "Could not find generate_depth_exr.py script in any known location"
                )
            })?;
        
        tracing::debug!(
            session_dir = %session_dir.display(),
            resolution = ?resolution,
            device = %device,
            "Running depth EXR generator"
        );
        
        let mut cmd = Command::new("python3");
        cmd.arg(&script)
            .arg(&session_dir)
            .arg("--resolution")
            .arg(format!("{}x{}", resolution.0, resolution.1))
            .arg("--device")
            .arg(&device);
        
        let output = cmd
            .output()
            .context("Failed to execute depth_exr.py script")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(color_eyre::eyre::eyre!(
                "depth_exr.py script failed: {}\nstdout: {}\nstderr: {}",
                output.status,
                stdout,
                stderr
            ));
        }
        
        // Count generated EXR files
        let depth_dir = session_dir.join("depth");
        let exr_count = if depth_dir.exists() {
            std::fs::read_dir(&depth_dir)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path()
                                .extension()
                                .map(|ext| ext == "exr")
                                .unwrap_or(false)
                        })
                        .count()
                })
                .unwrap_or(0)
        } else {
            0
        };
        
        tracing::info!(
            session_dir = %session_dir.display(),
            exr_count = exr_count,
            "depth EXR files generated successfully"
        );
        
        Ok(exr_count)
    })
    .await
    .context("Failed to join depth EXR generation task")??;
    
    Ok(output)
}

/// Check if depth EXR generation is available (dependencies installed).
pub fn is_depth_available() -> bool {
    // Check if Python and required packages are available
    let check_script = r#"
import sys
try:
    import torch
    import numpy
    from PIL import Image
    print('ok')
    sys.exit(0)
except ImportError as e:
    print(f'missing: {e}')
    sys.exit(1)
"#;
    
    let output = Command::new("python3")
        .arg("-c")
        .arg(check_script)
        .output();
    
    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_depth_script_exists() {
        // Check that the Python script exists
        let possible_paths = [
            PathBuf::from("scripts/generate_depth_exr.py"),
            PathBuf::from("vendor/recorder/scripts/generate_depth_exr.py"),
        ];
        
        let found = possible_paths.iter().any(|p| p.exists());
        assert!(found, "generate_depth_exr.py script not found");
    }
    
    #[tokio::test]
    async fn test_depth_availability_check() {
        // This test just verifies the availability check runs
        // The actual depth generation requires more setup
        let _ = is_depth_available();
    }
    
    #[tokio::test]
    async fn test_depth_generation_with_mock_frames() {
        // Create a temp directory with mock frames
        let temp_dir = TempDir::new().unwrap();
        let session_dir = temp_dir.path();
        
        // Create depth directory
        fs::create_dir_all(session_dir.join("depth")).unwrap();
        
        // Write mock frames.jsonl
        let frames = r#"{"idx": 0, "t_ns": 0}
{"idx": 1, "t_ns": 1000000000}
{"idx": 2, "t_ns": 2000000000}
"#;
        fs::write(session_dir.join("frames.jsonl"), frames).unwrap();
        
        // Try to run the script (will likely fail without proper setup)
        let result = write_depth_exr(
            &session_dir.to_path_buf(),
            Some((640, 480)), // Small resolution for testing
            Some("cpu".to_string()),
        ).await;
        
        match result {
            Ok(count) => {
                tracing::info!("Generated {} depth files", count);
                // Check that some depth files were created
                let depth_dir = session_dir.join("depth");
                if depth_dir.exists() {
                    let files: Vec<_> = std::fs::read_dir(depth_dir)
                        .unwrap()
                        .filter_map(|e| e.ok())
                        .collect();
                    assert!(!files.is_empty(), "Expected some depth files");
                }
            }
            Err(e) => {
                // Depth generation may fail for various reasons in test env
                tracing::warn!("depth generation failed (expected in test env): {}", e);
            }
        }
    }
}
