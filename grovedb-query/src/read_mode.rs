//! How a [`Query`](crate::Query) node reads the tree its (sub)path
//! names.
//!
//! `read_mode: None` on a `Query` is ordinary key selection — all of
//! today's behavior, byte-identical on the wire. A `Some(ReadMode)`
//! changes what the node means:
//!
//! - [`ReadMode::Axis`] reads the per-axis secondary of the indexed
//!   tree the node's path names, in aggregate order, instead of the
//!   tree's own keyspace.
//! - [`ReadMode::SumBudget`] walks the node's items in key order but
//!   stops on a running-sum budget instead of a result-count limit —
//!   the read `AggregateSumPathQuery` serves today, expressed in the
//!   unified vocabulary.
//!
//! Structural rules (which items/branches a carrying `Query` may have,
//! where in a `PathQuery` a read mode may appear) are owned by
//! `PathQuery::classify` in the `grovedb` crate; this module owns the
//! vocabulary, its encoding, and the per-mode well-formedness rules
//! that don't depend on position.
//!
//! Wire stability: mode tags (`Axis = 0`, `SumBudget = 1`) are frozen.

use std::fmt;

use bincode::{
    de::{BorrowDecoder, Decoder},
    enc::Encoder,
    error::{DecodeError, EncodeError},
    BorrowDecode, Decode, Encode,
};

use crate::{axis_query::AxisQuery, error::Error, query::Query};

/// A key-ordered read that stops once the running sum of matched
/// sum-item values reaches a budget. The unified-vocabulary form of
/// `AggregateSumQuery`'s stop condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SumBudgetRead {
    /// Stop once the running **net** sum of matched sum-item values
    /// reaches this. Distinct from a result-count limit: how many
    /// entries that takes depends on the data (and negative values give
    /// budget back). Must fit in `i64` — the budget arithmetic is the
    /// engine's signed saturating subtraction.
    pub sum_limit: u64,
    /// Stop after this many **matched** sum items, regardless of
    /// budget. `None` = no match cap. (Elements scanned but skipped —
    /// non-sum elements, references — do not count; the grove-version
    /// global scan cap bounds those separately.)
    pub match_limit: Option<u16>,
}

impl SumBudgetRead {
    /// Reject a budget that cannot describe any answer.
    pub fn validate(&self) -> Result<(), Error> {
        if self.sum_limit == 0 {
            return Err(Error::InvalidOperation(
                "sum-budget read: `sum_limit` must be at least 1; a zero budget stops before \
                 selecting anything",
            ));
        }
        if self.sum_limit > i64::MAX as u64 {
            return Err(Error::InvalidOperation(
                "sum-budget read: `sum_limit` must fit in i64 — the budget arithmetic is \
                 signed",
            ));
        }
        if self.match_limit == Some(0) {
            return Err(Error::InvalidOperation(
                "sum-budget read: `match_limit` must be at least 1 when set; a zero match cap \
                 selects nothing",
            ));
        }
        Ok(())
    }
}

impl Encode for SumBudgetRead {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        self.sum_limit.encode(encoder)?;
        self.match_limit.encode(encoder)
    }
}

impl<Context> Decode<Context> for SumBudgetRead {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Ok(Self {
            sum_limit: u64::decode(decoder)?,
            match_limit: Option::<u16>::decode(decoder)?,
        })
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for SumBudgetRead {
    fn borrow_decode<D: BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode(decoder)
    }
}

impl fmt::Display for SumBudgetRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SumBudget {{ sum_limit: {}, match_limit: {:?} }}",
            self.sum_limit, self.match_limit
        )
    }
}

/// How a [`Query`](crate::Query) node reads the tree its (sub)path
/// names. Absent (`None` on the `Query`) means plain key selection.
///
/// Wire tags are frozen: `Axis = 0`, `SumBudget = 1`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReadMode {
    /// Axis-ordered read of the indexed tree this node's path names.
    /// The carrying `Query` must have empty `items` and no subquery
    /// branches — the axis query is the whole read.
    Axis(AxisQuery),
    /// Key-ordered read of this node's items that stops on a running
    /// sum budget. The carrying `Query` must have non-empty `items`
    /// and no subquery branches.
    SumBudget(SumBudgetRead),
}

impl ReadMode {
    /// Position-independent well-formedness of the mode itself.
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            ReadMode::Axis(axis_query) => axis_query.validate(),
            ReadMode::SumBudget(budget) => budget.validate(),
        }
    }
}

