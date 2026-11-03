//! Kernel locks (`flock`). They go away with the process that holds them, so
//! a crash never leaves a stale lock behind. Files are opened close-on-exec,
//! so programs we start don't inherit them.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result};

pub struct FileLock {
    file: File,
}

fn open(path: &Path) -> Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening lock {}", path.display()))
}

impl FileLock {
    /// Blocks until the lock is ours.
    pub fn acquire(path: &Path) -> Result<FileLock> {
        let file = open(path)?;
        loop {
            let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if r == 0 {
                return Ok(FileLock { file });
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err).with_context(|| format!("locking {}", path.display()));
            }
        }
    }

    /// The lock if nobody holds it, else None.
    pub fn try_acquire(path: &Path) -> Result<Option<FileLock>> {
        let file = open(path)?;
        let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if r == 0 {
            return Ok(Some(FileLock { file }));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err).with_context(|| format!("locking {}", path.display()))
        }
    }

    /// Writes a note (a pid, a run id) into the lock file for people reading it.
    pub fn note(&mut self, text: &str) {
        let _ = self.file.set_len(0);
        let _ = (&self.file).write_all(text.as_bytes());
    }
}

/// Whether someone holds the lock right now.
pub fn is_held(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    matches!(FileLock::try_acquire(path), Ok(None))
}

pub fn read_note(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
