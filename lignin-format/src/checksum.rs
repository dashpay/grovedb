//! The record checksum: CRC32C, stored masked.
//!
//! See "Checksums" in `docs/lignindb/format.md`.

use crate::error::FormatError;

/// The most bytes one checksum may cover.
///
/// CRC32C detects every error of up to three bits only while the covered
/// bytes and the checksum together stay below `2^31 - 1` bits (256 MiB).
/// Formats split anything larger into several records.
pub const MAX_CHECKSUMMED_LEN: usize = 1 << 27;

const MASK_DELTA: u32 = 0xA282_EAD8;

fn crc32c(parts: &[&[u8]]) -> u32 {
    parts
        .iter()
        .fold(0, |crc, part| ::crc32c::crc32c_append(crc, part))
}

/// A record checksum in the form it has on disk: a masked CRC32C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Checksum(u32);

impl Checksum {
    /// Bytes a checksum takes on disk.
    pub const ENCODED_LEN: usize = 4;

    /// The checksum of the concatenation of `parts`.
    ///
    /// `parts` are the covered bytes in file order: every byte of the record
    /// except the checksum field itself.
    pub fn of(parts: &[&[u8]]) -> Self {
        Self(mask(crc32c(parts)))
    }

    /// The stored 32-bit value.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The four bytes written to disk.
    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.0.to_le_bytes()
    }

    /// Reads a stored checksum. Every bit pattern is a checksum.
    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Self {
        Self(u32::from_le_bytes(bytes))
    }

    /// Checks that this stored checksum matches the covered bytes.
    pub fn verify(self, parts: &[&[u8]]) -> Result<(), FormatError> {
        let computed = Self::of(parts);
        if computed == self {
            Ok(())
        } else {
            Err(FormatError::ChecksumMismatch {
                stored: self.0,
                computed: computed.0,
            })
        }
    }
}

