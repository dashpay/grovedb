//! Validate Element wrapper discriminants before descending into their payloads.
//!
//! Field decoding explicitly selects the ordinary or untrusted trait.
//! Public GroveDB deserialization boundaries select the untrusted safeguards.

use bincode::{
    de::Decoder,
    error::{AllowedEnumVariants, DecodeError},
    Decode, DecodeUntrusted,
};

use crate::{
    bidirectional_reference::{BackwardReferences, BidirectionalReference},
    element::Element,
    reference_path::ReferencePathType,
};

const ELEMENT_VARIANTS: AllowedEnumVariants = AllowedEnumVariants::Range { min: 0, max: 28 };

// Keep discriminants and domain checks in one schema. Each expansion calls only
// the selected trait for fields and its matching helper for wrapper recursion.
macro_rules! element_decoder {
    ($variant:ident, $decoder:path, $decode_trait:ident, $decode:ident) => {
        impl Element {
            fn $variant<D: $decoder>(
                decoder: &mut D,
                allow_wrapper: bool,
            ) -> Result<Self, DecodeError> {
                let variant = u32::$decode(decoder)?;
                match variant {
                    0 => Ok(Self::Item(
                        $decode_trait::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    1 => Ok(Self::Reference(
                        ReferencePathType::$decode(decoder)?,
                        Option::<u8>::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    2 => Ok(Self::Tree(
                        $decode_trait::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    3 => Ok(Self::SumItem(
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    4 => Ok(Self::SumTree(
                        $decode_trait::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    5 => Ok(Self::BigSumTree(
                        $decode_trait::$decode(decoder)?,
                        i128::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    6 => Ok(Self::CountTree(
                        $decode_trait::$decode(decoder)?,
                        u64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    7 => Ok(Self::CountSumTree(
                        $decode_trait::$decode(decoder)?,
                        u64::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    8 => Ok(Self::ProvableCountTree(
                        $decode_trait::$decode(decoder)?,
                        u64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    9 => Ok(Self::ItemWithSumItem(
                        $decode_trait::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    10 => Ok(Self::ProvableCountSumTree(
                        $decode_trait::$decode(decoder)?,
                        u64::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    11 => Ok(Self::CommitmentTree(
                        u64::$decode(decoder)?,
                        u8::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    12 => Ok(Self::MmrTree(
                        u64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    13 => Ok(Self::BulkAppendTree(
                        u64::$decode(decoder)?,
                        u8::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    14 => Ok(Self::DenseAppendOnlyFixedSizeTree(
                        u16::$decode(decoder)?,
                        u8::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    15..=17 if !allow_wrapper => Err(DecodeError::Other(
                        "nested Element wrappers are not allowed",
                    )),
                    15 => {
                        let inner = Self::$variant(decoder, false)?;
                        if matches!(
                            inner,
                            Self::BidirectionalReference(..)
                                | Self::ItemWithBackwardsReferences(..)
                                | Self::SumItemWithBackwardsReferences(..)
                                | Self::ItemWithSumItemWithBackwardsReferences(..)
                        ) {
                            return Err(DecodeError::Other(
                                "NonCounted cannot wrap a backward-references element",
                            ));
                        }
                        Ok(Self::NonCounted(Box::new(inner)))
                    }
                    16 => {
                        let inner = Self::$variant(decoder, false)?;
                        if !matches!(
                            inner,
                            Self::SumTree(..)
                                | Self::BigSumTree(..)
                                | Self::CountSumTree(..)
                                | Self::ProvableCountSumTree(..)
                                | Self::ProvableSumTree(..)
                                | Self::ProvableCountProvableSumTree(..)
                        ) {
                            return Err(DecodeError::Other(
                                "NotSummed inner element must be a sum-bearing tree",
                            ));
                        }
                        Ok(Self::NotSummed(Box::new(inner)))
                    }
                    17 => {
                        let inner = Self::$variant(decoder, false)?;
                        if !matches!(
                            inner,
                            Self::SumTree(..)
                                | Self::BigSumTree(..)
                                | Self::CountSumTree(..)
                                | Self::ProvableCountSumTree(..)
                                | Self::ProvableSumTree(..)
                                | Self::ProvableCountProvableSumTree(..)
                        ) {
                            return Err(DecodeError::Other(
                                "NotCountedOrSummed inner element must be a sum-bearing tree",
                            ));
                        }
                        Ok(Self::NotCountedOrSummed(Box::new(inner)))
                    }
                    18 => Ok(Self::ReferenceWithSumItem(
                        ReferencePathType::$decode(decoder)?,
                        Option::<u8>::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    19 => Ok(Self::ProvableSumTree(
                        $decode_trait::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    20 => Ok(Self::ProvableCountProvableSumTree(
                        $decode_trait::$decode(decoder)?,
                        u64::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    21 => Ok(Self::ProvableSumIndexedTree(
                        $decode_trait::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    22 => Ok(Self::ProvableCountIndexedTree(
                        $decode_trait::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                        u64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    23 => Ok(Self::ProvableCountProvableSumIndexedTree(
                        $decode_trait::$decode(decoder)?,
                        u64::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    24 => Ok(Self::PrivateDocumentStore(
                        u64::$decode(decoder)?,
                        u32::$decode(decoder)?,
                        u8::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    25 => Ok(Self::BidirectionalReference(
                        BidirectionalReference::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    26 => Ok(Self::ItemWithBackwardsReferences(
                        $decode_trait::$decode(decoder)?,
                        BackwardReferences::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    27 => Ok(Self::SumItemWithBackwardsReferences(
                        i64::$decode(decoder)?,
                        BackwardReferences::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    28 => Ok(Self::ItemWithSumItemWithBackwardsReferences(
                        $decode_trait::$decode(decoder)?,
                        i64::$decode(decoder)?,
                        BackwardReferences::$decode(decoder)?,
                        $decode_trait::$decode(decoder)?,
                    )),
                    found => Err(DecodeError::UnexpectedVariant {
                        type_name: "Element",
                        allowed: &ELEMENT_VARIANTS,
                        found,
                    }),
                }
            }
        }
    };
}
element_decoder!(decode_variant, Decoder, Decode, decode);
element_decoder!(
    decode_variant_untrusted,
    bincode::de::UntrustedDecoder,
    DecodeUntrusted,
    decode_untrusted
);

impl<Context> Decode<Context> for Element {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Self::decode_variant(decoder, true)
    }
}

bincode::impl_borrow_decode!(Element);

// Untrusted fields and wrappers stay on the untrusted trait path.
impl<C> DecodeUntrusted<C> for Element {
    fn decode_untrusted<D: bincode::de::UntrustedDecoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode_variant_untrusted(decoder, true)
    }
}
bincode::impl_borrow_decode_untrusted!(Element);

#[cfg(feature = "serde")]
impl<'de> bincode::serde::DeserializeUntrusted<'de> for Element {}
