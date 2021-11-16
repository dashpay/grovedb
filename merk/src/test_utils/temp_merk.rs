use crate::{Merk, Result};
use std::env::temp_dir;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::time::SystemTime;

/// Wraps a Merk instance and deletes it from disk it once it goes out of scope.
pub struct TempMerk<'a> {
    inner: Option<Merk<'a>>,
}

impl<'a> TempMerk<'a> {
    /// Opens a `TempMerk` at the given file path, creating a new one if it does
    /// not exist.
    pub fn open(db: &'a rocksdb::DB, prefix: &[u8]) -> Result<TempMerk<'a>> {
        let inner = Some(Merk::open(db, prefix)?);
        Ok(TempMerk { inner })
    }
}

impl<'a> Drop for TempMerk<'a> {
    fn drop(&mut self) {
        self.inner
            .take()
            .unwrap()
            .destroy()
            .expect("failed to delete db");
    }
}

impl<'a> Deref for TempMerk<'a> {
    type Target = Merk<'a>;

    fn deref(&self) -> &'a Merk {
        self.inner.as_ref().unwrap()
    }
}

impl<'a> DerefMut for TempMerk<'a> {
    fn deref_mut(&'a mut self) -> &'a mut Merk {
        self.inner.as_mut().unwrap()
    }
}
