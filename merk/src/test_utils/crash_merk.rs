use std::{
    fs,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
};

use tempdir::TempDir;

use crate::{Merk, Result};

/// Wraps a Merk instance and drops it without flushing once it goes out of
/// scope.
pub struct CrashMerk {
    merk: Merk,
    path: Option<TempDir>,
}

impl CrashMerk {
    /// Opens a `CrashMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn open() -> Result<CrashMerk> {
        let path = TempDir::new("db").expect("cannot create tempdir");
        let db = super::default_rocksdb(path.path());
        let merk = Merk::open(db, Vec::new())?;
        Ok(CrashMerk {
            merk,
            path: Some(path),
        })
    }

    pub fn crash(&mut self) {
        self.path.take().map(|x| drop(x));
    }
}

impl Deref for CrashMerk {
    type Target = Merk;

    fn deref(&self) -> &Merk {
        &self.merk
    }
}

impl DerefMut for CrashMerk {
    fn deref_mut(&mut self) -> &mut Merk {
        &mut self.merk
    }
}

#[cfg(test)]
mod tests {
    use super::CrashMerk;
    use crate::Op;

    #[test]
    #[ignore] // currently this still works because we enabled the WAL
    fn crash() {
        let path = std::thread::current().name().unwrap().to_owned();

        let mut merk = CrashMerk::open().expect("failed to open merk");
        merk.apply(&[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))], &[])
            .expect("apply failed");

        merk.crash();

        assert_eq!(merk.get(&[1, 2, 3]).expect("failed to get"), None);
    }
}
