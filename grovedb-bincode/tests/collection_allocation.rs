//! Untrusted decoding must not allocate collection storage from length headers alone.
#![cfg(feature = "std")]

use bincode::{
    config::Config, error::DecodeError, BorrowDecodeUntrusted as BorrowDecode,
    DecodeUntrusted as Decode,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
};

#[derive(Clone, Copy, Default)]
struct AllocationState {
    active: bool,
    fail: bool,
    largest: usize,
}

thread_local! {
    static ALLOCATIONS: Cell<AllocationState> = const { Cell::new(AllocationState {
        active: false, fail: false, largest: 0,
    }) };
}

fn record_allocation(size: usize) -> bool {
    ALLOCATIONS
        .try_with(|cell| {
            let mut state = cell.get();
            if !state.active {
                return false;
            }
            state.largest = state.largest.max(size);
            cell.set(state);
            state.fail
        })
        .unwrap_or(false)
}

struct ObservedAllocator;

// The observer only records sizes in allocation-free thread-local cells. All
// pointers and layouts are forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for ObservedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if record_allocation(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if record_allocation(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if record_allocation(size) {
            std::ptr::null_mut()
        } else {
            unsafe { System.realloc(ptr, layout, size) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

fn observe<T>(fail: bool, f: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ALLOCATIONS.with(|cell| cell.set(AllocationState::default()));
        }
    }
    ALLOCATIONS.with(|cell| {
        assert!(!cell.get().active);
        cell.set(AllocationState {
            active: true,
            fail,
            largest: 0,
        });
    });
    let reset = Reset;
    let result = f();
    let largest = ALLOCATIONS.with(|cell| cell.get().largest);
    drop(reset);
    (result, largest)
}

fn bounded_error<T>(f: impl FnOnce() -> Result<T, DecodeError>) {
    let (result, largest) = observe(false, f);
    assert!(result.is_err(), "incomplete collection was accepted");
    assert!(largest <= 4096, "unbacked allocation of {largest} bytes");
}

fn reject_all_routes<T, C>(bytes: &[u8], config: C)
where
    T: Decode<()> + for<'de> BorrowDecode<'de, ()>,
    C: Config + Copy,
{
    bounded_error(|| bincode::decode_from_slice_untrusted::<T, _>(bytes, config));
    bounded_error(|| bincode::borrow_decode_from_slice_untrusted::<T, _>(bytes, config));
    bounded_error(|| {
        bincode::decode_from_slice_untrusted_with_context::<_, T, _>(bytes, config, ())
    });
    bounded_error(|| {
        bincode::borrow_decode_from_slice_untrusted_with_context::<_, T, _>(bytes, config, ())
    });
    bounded_error(|| {
        bincode::decode_from_reader_untrusted::<T, _, _>(
            bincode::de::read::SliceReader::new(bytes),
            config,
        )
    });
    // std::io::Read has no peek_read, so this also tests incremental reads.
    bounded_error(|| bincode::decode_from_std_read_untrusted::<T, _, _>(&mut &*bytes, config));
    bounded_error(|| {
        bincode::decode_from_std_read_untrusted_with_context::<_, T, _, _>(&mut &*bytes, config, ())
    });
}

#[test]
fn ordinary_decoding_retains_upstream_eager_allocation() {
    let config = bincode_upstream::config::standard();
    let bytes = bincode_upstream::encode_to_vec(65_536u64, config).unwrap();
    macro_rules! compare {
        ($ty:ty) => {{
            let (expected, largest) = observe(false, || {
                bincode_upstream::decode_from_slice::<$ty, _>(&bytes, config)
            });
            assert!(expected.is_err());
            assert!(
                largest >= 65_536,
                "control must demonstrate eager allocation"
            );
            let local = bincode::config::standard();
            let (result, allocated) = observe(false, || {
                bincode::decode_from_slice::<$ty, _>(&bytes, local)
            });
            assert_eq!(format!("{result:?}"), format!("{expected:?}"));
            assert_eq!(allocated, largest);
            let (result, allocated) = observe(false, || {
                bincode::borrow_decode_from_slice::<$ty, _>(&bytes, local)
            });
            assert_eq!(format!("{result:?}"), format!("{expected:?}"));
            assert_eq!(allocated, largest);
            let (result, allocated) = observe(false, || {
                bincode::decode_from_std_read::<$ty, _, _>(&mut bytes.as_slice(), local)
            });
            assert!(result.is_err());
            assert_eq!(allocated, largest);
            // Decoder capability must not silently change an ordinary trait call.
            let (result, allocated) = observe(false, || {
                let mut decoder = bincode::de::DecoderImpl::new_untrusted(
                    bincode::de::read::SliceReader::new(&bytes),
                    local,
                    (),
                );
                <$ty as bincode::Decode<()>>::decode(&mut decoder)
            });
            assert!(result.is_err());
            assert_eq!(allocated, largest);
            let (result, allocated) = observe(false, || {
                let mut decoder = bincode::de::DecoderImpl::new_untrusted(
                    bincode::de::read::SliceReader::new(&bytes),
                    local,
                    (),
                );
                <$ty as bincode::BorrowDecode<'_, ()>>::borrow_decode(&mut decoder)
            });
            assert!(result.is_err());
            assert_eq!(allocated, largest);
        }};
    }
    compare!(Vec<u8>);
    compare!(Vec<u16>);
    compare!(HashMap<u8, u8>);
    compare!(HashSet<u8>);
}

