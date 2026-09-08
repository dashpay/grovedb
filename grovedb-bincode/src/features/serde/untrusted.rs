//! Explicit opt-in for an audited Serde deserializer and its nested graph.

use super::{de_borrowed, de_owned, BorrowCompat, Compat};
use crate::{
    de::{BorrowUntrustedDecoder, UntrustedDecoder},
    error::DecodeError,
    BorrowDecodeUntrusted, DecodeUntrusted,
};

/// Opt in a Serde implementation to untrusted decoding.
///
/// Implementors must audit their entire Serde decoding graph, including custom
/// visitors, allocations, and domain checks. Serde's own trait dispatch cannot
/// enforce this recursively. Library containers opt in only when their contents
/// do; there is no blanket implementation for arbitrary `Deserialize` types.
///
/// ```compile_fail
/// #[derive(serde::Deserialize)]
/// struct Ordinary(u8);
/// let _ = bincode::serde::decode_from_slice_untrusted::<Ordinary, _>(&[1], bincode::config::standard());
/// ```
/// The same opt-in is required for Serde fields in a native untrusted derive:
/// ```compile_fail
/// #[derive(serde::Deserialize)]
/// struct Ordinary(u8);
/// #[derive(bincode::DecodeUntrusted)]
/// struct External { #[bincode(with_serde)] value: Ordinary }
/// ```
pub trait DeserializeUntrusted<'de>: serde::Deserialize<'de> {}

/// Explicitly opt in a Serde seed, including the graph it chooses to deserialize.
///
/// ```compile_fail
/// struct Seed;
/// impl<'de> serde::de::DeserializeSeed<'de> for Seed {
///     type Value = u8;
///     fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<u8, D::Error> {
///         serde::Deserialize::deserialize(d)
///     }
/// }
/// let _ = bincode::serde::seed_decode_from_slice_untrusted(Seed, &[1], bincode::config::standard());
/// ```
pub trait DeserializeSeedUntrusted<'de>: serde::de::DeserializeSeed<'de> {}

impl<'de, T: DeserializeUntrusted<'de>> DeserializeSeedUntrusted<'de>
    for core::marker::PhantomData<T>
{
}

impl<C, T: for<'de> DeserializeUntrusted<'de>> DecodeUntrusted<C> for Compat<T> {
    fn decode_untrusted<D: UntrustedDecoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        T::deserialize(de_owned::SerdeDecoder { de: decoder }).map(Compat)
    }
}
impl<'de, C, T: for<'a> DeserializeUntrusted<'a>> BorrowDecodeUntrusted<'de, C> for Compat<T> {
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode_untrusted(decoder)
    }
}
impl<'de, C, T: DeserializeUntrusted<'de>> BorrowDecodeUntrusted<'de, C> for BorrowCompat<T> {
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        T::deserialize(de_borrowed::SerdeDecoder {
            de: decoder,
            pd: core::marker::PhantomData,
        })
        .map(BorrowCompat)
    }
}

macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(impl<'de> DeserializeUntrusted<'de> for $ty {})*};
}
leaf!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    char,
    core::time::Duration,
    core::num::NonZeroU8,
    core::num::NonZeroU16,
    core::num::NonZeroU32,
    core::num::NonZeroU64,
    core::num::NonZeroU128,
    core::num::NonZeroUsize,
    core::num::NonZeroI8,
    core::num::NonZeroI16,
    core::num::NonZeroI32,
    core::num::NonZeroI64,
    core::num::NonZeroI128,
    core::num::NonZeroIsize
);
impl<'de: 'a, 'a> DeserializeUntrusted<'de> for &'a str {}
impl<'de: 'a, 'a> DeserializeUntrusted<'de> for &'a [u8] {}
impl<'de, T: ?Sized> DeserializeUntrusted<'de> for core::marker::PhantomData<T> {}
impl<'de, T: DeserializeUntrusted<'de>> DeserializeUntrusted<'de> for Option<T> {}
impl<'de, T: DeserializeUntrusted<'de>, E: DeserializeUntrusted<'de>> DeserializeUntrusted<'de>
    for Result<T, E>
{
}
impl<'de, T: DeserializeUntrusted<'de>, const N: usize> DeserializeUntrusted<'de> for [T; N] where
    [T; N]: serde::Deserialize<'de>
{
}

macro_rules! tuple {
    ($($t:ident),+) => {impl<'de,$($t:DeserializeUntrusted<'de>),+> DeserializeUntrusted<'de> for ($($t,)+) {}};
}
tuple!(A);
tuple!(A, B);
tuple!(A, B, C);
tuple!(A, B, C, D);
tuple!(A, B, C, D, E);
tuple!(A, B, C, D, E, F);
tuple!(A, B, C, D, E, F, G);
tuple!(A, B, C, D, E, F, G, H);
tuple!(A, B, C, D, E, F, G, H, I);
tuple!(A, B, C, D, E, F, G, H, I, J);
tuple!(A, B, C, D, E, F, G, H, I, J, K);
tuple!(A, B, C, D, E, F, G, H, I, J, K, L);
tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M);
tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N);
tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O);
tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);

#[cfg(feature = "alloc")]
mod allocated {
    use super::*;
    use alloc::{
        boxed::Box,
        collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
        string::String,
        vec::Vec,
    };
    leaf!(String, Box<str>);
    impl<'de, T: DeserializeUntrusted<'de>> DeserializeUntrusted<'de> for Vec<T> {}
    impl<'de, T: DeserializeUntrusted<'de>> DeserializeUntrusted<'de> for Box<T> {}
    impl<'de, T: DeserializeUntrusted<'de>> DeserializeUntrusted<'de> for Box<[T]> {}
    impl<'de, T: DeserializeUntrusted<'de>> DeserializeUntrusted<'de> for VecDeque<T> {}
    impl<'de, T: DeserializeUntrusted<'de> + Ord> DeserializeUntrusted<'de> for BinaryHeap<T> {}
    impl<'de, T: DeserializeUntrusted<'de> + Ord> DeserializeUntrusted<'de> for BTreeSet<T> {}
    impl<'de, K: DeserializeUntrusted<'de> + Ord, V: DeserializeUntrusted<'de>>
        DeserializeUntrusted<'de> for BTreeMap<K, V>
    {
    }
}
#[cfg(feature = "std")]
mod standard {
    use super::*;
    use std::{
        collections::{HashMap, HashSet},
        hash::{BuildHasher, Hash},
        sync::{Mutex, RwLock},
    };
    leaf!(
        std::time::SystemTime,
        std::ffi::CString,
        std::path::PathBuf,
        std::net::IpAddr,
        std::net::Ipv4Addr,
        std::net::Ipv6Addr,
        std::net::SocketAddr,
        std::net::SocketAddrV4,
        std::net::SocketAddrV6
    );
    impl<'de, T: DeserializeUntrusted<'de> + Eq + Hash, S: BuildHasher + Default>
        DeserializeUntrusted<'de> for HashSet<T, S>
    {
    }
    impl<
            'de,
            K: DeserializeUntrusted<'de> + Eq + Hash,
            V: DeserializeUntrusted<'de>,
            S: BuildHasher + Default,
        > DeserializeUntrusted<'de> for HashMap<K, V, S>
    {
    }
    impl<'de, T: DeserializeUntrusted<'de>> DeserializeUntrusted<'de> for Mutex<T> {}
    impl<'de, T: DeserializeUntrusted<'de>> DeserializeUntrusted<'de> for RwLock<T> {}
}
