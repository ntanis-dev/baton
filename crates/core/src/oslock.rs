//! Locks the operating system keeps: taken on an open file, and let go by the system the moment the
//! process that holds one ends, however it ends. So nothing has to judge whether a lock was left behind
//! (no process id to look up, no age to compare), and nothing is to clean up after a crash.
//!
//! The file a lock is taken on holds nothing and stays where it is: removing it would let a third
//! process make it anew while a second still holds the old one.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// How long a waiting [`OsLock::take`] sleeps between two tries.
const RETRY: Duration = Duration::from_millis(10);

/// A lock, held until it is dropped (or its process ends).
#[derive(Debug)]
pub struct OsLock {
    /// The open file the lock is on.
    file: File,
}

/// Opens a lock's file, making it when `create` says so.
fn open(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

impl OsLock {
    /// Takes the lock on `path` (made when it is not there, with its folder); None when it is held.
    pub fn try_take(path: &Path) -> io::Result<Option<OsLock>> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        Self::lock(open(path, true)?)
    }

    /// [`OsLock::try_take`], trying again for up to `wait` while it is held.
    pub fn take(path: &Path, wait: Duration) -> io::Result<Option<OsLock>> {
        let started = Instant::now();
        loop {
            if let Some(lock) = Self::try_take(path)? {
                return Ok(Some(lock));
            }
            if started.elapsed() >= wait {
                return Ok(None);
            }
            thread::sleep(RETRY);
        }
    }

    /// Locks an open file; None when another handle holds the lock.
    fn lock(file: File) -> io::Result<Option<OsLock>> {
        match file.try_lock() {
            Ok(()) => Ok(Some(OsLock { file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for OsLock {
    fn drop(&mut self) {
        // Closing the file lets the lock go too, but not at once on every system.
        let _ = self.file.unlock();
    }
}

/// Whether the lock on `path` is held now, by any process (this one included). A lock that was never
/// taken has no file and is not held.
pub fn is_held(path: &Path) -> bool {
    match open(path, false) {
        Ok(file) => matches!(OsLock::lock(file), Ok(None)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time_and_free_again_when_it_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("a.lock");
        assert!(!is_held(&path), "never taken");
        let first = OsLock::try_take(&path).unwrap().unwrap();
        assert!(is_held(&path));
        assert!(OsLock::try_take(&path).unwrap().is_none());
        assert!(OsLock::take(&path, Duration::from_millis(30)).unwrap().is_none());
        drop(first);
        assert!(!is_held(&path));
        assert!(path.exists(), "the file stays");
        let again = OsLock::take(&path, Duration::from_millis(30)).unwrap();
        assert!(again.is_some());
    }

    #[test]
    fn a_waiting_take_gets_the_lock_once_it_is_free() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.lock");
        let first = OsLock::try_take(&path).unwrap().unwrap();
        let waiting = {
            let path = path.clone();
            thread::spawn(move || OsLock::take(&path, Duration::from_secs(5)).unwrap().is_some())
        };
        thread::sleep(Duration::from_millis(40));
        drop(first);
        assert!(waiting.join().unwrap());
    }
}
