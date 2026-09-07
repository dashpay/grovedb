//! Allocation-bounded bincode decoders for storage elements.
//!
//! Bincode's standard `Vec` decoder reserves the declared collection length
//! before asking its reader whether those bytes exist. These decoders instead
//! grow collections only after successfully reading their contents. This keeps
//! malformed element bytes from turning a short input into a large allocation.

use std::mem::size_of;

use bincode::{
    de::{read::Reader, Decoder},
    error::{AllowedEnumVariants, DecodeError},
    Decode,
};

use crate::{
    bidirectional_reference::{BackwardReference, BackwardReferences, BidirectionalReference},
    element::Element,
    indexed::{IndexedTreeAxes, IndexedTreeAxisEntry},
    reference_path::ReferencePathType,
};

const ELEMENT_VARIANTS: AllowedEnumVariants = AllowedEnumVariants::Range { min: 0, max: 28 };
const REFERENCE_PATH_VARIANTS: AllowedEnumVariants = AllowedEnumVariants::Range { min: 0, max: 6 };
const OPTION_VARIANTS: AllowedEnumVariants = AllowedEnumVariants::Range { min: 0, max: 1 };
const READ_CHUNK_SIZE: usize = 1024;

fn decode_len<D: Decoder>(decoder: &mut D) -> Result<usize, DecodeError> {
    let len = u64::decode(decoder)?;
    len.try_into()
        .map_err(|_| DecodeError::OutsideUsizeRange(len))
}

/// Decode bytes without reserving storage until the corresponding input has
/// actually been read. `claim_container_read` preserves any limit configured
/// by callers of the public `Decode` implementation.
fn decode_bytes<D: Decoder>(decoder: &mut D) -> Result<Vec<u8>, DecodeError> {
    let len = decode_len(decoder)?;
    decoder.claim_container_read::<u8>(len)?;

    // Slice readers can prove the complete payload is present before the
    // allocation and then retain the usual single-copy fast path.
    if decoder.reader().peek_read(len).is_some() {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| DecodeError::LimitExceeded)?;
        bytes.resize(len, 0);
        decoder.reader().read(&mut bytes)?;
        return Ok(bytes);
    }

    // Readers without look-ahead are grown only as successfully read input
    // arrives, so a declared length alone never controls their allocation.
    let mut bytes = Vec::new();
    let mut remaining = len;
    let mut chunk = [0u8; READ_CHUNK_SIZE];
    while remaining != 0 {
        let chunk_len = remaining.min(READ_CHUNK_SIZE);
        decoder.reader().read(&mut chunk[..chunk_len])?;
        bytes
            .try_reserve(chunk_len)
            .map_err(|_| DecodeError::LimitExceeded)?;
        bytes.extend_from_slice(&chunk[..chunk_len]);
        remaining -= chunk_len;
    }
    Ok(bytes)
}

fn decode_optional_bytes<D: Decoder>(decoder: &mut D) -> Result<Option<Vec<u8>>, DecodeError> {
    match u8::decode(decoder)? {
        0 => Ok(None),
        1 => decode_bytes(decoder).map(Some),
        found => Err(DecodeError::UnexpectedVariant {
            type_name: "Option<Vec<u8>>",
            allowed: &OPTION_VARIANTS,
            found: found.into(),
        }),
    }
}

fn decode_byte_path<D: Decoder>(decoder: &mut D) -> Result<Vec<Vec<u8>>, DecodeError> {
    let len = decode_len(decoder)?;
    decoder.claim_container_read::<Vec<u8>>(len)?;

    let mut path = Vec::new();
    for _ in 0..len {
        decoder.unclaim_bytes_read(size_of::<Vec<u8>>());
        let segment = decode_bytes(decoder)?;
        path.try_reserve(1)
            .map_err(|_| DecodeError::LimitExceeded)?;
        path.push(segment);
    }
    Ok(path)
}

fn decode_axes<D: Decoder>(decoder: &mut D) -> Result<IndexedTreeAxes, DecodeError> {
    let len = decode_len(decoder)?;
    decoder.claim_container_read::<IndexedTreeAxisEntry>(len)?;

    let mut axes = Vec::new();
    for _ in 0..len {
        decoder.unclaim_bytes_read(size_of::<IndexedTreeAxisEntry>());
        let axis = (u8::decode(decoder)?, decode_optional_bytes(decoder)?);
        axes.try_reserve(1)
            .map_err(|_| DecodeError::LimitExceeded)?;
        axes.push(axis);
    }
    Ok(axes)
}

fn decode_vec<T, D>(decoder: &mut D) -> Result<Vec<T>, DecodeError>
where
    D: Decoder,
    T: Decode<D::Context>,
{
    let len = decode_len(decoder)?;
    decoder.claim_container_read::<T>(len)?;

    let mut values = Vec::new();
    for _ in 0..len {
        decoder.unclaim_bytes_read(size_of::<T>());
        let value = T::decode(decoder)?;
        values
            .try_reserve(1)
            .map_err(|_| DecodeError::LimitExceeded)?;
        values.push(value);
    }
    Ok(values)
}

