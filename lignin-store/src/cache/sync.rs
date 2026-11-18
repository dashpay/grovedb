//! The locks, atomics and access tracking the pool is built from.
//!
//! A normal build takes them from `std`. A build with `--cfg loom` takes them
//! from `loom`, which runs the pool's own code under every interleaving of a
//! model and checks each access to a frame's bytes against the others.

#[cfg(loom)]
pub(super) use loom::sync::{
    atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering},
    Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use std::sync::{atomic::AtomicUsize, PoisonError, TryLockError};
#[cfg(not(loom))]
pub(super) use std::sync::{
    atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering},
    Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
};

/// A number the pool reports and decides nothing by. It is a plain atomic in
/// every build, so that a model does not explore the orders of its updates.
#[derive(Debug, Default)]
pub(super) struct Counter(AtomicUsize);

impl Counter {
    pub(super) fn add(&self, amount: usize) {
        self.0
            .fetch_add(amount, std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn sub(&self, amount: usize) {
        self.0
            .fetch_sub(amount, std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn get(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Locks a mutex. A panic in another holder does not stop the pool: every
/// change under one of its locks is finished before anything that can panic.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn try_read<T>(lock: &RwLock<T>) -> Option<RwLockReadGuard<'_, T>> {
    match lock.try_read() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

pub(super) fn try_write<T>(lock: &RwLock<T>) -> Option<RwLockWriteGuard<'_, T>> {
    match lock.try_write() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// What a model checks the accesses to a frame's bytes against.
///
/// Whoever reads the bytes holds a [`Reading`] for as long as it may read,
/// and whoever writes them holds a [`Writing`]. Under loom a `Writing` beside
/// any other access fails the model, and so does one that is not ordered
/// after every earlier access by a lock or an atomic. A normal build keeps
/// nothing.
#[derive(Debug)]
pub(super) struct Tracked {
    #[cfg(loom)]
    cell: loom::cell::UnsafeCell<()>,
}

/// A read access to a frame's bytes, for as long as it lives.
#[derive(Debug)]
pub(super) struct Reading {
    #[cfg(loom)]
    _access: loom::cell::ConstPtr<()>,
}

/// The one write access to a frame's bytes, for as long as it lives.
#[derive(Debug)]
pub(super) struct Writing {
    #[cfg(loom)]
    _access: loom::cell::MutPtr<()>,
}

impl Tracked {
    pub(super) fn new() -> Self {
        Self {
            #[cfg(loom)]
            cell: loom::cell::UnsafeCell::new(()),
        }
    }

    pub(super) fn reading(&self) -> Reading {
        Reading {
            #[cfg(loom)]
            _access: self.cell.get(),
        }
    }

    pub(super) fn writing(&self) -> Writing {
        Writing {
            #[cfg(loom)]
            _access: self.cell.get_mut(),
        }
    }
}