struct WithChangedContext<T>(T);

impl<C, T: Decode<u8>> Decode<C> for WithChangedContext<T> {
    fn decode_untrusted<D: bincode::de::UntrustedDecoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let mut nested = decoder.with_context(42u8);
        // Serde adapters and user decoders can introduce multiple mutable references.
        T::decode_untrusted(&mut &mut nested).map(Self)
    }
}

impl<'de, C, T: BorrowDecode<'de, u8>> BorrowDecode<'de, C> for WithChangedContext<T> {
    fn borrow_decode_untrusted<D: bincode::de::BorrowUntrustedDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let mut nested = decoder.with_context(42u8);
        T::borrow_decode_untrusted(&mut &mut nested).map(Self)
    }
}

#[test]
fn context_changes_preserve_the_untrusted_policy() {
    let config = bincode::config::standard();
    let bytes = bincode::encode_to_vec(u64::MAX, config).unwrap();
    reject_all_routes::<WithChangedContext<Vec<Vec<u8>>>, _>(&bytes, config);

    let value = vec![vec![1u8, 2, 3]];
    let bytes = bincode::encode_to_vec(&value, config).unwrap();
    let (decoded, consumed): (WithChangedContext<Vec<Vec<u8>>>, _) =
        bincode::decode_from_slice_untrusted_with_context(&bytes, config, "application context")
            .unwrap();
    assert_eq!(decoded.0, value);
    assert_eq!(consumed, bytes.len());
    let (decoded, consumed): (WithChangedContext<Vec<&[u8]>>, _) =
        bincode::borrow_decode_from_slice_untrusted_with_context(
            &bytes,
            config,
            "application context",
        )
        .unwrap();
    assert_eq!(decoded.0, value);
    assert_eq!(consumed, bytes.len());
}

#[test]
fn declared_lengths_do_not_allocate_before_contents() {
    check_declared_lengths(bincode::config::standard());
    check_declared_lengths(bincode::config::standard().with_big_endian());
    check_declared_lengths(bincode::config::legacy());
    check_declared_lengths(bincode::config::legacy().with_big_endian());
}

fn check_declared_lengths<C: Config + Copy>(config: C) {
    for len in [65_536u64, u64::MAX] {
        let bytes = bincode::encode_to_vec(len, config).unwrap();
        reject_all_routes::<Vec<u8>, _>(&bytes, config);
        reject_all_routes::<Vec<u16>, _>(&bytes, config);
        reject_all_routes::<Vec<Vec<u8>>, _>(&bytes, config);
        reject_all_routes::<HashMap<u8, u8>, _>(&bytes, config);
        reject_all_routes::<HashSet<u8>, _>(&bytes, config);
        reject_all_routes::<String, _>(&bytes, config);
        reject_all_routes::<Box<str>, _>(&bytes, config);
        reject_all_routes::<std::rc::Rc<str>, _>(&bytes, config);
        reject_all_routes::<std::sync::Arc<str>, _>(&bytes, config);
        reject_all_routes::<std::ffi::CString, _>(&bytes, config);
        reject_all_routes::<std::path::PathBuf, _>(&bytes, config);
        reject_all_routes::<Box<[u8]>, _>(&bytes, config);
        reject_all_routes::<VecDeque<u8>, _>(&bytes, config);
        reject_all_routes::<BinaryHeap<u8>, _>(&bytes, config);
    }
}

