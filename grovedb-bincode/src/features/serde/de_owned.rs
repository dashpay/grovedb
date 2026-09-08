use super::de_policy::{Ordinary, Policy, Untrusted};
use super::DeserializeUntrusted;
use super::{de_borrowed::borrow_decode_from_slice, DecodeError as SerdeDecodeError};
use crate::de::UntrustedDecoder;
use crate::{
    config::Config,
    de::{read::Reader, Decode, Decoder, DecoderImpl},
    error::DecodeError,
};
use serde::de::*;

#[cfg(feature = "std")]
use crate::features::IoReader;

/// Serde decoder encapsulating an owned reader.
pub struct OwnedSerdeDecoder<DE: Decoder> {
    pub(super) de: DE,
}

impl<DE: Decoder> OwnedSerdeDecoder<DE> {
    /// Return a type implementing `serde::Deserializer`.
    pub fn as_deserializer<'a>(
        &'a mut self,
    ) -> impl for<'de> serde::Deserializer<'de, Error = DecodeError> + 'a {
        SerdeDecoder {
            de: &mut self.de,
            policy: Ordinary,
        }
    }
}

#[cfg(feature = "std")]
impl<'r, C: Config, R: std::io::Read> OwnedSerdeDecoder<DecoderImpl<IoReader<&'r mut R>, C, ()>> {
    /// Create a standard-reader decoder with the [untrusted collection safeguards](crate#untrusted-input).
    pub fn from_std_read_untrusted(
        src: &'r mut R,
        config: C,
    ) -> OwnedUntrustedSerdeDecoder<DecoderImpl<IoReader<&'r mut R>, C, (), true>> {
        OwnedUntrustedSerdeDecoder {
            inner: OwnedSerdeDecoder {
                de: DecoderImpl::new_untrusted(IoReader::new(src), config, ()),
            },
        }
    }

    /// Creates the decoder from an `std::io::Read` implementor.
    pub fn from_std_read(
        src: &'r mut R,
        config: C,
    ) -> OwnedSerdeDecoder<DecoderImpl<IoReader<&'r mut R>, C, ()>>
    where
        C: Config,
    {
        let reader = IoReader::new(src);
        let decoder = DecoderImpl::new(reader, config, ());
        Self { de: decoder }
    }
}

impl<C: Config, R: Reader> OwnedSerdeDecoder<DecoderImpl<R, C, ()>> {
    /// Create a custom-reader decoder with the [untrusted collection safeguards](crate#untrusted-input).
    pub fn from_reader_untrusted(
        reader: R,
        config: C,
    ) -> OwnedUntrustedSerdeDecoder<DecoderImpl<R, C, (), true>> {
        OwnedUntrustedSerdeDecoder {
            inner: OwnedSerdeDecoder {
                de: DecoderImpl::new_untrusted(reader, config, ()),
            },
        }
    }

    /// Creates the decoder from a [`Reader`] implementor.
    pub fn from_reader(reader: R, config: C) -> OwnedSerdeDecoder<DecoderImpl<R, C, ()>>
    where
        C: Config,
    {
        let decoder = DecoderImpl::new(reader, config, ());
        Self { de: decoder }
    }
}

/// Attempt to decode a given type `D` from the given slice. Returns the decoded output and the amount of bytes read.
///
/// Note that this does not work with borrowed types like `&str` or `&[u8]`. For that use [borrow_decode_from_slice].
///
/// See the [config] module for more information on configurations.
///
/// [borrow_decode_from_slice]: fn.borrow_decode_from_slice.html
/// [config]: ../config/index.html
pub fn decode_from_slice<D, C>(slice: &[u8], config: C) -> Result<(D, usize), DecodeError>
where
    D: DeserializeOwned,
    C: Config,
{
    borrow_decode_from_slice(slice, config)
}

/// Decode type `D` from the given reader with the given `Config`. The reader can be any type that implements `std::io::Read`, e.g. `std::fs::File`.
///
/// See the [config] module for more information about config options.
///
/// [config]: ../config/index.html
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
pub fn decode_from_std_read<'r, D: DeserializeOwned, C: Config, R: std::io::Read>(
    src: &'r mut R,
    config: C,
) -> Result<D, DecodeError> {
    let mut serde_decoder =
        OwnedSerdeDecoder::<DecoderImpl<IoReader<&'r mut R>, C, ()>>::from_std_read(src, config);
    D::deserialize(serde_decoder.as_deserializer())
}