impl<Context> Decode<Context> for ReferencePathType {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        let variant = u32::decode(decoder)?;
        match variant {
            0 => Ok(Self::AbsolutePathReference(decode_byte_path(decoder)?)),
            1 => Ok(Self::UpstreamRootHeightReference(
                u8::decode(decoder)?,
                decode_byte_path(decoder)?,
            )),
            2 => Ok(Self::UpstreamRootHeightWithParentPathAdditionReference(
                u8::decode(decoder)?,
                decode_byte_path(decoder)?,
            )),
            3 => Ok(Self::UpstreamFromElementHeightReference(
                u8::decode(decoder)?,
                decode_byte_path(decoder)?,
            )),
            4 => Ok(Self::CousinReference(decode_bytes(decoder)?)),
            5 => Ok(Self::RemovedCousinReference(decode_byte_path(decoder)?)),
            6 => Ok(Self::SiblingReference(decode_bytes(decoder)?)),
            found => Err(DecodeError::UnexpectedVariant {
                type_name: "ReferencePathType",
                allowed: &REFERENCE_PATH_VARIANTS,
                found,
            }),
        }
    }
}

bincode::impl_borrow_decode!(ReferencePathType);

impl<Context> Decode<Context> for BackwardReference {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Ok(Self {
            inverted_reference: ReferencePathType::decode(decoder)?,
            cascade_on_update: bool::decode(decoder)?,
        })
    }
}

bincode::impl_borrow_decode!(BackwardReference);

impl<Context> Decode<Context> for BackwardReferences {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Ok(Self {
            max_incoming: u16::decode(decoder)?,
            entries: decode_vec(decoder)?,
        })
    }
}

bincode::impl_borrow_decode!(BackwardReferences);

impl<Context> Decode<Context> for BidirectionalReference {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Ok(Self {
            forward_reference_path: ReferencePathType::decode(decoder)?,
            cascade_on_update: bool::decode(decoder)?,
            max_hop: Option::<u8>::decode(decoder)?,
            backward_references: decode_vec(decoder)?,
        })
    }
}

bincode::impl_borrow_decode!(BidirectionalReference);

pub(crate) struct BackwardReferenceVec(pub Vec<BackwardReference>);

impl<Context> Decode<Context> for BackwardReferenceVec {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        decode_vec(decoder).map(Self)
    }
}

bincode::impl_borrow_decode!(BackwardReferenceVec);

impl Element {
    fn decode_variant<D: Decoder>(
        decoder: &mut D,
        allow_wrapper: bool,
    ) -> Result<Self, DecodeError> {
        let variant = u32::decode(decoder)?;
        match variant {
            0 => Ok(Self::Item(
                decode_bytes(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            1 => Ok(Self::Reference(
                ReferencePathType::decode(decoder)?,
                Option::<u8>::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            2 => Ok(Self::Tree(
                decode_optional_bytes(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            3 => Ok(Self::SumItem(
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            4 => Ok(Self::SumTree(
                decode_optional_bytes(decoder)?,
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            5 => Ok(Self::BigSumTree(
                decode_optional_bytes(decoder)?,
                i128::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            6 => Ok(Self::CountTree(
                decode_optional_bytes(decoder)?,
                u64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            7 => Ok(Self::CountSumTree(
                decode_optional_bytes(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            8 => Ok(Self::ProvableCountTree(
                decode_optional_bytes(decoder)?,
                u64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            9 => Ok(Self::ItemWithSumItem(
                decode_bytes(decoder)?,
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            10 => Ok(Self::ProvableCountSumTree(
                decode_optional_bytes(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            11 => Ok(Self::CommitmentTree(
                u64::decode(decoder)?,
                u8::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            12 => Ok(Self::MmrTree(
                u64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            13 => Ok(Self::BulkAppendTree(
                u64::decode(decoder)?,
                u8::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            14 => Ok(Self::DenseAppendOnlyFixedSizeTree(
                u16::decode(decoder)?,
                u8::decode(decoder)?,
                decode_optional_bytes(decoder)?,
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
                decode_optional_bytes(decoder)?,
            )),
            19 => Ok(Self::ProvableSumTree(
                decode_optional_bytes(decoder)?,
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            20 => Ok(Self::ProvableCountProvableSumTree(
                decode_optional_bytes(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            21 => Ok(Self::ProvableSumIndexedTree(
                decode_optional_bytes(decoder)?,
                decode_optional_bytes(decoder)?,
                i64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            22 => Ok(Self::ProvableCountIndexedTree(
                decode_optional_bytes(decoder)?,
                decode_optional_bytes(decoder)?,
                u64::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            23 => Ok(Self::ProvableCountProvableSumIndexedTree(
                decode_optional_bytes(decoder)?,
                u64::decode(decoder)?,
                i64::decode(decoder)?,
                decode_axes(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            24 => Ok(Self::PrivateDocumentStore(
                u64::decode(decoder)?,
                u32::decode(decoder)?,
                u8::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            25 => Ok(Self::BidirectionalReference(
                BidirectionalReference::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            26 => Ok(Self::ItemWithBackwardsReferences(
                decode_bytes(decoder)?,
                BackwardReferences::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            27 => Ok(Self::SumItemWithBackwardsReferences(
                i64::decode(decoder)?,
                BackwardReferences::decode(decoder)?,
                decode_optional_bytes(decoder)?,
            )),
            28 => Ok(Self::ItemWithSumItemWithBackwardsReferences(
                decode_bytes(decoder)?,
                i64::decode(decoder)?,
                BackwardReferences::decode(decoder)?,
                decode_optional_bytes(decoder)?,
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
