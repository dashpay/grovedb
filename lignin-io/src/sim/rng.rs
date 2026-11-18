//! The simulator's random numbers.
//!
//! The generator is SplitMix64, written out here. A seed has to give the same
//! numbers on every machine and after every dependency update, so nothing
//! outside this file decides them: no crate, no floating point, and no value
//! whose width depends on the platform.

use std::fmt;

const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// The streams the simulator derives its generators from, by what they
/// decide: whether an operation fails, what the failure leaves, what becomes
/// of changes a failed sync left unknown, which changes a crash keeps, and
/// the seed of the file system after the crash.
pub(super) mod stream {
    pub(in crate::sim) const FAULT: u64 = 1;
    pub(in crate::sim) const DETAIL: u64 = 2;
    pub(in crate::sim) const SETTLE: u64 = 3;
    pub(in crate::sim) const CRASH: u64 = 4;
    pub(in crate::sim) const DRAW: u64 = 5;
    pub(in crate::sim) const CHILD: u64 = 6;
}

/// SplitMix64 (Steele, Lea and Flood, 2014).
///
/// The `n`-th number of a generator is a function of its seed and `n` alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimRng {
    state: u64,
}

impl SimRng {
    /// The generator that starts at `seed`.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The generator for item `index` of stream `stream` under `seed`.
    ///
    /// The simulator draws every decision from a generator made this way, so
    /// that the decision for one operation does not depend on how many
    /// numbers another one used.
    pub fn derive(seed: u64, stream: u64, index: u64) -> Self {
        let keyed = Self::new(seed).next_u64() ^ stream;
        Self::new(Self::new(keyed).next_u64() ^ index)
    }

    /// The next number.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GAMMA);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below `bound`, each one equally likely.
    ///
    /// # Panics
    ///
    /// If `bound` is 0.
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "there is no number below 0");
        let mut product = u128::from(self.next_u64()) * u128::from(bound);
        if (product as u64) < bound {
            let threshold = bound.wrapping_neg() % bound;
            while (product as u64) < threshold {
                product = u128::from(self.next_u64()) * u128::from(bound);
            }
        }
        (product >> 64) as u64
    }

    /// Whether something that happens with `chance` happens this time.
    pub fn chance(&mut self, chance: Chance) -> bool {
        if chance.numerator == 0 {
            false
        } else if chance.numerator == chance.denominator {
            true
        } else {
            self.below(chance.denominator.into()) < u64::from(chance.numerator)
        }
    }
}

/// How often something happens: `numerator` times in `denominator`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Chance {
    numerator: u32,
    denominator: u32,
}

impl Chance {
    /// It never happens.
    pub const NEVER: Self = Self::new(0, 1);
    /// It always happens.
    pub const ALWAYS: Self = Self::new(1, 1);

    /// `numerator` times in `denominator`.
    ///
    /// # Panics
    ///
    /// If `denominator` is 0 or smaller than `numerator`.
    pub const fn new(numerator: u32, denominator: u32) -> Self {
        assert!(
            denominator > 0 && numerator <= denominator,
            "a chance is between 0 and 1"
        );
        Self {
            numerator,
            denominator,
        }
    }

    /// Once in `n` times.
    ///
    /// # Panics
    ///
    /// If `n` is 0.
    pub const fn one_in(n: u32) -> Self {
        Self::new(1, n)
    }
}

impl fmt::Debug for Chance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}
