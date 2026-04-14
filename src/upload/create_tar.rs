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
/// RAII guard that cleans up a partially created tar file on drop.
/// Disarmed when tar creation succeeds via `into_path()`.
struct CreatingTarGuard {
    path: Option<PathBuf>,
}

impl CreatingTarGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Disarm the guard and return the path, preventing cleanup on drop.
    fn into_path(mut self) -> PathBuf {
        self.path.take().expect("path always exists before disarm")
    }
}

impl Drop for CreatingTarGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            if path.is_file() {
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(
                        "CreatingTarGuard: failed to clean up partial tar at {}: {}",
                        path.display(),
                        e
                    );
                } else {
                    tracing::info!(
                        "CreatingTarGuard: cleaned up partial tar at {}",
                        path.display()
                    );
                }
            }
        }
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
            let tar_path = recording_path.join(format!(
                "{}.tar",
                &uuid::Uuid::new_v4().simple().to_string()[0..16]
            ));

            // Guard to clean up partial tar if creation fails
            let guard = CreatingTarGuard::new(tar_path.clone());

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

            // Log final tar file size and creation time
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

            // Disarm the guard: tar creation succeeded, don't clean up
            Ok(guard.into_path())
        }
    })
    .await?
}
