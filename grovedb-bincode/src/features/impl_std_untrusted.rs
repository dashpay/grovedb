//! Explicit std support for untrusted decoding.
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
    string::String,
    sync::{Mutex, RwLock},
    time::SystemTime,
    vec::Vec,
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
    IpAddr,
    Ipv4Addr,
    Ipv6Addr,
    SocketAddr,
    SocketAddrV4,
    SocketAddrV6
);
impl<C> DecodeUntrusted<C> for CString {
    fn decode_untrusted<D: UntrustedDecoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let bytes = Vec::<u8>::decode_untrusted(decoder)?;
        CString::new(bytes).map_err(|inner| DecodeError::CStringNulError {
            position: inner.nul_position(),
        })
    }
}
crate::impl_borrow_decode_untrusted!(CString);
impl<C> DecodeUntrusted<C> for PathBuf {
    fn decode_untrusted<D: UntrustedDecoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        String::decode_untrusted(decoder).map(Into::into)
    }
}
crate::impl_borrow_decode_untrusted!(PathBuf);

fn decode_hash_map<K: Eq + Hash, V, S: std::hash::BuildHasher + Default, D: UntrustedDecoder>(
    decoder: &mut D,
    mut key: impl FnMut(&mut D) -> Result<K, DecodeError>,
    mut value: impl FnMut(&mut D) -> Result<V, DecodeError>,
) -> Result<HashMap<K, V, S>, DecodeError> {
    let len = crate::de::decode_slice_len(decoder)?;
    decoder.claim_container_read::<(K, V)>(len)?;

    let hash_builder: S = Default::default();
    let mut map = HashMap::with_hasher(hash_builder);
    for _ in 0..len {
        // See the documentation on `unclaim_bytes_read` as to why we're doing this here
        decoder.unclaim_bytes_read(core::mem::size_of::<(K, V)>());

        let k = key(decoder)?;
        let v = value(decoder)?;
        map.try_reserve(1).map_err(|_| DecodeError::LimitExceeded)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn decode_hash_set<T: Eq + Hash, S: std::hash::BuildHasher + Default, D: UntrustedDecoder>(
    decoder: &mut D,
    mut decode: impl FnMut(&mut D) -> Result<T, DecodeError>,
) -> Result<HashSet<T, S>, DecodeError> {
    let len = crate::de::decode_slice_len(decoder)?;
    decoder.claim_container_read::<T>(len)?;

    let hash_builder: S = Default::default();
    let mut map: HashSet<T, S> = HashSet::with_hasher(hash_builder);
    for _ in 0..len {
        // See the documentation on `unclaim_bytes_read` as to why we're doing this here
        decoder.unclaim_bytes_read(core::mem::size_of::<T>());

        let key = decode(decoder)?;
        map.try_reserve(1).map_err(|_| DecodeError::LimitExceeded)?;
        map.insert(key);
    }
    Ok(map)
}

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
