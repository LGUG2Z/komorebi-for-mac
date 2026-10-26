//! Telling a recorder that is running from one that died.
//!
//! A session's manifest says `recording` both while a recorder is alive and after one was killed,
//! and the right thing to do differs: wait, or `reindex`. The recorder therefore holds an exclusive
//! `flock` on `recorder.lock` for as long as it runs. The kernel drops the lock the instant the
//! process dies, however it dies, so a lock that is still held means a live recorder and one that
//! is free means there is none.

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

pub const LOCK_FILE: &str = "recorder.lock";

/// Held by a running recorder; released when dropped or when the process ends.
#[derive(Debug)]
pub struct RecorderLock {
    _file: File,
}

impl RecorderLock {
    /// Take the lock for the session in `dir` and write our pid into it.
    pub fn acquire(dir: &Path) -> std::io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;

        // SAFETY: the descriptor is valid for the lifetime of `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error());
        }

        // only now that the lock is ours is it safe to replace what the previous owner wrote
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Self { _file: file })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorder {
    /// A recorder holds the lock; the pid it wrote, if readable.
    Running(Option<u32>),
    /// The lock file exists and nobody holds it.
    NotRunning,
    /// There is no lock file: the session predates it.
    Unknown,
}

#[must_use]
pub fn recorder(dir: &Path) -> Recorder {
    let path = dir.join(LOCK_FILE);
    let Ok(file) = File::open(&path) else {
        return Recorder::Unknown;
    };

    // SAFETY: the descriptor is valid for the lifetime of `file`.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        // SAFETY: as above; releasing the lock we just took.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return Recorder::NotRunning;
    }

    let pid = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.trim().parse().ok());
    Recorder::Running(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-lock-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn a_session_without_a_lock_file_is_unknown() {
        assert_eq!(recorder(&temp("none")), Recorder::Unknown);
    }

    #[test]
    fn the_lock_is_held_while_the_recorder_lives_and_free_after() {
        let dir = temp("held");
        let lock = RecorderLock::acquire(&dir).expect("acquire");

        assert_eq!(recorder(&dir), Recorder::Running(Some(std::process::id())));

        drop(lock);
        assert_eq!(recorder(&dir), Recorder::NotRunning);
    }

    #[test]
    fn two_recorders_cannot_share_a_session() {
        let dir = temp("twice");
        let _first = RecorderLock::acquire(&dir).expect("first");
        assert!(RecorderLock::acquire(&dir).is_err());
    }
}
