use std::path::{Path, PathBuf};

use tokio::task::JoinError;

use crate::validation::ValidationResult;

#[derive(Debug)]
pub enum CreateTarError {
    Join(JoinError),
    InvalidFilename(PathBuf),
    Io(std::io::Error),
}
impl std::fmt::Display for CreateTarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateTarError::Join(e) => write!(f, "Join error: {e}"),
            CreateTarError::InvalidFilename(path) => write!(f, "Invalid filename: {path:?}"),
            CreateTarError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}
impl std::error::Error for CreateTarError {}
impl From<JoinError> for CreateTarError {
    fn from(e: JoinError) -> Self {
        CreateTarError::Join(e)
    }
}
impl From<std::io::Error> for CreateTarError {
    fn from(e: std::io::Error) -> Self {
        CreateTarError::Io(e)
    }
}
pub async fn create_tar_file(
    recording_path: &Path,
    validation: &ValidationResult,
) -> Result<PathBuf, CreateTarError> {
    tokio::task::spawn_blocking({
        let recording_path = recording_path.to_path_buf();
        let validation = validation.clone();
        move || {
            // Create tar file inside the recording folder
            let uuid_str = uuid::Uuid::new_v4().simple().to_string();
            let tar_name = format!("{}.tar", uuid_str.get(0..16).unwrap_or(&uuid_str));
            let tar_path = recording_path.join(tar_name);

            // Files to include in the tar archive
            let source_files = [
                ("video", &validation.mp4_path),
                ("inputs", &validation.csv_path),
                ("metadata", &validation.meta_path),
            ];

            // Log source file sizes before tar creation for diagnostics
            let mut total_source_size: u64 = 0;
            for (label, path) in &source_files {
                match std::fs::metadata(path) {
                    Ok(meta) => {
                        let size = meta.len();
                        total_source_size += size;
                        tracing::info!(
                            "Tar source file: {} = {} bytes ({:.2} MB)",
                            label,
                            size,
                            size as f64 / (1024.0 * 1024.0)
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get size of {} file {:?}: {}", label, path, e);
                    }
                }
            }
            tracing::info!(
                "Total source files size: {} bytes ({:.2} MB)",
                total_source_size,
                total_source_size as f64 / (1024.0 * 1024.0)
            );

            let start_time = std::time::Instant::now();

            // Create the tar file and ensure cleanup on error
            let result = (|| -> Result<PathBuf, CreateTarError> {
                let mut tar = tar::Builder::new(std::fs::File::create(&tar_path)?);
                for (_label, path) in &source_files {
                    tar.append_file(
                        path.file_name()
                            .ok_or_else(|| CreateTarError::InvalidFilename((*path).to_owned()))?,
                        &mut std::fs::File::open(path)?,
                    )?;
                }
                // Explicitly finish the tar to ensure proper closure
                tar.finish()?;
                Ok(tar_path.clone())
            })();

            // Clean up partial tar file on error
            if result.is_err() && tar_path.exists() {
                tracing::warn!(
                    "Tar creation failed, cleaning up partial tar file at {}",
                    tar_path.display()
                );
                if let Err(e) = std::fs::remove_file(&tar_path) {
                    tracing::warn!("Failed to remove partial tar file {}: {}", tar_path.display(), e);
                }
            }

            // Log final tar file size and creation time on success
            if result.is_ok() {
                let elapsed = start_time.elapsed();
                match std::fs::metadata(&tar_path) {
                    Ok(meta) => {
                        let tar_size = meta.len();
                        tracing::info!(
                            "Tar file created: {} bytes ({:.2} MB) in {:.2}s",
                            tar_size,
                            tar_size as f64 / (1024.0 * 1024.0),
                            elapsed.as_secs_f64()
                        );
                        // Warn if tar is significantly larger than source files (shouldn't happen)
                        if tar_size > total_source_size + 1024 * 1024 {
                            tracing::warn!(
                                "Tar file ({} bytes) is larger than expected based on source files ({} bytes)",
                                tar_size,
                                total_source_size
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get tar file size: {}", e);
                    }
                }
            }

            result
        }
    })
    .await?
}
