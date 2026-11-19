//! The locks, atomics and access tracking the pool is built from.
//!
//! A normal build takes them from `std`. A build with `--cfg loom` takes them
//! from `loom`, which runs the pool's own code under every interleaving of a
//! model and checks each access to a frame's bytes against the others.

#[cfg(loom)]
pub(super) use loom::sync::{
    atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering},
    Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
#[cfg(not(loom))]
pub(super) use std::sync::{
    atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering},
    Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use std::{
    ops::{Deref, DerefMut},
    sync::{PoisonError, TryLockError},
};

/// A number the pool reports and decides nothing by. It is a plain atomic in
/// every build, so that a model does not explore the orders of its updates.
#[derive(Debug, Default)]
pub(super) struct Counter(std::sync::atomic::AtomicUsize);

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

/// A value on cache lines of its own, so that a thread that writes it does
/// not slow the threads that read its neighbours.
#[derive(Debug, Default)]
#[repr(align(128))]
pub(super) struct Padded<T>(pub(super) T);

impl<T> Deref for Padded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Padded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// An atomic that a thread stores to and then reads another, where the
/// other thread does the same the other way round, starts with a value
/// that was stored `SeqCst`.
///
/// Loom takes the first value of an atomic for a weaker store, which a
/// `SeqCst` load could still return after a later `SeqCst` store. So under
/// loom the value is stored once more, the way every later one is.
pub(super) fn settled_u64(value: u64) -> AtomicU64 {
    let atomic = AtomicU64::new(value);
    #[cfg(loom)]
    atomic.store(value, Ordering::SeqCst);
    atomic
}

/// As [`settled_u64`].
pub(super) fn settled_u32(value: u32) -> AtomicU32 {
    let atomic = AtomicU32::new(value);
    #[cfg(loom)]
    atomic.store(value, Ordering::SeqCst);
    atomic
}

/// As [`settled_u64`].
pub(super) fn settled_usize(value: usize) -> AtomicUsize {
    let atomic = AtomicUsize::new(value);
    #[cfg(loom)]
    atomic.store(value, Ordering::SeqCst);
    atomic
}

/// The numbers of the threads that have ended, for the next threads to
/// take, and the first number that no thread has had.
#[cfg(not(loom))]
static THREAD_NUMBERS: std::sync::Mutex<(Vec<usize>, usize)> =
    std::sync::Mutex::new((Vec::new(), 0));

/// The number of one thread, given back when the thread ends.
#[cfg(not(loom))]
struct ThreadNumber(usize);

#[cfg(not(loom))]
impl ThreadNumber {
    fn take() -> Self {
        let mut numbers = THREAD_NUMBERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Self(numbers.0.pop().unwrap_or_else(|| {
            numbers.1 += 1;
            numbers.1 - 1
        }))
    }
}

#[cfg(not(loom))]
impl Drop for ThreadNumber {
    fn drop(&mut self) {
        THREAD_NUMBERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .push(self.0);
    }
}

/// A number for the calling thread, which it keeps until it ends: the
/// smallest numbers that no running thread has are given out first, so the
/// threads that run at one time have numbers below the most that ever ran
/// at once.
///
/// A thread takes its number when it first asks, under a lock, and asks
/// without one from then on. A thread that is ending and has given its
/// number back is 0.
#[cfg(not(loom))]
pub(super) fn thread_number() -> usize {
    thread_local! {
        static NUMBER: ThreadNumber = ThreadNumber::take();
    }
    NUMBER.try_with(|number| number.0).unwrap_or(0)
}

#[cfg(loom)]
loom::thread_local! {
    static NUMBER: std::cell::Cell<usize> = std::cell::Cell::new(0);
}

/// The number a model gave the calling thread, or 0: a model's threads are
/// numbered by the model, so that every run of it is the same.
#[cfg(loom)]
pub(super) fn thread_number() -> usize {
    NUMBER.with(std::cell::Cell::get)
}

/// Numbers the calling thread of a model.
#[cfg(all(loom, test))]
pub(super) fn set_thread_number(number: usize) {
    NUMBER.with(|cell| cell.set(number));
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

/// A moment in a read that takes no lock, at which a test changes the pool
/// under the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Moment {
    /// The reader has picked a frame and has not announced it.
    Picked,
    /// The reader has announced the frame and has not looked at the count
    /// of changes again.
    Announced,
}