impl Encode for ReadMode {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        match self {
            ReadMode::Axis(axis_query) => {
                0u8.encode(encoder)?;
                axis_query.encode(encoder)
            }
            ReadMode::SumBudget(budget) => {
                1u8.encode(encoder)?;
                budget.encode(encoder)
            }
        }
    }
}

impl<Context> Decode<Context> for ReadMode {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        match u8::decode(decoder)? {
            0 => Ok(ReadMode::Axis(AxisQuery::decode(decoder)?)),
            1 => Ok(ReadMode::SumBudget(SumBudgetRead::decode(decoder)?)),
            _ => Err(DecodeError::Other("unknown read mode tag")),
        }
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for ReadMode {
    fn borrow_decode<D: BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode(decoder)
    }
}

impl fmt::Display for ReadMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadMode::Axis(axis_query) => write!(f, "Axis({axis_query})"),
            ReadMode::SumBudget(budget) => write!(f, "{budget}"),
        }
    }
}

impl Query {
    /// Whether this query — or any query nested in its subquery
    /// branches — carries a [`ReadMode`]. Entry points that don't serve
    /// read modes use this to fail closed instead of silently running a
    /// read-mode query as plain key selection.
    pub fn has_read_mode_anywhere(&self) -> bool {
        if self.read_mode.is_some() {
            return true;
        }
        if let Some(sub) = self.default_subquery_branch.subquery.as_deref()
            && sub.has_read_mode_anywhere()
        {
            return true;
        }
        if let Some(branches) = &self.conditional_subquery_branches {
            for branch in branches.values() {
                if let Some(sub) = branch.subquery.as_deref()
                    && sub.has_read_mode_anywhere()
                {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use bincode::config;

    use super::*;
    use crate::axis_query::IndexAxis;

    #[test]
    fn read_mode_round_trips() {
        let modes = [
            ReadMode::Axis(AxisQuery::top_k(IndexAxis::Sum, 10, 20, true)),
            ReadMode::SumBudget(SumBudgetRead {
                sum_limit: 1000,
                match_limit: Some(50),
            }),
            ReadMode::SumBudget(SumBudgetRead {
                sum_limit: 1,
                match_limit: None,
            }),
        ];
        for mode in modes {
            let bytes = bincode::encode_to_vec(&mode, config::standard()).unwrap();
            let (decoded, consumed): (ReadMode, usize) =
                bincode::decode_from_slice(&bytes, config::standard()).unwrap();
            assert_eq!(consumed, bytes.len());
            assert_eq!(decoded, mode);
        }
    }

    #[test]
    fn read_mode_wire_tags_are_frozen() {
        let axis = ReadMode::Axis(AxisQuery::top_k(IndexAxis::Count, 1, 0, false));
        assert_eq!(
            bincode::encode_to_vec(&axis, config::standard()).unwrap()[0],
            0
        );
        let budget = ReadMode::SumBudget(SumBudgetRead {
            sum_limit: 1,
            match_limit: None,
        });
        assert_eq!(
            bincode::encode_to_vec(&budget, config::standard()).unwrap()[0],
            1
        );
        assert!(
            bincode::decode_from_slice::<ReadMode, _>(&[2u8], config::standard()).is_err(),
            "unknown mode tag must be rejected"
        );
    }

    #[test]
    fn sum_budget_validation() {
        assert!(SumBudgetRead {
            sum_limit: 0,
            match_limit: None
        }
        .validate()
        .is_err());
        assert!(SumBudgetRead {
            sum_limit: 1,
            match_limit: Some(0)
        }
        .validate()
        .is_err());
        assert!(SumBudgetRead {
            sum_limit: 1,
            match_limit: Some(1)
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn has_read_mode_anywhere_walks_subqueries() {
        let mut plain = Query::new_single_key(b"k".to_vec());
        assert!(!plain.has_read_mode_anywhere());

        // Directly on the node.
        let mut direct = Query::new();
        direct.read_mode = Some(ReadMode::Axis(AxisQuery::top_k(
            IndexAxis::Count,
            1,
            0,
            true,
        )));
        assert!(direct.has_read_mode_anywhere());

        // Hidden in the default subquery branch.
        plain.set_subquery(direct.clone());
        assert!(plain.has_read_mode_anywhere());

        // Hidden in a conditional subquery branch.
        let mut conditional = Query::new_single_key(b"k".to_vec());
        conditional.add_conditional_subquery(
            crate::QueryItem::Key(b"k".to_vec()),
            None,
            Some(direct),
        );
        assert!(conditional.has_read_mode_anywhere());
    }
}
