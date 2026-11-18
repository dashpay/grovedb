//! The pinned random number generator behind every workload.
//!
//! A workload stream has to be the same on every machine and after every
//! dependency bump, so nothing here is borrowed from a crate whose output
//! could change: the generator is xoshiro256** seeded through SplitMix64,
//! both written out below and pinned by test vectors. Every draw is a `u64`;
//! no value depends on `usize`, on a float, or on the iteration order of a
//! hash map.

/// One SplitMix64 step.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// xoshiro256** with SplitMix64 seeding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    state: [u64; 4],
}

impl Rng {
    /// A generator for `seed`.
    pub fn new(seed: u64) -> Self {
        let mut sm = seed;
        Self::from_state([
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
        ])
    }

    /// A generator for one numbered item of one named substream of `seed`.
    ///
    /// The same `(seed, domain, index)` always gives the same generator, so
    /// a workload can recompute the bytes of its `index`-th document, key or
    /// identity without keeping them.
    pub fn derive(seed: u64, domain: u64, index: u64) -> Self {
        let mut sm = seed;
        let mut sm = splitmix64(&mut sm) ^ domain.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut sm = splitmix64(&mut sm) ^ index.wrapping_mul(0xD1B5_4A32_D192_ED03);
        Self::from_state([
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
        ])
    }

    /// A generator starting from a raw xoshiro256** state.
    fn from_state(state: [u64; 4]) -> Self {
        debug_assert!(state != [0; 4], "xoshiro256** must not start at zero");
        Self { state }
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.state;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// A uniform value in `0..bound`. `bound` must not be zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "below(0) has no values to draw from");
        let mut product = u128::from(self.next_u64()) * u128::from(bound);
        if (product as u64) < bound {
            let threshold = bound.wrapping_neg() % bound;
            while (product as u64) < threshold {
                product = u128::from(self.next_u64()) * u128::from(bound);
            }
        }
        (product >> 64) as u64
    }

    /// A uniform value in `low..=high`.
    pub fn between(&mut self, low: u64, high: u64) -> u64 {
        assert!(low <= high, "between({low}, {high}) is an empty range");
        match (high - low).checked_add(1) {
            Some(span) => low + self.below(span),
            None => self.next_u64(),
        }
    }

    /// True `numerator` times out of `denominator`.
    pub fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.below(denominator) < numerator
    }

    /// Fills `buffer` from the little-endian bytes of successive draws.
    pub fn fill(&mut self, buffer: &mut [u8]) {
        for chunk in buffer.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    /// `len` random bytes.
    pub fn bytes(&mut self, len: u32) -> Vec<u8> {
        let mut buffer = vec![0u8; len as usize];
        self.fill(&mut buffer);
        buffer
    }

    /// Thirty-two random bytes.
    pub fn id(&mut self) -> [u8; 32] {
        let mut id = [0u8; 32];
        self.fill(&mut id);
        id
    }

    /// A uniform index into a collection of `len` items.
    pub fn index(&mut self, len: usize) -> usize {
        self.below(len as u64) as usize
    }

    /// Shuffles `items` in place (Fisher-Yates, from the back).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for last in (1..items.len()).rev() {
            let other = self.index(last + 1);
            items.swap(last, other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix64_matches_the_reference_vector() {
        let mut state = 1234567u64;
        let got: Vec<u64> = (0..5).map(|_| splitmix64(&mut state)).collect();
        assert_eq!(
            got,
            [
                6457827717110365317,
                3203168211198807973,
                9817491932198370423,
                4593380528125082431,
                16408922859458223821,
            ]
        );
    }

    #[test]
    fn xoshiro256_star_star_matches_the_reference_vector() {
        let mut rng = Rng::from_state([1, 2, 3, 4]);
        let got: Vec<u64> = (0..10).map(|_| rng.next_u64()).collect();
        assert_eq!(
            got,
            [
                11520,
                0,
                1509978240,
                1215971899390074240,
                1216172134540287360,
                607988272756665600,
                16172922978634559625,
                8476171486693032832,
                10595114339597558777,
                2904607092377533576,
            ]
        );
    }

    #[test]
    fn seeded_and_derived_streams_are_pinned() {
        let mut rng = Rng::new(0);
        assert_eq!(
            [rng.next_u64(), rng.next_u64(), rng.next_u64()],
            PINNED_SEED_ZERO
        );
        let mut rng = Rng::derive(42, 7, 1_000_000);
        assert_eq!(
            [rng.next_u64(), rng.next_u64(), rng.next_u64()],
            PINNED_DERIVED
        );
        assert_eq!(hex::encode(Rng::new(0xDEAD_BEEF).bytes(13)), PINNED_BYTES);
    }

    const PINNED_SEED_ZERO: [u64; 3] = [
        11091344671253066420,
        13793997310169335082,
        1900383378846508768,
    ];
    const PINNED_DERIVED: [u64; 3] = [
        15208760477041760810,
        15027016039831008647,
        14898077298385544713,
    ];
    const PINNED_BYTES: &str = "837e4da7445455c5386eb1b437";

    #[test]
    fn below_stays_in_range_and_reaches_every_value() {
        let mut rng = Rng::new(99);
        let mut seen = [false; 7];
        for _ in 0..2_000 {
            seen[rng.below(7) as usize] = true;
        }
        assert_eq!(seen, [true; 7]);
        for _ in 0..2_000 {
            let value = rng.between(250, 255);
            assert!((250..=255).contains(&value));
        }
        assert_eq!(rng.below(1), 0);
        rng.between(0, u64::MAX);
    }

    #[test]
    fn derived_streams_differ_by_domain_and_index() {
        let base = Rng::derive(1, 2, 3);
        assert_ne!(base, Rng::derive(1, 2, 4));
        assert_ne!(base, Rng::derive(1, 3, 3));
        assert_ne!(base, Rng::derive(2, 2, 3));
        assert_eq!(base, Rng::derive(1, 2, 3));
    }
}
