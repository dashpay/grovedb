//! Recursive untrusted implementations; scalar validation uses audited concrete decoders.
use super::{
    read::Reader, BorrowDecode, BorrowDecodeUntrusted, BorrowUntrustedDecoder, Decode,
    DecodeUntrusted, UntrustedDecoder,
};
use crate::error::DecodeError;
use core::{
    cell::{Cell, RefCell},
    cmp::Reverse,
    num::*,
    ops::{Bound, Range, RangeInclusive},
    time::Duration,
};

// This is an explicit list of library-controlled leaf types, never a blanket
// implementation for arbitrary user Decode implementations.
macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        impl<C> DecodeUntrusted<C> for $ty {
            fn decode_untrusted<D: UntrustedDecoder<Context=C>>(d: &mut D) -> Result<Self,DecodeError> {
                <Self as Decode<C>>::decode(d)
            }
        }
        crate::impl_borrow_decode_untrusted!($ty);
    )*};
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
    Duration,
    NonZeroU8,
    NonZeroU16,
    NonZeroU32,
    NonZeroU64,
    NonZeroU128,
    NonZeroUsize,
    NonZeroI8,
    NonZeroI16,
    NonZeroI32,
    NonZeroI64,
    NonZeroI128,
    NonZeroIsize
);

impl<'a, 'de: 'a, C> BorrowDecodeUntrusted<'de, C> for &'a [u8] {
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = C>>(
        d: &mut D,
    ) -> Result<Self, DecodeError> {
        BorrowDecode::borrow_decode(d)
    }
}
impl<'a, 'de: 'a, C> BorrowDecodeUntrusted<'de, C> for &'a str {
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = C>>(
        d: &mut D,
    ) -> Result<Self, DecodeError> {
        BorrowDecode::borrow_decode(d)
    }
}

crate::impl_borrow_decode_untrusted!(core::marker::PhantomData<T>, T);
impl<Context, T: DecodeUntrusted<Context>> DecodeUntrusted<Context> for Wrapping<T> {
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Wrapping(T::decode_untrusted(decoder)?))
    }
}

impl<'de, Context, T: BorrowDecodeUntrusted<'de, Context>> BorrowDecodeUntrusted<'de, Context>
    for Wrapping<T>
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Wrapping(T::borrow_decode_untrusted(decoder)?))
    }
}

impl<Context, T: DecodeUntrusted<Context>> DecodeUntrusted<Context> for Reverse<T> {
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Reverse(T::decode_untrusted(decoder)?))
    }
}

impl<'de, Context, T: BorrowDecodeUntrusted<'de, Context>> BorrowDecodeUntrusted<'de, Context>
    for Reverse<T>
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(Reverse(T::borrow_decode_untrusted(decoder)?))
    }
}

impl<Context, T, const N: usize> DecodeUntrusted<Context> for [T; N]
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decoder.claim_bytes_read(core::mem::size_of::<[T; N]>())?;

        if unty::type_equal::<T, u8>() {
            let mut buf = [0u8; N];
            decoder.reader().read(&mut buf)?;
            let ptr = &mut buf as *mut _ as *mut [T; N];

            // Safety: we know that T is a u8, so it is perfectly safe to
            // translate an array of u8 into an array of T
            let res = unsafe { ptr.read() };
            Ok(res)
        } else {
            let result = super::impl_core::collect_into_array(&mut (0..N).map(|_| {
                // See the documentation on `unclaim_bytes_read` as to why we're doing this here
                decoder.unclaim_bytes_read(core::mem::size_of::<T>());
                T::decode_untrusted(decoder)
            }));

            // result is only None if N does not match the values of `(0..N)`, which it always should
            // So this unwrap should never occur
            result.unwrap()
        }
    }
}

