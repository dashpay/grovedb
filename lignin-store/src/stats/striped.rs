//! A count that each thread adds to in a cache line of its own.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// How many stripes a count has. Threads past this many share them.
const STRIPES: usize = 32;

static NEXT_THREAD: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static STRIPE: usize = NEXT_THREAD.fetch_add(1, Ordering::Relaxed) % STRIPES;
}

#[derive(Debug, Default)]
#[repr(align(128))]
struct Stripe(AtomicU64);

/// A count in stripes: an add touches the calling thread's stripe, and a
/// read sums them all.
#[derive(Debug)]
pub(super) struct Striped(Box<[Stripe; STRIPES]>);

impl Default for Striped {
    fn default() -> Self {
        Self(Box::new(std::array::from_fn(|_| Stripe::default())))
    }
}

impl Striped {
    pub(super) fn add(&self, by: u64) {
        let stripe = STRIPE.with(|stripe| *stripe);
        self.0[stripe].0.fetch_add(by, Ordering::Relaxed);
    }

    pub(super) fn sum(&self) -> u64 {
        self.0.iter().fold(0u64, |sum, stripe| {
            sum.wrapping_add(stripe.0.load(Ordering::Relaxed))
        })
    }
}
