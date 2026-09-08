//! Untrusted collection implementations with recursive trait bounds.
use crate::{
    de::read::Reader,
    de::{BorrowDecodeUntrusted, BorrowUntrustedDecoder, DecodeUntrusted, UntrustedDecoder},
    error::DecodeError,
};
#[cfg(target_has_atomic = "ptr")]
use alloc::sync::Arc;
use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::*,
    rc::Rc,
    string::String,
    vec::Vec,
};
// Allocating string wrappers must use the guarded byte-vector implementation.
impl<C> DecodeUntrusted<C> for String {
    fn decode_untrusted<D: UntrustedDecoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let bytes = Vec::<u8>::decode_untrusted(decoder)?;
        String::from_utf8(bytes).map_err(|e| DecodeError::Utf8 {
            inner: e.utf8_error(),
        })
    }
}
crate::impl_borrow_decode_untrusted!(String);

macro_rules! string_wrapper {
    ($ty:ty) => {
        impl<C> DecodeUntrusted<C> for $ty {
            fn decode_untrusted<D: UntrustedDecoder<Context = C>>(
                decoder: &mut D,
            ) -> Result<Self, DecodeError> {
                String::decode_untrusted(decoder).map(Into::into)
            }
        }
        crate::impl_borrow_decode_untrusted!($ty);
    };
}
string_wrapper!(Box<str>);
string_wrapper!(Rc<str>);
#[cfg(target_has_atomic = "ptr")]
string_wrapper!(Arc<str>);

/// Allocate byte storage only after the reader has demonstrated its contents.
/// Slice readers retain a single-allocation fast path; other readers use a
/// bounded stack buffer so a length header alone cannot request heap storage.
fn decode_byte_vec<D: UntrustedDecoder>(
    decoder: &mut D,
    len: usize,
) -> Result<Vec<u8>, DecodeError> {
    let mut vec = Vec::new();
    if decoder.reader().peek_read(len).is_some() {
        vec.try_reserve_exact(len)
            .map_err(|_| DecodeError::LimitExceeded)?;
        vec.resize(len, 0);
        decoder.reader().read(&mut vec)?;
    } else {
        let mut remaining = len;
        let mut chunk = [0u8; 1024];
        while remaining != 0 {
            let count = remaining.min(chunk.len());
            decoder.reader().read(&mut chunk[..count])?;
            vec.try_reserve(count)
                .map_err(|_| DecodeError::LimitExceeded)?;
            vec.extend_from_slice(&chunk[..count]);
            remaining -= count;
        }
    }
    Ok(vec)
}

fn decode_vec<T, D: UntrustedDecoder>(
    decoder: &mut D,
    mut decode: impl FnMut(&mut D) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let len = crate::de::decode_slice_len(decoder)?;

    if unty::type_equal::<T, u8>() {
        decoder.claim_container_read::<T>(len)?;
        let vec = decode_byte_vec(decoder, len)?;
        // Safety: Vec<T> is Vec<u8>
        Ok(unsafe { core::mem::transmute::<Vec<u8>, Vec<T>>(vec) })
    } else {
        decoder.claim_container_read::<T>(len)?;

        let mut vec = Vec::new();
        for _ in 0..len {
            // See the documentation on `unclaim_bytes_read` as to why we're doing this here
            decoder.unclaim_bytes_read(core::mem::size_of::<T>());

            let value = decode(decoder)?;
            vec.try_reserve(1).map_err(|_| DecodeError::LimitExceeded)?;
            vec.push(value);
        }
        Ok(vec)
    }
}

impl<Context, T> DecodeUntrusted<Context> for BinaryHeap<T>
where
    T: DecodeUntrusted<Context> + Ord,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Vec::<T>::decode_untrusted(decoder)?.into())
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for BinaryHeap<T>
where
    T: BorrowDecodeUntrusted<'de, Context> + Ord,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Vec::<T>::borrow_decode_untrusted(decoder)?.into())
    }
}

impl<Context, K, V> DecodeUntrusted<Context> for BTreeMap<K, V>
where
    K: DecodeUntrusted<Context> + Ord,
    V: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let len = crate::de::decode_slice_len(decoder)?;
        decoder.claim_container_read::<(K, V)>(len)?;

        let mut map = BTreeMap::new();
        for _ in 0..len {
            // See the documentation on `unclaim_bytes_read` as to why we're doing this here
            decoder.unclaim_bytes_read(core::mem::size_of::<(K, V)>());

            let key = K::decode_untrusted(decoder)?;
            let value = V::decode_untrusted(decoder)?;
            map.insert(key, value);
        }
        Ok(map)
    }
}

