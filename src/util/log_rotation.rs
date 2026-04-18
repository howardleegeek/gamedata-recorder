use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing_subscriber::fmt::MakeWriter;

const MAX_LOG_SIZE: u64 = 20 * 1024 * 1024; // 20MB
const MAX_LOG_FILES: usize = 2;

/// A rotating file writer that maintains up to MAX_LOG_FILES of MAX_LOG_SIZE each.
/// When the current log file exceeds MAX_LOG_SIZE, it rotates to .1, .2, etc.,
/// removing the oldest file if MAX_LOG_FILES is exceeded.
pub struct RotatingFileWriter {
    log_dir: PathBuf,
    log_name: String,
}

impl RotatingFileWriter {
    pub fn new(log_dir: PathBuf, log_name: String) -> std::io::Result<Self> {
        Ok(Self { log_dir, log_name })
    }

    fn log_path(&self, suffix: Option<usize>) -> PathBuf {
        match suffix {
            None => self.log_dir.join(&self.log_name),
            Some(n) => self.log_dir.join(format!("{}.{}", &self.log_name, n)),
        }
    }

    /// Rotate log files: .2 -> .3 (if within limit), .1 -> .2, current -> .1
    fn rotate_logs(&self) -> std::io::Result<()> {
        // Remove oldest file if it exists and we're at the limit
        let oldest_path = self.log_path(Some(MAX_LOG_FILES));
        if oldest_path.exists() {
            std::fs::remove_file(&oldest_path)?;
        }

        // Rotate existing files: .2 -> .3, .1 -> .2, etc.
        for i in (1..MAX_LOG_FILES).rev() {
            let from = self.log_path(Some(i));
            let to = self.log_path(Some(i + 1));
            if from.exists() {
                std::fs::rename(&from, &to)?;
            }
        }

        // Rotate current log to .1
        let current = self.log_path(None);
        let rotated = self.log_path(Some(1));
        if current.exists() {
            std::fs::rename(&current, &rotated)?;
        }

        Ok(())
    }

    fn check_and_rotate(&self) -> std::io::Result<()> {
        let current_path = self.log_path(None);

        if current_path.exists() {
            let metadata = std::fs::metadata(&current_path)?;
            if metadata.len() >= MAX_LOG_SIZE {
                self.rotate_logs()?;
            }
        }

        Ok(())
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.check_and_rotate()?;

        let path = self.log_path(None);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

        file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let path = self.log_path(None);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

        file.flush()
    }
}

// Implement MakeWriter so this works with tracing-subscriber
impl<'a> MakeWriter<'a> for RotatingFileWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingFileWriter {
            log_dir: self.log_dir.clone(),
            log_name: self.log_name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_rotation_logic() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_dir = temp_dir.path();
        let log_name = "test.log".to_string();
        let writer = RotatingFileWriter::new(log_dir.to_path_buf(), log_name).unwrap();

        // Test that log paths are correct
        assert_eq!(writer.log_path(None), log_dir.join("test.log"));
        assert_eq!(writer.log_path(Some(1)), log_dir.join("test.log.1"));
        assert_eq!(writer.log_path(Some(3)), log_dir.join("test.log.3"));
    }
}
