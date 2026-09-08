//! Ordinary entry points retain observable upstream 2.0.1 decoder behavior.
#![cfg(feature = "std")]

use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct TracedReader<'a> {
    remaining: &'a [u8],
    reads: Vec<usize>,
}

// Implement the same reader for each codec so error details, read sizes, and
// consumption can be compared without relying on either codec's reader.
macro_rules! impl_reader {
    ($codec:ident) => {
        impl $codec::de::read::Reader for TracedReader<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> Result<(), $codec::error::DecodeError> {
                self.reads.push(bytes.len());
                if bytes.len() > self.remaining.len() {
                    return Err($codec::error::DecodeError::UnexpectedEnd {
                        additional: bytes.len() - self.remaining.len(),
                    });
                }
                let (read, remaining) = self.remaining.split_at(bytes.len());
                bytes.copy_from_slice(read);
                self.remaining = remaining;
                Ok(())
            }
        }
    };
}
impl_reader!(bincode);
impl_reader!(bincode_upstream);

#[test]
fn ordinary_byte_vectors_keep_upstream_reads_errors_and_consumption() {
    for payload_len in [0, 1, 1024, 2048, 4097] {
        let mut bytes = bincode::encode_to_vec(4097u64, bincode::config::standard()).unwrap();
        bytes.extend(vec![7; payload_len]);
        let mut local = TracedReader {
            remaining: &bytes,
            ..Default::default()
        };
        let mut upstream = TracedReader {
            remaining: &bytes,
            ..Default::default()
        };
        let actual =
            bincode::decode_from_reader::<Vec<u8>, _, _>(&mut local, bincode::config::standard());
        let expected = bincode_upstream::decode_from_reader::<Vec<u8>, _, _>(
            &mut upstream,
            bincode_upstream::config::standard(),
        );
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        assert_eq!(local.reads, upstream.reads);
        assert_eq!(local.reads.last(), Some(&4097));
        assert_eq!(local.remaining, upstream.remaining);

        let actual = bincode::decode_from_slice::<Vec<u8>, _>(&bytes, bincode::config::standard());
        let expected = bincode_upstream::decode_from_slice::<Vec<u8>, _>(
            &bytes,
            bincode_upstream::config::standard(),
        );
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        let mut local = bytes.as_slice();
        let mut upstream = bytes.as_slice();
        let actual =
            bincode::decode_from_std_read::<Vec<u8>, _, _>(&mut local, bincode::config::standard());
        let expected = bincode_upstream::decode_from_std_read::<Vec<u8>, _, _>(
            &mut upstream,
            bincode_upstream::config::standard(),
        );
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        assert_eq!(local, upstream);
    }
}

#[test]
fn ordinary_collection_capacity_matches_upstream() {
    let config = bincode::config::standard();
    let original = bincode_upstream::config::standard();
    for len in [0, 1, 5, 251, 1025] {
        let bytes = bincode::encode_to_vec(vec![42u16; len], config).unwrap();
        let (actual, _): (Vec<u16>, _) = bincode::decode_from_slice(&bytes, config).unwrap();
        let (expected, _): (Vec<u16>, _) =
            bincode_upstream::decode_from_slice(&bytes, original).unwrap();
        assert_eq!(actual.capacity(), expected.capacity());
        let (actual, _): (HashSet<u16>, _) = bincode::decode_from_slice(&bytes, config).unwrap();
        let (expected, _): (HashSet<u16>, _) =
            bincode_upstream::decode_from_slice(&bytes, original).unwrap();
        assert_eq!(actual.capacity(), expected.capacity());
        let bytes = bincode::encode_to_vec(vec![(42u16, 43u16); len], config).unwrap();
        let (actual, _): (HashMap<u16, u16>, _) =
            bincode::decode_from_slice(&bytes, config).unwrap();
        let (expected, _): (HashMap<u16, u16>, _) =
            bincode_upstream::decode_from_slice(&bytes, original).unwrap();
        assert_eq!(actual.capacity(), expected.capacity());
    }
}

#[cfg(feature = "serde")]
mod serde_hints {
    use serde::de::{Deserialize, MapAccess, SeqAccess, Visitor};
    use std::fmt;

