use fs2::FileExt;
use std::{fs::File, io};

/// Unlock explicitly before closing the descriptor. A subprocess can briefly
/// inherit a duplicate during startup; closing only the parent's descriptor
/// would otherwise leave its flock held until that duplicate is closed.
pub(crate) struct FileLock(File);

impl FileLock {
    pub(crate) fn acquire(file: File) -> io::Result<Self> {
        file.try_lock_exclusive()?;
        Ok(Self(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    #[test]
    fn owner_drop_unlocks_even_while_a_duplicate_descriptor_remains_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writer.lock");
        let open = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap()
        };
        let owner = open();
        let inherited = owner.try_clone().unwrap();
        let lock = FileLock::acquire(owner).unwrap();
        assert!(FileLock::acquire(open()).is_err());
        drop(lock);
        let next_owner = FileLock::acquire(open()).unwrap();
        assert!(FileLock::acquire(open()).is_err());
        drop(inherited);
        assert!(FileLock::acquire(open()).is_err());
        drop(next_owner);
        assert!(FileLock::acquire(open()).is_ok());
    }
}
