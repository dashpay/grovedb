use crate::{Merk, Result};
use std::fs;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::path::Path;

/// Wraps a Merk instance and drops it without flushing once it goes out of
/// scope.
pub struct CrashMerk<'a> {
    inner: Option<ManuallyDrop<Merk<'a>>>,
    path: Box<Path>,
    prefix: Vec<u8>,
}

impl<'a> CrashMerk<'a> {
    /// Opens a `CrashMerk` at the given file path, creating a new one if it does
    /// not exist.
    pub fn open(db: &rocksdb::DB, prefix: Vec<u8>) -> Result<CrashMerk<'a>> {
        let merk = Merk::open(db, &prefix)?;
        let inner = Some(ManuallyDrop::new(merk));
        Ok(CrashMerk {
            inner,
            path: path.as_ref().into(),
            prefix
        })
    }

    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn crash(&mut self) -> Result<()> {
        ManuallyDrop::drop(&mut self.inner.take().unwrap());

        // rename to invalidate rocksdb's lock
        let file_name = format!(
            "{}_crashed",
            self.path.file_name().unwrap().to_str().unwrap()
        );
        let new_path = self.path.with_file_name(file_name);
        fs::rename(&self.path, &new_path)?;

        let mut new_merk = CrashMerk::open(&new_path, self.prefix.clone())?;
        self.inner = new_merk.inner.take();
        self.path = new_merk.path;
        Ok(())
    }

    pub fn into_inner(self) -> Merk<'a> {
        ManuallyDrop::into_inner(self.inner.unwrap())
    }

    pub fn destroy(self) -> Result<()> {
        self.into_inner().destroy()
    }
}

impl<'a> Deref for CrashMerk<'a> {
    type Target = Merk<'a>;

    fn deref(&self) -> &Merk {
        self.inner.as_ref().unwrap()
    }
}

impl<'a> DerefMut for CrashMerk<'a> {
    fn deref_mut(&mut self) -> &mut Merk {
        self.inner.as_mut().unwrap()
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

        let mut merk = CrashMerk::open(&path).expect("failed to open merk");
        merk.apply(&[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))], &[])
            .expect("apply failed");
        unsafe {
            merk.crash().unwrap();
        }
        assert_eq!(merk.get(&[1, 2, 3]).expect("failed to get"), None);
        merk.into_inner().destroy().unwrap();
    }
}