impl<'de, T, const N: usize, Context> BorrowDecodeUntrusted<'de, Context> for [T; N]
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        decoder.claim_bytes_read(core::mem::size_of::<[T; N]>())?;

        if unty::type_equal::<T, u8>() {
            let mut buf = [0u8; N];
            decoder.reader().read(&mut buf)?;
            let ptr = &mut buf as *mut _ as *mut [T; N];

            // Safety: we know that T is a u8, so it is perfectly safe to
            // translate an array of u8 into an array of T
            let res = unsafe { ptr.read() };
            Ok(res)
        } else {
            let result = super::impl_core::collect_into_array(&mut (0..N).map(|_| {
                // See the documentation on `unclaim_bytes_read` as to why we're doing this here
                decoder.unclaim_bytes_read(core::mem::size_of::<T>());
                T::borrow_decode_untrusted(decoder)
            }));

            // result is only None if N does not match the values of `(0..N)`, which it always should
            // So this unwrap should never occur
            result.unwrap()
        }
    }
}

impl<Context, T> DecodeUntrusted<Context> for core::marker::PhantomData<T> {
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        _: &mut D,
    ) -> Result<Self, DecodeError> {
        Ok(core::marker::PhantomData)
    }
}

impl<Context, T> DecodeUntrusted<Context> for Option<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        match super::decode_option_variant(decoder, core::any::type_name::<Option<T>>())? {
            Some(_) => {
                let val = T::decode_untrusted(decoder)?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Option<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        match super::decode_option_variant(decoder, core::any::type_name::<Option<T>>())? {
            Some(_) => {
                let val = T::borrow_decode_untrusted(decoder)?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }
}

impl<Context, T, U> DecodeUntrusted<Context> for Result<T, U>
where
    T: DecodeUntrusted<Context>,
    U: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let is_ok = u32::decode_untrusted(decoder)?;
        match is_ok {
            0 => {
                let t = T::decode_untrusted(decoder)?;
                Ok(Ok(t))
            }
            1 => {
                let u = U::decode_untrusted(decoder)?;
                Ok(Err(u))
            }
            x => Err(DecodeError::UnexpectedVariant {
                found: x,
                allowed: &crate::error::AllowedEnumVariants::Range { max: 1, min: 0 },
                type_name: core::any::type_name::<Result<T, U>>(),
            }),
        }
    }
}

impl<'de, T, U, Context> BorrowDecodeUntrusted<'de, Context> for Result<T, U>
where
    T: BorrowDecodeUntrusted<'de, Context>,
    U: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let is_ok = u32::decode_untrusted(decoder)?;
        match is_ok {
            0 => {
                let t = T::borrow_decode_untrusted(decoder)?;
                Ok(Ok(t))
            }
            1 => {
                let u = U::borrow_decode_untrusted(decoder)?;
                Ok(Err(u))
            }
            x => Err(DecodeError::UnexpectedVariant {
                found: x,
                allowed: &crate::error::AllowedEnumVariants::Range { max: 1, min: 0 },
                type_name: core::any::type_name::<Result<T, U>>(),
            }),
        }
    }
}

impl<Context, T> DecodeUntrusted<Context> for Cell<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::decode_untrusted(decoder)?;
        Ok(Cell::new(t))
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Cell<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::borrow_decode_untrusted(decoder)?;
        Ok(Cell::new(t))
    }
}

impl<Context, T> DecodeUntrusted<Context> for RefCell<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::decode_untrusted(decoder)?;
        Ok(RefCell::new(t))
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for RefCell<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let t = T::borrow_decode_untrusted(decoder)?;
        Ok(RefCell::new(t))
    }
}

impl<Context, T> DecodeUntrusted<Context> for Range<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let min = T::decode_untrusted(decoder)?;
        let max = T::decode_untrusted(decoder)?;
        Ok(min..max)
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Range<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let min = T::borrow_decode_untrusted(decoder)?;
        let max = T::borrow_decode_untrusted(decoder)?;
        Ok(min..max)
    }
}