#[test]
fn partial_payloads_only_allocate_for_decoded_contents() {
    let config = bincode::config::standard();
    let mut bytes = bincode::encode_to_vec(u64::MAX, config).unwrap();
    bytes.extend([7; 2048]);
    reject_all_routes::<Vec<u8>, _>(&bytes, config);

    // One complete path segment followed by an unbacked segment length.
    let mut path = bincode::encode_to_vec(2u64, config).unwrap();
    path.extend(bincode::encode_to_vec(vec![1u8, 2, 3], config).unwrap());
    path.extend(bincode::encode_to_vec(u64::MAX, config).unwrap());
    reject_all_routes::<Vec<Vec<u8>>, _>(&path, config);

    // A complete map key is not sufficient to allocate storage for its entry.
    let mut map = bincode::encode_to_vec(65_536u64, config).unwrap();
    map.push(1);
    reject_all_routes::<HashMap<u8, u8>, _>(&map, config);
}

#[test]
fn allocation_failure_is_a_decode_error() {
    fn check<T: Decode<()> + for<'de> BorrowDecode<'de, ()>>(bytes: &[u8]) {
        let config = bincode::config::standard();
        let (owned, _) = observe(true, || {
            bincode::decode_from_slice_untrusted::<T, _>(bytes, config)
        });
        assert!(matches!(owned, Err(DecodeError::LimitExceeded)));
        let (borrowed, _) = observe(true, || {
            bincode::borrow_decode_from_slice_untrusted::<T, _>(bytes, config)
        });
        assert!(matches!(borrowed, Err(DecodeError::LimitExceeded)));
        let (streamed, _) = observe(true, || {
            bincode::decode_from_std_read_untrusted::<T, _, _>(&mut &*bytes, config)
        });
        assert!(matches!(streamed, Err(DecodeError::LimitExceeded)));
    }
    check::<Vec<u8>>(&[1, 42]);
    check::<Vec<u16>>(&[1, 42]);
    check::<HashMap<u8, u8>>(&[1, 42, 43]);
    check::<HashSet<u8>>(&[1, 42]);
}

#[test]
fn malformed_collection_corpus_stays_bounded() {
    use rand::{RngCore, SeedableRng};
    let mut random = rand::rngs::StdRng::seed_from_u64(0x902_903);
    let mut bytes = [0u8; 64];
    for len in 0..=bytes.len() {
        for _ in 0..32 {
            random.fill_bytes(&mut bytes);
            let bytes = &bytes[..len];
            // These element types all consume bytes, so random headers
            // cannot create valid zero-wire collections with huge counts.
            let (_, largest) = observe(false, || {
                let config = bincode::config::standard();
                let _ = bincode::decode_from_slice_untrusted::<Vec<Vec<u8>>, _>(bytes, config);
                let _ = bincode::borrow_decode_from_slice_untrusted::<HashMap<u8, Vec<u8>>, _>(
                    bytes, config,
                );
                let _ =
                    bincode::decode_from_std_read_untrusted::<Vec<u8>, _, _>(&mut &*bytes, config);
                let _ = bincode::decode_from_slice_untrusted::<Vec<u8>, _>(
                    bytes,
                    bincode::config::legacy(),
                );
                let _ = bincode::decode_from_slice_untrusted::<Vec<u16>, _>(
                    bytes,
                    bincode::config::standard().with_big_endian(),
                );
            });
            assert!(
                largest <= 4096,
                "random short input requested {largest} bytes"
            );
        }
    }
}

#[cfg(feature = "serde")]
mod serde_tests {
    use super::*;
    use serde::de::{MapAccess, SeqAccess, Visitor};
    use std::fmt;