/// Attempt to decode a given type `D` from the given [Reader].
///
/// See the [config] module for more information on configurations.
///
/// [config]: ../config/index.html
pub fn decode_from_reader<D: DeserializeOwned, R: Reader, C: Config>(
    reader: R,
    config: C,
) -> Result<D, DecodeError> {
    let mut serde_decoder = OwnedSerdeDecoder::<DecoderImpl<R, C, ()>>::from_reader(reader, config);
    D::deserialize(serde_decoder.as_deserializer())
}

/// Decode from a slice with the [untrusted collection safeguards](crate#untrusted-input).
/// Returns the decoded value and the number of bytes consumed.
pub fn decode_from_slice_untrusted<D: for<'de> DeserializeUntrusted<'de>, C: Config>(
    slice: &[u8],
    config: C,
) -> Result<(D, usize), DecodeError> {
    super::de_borrowed::borrow_decode_from_slice_untrusted(slice, config)
}

/// Decode from a standard reader with the [untrusted collection safeguards](crate#untrusted-input).
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
pub fn decode_from_std_read_untrusted<
    D: for<'de> DeserializeUntrusted<'de>,
    C: Config,
    R: std::io::Read,
>(
    src: &mut R,
    config: C,
) -> Result<D, DecodeError> {
    let mut decoder = OwnedSerdeDecoder::from_std_read_untrusted(src, config);
    decoder.decode()
}

/// Decode from a custom reader with the [untrusted collection safeguards](crate#untrusted-input).
pub fn decode_from_reader_untrusted<D: for<'de> DeserializeUntrusted<'de>, R: Reader, C: Config>(
    reader: R,
    config: C,
) -> Result<D, DecodeError> {
    let mut decoder = OwnedSerdeDecoder::from_reader_untrusted(reader, config);
    decoder.decode()
}

pub(super) struct SerdeDecoder<'a, DE: Decoder, P: Policy<DE>> {
    pub(super) de: &'a mut DE,
    pub(super) policy: P,
}