const fn mask(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(MASK_DELTA)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::SplitMix64;

    fn reference_crc32c(data: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0x82F6_3B78 & 0u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }

    const fn unmask(masked: u32) -> u32 {
        masked.wrapping_sub(MASK_DELTA).rotate_left(15)
    }

    #[test]
    fn crc32c_matches_published_vectors() {
        let ascending: Vec<u8> = (0..32).collect();
        let descending: Vec<u8> = (0..32).rev().collect();
        let vectors: [(&[u8], u32, u32); 6] = [
            (b"", 0x0000_0000, 0xA282_EAD8),
            (b"123456789", 0xE306_9283, 0xC78A_B0E5),
            (&[0x00; 32], 0x8A91_36AA, 0x0FD7_FFFA),
            (&[0xFF; 32], 0x62A8_AB43, 0xF909_B029),
            (&ascending, 0x46DD_794E, 0x951F_7892),
            (&descending, 0x113F_DB5C, 0x593B_0D57),
        ];
        for (data, crc, masked) in vectors {
            assert_eq!(crc32c(&[data]), crc);
            assert_eq!(reference_crc32c(data), crc);
            assert_eq!(Checksum::of(&[data]).get(), masked);
        }
    }

    #[test]
    fn crc32c_matches_the_bitwise_reference_on_random_input() {
        let mut rng = SplitMix64::new(0x00C0_FFEE);
        for len in (0..300).chain([4095, 4096, 4097, 8192, 65_537]) {
            let data = rng.bytes(len);
            assert_eq!(crc32c(&[&data]), reference_crc32c(&data), "len {len}");
        }
    }

    #[test]
    fn splitting_the_covered_bytes_does_not_change_the_checksum() {
        let mut rng = SplitMix64::new(7);
        let data = rng.bytes(1000);
        let whole = Checksum::of(&[&data]);
        for split in [0, 1, 7, 8, 9, 500, 999, 1000] {
            let (head, tail) = data.split_at(split);
            assert_eq!(Checksum::of(&[head, tail]), whole);
            assert_eq!(Checksum::of(&[head, &[], tail]), whole);
        }
        assert_eq!(Checksum::of(&[]), Checksum::of(&[&[]]));
    }

    #[test]
    fn checksum_round_trips() {
        let mut rng = SplitMix64::new(11);
        for raw in [0, 1, u32::MAX, MASK_DELTA]
            .into_iter()
            .chain((0..1000).map(|_| rng.next_u64() as u32))
        {
            let bytes = raw.to_le_bytes();
            let checksum = Checksum::decode(bytes);
            assert_eq!(checksum.get(), raw);
            assert_eq!(checksum.encode(), bytes);
            assert_eq!(mask(unmask(raw)), raw);
            assert_eq!(unmask(mask(raw)), raw);
        }
    }

    #[test]
    fn verify_accepts_the_covered_bytes_and_rejects_any_single_bit_flip() {
        let mut rng = SplitMix64::new(13);
        let data = rng.bytes(64);
        let checksum = Checksum::of(&[&data]);
        assert_eq!(checksum.verify(&[&data]), Ok(()));
        for bit in 0..data.len() * 8 {
            let mut damaged = data.clone();
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert_eq!(
                checksum.verify(&[&damaged]),
                Err(FormatError::ChecksumMismatch {
                    stored: checksum.get(),
                    computed: Checksum::of(&[&damaged]).get(),
                })
            );
        }
    }

    #[test]
    fn zero_filled_bytes_never_carry_a_zero_checksum() {
        let mut table = [0u32; 256];
        for (byte, entry) in (0u32..).zip(table.iter_mut()) {
            *entry = (0..8).fold(byte, |crc, _| {
                (crc >> 1) ^ (0x82F6_3B78 & 0u32.wrapping_sub(crc & 1))
            });
        }
        let zeros = vec![0u8; MAX_CHECKSUMMED_LEN];
        let crc_that_masks_to_zero = unmask(0);
        let mut register = u32::MAX;
        for len in 0..=MAX_CHECKSUMMED_LEN {
            let crc = !register;
            assert_ne!(crc, crc_that_masks_to_zero, "{len} zero bytes");
            if len.is_power_of_two() || len == 0 || len == 65_535 {
                assert_eq!(crc, crc32c(&[&zeros[..len]]), "{len} zero bytes");
                assert_ne!(Checksum::of(&[&zeros[..len]]).get(), 0);
            }
            register = table[(register & 0xFF) as usize] ^ (register >> 8);
        }
    }

    #[test]
    fn the_length_limit_stays_inside_the_range_where_three_bit_errors_are_caught() {
        const POLYNOMIAL: u64 = 0x1_1EDC_6F41;
        const PERIOD: u64 = (1 << 31) - 1;

        fn multiply(mut a: u64, mut b: u64) -> u64 {
            let mut product = 0;
            while b != 0 {
                if b & 1 == 1 {
                    product ^= a;
                }
                b >>= 1;
                a <<= 1;
                if a >> 32 == 1 {
                    a ^= POLYNOMIAL;
                }
            }
            product
        }
        fn x_to_the(mut exponent: u64) -> u64 {
            let (mut power, mut square) = (1, 2);
            while exponent != 0 {
                if exponent & 1 == 1 {
                    power = multiply(power, square);
                }
                square = multiply(square, square);
                exponent >>= 1;
            }
            power
        }

        assert_eq!(POLYNOMIAL.count_ones() % 2, 0);
        assert_eq!(x_to_the(PERIOD), 1);
        assert_ne!(x_to_the(1), 1);
        assert!((2..46_341).all(|divisor| !PERIOD.is_multiple_of(divisor)));

        let codeword_bits = (MAX_CHECKSUMMED_LEN as u64 + Checksum::ENCODED_LEN as u64) * 8;
        assert!(codeword_bits <= PERIOD);
    }

    #[test]
    fn masking_keeps_an_outer_checksum_sensitive_to_inner_records() {
        fn nested(payloads: [&[u8]; 2], inner: fn(&[u8]) -> u32) -> u32 {
            let mut bytes = Vec::new();
            for payload in payloads {
                bytes.extend_from_slice(payload);
                bytes.extend_from_slice(&inner(payload).to_le_bytes());
            }
            crc32c(&[&bytes])
        }
        let unmasked = |payload: &[u8]| crc32c(&[payload]);
        let masked = |payload: &[u8]| Checksum::of(&[payload]).get();

        assert_eq!(
            nested([b"aaaa", b"bbbb"], unmasked),
            nested([b"cccc", b"dddd"], unmasked)
        );
        assert_ne!(
            nested([b"aaaa", b"bbbb"], masked),
            nested([b"cccc", b"dddd"], masked)
        );
    }
}