    struct HintVisitor;
    impl<'de> Visitor<'de> for HintVisitor {
        type Value = ();

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a collection")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<(), A::Error> {
            assert_eq!(
                access.size_hint(),
                None,
                "unverified sequence length leaked"
            );
            while access.next_element::<u8>()?.is_some() {
                assert_eq!(access.size_hint(), None);
            }
            Ok(())
        }

        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<(), A::Error> {
            assert_eq!(access.size_hint(), None, "unverified map length leaked");
            while access.next_entry::<u8, u8>()?.is_some() {
                assert_eq!(access.size_hint(), None);
            }
            Ok(())
        }
    }

    struct Sequence;
    impl<'de> bincode::serde::DeserializeUntrusted<'de> for Sequence {}
    impl<'de> serde::Deserialize<'de> for Sequence {
        fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
            de.deserialize_seq(HintVisitor).map(|()| Self)
        }
    }

    struct Map;
    impl<'de> bincode::serde::DeserializeUntrusted<'de> for Map {}
    impl<'de> serde::Deserialize<'de> for Map {
        fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
            de.deserialize_map(HintVisitor).map(|()| Self)
        }
    }

    fn reject_serde_routes<T: for<'de> bincode::serde::DeserializeUntrusted<'de>>(bytes: &[u8]) {
        let config = bincode::config::standard();
        bounded_error(|| bincode::serde::decode_from_slice_untrusted::<T, _>(bytes, config));
        bounded_error(|| bincode::serde::borrow_decode_from_slice_untrusted::<T, _>(bytes, config));
        bounded_error(|| {
            bincode::serde::decode_from_std_read_untrusted::<T, _, _>(&mut &*bytes, config)
        });
        bounded_error(|| {
            bincode::serde::decode_from_reader_untrusted::<T, _, _>(
                bincode::de::read::SliceReader::new(bytes),
                config,
            )
        });
        bounded_error(|| {
            bincode::serde::seed_decode_from_slice_untrusted(
                std::marker::PhantomData::<T>,
                bytes,
                config,
            )
        });
        bounded_error(|| {
            let mut decoder = bincode::serde::BorrowedSerdeDecoder::from_slice_untrusted(
                bytes, config, "context",
            );
            decoder.decode::<T>()
        });
        bounded_error(|| {
            let mut decoder = bincode::serde::OwnedSerdeDecoder::from_reader_untrusted(
                bincode::de::read::SliceReader::new(bytes),
                config,
            );
            decoder.decode::<T>()
        });
        bounded_error(|| {
            let mut src = bytes;
            let mut decoder =
                bincode::serde::OwnedSerdeDecoder::from_std_read_untrusted(&mut src, config);
            decoder.decode::<T>()
        });
        bounded_error(|| {
            bincode::decode_from_slice_untrusted::<bincode::serde::Compat<T>, _>(bytes, config)
        });
        bounded_error(|| {
            bincode::borrow_decode_from_slice_untrusted::<bincode::serde::BorrowCompat<T>, _>(
                bytes, config,
            )
        });
        bounded_error(|| {
            bincode::decode_from_slice_untrusted::<WithChangedContext<bincode::serde::Compat<T>>, _>(
                bytes, config,
            )
        });
        bounded_error(|| {
            bincode::borrow_decode_from_slice_untrusted::<
                WithChangedContext<bincode::serde::BorrowCompat<T>>,
                _,
            >(bytes, config)
        });
    }

    #[cfg(feature = "derive")]
    #[test]
    fn derived_mixed_native_and_serde_fields_inherit_untrusted_mode() {
        #[derive(bincode::DecodeUntrusted)]
        struct Mixed {
            prefix: u8,
            #[bincode(with_serde)]
            values: Vec<Vec<u8>>,
        }
        let config = bincode::config::standard();
        let mut bytes = vec![42];
        bytes.extend(bincode::encode_to_vec(u64::MAX, config).unwrap());
        reject_all_routes::<Mixed, _>(&bytes, config);
        let (decoded, consumed): (Mixed, _) =
            bincode::decode_from_slice_untrusted(&[42, 1, 1, 7], config).unwrap();
        assert_eq!(decoded.prefix, 42);
        assert_eq!(decoded.values, [vec![7]]);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn serde_does_not_advertise_unverified_lengths() {
        let config = bincode::config::standard();
        for len in [65_536u64, u64::MAX] {
            let mut bytes = bincode::encode_to_vec(len, config).unwrap();
            reject_serde_routes::<Sequence>(&bytes);
            reject_serde_routes::<Map>(&bytes);
            reject_serde_routes::<Vec<u8>>(&bytes);
            reject_serde_routes::<HashMap<u8, u8>>(&bytes);
            bytes.extend([1, 2]);
            reject_serde_routes::<Sequence>(&bytes);
            reject_serde_routes::<Map>(&bytes);
        }
    }
}
