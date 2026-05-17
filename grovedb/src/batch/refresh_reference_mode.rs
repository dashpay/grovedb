//! Mode discriminant for [`GroveOp::RefreshReference`].
//!
//! Encodes both the on-disk shape being refreshed (plain `Reference`
//! vs. `ReferenceWithSumItem`) and the trust mode
//! (caller-asserted-shape vs. read-and-validate-on-disk) in a single
//! enum so invalid combinations are unrepresentable.
//!
//! [`GroveOp::RefreshReference`]: super::GroveOp::RefreshReference

#[cfg(feature = "minimal")]
use crate::element::SumValue;

/// Fully specifies a [`GroveOp::RefreshReference`] op: which on-disk
/// shape is being refreshed, the trust mode, and (for sum-item value
/// updates) the new carried sum.
///
/// Trust mode is encoded in the variant itself, so invalid
/// combinations are unrepresentable. In particular, "refresh a
/// `ReferenceWithSumItem` without changing its carried sum" only
/// makes sense in untrusted mode (under trusted the apply path has
/// no sum to write without reading disk) — only the
/// `Untrusted...NoValueUpdate` variant covers that case; there is
/// no trusted counterpart.
///
/// * [`Self::PlainReferenceTrusted`]: refresh a plain
///   [`Element::Reference`]; apply writes the op's payload
///   verbatim. No disk read; if on-disk is not a plain `Reference`
///   it gets silently coerced (caller asserts the shape).
///
/// * [`Self::PlainReferenceUntrusted`]: refresh a plain
///   [`Element::Reference`]; apply reads on-disk and writes it
///   back. A non-plain-`Reference` on disk is rejected.
///
/// * [`Self::SumItemReferenceTrusted`]: refresh an
///   [`Element::ReferenceWithSumItem`] with the contained
///   [`SumValue`]; apply writes the op's payload verbatim with that
///   sum. Cross-type coercion is the caller's responsibility.
///
/// * [`Self::SumItemReferenceUntrustedValueUpdate`]: refresh an
///   [`Element::ReferenceWithSumItem`]; apply reads on-disk for
///   path/wrapper and overrides the carried sum with the contained
///   value. On-disk must be `ReferenceWithSumItem`.
///
/// * [`Self::SumItemReferenceUntrustedNoValueUpdate`]: refresh an
///   [`Element::ReferenceWithSumItem`]; apply reads on-disk and
///   writes it back verbatim, preserving the carried sum. On-disk
///   must be `ReferenceWithSumItem`.
///
/// [`Element::Reference`]: crate::Element::Reference
/// [`Element::ReferenceWithSumItem`]: crate::Element::ReferenceWithSumItem
/// [`GroveOp::RefreshReference`]: super::GroveOp::RefreshReference
#[cfg(feature = "minimal")]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum RefreshReferenceMode {
    /// Trusted refresh of a plain [`Element::Reference`].
    ///
    /// [`Element::Reference`]: crate::Element::Reference
    PlainReferenceTrusted,
    /// Untrusted refresh of a plain [`Element::Reference`].
    ///
    /// [`Element::Reference`]: crate::Element::Reference
    PlainReferenceUntrusted,
    /// Trusted refresh of an [`Element::ReferenceWithSumItem`] with
    /// the contained sum value.
    ///
    /// [`Element::ReferenceWithSumItem`]: crate::Element::ReferenceWithSumItem
    SumItemReferenceTrusted(SumValue),
    /// Untrusted refresh of an [`Element::ReferenceWithSumItem`]
    /// that overrides the carried sum with the contained value.
    ///
    /// [`Element::ReferenceWithSumItem`]: crate::Element::ReferenceWithSumItem
    SumItemReferenceUntrustedValueUpdate(SumValue),
    /// Untrusted refresh of an [`Element::ReferenceWithSumItem`]
    /// that preserves the on-disk carried sum.
    ///
    /// [`Element::ReferenceWithSumItem`]: crate::Element::ReferenceWithSumItem
    SumItemReferenceUntrustedNoValueUpdate,
}

#[cfg(feature = "minimal")]
impl RefreshReferenceMode {
    /// True for the variants where the apply path uses the op's
    /// declared shape verbatim (no on-disk read for the writeable
    /// fields). Used by `follow_reference_get_value_hash` to decide
    /// whether dependent refs should resolve against the op's path
    /// or the on-disk path.
    pub fn is_trusted(&self) -> bool {
        matches!(
            self,
            Self::PlainReferenceTrusted | Self::SumItemReferenceTrusted(_)
        )
    }
}