impl<Context, T> DecodeUntrusted<Context> for RangeInclusive<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let min = T::decode_untrusted(decoder)?;
        let max = T::decode_untrusted(decoder)?;
        Ok(RangeInclusive::new(min, max))
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for RangeInclusive<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let min = T::borrow_decode_untrusted(decoder)?;
        let max = T::borrow_decode_untrusted(decoder)?;
        Ok(RangeInclusive::new(min, max))
    }
}

impl<T, Context> DecodeUntrusted<Context> for Bound<T>
where
    T: DecodeUntrusted<Context>,
{
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        match u32::decode_untrusted(decoder)? {
            0 => Ok(Bound::Unbounded),
            1 => Ok(Bound::Included(T::decode_untrusted(decoder)?)),
            2 => Ok(Bound::Excluded(T::decode_untrusted(decoder)?)),
            x => Err(DecodeError::UnexpectedVariant {
                allowed: &crate::error::AllowedEnumVariants::Range { max: 2, min: 0 },
                found: x,
                type_name: core::any::type_name::<Bound<T>>(),
            }),
        }
    }
}

impl<'de, T, Context> BorrowDecodeUntrusted<'de, Context> for Bound<T>
where
    T: BorrowDecodeUntrusted<'de, Context>,
{
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        match u32::decode_untrusted(decoder)? {
            0 => Ok(Bound::Unbounded),
            1 => Ok(Bound::Included(T::borrow_decode_untrusted(decoder)?)),
            2 => Ok(Bound::Excluded(T::borrow_decode_untrusted(decoder)?)),
            x => Err(DecodeError::UnexpectedVariant {
                allowed: &crate::error::AllowedEnumVariants::Range { max: 2, min: 0 },
                found: x,
                type_name: core::any::type_name::<Bound<T>>(),
            }),
        }
    }
}

macro_rules! impl_tuple {
    () => {};
    ($first:ident $(, $extra:ident)*) => {
        impl<'de, $first $(, $extra)*, Context> BorrowDecodeUntrusted<'de, Context> for ($first, $($extra, )*)
        where
            $first: BorrowDecodeUntrusted<'de, Context>,
        $(
            $extra : BorrowDecodeUntrusted<'de, Context>,
        )*
         {
            fn borrow_decode_untrusted<BD: BorrowUntrustedDecoder<'de, Context = Context>>(decoder: &mut BD) -> Result<Self, DecodeError> {
                Ok((
                    $first::borrow_decode_untrusted(decoder)?,
                    $($extra :: borrow_decode_untrusted(decoder)?, )*
                ))
            }
        }

        impl<Context, $first $(, $extra)*> DecodeUntrusted<Context> for ($first, $($extra, )*)
        where
            $first: DecodeUntrusted<Context>,
        $(
            $extra : DecodeUntrusted<Context>,
        )*
        {
            fn decode_untrusted<DE: UntrustedDecoder<Context = Context>>(decoder: &mut DE) -> Result<Self, DecodeError> {
                Ok((
                    $first::decode_untrusted(decoder)?,
                    $($extra :: decode_untrusted(decoder)?, )*
                ))
            }
        }
    }
}

impl_tuple!(A);
impl_tuple!(A, B);
impl_tuple!(A, B, C);
impl_tuple!(A, B, C, D);
impl_tuple!(A, B, C, D, E);
impl_tuple!(A, B, C, D, E, F);
impl_tuple!(A, B, C, D, E, F, G);
impl_tuple!(A, B, C, D, E, F, G, H);
impl_tuple!(A, B, C, D, E, F, G, H, I);
impl_tuple!(A, B, C, D, E, F, G, H, I, J);
impl_tuple!(A, B, C, D, E, F, G, H, I, J, K);
impl_tuple!(A, B, C, D, E, F, G, H, I, J, K, L);
impl_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M);
impl_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N);
impl_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O);
impl_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);
