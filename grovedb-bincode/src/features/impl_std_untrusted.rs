//! Explicit std support for untrusted decoding.
use super::impl_std::{decode_hash_map, decode_hash_set};
use crate::{
    de::{
        BorrowDecode, BorrowDecodeUntrusted, BorrowUntrustedDecoder, Decode, DecodeUntrusted,
        UntrustedDecoder,
    },
    error::DecodeError,
};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    hash::Hash,
    net::*,
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
    time::SystemTime,
};
macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        impl<C> DecodeUntrusted<C> for $ty {
            fn decode_untrusted<D: UntrustedDecoder<Context=C>>(d:&mut D)->Result<Self,DecodeError> { <Self as Decode<C>>::decode(d) }
        }
        crate::impl_borrow_decode_untrusted!($ty);
    )*};
}
leaf!(
    SystemTime,
    CString,
    PathBuf,
    IpAddr,
    Ipv4Addr,
    Ipv6Addr,
    SocketAddr,
    SocketAddrV4,
    SocketAddrV6
);
impl<'de, C> BorrowDecodeUntrusted<'de, C> for &'de Path {
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = C>>(
        d: &mut D,
    ) -> Result<Self, DecodeError> {
        BorrowDecode::borrow_decode(d)
    }
}
impl<Context, T> DecodeUntrusted<Context> for Mutex<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::decode_untrusted(decoder)?;
        Ok(Mutex::new(t))
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Mutex<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::borrow_decode_untrusted(decoder)?;
        Ok(Mutex::new(t))
    }
}

impl<Context, T> DecodeUntrusted<Context> for RwLock<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::decode_untrusted(decoder)?;
        Ok(RwLock::new(t))
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for RwLock<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::borrow_decode_untrusted(decoder)?;
        Ok(RwLock::new(t))
    }
}

impl<Context, K, V, S> DecodeUntrusted<Context> for HashMap<K, V, S>
where
    K: DecodeUntrusted<Context> + Eq + std::hash::Hash,
    V: DecodeUntrusted<Context>,
    S: std::hash::BuildHasher + Default,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decode_hash_map(decoder, K::decode_untrusted, V::decode_untrusted)
    }
}

impl<'de, K, V, S, Context> BorrowDecodeUntrusted<'de, Context> for HashMap<K, V, S>
where
    K: BorrowDecodeUntrusted<'de, Context> + Eq + std::hash::Hash,
    V: BorrowDecodeUntrusted<'de, Context>,
    S: std::hash::BuildHasher + Default,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decode_hash_map(
            decoder,
            K::borrow_decode_untrusted,
            V::borrow_decode_untrusted,
        )
    }
}

impl<Context, T, S> DecodeUntrusted<Context> for HashSet<T, S>
where
    T: DecodeUntrusted<Context> + Eq + Hash,
    S: std::hash::BuildHasher + Default,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decode_hash_set(decoder, T::decode_untrusted)
    }
}

impl<'de, T, S, Context> BorrowDecodeUntrusted<'de, Context> for HashSet<T, S>
where
    T: BorrowDecodeUntrusted<'de, Context> + Eq + Hash,
    S: std::hash::BuildHasher + Default,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decode_hash_set(decoder, T::borrow_decode_untrusted)
    }
}
