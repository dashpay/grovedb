//! Native allocation dispatch and visitor hints for the shared Serde wire adapter.
use crate::de::{Decoder, UntrustedDecoder};
#[cfg(feature = "alloc")]
use crate::{error::DecodeError, Decode, DecodeUntrusted};

pub(super) trait Policy<D: Decoder>: Copy + 'static {
    #[cfg(feature = "alloc")]
    fn decode<T: Decode<D::Context> + DecodeUntrusted<D::Context>>(
        decoder: &mut D,
    ) -> Result<T, DecodeError>;
    fn size_hint(len: usize) -> Option<usize>;
}

#[derive(Clone, Copy)]
pub(super) struct Ordinary;
impl<D: Decoder> Policy<D> for Ordinary {
    #[cfg(feature = "alloc")]
    fn decode<T: Decode<D::Context> + DecodeUntrusted<D::Context>>(
        decoder: &mut D,
    ) -> Result<T, DecodeError> {
        T::decode(decoder)
    }
    fn size_hint(len: usize) -> Option<usize> {
        Some(len)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Untrusted;
impl<D: UntrustedDecoder> Policy<D> for Untrusted {
    #[cfg(feature = "alloc")]
    fn decode<T: Decode<D::Context> + DecodeUntrusted<D::Context>>(
        decoder: &mut D,
    ) -> Result<T, DecodeError> {
        T::decode_untrusted(decoder)
    }
    fn size_hint(_: usize) -> Option<usize> {
        None
    }
}