impl<'de, DE: Decoder, P: Policy<DE>> Deserializer<'de> for SerdeDecoder<'_, DE, P> {
    type Error = DecodeError;

    fn deserialize_any<V>(self, _: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(SerdeDecodeError::AnyNotSupported.into())
    }

    fn deserialize_bool<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_bool(Decode::decode(&mut self.de)?)
    }

    fn deserialize_i8<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_i8(Decode::decode(&mut self.de)?)
    }

    fn deserialize_i16<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_i16(Decode::decode(&mut self.de)?)
    }

    fn deserialize_i32<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_i32(Decode::decode(&mut self.de)?)
    }

    fn deserialize_i64<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_i64(Decode::decode(&mut self.de)?)
    }

    serde::serde_if_integer128! {
        fn deserialize_i128<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            visitor.visit_i128(Decode::decode(&mut self.de)?)
        }
    }

    fn deserialize_u8<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_u8(Decode::decode(&mut self.de)?)
    }

    fn deserialize_u16<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_u16(Decode::decode(&mut self.de)?)
    }

    fn deserialize_u32<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_u32(Decode::decode(&mut self.de)?)
    }

    fn deserialize_u64<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_u64(Decode::decode(&mut self.de)?)
    }

    serde::serde_if_integer128! {
        fn deserialize_u128<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            visitor.visit_u128(Decode::decode(&mut self.de)?)
        }
    }

    fn deserialize_f32<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_f32(Decode::decode(&mut self.de)?)
    }

    fn deserialize_f64<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_f64(Decode::decode(&mut self.de)?)
    }

    fn deserialize_char<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_char(Decode::decode(&mut self.de)?)
    }

    #[cfg(feature = "alloc")]
    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_string(P::decode(self.de)?)
    }

    #[cfg(not(feature = "alloc"))]
    fn deserialize_str<V>(self, _: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(SerdeDecodeError::CannotBorrowOwnedData.into())
    }

    #[cfg(feature = "alloc")]
    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_string(P::decode(self.de)?)
    }

    #[cfg(not(feature = "alloc"))]
    fn deserialize_string<V>(self, _: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(SerdeDecodeError::CannotAllocate.into())
    }

    #[cfg(feature = "alloc")]
    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_byte_buf(P::decode(self.de)?)
    }

    #[cfg(not(feature = "alloc"))]
    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(SerdeDecodeError::CannotBorrowOwnedData.into())
    }

    #[cfg(feature = "alloc")]
    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_byte_buf(P::decode(self.de)?)
    }
    #[cfg(not(feature = "alloc"))]
    fn deserialize_byte_buf<V>(self, _: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(SerdeDecodeError::CannotAllocate.into())
    }

    fn deserialize_option<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let variant = crate::de::decode_option_variant(&mut self.de, "Option<T>")?;
        if variant.is_some() {
            visitor.visit_some(self)
        } else {
            visitor.visit_none()
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let len = usize::decode(&mut self.de)?;
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple<V>(mut self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        struct Access<'a, 'b, DE: Decoder, P: Policy<DE>> {
            deserializer: &'a mut SerdeDecoder<'b, DE, P>,
            len: usize,
        }

        impl<'de, 'a, 'b: 'a, DE: Decoder + 'b, P: Policy<DE> + 'b> SeqAccess<'de>
            for Access<'a, 'b, DE, P>
        {
            type Error = DecodeError;

            fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, DecodeError>
            where
                T: DeserializeSeed<'de>,
            {
                if self.len > 0 {
                    self.len -= 1;
                    let value = DeserializeSeed::deserialize(
                        seed,
                        SerdeDecoder {
                            de: self.deserializer.de,
                            policy: self.deserializer.policy,
                        },
                    )?;
                    Ok(Some(value))
                } else {
                    Ok(None)
                }
            }

            fn size_hint(&self) -> Option<usize> {
                P::size_hint(self.len)
            }
        }

        visitor.visit_seq(Access {
            deserializer: &mut self,
            len,
        })
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        struct Access<'a, 'b, DE: Decoder, P: Policy<DE>> {
            deserializer: &'a mut SerdeDecoder<'b, DE, P>,
            len: usize,
        }

        impl<'de, 'a, 'b: 'a, DE: Decoder + 'b, P: Policy<DE> + 'b> MapAccess<'de>
            for Access<'a, 'b, DE, P>
        {
            type Error = DecodeError;

            fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, DecodeError>
            where
                K: DeserializeSeed<'de>,
            {
                if self.len > 0 {
                    self.len -= 1;
                    let key = DeserializeSeed::deserialize(
                        seed,
                        SerdeDecoder {
                            de: self.deserializer.de,
                            policy: self.deserializer.policy,
                        },
                    )?;
                    Ok(Some(key))
                } else {
                    Ok(None)
                }
            }

            fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, DecodeError>
            where
                V: DeserializeSeed<'de>,
            {
                let value = DeserializeSeed::deserialize(
                    seed,
                    SerdeDecoder {
                        de: self.deserializer.de,
                        policy: self.deserializer.policy,
                    },
                )?;
                Ok(value)
            }

            fn size_hint(&self) -> Option<usize> {
                P::size_hint(self.len)
            }
        }

        let len = usize::decode(&mut self.de)?;

        visitor.visit_map(Access {
            deserializer: &mut self,
            len,
        })
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_enum(self)
    }

    fn deserialize_identifier<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(SerdeDecodeError::IdentifierNotSupported.into())
    }

    fn deserialize_ignored_any<V>(self, _: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(SerdeDecodeError::IgnoredAnyNotSupported.into())
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

impl<'de, DE: Decoder, P: Policy<DE>> EnumAccess<'de> for SerdeDecoder<'_, DE, P> {
    type Error = DecodeError;
    type Variant = Self;

    fn variant_seed<V>(mut self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let idx = u32::decode(&mut self.de)?;
        let val = seed.deserialize(idx.into_deserializer())?;
        Ok((val, self))
    }
}

impl<'de, DE: Decoder, P: Policy<DE>> VariantAccess<'de> for SerdeDecoder<'_, DE, P> {
    type Error = DecodeError;

    fn unit_variant(self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        DeserializeSeed::deserialize(seed, self)
    }

    fn tuple_variant<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Deserializer::deserialize_tuple(self, len, visitor)
    }

    fn struct_variant<V>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Deserializer::deserialize_tuple(self, fields.len(), visitor)
    }
}

/// An untrusted Serde decoder exposing only explicitly opted-in values.
///
/// Unlike the ordinary decoder this does not expose an unrestricted Serde deserializer.
/// ```compile_fail
/// let reader = bincode::de::read::SliceReader::new(&[1]);
/// let mut decoder = bincode::serde::OwnedSerdeDecoder::from_reader_untrusted(reader, bincode::config::standard());
/// let _ = decoder.as_deserializer();
/// ```
pub struct OwnedUntrustedSerdeDecoder<DE: UntrustedDecoder> {
    inner: OwnedSerdeDecoder<DE>,
}
impl<DE: UntrustedDecoder> OwnedUntrustedSerdeDecoder<DE> {
    /// Decode a value whose complete Serde graph explicitly opts in.
    pub fn decode<T: for<'de> DeserializeUntrusted<'de>>(&mut self) -> Result<T, DecodeError> {
        T::deserialize(SerdeDecoder {
            de: &mut self.inner.de,
            policy: Untrusted,
        })
    }
}
