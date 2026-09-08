use crate::{de::Decode, enc::Encode, impl_borrow_decode};
use core::sync::atomic::Ordering;

#[cfg(target_has_atomic = "ptr")]
use core::sync::atomic::{AtomicIsize, AtomicUsize};

#[cfg(target_has_atomic = "8")]
use core::sync::atomic::{AtomicBool, AtomicI8, AtomicU8};

#[cfg(target_has_atomic = "16")]
use core::sync::atomic::{AtomicI16, AtomicU16};

#[cfg(target_has_atomic = "32")]
use core::sync::atomic::{AtomicI32, AtomicU32};

#[cfg(target_has_atomic = "64")]
use core::sync::atomic::{AtomicI64, AtomicU64};

#[cfg(target_has_atomic = "8")]
impl Encode for AtomicBool {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "8")]
impl<Context> Decode<Context> for AtomicBool {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicBool::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "8")]
impl_borrow_decode!(AtomicBool);

#[cfg(target_has_atomic = "8")]
impl Encode for AtomicU8 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "8")]
impl<Context> Decode<Context> for AtomicU8 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicU8::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "8")]
impl_borrow_decode!(AtomicU8);

#[cfg(target_has_atomic = "16")]
impl Encode for AtomicU16 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "16")]
impl<Context> Decode<Context> for AtomicU16 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicU16::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "16")]
impl_borrow_decode!(AtomicU16);

#[cfg(target_has_atomic = "32")]
impl Encode for AtomicU32 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "32")]
impl<Context> Decode<Context> for AtomicU32 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicU32::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "32")]
impl_borrow_decode!(AtomicU32);

#[cfg(target_has_atomic = "64")]
impl Encode for AtomicU64 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "64")]
impl<Context> Decode<Context> for AtomicU64 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicU64::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "64")]
impl_borrow_decode!(AtomicU64);

#[cfg(target_has_atomic = "ptr")]
impl Encode for AtomicUsize {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "ptr")]
impl<Context> Decode<Context> for AtomicUsize {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicUsize::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "ptr")]
impl_borrow_decode!(AtomicUsize);

#[cfg(target_has_atomic = "8")]
impl Encode for AtomicI8 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "8")]
impl<Context> Decode<Context> for AtomicI8 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicI8::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "8")]
impl_borrow_decode!(AtomicI8);

#[cfg(target_has_atomic = "16")]
impl Encode for AtomicI16 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "16")]
impl<Context> Decode<Context> for AtomicI16 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicI16::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "16")]
impl_borrow_decode!(AtomicI16);

#[cfg(target_has_atomic = "32")]
impl Encode for AtomicI32 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "32")]
impl<Context> Decode<Context> for AtomicI32 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicI32::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "32")]
impl_borrow_decode!(AtomicI32);

#[cfg(target_has_atomic = "64")]
impl Encode for AtomicI64 {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "64")]
impl<Context> Decode<Context> for AtomicI64 {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicI64::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "64")]
impl_borrow_decode!(AtomicI64);

#[cfg(target_has_atomic = "ptr")]
impl Encode for AtomicIsize {
    fn encode<E: crate::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), crate::error::EncodeError> {
        self.load(Ordering::SeqCst).encode(encoder)
    }
}

#[cfg(target_has_atomic = "ptr")]
impl<Context> Decode<Context> for AtomicIsize {
    fn decode<D: crate::de::Decoder>(decoder: &mut D) -> Result<Self, crate::error::DecodeError> {
        Ok(AtomicIsize::new(Decode::decode(decoder)?))
    }
}
#[cfg(target_has_atomic = "ptr")]
impl_borrow_decode!(AtomicIsize);

#[cfg(target_has_atomic = "8")]
impl<C> crate::DecodeUntrusted<C> for AtomicBool {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "8")]
crate::impl_borrow_decode_untrusted!(AtomicBool);
#[cfg(target_has_atomic = "8")]
impl<C> crate::DecodeUntrusted<C> for AtomicU8 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "8")]
crate::impl_borrow_decode_untrusted!(AtomicU8);
#[cfg(target_has_atomic = "16")]
impl<C> crate::DecodeUntrusted<C> for AtomicU16 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "16")]
crate::impl_borrow_decode_untrusted!(AtomicU16);
#[cfg(target_has_atomic = "32")]
impl<C> crate::DecodeUntrusted<C> for AtomicU32 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "32")]
crate::impl_borrow_decode_untrusted!(AtomicU32);
#[cfg(target_has_atomic = "64")]
impl<C> crate::DecodeUntrusted<C> for AtomicU64 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "64")]
crate::impl_borrow_decode_untrusted!(AtomicU64);
#[cfg(target_has_atomic = "ptr")]
impl<C> crate::DecodeUntrusted<C> for AtomicUsize {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "ptr")]
crate::impl_borrow_decode_untrusted!(AtomicUsize);
#[cfg(target_has_atomic = "8")]
impl<C> crate::DecodeUntrusted<C> for AtomicI8 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "8")]
crate::impl_borrow_decode_untrusted!(AtomicI8);
#[cfg(target_has_atomic = "16")]
impl<C> crate::DecodeUntrusted<C> for AtomicI16 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "16")]
crate::impl_borrow_decode_untrusted!(AtomicI16);
#[cfg(target_has_atomic = "32")]
impl<C> crate::DecodeUntrusted<C> for AtomicI32 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "32")]
crate::impl_borrow_decode_untrusted!(AtomicI32);
#[cfg(target_has_atomic = "64")]
impl<C> crate::DecodeUntrusted<C> for AtomicI64 {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "64")]
crate::impl_borrow_decode_untrusted!(AtomicI64);
#[cfg(target_has_atomic = "ptr")]
impl<C> crate::DecodeUntrusted<C> for AtomicIsize {
    fn decode_untrusted<D: crate::de::UntrustedDecoder<Context = C>>(
        d: &mut D,
    ) -> Result<Self, crate::error::DecodeError> {
        <Self as crate::Decode<C>>::decode(d)
    }
}
#[cfg(target_has_atomic = "ptr")]
crate::impl_borrow_decode_untrusted!(AtomicIsize);