    #[derive(Debug, PartialEq)]
    struct Hints<const MAP: bool>(Vec<Option<usize>>);

    impl<'de, const MAP: bool> Deserialize<'de> for Hints<MAP> {
        fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
            struct HintVisitor;
            impl<'de> Visitor<'de> for HintVisitor {
                type Value = Vec<Option<usize>>;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("a collection")
                }
                fn visit_seq<A: SeqAccess<'de>>(
                    self,
                    mut access: A,
                ) -> Result<Self::Value, A::Error> {
                    let mut hints = vec![access.size_hint()];
                    while access.next_element::<u8>()?.is_some() {
                        hints.push(access.size_hint());
                    }
                    Ok(hints)
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut access: A,
                ) -> Result<Self::Value, A::Error> {
                    let mut hints = vec![access.size_hint()];
                    while access.next_entry::<u8, u8>()?.is_some() {
                        hints.push(access.size_hint());
                    }
                    Ok(hints)
                }
            }
            if MAP {
                de.deserialize_map(HintVisitor).map(Self)
            } else {
                de.deserialize_seq(HintVisitor).map(Self)
            }
        }
    }

    fn compare<const MAP: bool>(bytes: &[u8]) {
        let config = bincode::config::standard();
        let (upstream, consumed): (Hints<MAP>, _) =
            bincode_upstream::serde::decode_from_slice(bytes, bincode_upstream::config::standard())
                .unwrap();
        assert_eq!(upstream.0, [Some(2), Some(1), Some(0)]);
        assert_eq!(consumed, bytes.len());
        let (ordinary, _): (Hints<MAP>, _) =
            bincode::serde::decode_from_slice(bytes, config).unwrap();
        assert_eq!(ordinary, upstream);
        let (ordinary, consumed) = bincode::serde::seed_decode_from_slice(
            std::marker::PhantomData::<Hints<MAP>>,
            bytes,
            config,
        )
        .unwrap();
        assert_eq!(ordinary, upstream);
        assert_eq!(consumed, bytes.len());
        let mut decoder: bincode::serde::BorrowedSerdeDecoder<
            '_,
            bincode::de::DecoderImpl<_, _, ()>,
        > = bincode::serde::BorrowedSerdeDecoder::from_slice(bytes, config, ());
        assert_eq!(
            Hints::<MAP>::deserialize(decoder.as_deserializer()).unwrap(),
            upstream
        );
        let mut decoder: bincode::serde::OwnedSerdeDecoder<bincode::de::DecoderImpl<_, _, ()>> =
            bincode::serde::OwnedSerdeDecoder::from_reader(
                bincode::de::read::SliceReader::new(bytes),
                config,
            );
        assert_eq!(
            Hints::<MAP>::deserialize(decoder.as_deserializer()).unwrap(),
            upstream
        );
        let ordinary: Hints<MAP> =
            bincode::serde::decode_from_std_read(&mut &*bytes, config).unwrap();
        assert_eq!(ordinary, upstream);
        let (untrusted, _): (Hints<MAP>, _) =
            bincode::serde::decode_from_slice_untrusted(bytes, config).unwrap();
        assert_eq!(untrusted.0, [None, None, None]);
        let (untrusted, consumed) = bincode::serde::seed_decode_from_slice_untrusted(
            std::marker::PhantomData::<Hints<MAP>>,
            bytes,
            config,
        )
        .unwrap();
        assert_eq!(untrusted.0, [None, None, None]);
        assert_eq!(consumed, bytes.len());
        let untrusted: Hints<MAP> = bincode::serde::decode_from_reader_untrusted(
            bincode::de::read::SliceReader::new(bytes),
            config,
        )
        .unwrap();
        assert_eq!(untrusted.0, [None, None, None]);
        let untrusted: Hints<MAP> =
            bincode::serde::decode_from_std_read_untrusted(&mut &*bytes, config).unwrap();
        assert_eq!(untrusted.0, [None, None, None]);
    }

    #[test]
    fn only_untrusted_serde_decoding_withholds_size_hints() {
        compare::<false>(&[2, 42, 43]);
        compare::<true>(&[2, 42, 43, 44, 45]);
    }
}
