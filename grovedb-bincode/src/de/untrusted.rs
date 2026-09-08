//! Explicit support for decoding externally supplied bytes.

use super::{BorrowDecoder, Decoder};
use crate::error::DecodeError;

/// A sealed decoder that always enables the untrusted collection policy.
///
/// Construct one with [`super::DecoderImpl::new_untrusted`]. Context changes and
/// mutable references preserve this property.
pub trait UntrustedDecoder: Decoder {}

/// An untrusted decoder that can also borrow from its input.
pub trait BorrowUntrustedDecoder<'de>: BorrowDecoder<'de> + UntrustedDecoder {}

impl<'de, D: BorrowDecoder<'de> + UntrustedDecoder + ?Sized> BorrowUntrustedDecoder<'de> for D {}

/// Explicitly implements the untrusted decoding contract for a type.
///
/// This trait is independent of [`super::Decode`]. Deriving it generates only
/// `DecodeUntrusted` and [`BorrowDecodeUntrusted`]; derive ordinary `Decode`
/// separately if both APIs should be supported. Nested fields must implement
/// the corresponding untrusted trait. Manual implementations are responsible
/// for their resource use and domain validation; this is not a universal CPU,
/// memory, or recursion budget.
///
/// Ordinary-only types cannot enter the untrusted API:
/// ```compile_fail
/// #[derive(bincode::Decode)]
/// struct Ordinary(u8);
/// let _ = bincode::decode_from_slice_untrusted::<Ordinary, _>(&[1], bincode::config::standard());
/// ```
/// Untrusted-only types cannot enter the ordinary API:
/// ```compile_fail
/// #[derive(bincode::DecodeUntrusted)]
/// struct External(u8);
/// let _ = bincode::decode_from_slice::<External, _>(&[1], bincode::config::standard());
/// ```
/// Derivation also checks concrete field types:
/// ```compile_fail
/// #[derive(bincode::Decode)]
/// struct Ordinary(u8);
/// #[derive(bincode::DecodeUntrusted)]
/// struct External { field: Option<Ordinary> }
/// ```
/// Enum fields and map contents require the same explicit support:
/// ```compile_fail
/// #[derive(bincode::Decode)]
/// struct Ordinary(u8);
/// #[derive(bincode::DecodeUntrusted)]
/// enum External { Map(std::collections::HashMap<u8, Ordinary>) }
/// ```
/// Borrowed entry points also reject ordinary-only implementations:
/// ```compile_fail
/// #[derive(bincode::BorrowDecode)]
/// struct Ordinary<'a>(&'a str);
/// let _ = bincode::borrow_decode_from_slice_untrusted::<Ordinary<'_>, _>(&[0], bincode::config::standard());
/// ```
/// Untrusted methods require a decoder with the policy enabled:
/// ```compile_fail
/// use bincode::DecodeUntrusted;
/// let reader = bincode::de::read::SliceReader::new(&[1]);
/// let mut decoder = bincode::de::DecoderImpl::new(reader, bincode::config::standard(), ());
/// let _ = u8::decode_untrusted(&mut decoder);
/// ```
pub trait DecodeUntrusted<Context>: Sized {
    /// Decode using the enforced untrusted policy.
    fn decode_untrusted<D: UntrustedDecoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError>;
}

/// Explicit untrusted decoding for types borrowing from their input.
///
/// Use `#[derive(BorrowDecodeUntrusted)]` for borrowed types. Deriving
/// [`DecodeUntrusted`] already generates this implementation for owned types.
pub trait BorrowDecodeUntrusted<'de, Context>: Sized {
    /// Borrow-decode using the enforced untrusted policy.
    fn borrow_decode_untrusted<D: BorrowUntrustedDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError>;
}

/// Implement borrowed untrusted decoding by delegating to an explicit owned implementation.
#[macro_export]
macro_rules! impl_borrow_decode_untrusted {
    ($ty:ty $(, $param:tt)*) => {
        impl<'de $(, $param)*, __Context> $crate::BorrowDecodeUntrusted<'de, __Context> for $ty {
            fn borrow_decode_untrusted<D: $crate::de::BorrowUntrustedDecoder<'de, Context = __Context>>(
                decoder: &mut D,
            ) -> core::result::Result<Self, $crate::error::DecodeError> {
                $crate::DecodeUntrusted::decode_untrusted(decoder)
            }
        }
    };
}
