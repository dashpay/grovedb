use std::fmt;

use bincode::{
    de::{BorrowDecoder, Decoder},
    error::DecodeError,
    BorrowDecode, Decode, Encode,
};

use crate::{hex_to_ascii, Path, Query};

/// Subquery branch
///
/// Uses a custom `Decode` implementation that tracks nesting depth through
/// the mutual recursion with [`Query`] to prevent stack overflow from
/// deeply nested subqueries during deserialization.
#[derive(Debug, Default, Clone, PartialEq, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SubqueryBranch {
    /// Subquery path
    pub subquery_path: Option<Path>,
    /// Subquery
    pub subquery: Option<Box<Query>>,
}

impl SubqueryBranch {
    pub(crate) fn decode_with_depth<D: Decoder>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        let subquery_path = Option::<Path>::decode(decoder)?;
        let has_subquery = u8::decode(decoder)?;
        let subquery = if has_subquery == 1 {
            Some(Box::new(Query::decode_with_depth(decoder, depth + 1)?))
        } else {
            None
        };
        Ok(SubqueryBranch {
            subquery_path,
            subquery,
        })
    }

    pub(crate) fn borrow_decode_with_depth<'de, D: BorrowDecoder<'de>>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        let subquery_path = Option::<Path>::borrow_decode(decoder)?;
        let has_subquery = u8::borrow_decode(decoder)?;
        let subquery = if has_subquery == 1 {
            Some(Box::new(Query::borrow_decode_with_depth(
                decoder,
                depth + 1,
            )?))
        } else {
            None
        };
        Ok(SubqueryBranch {
            subquery_path,
            subquery,
        })
    }
}

impl<Context> Decode<Context> for SubqueryBranch {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Self::decode_with_depth(decoder, 0)
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for SubqueryBranch {
    fn borrow_decode<D: BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::borrow_decode_with_depth(decoder, 0)
    }
}

impl SubqueryBranch {
    /// Returns the depth of the subquery branch
    /// This depth is how many GroveDB layers down we could query at maximum
    #[inline]
    pub fn max_depth(&self) -> Option<u16> {
        self.max_depth_internal(u8::MAX)
    }

    /// Returns the depth of the subquery branch
    /// This depth is how many GroveDB layers down we could query at maximum
    #[inline]
    pub(crate) fn max_depth_internal(&self, recursion_limit: u8) -> Option<u16> {
        if recursion_limit == 0 {
            return None;
        }
        let subquery_path_depth = self.subquery_path.as_ref().map_or(Some(0), |path| {
            let path_len = path.len();
            if path_len > u16::MAX as usize {
                None
            } else {
                Some(path_len as u16)
            }
        })?;
        let subquery_depth = self.subquery.as_ref().map_or(Some(0), |query| {
            query.max_depth_internal(recursion_limit - 1)
        })?;
        subquery_path_depth.checked_add(subquery_depth)
    }
}

impl fmt::Display for SubqueryBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SubqueryBranch {{ ")?;
        if let Some(path) = &self.subquery_path {
            write!(f, "subquery_path: [")?;
            for (i, path_part) in path.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?
                }
                write!(f, "{}", hex_to_ascii(path_part))?;
            }
            write!(f, "], ")?;
        } else {
            write!(f, "subquery_path: None ")?;
        }
        if let Some(subquery) = &self.subquery {
            write!(f, "subquery: {} ", subquery)?;
        } else {
            write!(f, "subquery: None ")?;
        }
        write!(f, "}}")
    }
}