impl<'de, K, V, Context> BorrowDecodeUntrusted<'de, Context> for BTreeMap<K, V>
where
    K: BorrowDecodeUntrusted<'de, Context> + Ord,
    V: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let len = crate::de::decode_slice_len(decoder)?;
        decoder.claim_container_read::<(K, V)>(len)?;

        let mut map = BTreeMap::new();
        for _ in 0..len {
            // See the documentation on `unclaim_bytes_read` as to why we're doing this here
            decoder.unclaim_bytes_read(core::mem::size_of::<(K, V)>());

            let key = K::borrow_decode_untrusted(decoder)?;
            let value = V::borrow_decode_untrusted(decoder)?;
            map.insert(key, value);
        }
        Ok(map)
    }
}

impl<Context, T> DecodeUntrusted<Context> for BTreeSet<T>
where
    T: DecodeUntrusted<Context> + Ord,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let len = crate::de::decode_slice_len(decoder)?;
        decoder.claim_container_read::<T>(len)?;

        let mut map = BTreeSet::new();
        for _ in 0..len {
            // See the documentation on `unclaim_bytes_read` as to why we're doing this here
            decoder.unclaim_bytes_read(core::mem::size_of::<T>());

            let key = T::decode_untrusted(decoder)?;
            map.insert(key);
        }
        Ok(map)
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for BTreeSet<T>
where
    T: BorrowDecodeUntrusted<'de, Context> + Ord,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let len = crate::de::decode_slice_len(decoder)?;
        decoder.claim_container_read::<T>(len)?;

        let mut map = BTreeSet::new();
        for _ in 0..len {
            // See the documentation on `unclaim_bytes_read` as to why we're doing this here
            decoder.unclaim_bytes_read(core::mem::size_of::<T>());

            let key = T::borrow_decode_untrusted(decoder)?;
            map.insert(key);
        }
        Ok(map)
    }
}

impl<Context, T> DecodeUntrusted<Context> for VecDeque<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Vec::<T>::decode_untrusted(decoder)?.into())
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for VecDeque<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Vec::<T>::borrow_decode_untrusted(decoder)?.into())
    }
}

impl<Context, T> DecodeUntrusted<Context> for Vec<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decode_vec(decoder, T::decode_untrusted)
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Vec<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decode_vec(decoder, T::borrow_decode_untrusted)
    }
}

impl<Context, T> DecodeUntrusted<Context> for Box<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::decode_untrusted(decoder)?;
        Ok(Box::new(t))
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Box<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::borrow_decode_untrusted(decoder)?;
        Ok(Box::new(t))
    }
}

impl<Context, T> DecodeUntrusted<Context> for Box<[T]>
where
    T: DecodeUntrusted<Context> + 'static,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let vec = Vec::decode_untrusted(decoder)?;
        Ok(vec.into_boxed_slice())
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Box<[T]>
where
    T: BorrowDecodeUntrusted<'de, Context> + 'de,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let vec = Vec::borrow_decode_untrusted(decoder)?;
        Ok(vec.into_boxed_slice())
    }
}

impl<Context, T> DecodeUntrusted<Context> for Cow<'_, T>
where
    T: ToOwned + ?Sized,
    <T as ToOwned>::Owned: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = <T as ToOwned>::Owned::decode_untrusted(decoder)?;
        Ok(Cow::Owned(t))
    }
}

impl<'cow, T, Context> BorrowDecodeUntrusted<'cow, Context> for Cow<'cow, T>
where
    T: ToOwned + ?Sized,
    &'cow T: BorrowDecodeUntrusted<'cow, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'cow, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = <&T>::borrow_decode_untrusted(decoder)?;
        Ok(Cow::Borrowed(t))
    }
}

impl<Context, T> DecodeUntrusted<Context> for Rc<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::decode_untrusted(decoder)?;
        Ok(Rc::new(t))
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Rc<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::borrow_decode_untrusted(decoder)?;
        Ok(Rc::new(t))
    }
}

impl<Context, T> DecodeUntrusted<Context> for Rc<[T]>
where
    T: DecodeUntrusted<Context> + 'static,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let vec = Vec::decode_untrusted(decoder)?;
        Ok(vec.into())
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Rc<[T]>
where
    T: BorrowDecodeUntrusted<'de, Context> + 'de,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let vec = Vec::borrow_decode_untrusted(decoder)?;
        Ok(vec.into())
    }
}

#[cfg(target_has_atomic = "ptr")]
impl<Context, T> DecodeUntrusted<Context> for Arc<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::decode_untrusted(decoder)?;
        Ok(Arc::new(t))
    }
}

#[cfg(target_has_atomic = "ptr")]
impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Arc<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::borrow_decode_untrusted(decoder)?;
        Ok(Arc::new(t))
    }
}

#[cfg(target_has_atomic = "ptr")]
impl<Context, T> DecodeUntrusted<Context> for Arc<[T]>
where
    T: DecodeUntrusted<Context> + 'static,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let vec = Vec::decode_untrusted(decoder)?;
        Ok(vec.into())
    }
}

#[cfg(target_has_atomic = "ptr")]
impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Arc<[T]>
where
    T: BorrowDecodeUntrusted<'de, Context> + 'de,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let vec = Vec::borrow_decode_untrusted(decoder)?;
        Ok(vec.into())
    }
}
