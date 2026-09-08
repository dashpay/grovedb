//! Validate Element wrapper discriminants before descending into their payloads.
//!
//! Collection decoding is delegated to grovedb-bincode's shared safeguards.

use bincode::{
    de::Decoder,
    error::{AllowedEnumVariants, DecodeError},
    Decode,
};

use crate::{
    bidirectional_reference::{BackwardReferences, BidirectionalReference},
    element::Element,
    reference_path::ReferencePathType,
};

const ELEMENT_VARIANTS: AllowedEnumVariants = AllowedEnumVariants::Range { min: 0, max: 28 };

impl Element {
    fn decode_variant<D: Decoder>(
        decoder: &mut D,
        allow_wrapper: bool,
    ) -> Result<Self, DecodeError> {
        let variant = u32::decode(decoder)?;
        match variant {
            0 => Ok(Self::Item(
                Decode::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            1 => Ok(Self::Reference(
                ReferencePathType::decode(decoder)?,
                Option::<u8>::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            2 => Ok(Self::Tree(
                Decode::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            3 => Ok(Self::SumItem(
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            4 => Ok(Self::SumTree(
                Decode::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            5 => Ok(Self::BigSumTree(
                Decode::decode(decoder)?,
                i128::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            6 => Ok(Self::CountTree(
                Decode::decode(decoder)?,
                u64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            7 => Ok(Self::CountSumTree(
                Decode::decode(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            8 => Ok(Self::ProvableCountTree(
                Decode::decode(decoder)?,
                u64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            9 => Ok(Self::ItemWithSumItem(
                Decode::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            10 => Ok(Self::ProvableCountSumTree(
                Decode::decode(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            11 => Ok(Self::CommitmentTree(
                u64::decode(decoder)?,
                u8::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            12 => Ok(Self::MmrTree(
                u64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            13 => Ok(Self::BulkAppendTree(
                u64::decode(decoder)?,
                u8::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            14 => Ok(Self::DenseAppendOnlyFixedSizeTree(
                u16::decode(decoder)?,
                u8::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            15..=17 if !allow_wrapper => Err(DecodeError::Other(
                "nested Element wrappers are not allowed",
            )),
            15 => {
                let inner = Self::decode_variant(decoder, false)?;
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
                let inner = Self::decode_variant(decoder, false)?;
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
                let inner = Self::decode_variant(decoder, false)?;
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
                ReferencePathType::decode(decoder)?,
                Option::<u8>::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            19 => Ok(Self::ProvableSumTree(
                Decode::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            20 => Ok(Self::ProvableCountProvableSumTree(
                Decode::decode(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            21 => Ok(Self::ProvableSumIndexedTree(
                Decode::decode(decoder)?,
                Decode::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            22 => Ok(Self::ProvableCountIndexedTree(
                Decode::decode(decoder)?,
                Decode::decode(decoder)?,
                u64::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            23 => Ok(Self::ProvableCountProvableSumIndexedTree(
                Decode::decode(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                Decode::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            24 => Ok(Self::PrivateDocumentStore(
                u64::decode(decoder)?,
                u32::decode(decoder)?,
                u8::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            25 => Ok(Self::BidirectionalReference(
                BidirectionalReference::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            26 => Ok(Self::ItemWithBackwardsReferences(
                Decode::decode(decoder)?,
                BackwardReferences::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            27 => Ok(Self::SumItemWithBackwardsReferences(
                i64::decode(decoder)?,
                BackwardReferences::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            28 => Ok(Self::ItemWithSumItemWithBackwardsReferences(
                Decode::decode(decoder)?,
                i64::decode(decoder)?,
                BackwardReferences::decode(decoder)?,
                Decode::decode(decoder)?,
            )),
            found => Err(DecodeError::UnexpectedVariant {
                type_name: "Element",
                allowed: &ELEMENT_VARIANTS,
                found,
            }),
        }
    }
}

impl<Context> Decode<Context> for Element {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Self::decode_variant(decoder, true)
    }
}

bincode::impl_borrow_decode!(Element);
