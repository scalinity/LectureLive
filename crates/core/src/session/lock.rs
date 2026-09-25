use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// Exclusive advisory lock on a lecture folder (`.live_notes/lock`, spec §8); released on drop.
pub struct FolderLock {
    _file: File,
}

impl FolderLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join(".live_notes").join("lock");
        std::fs::create_dir_all(path.parent().unwrap())?;
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => bail!("another LectureLive session is using {}", dir.display()),
            Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_has_one_session_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let first = FolderLock::acquire(dir.path()).unwrap();
        let err = FolderLock::acquire(dir.path()).err().unwrap();
        assert!(format!("{err:#}").contains("another LectureLive session"), "{err:#}");
        drop(first);
        FolderLock::acquire(dir.path()).unwrap();
    }
}
