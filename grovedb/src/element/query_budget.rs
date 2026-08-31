//! The mutable budget state of one frame of the trusted query walk.
//!
//! Each recursion frame of `Element::get_query_apply_function` owns one
//! [`QueryBudget`]. The **global** budget (`SizedQuery::limit`) and the
//! **offset** follow the long-standing copy-and-reconcile scheme: a
//! subquery descent seeds the child frame with the parent's current
//! values, and the parent settles its own counters from what the child
//! reports back. The **instance** budget is the per-instance limit
//! chain (`Query::limit`): the child frame starts from
//! `min(parent's remaining instance budget, its own Query::limit)`, so
//! an ancestor's cap keeps bounding everything below it while each
//! query node's cap starts fresh for every parent key it runs under.
//!
//! `consumed` is the frame's report back to its parent: everything this
//! frame's subtree charged against the global budget — result rows plus
//! empty-subtree charges. The v2 `path_query_push` engine reconciles
//! the parent's global budget by `consumed` (matching the prover's
//! shared-counter accounting), and the instance chain by rows only —
//! unless [`QueryOptions::decrease_instance_limits_on_range_with_no_sub_elements`]
//! opts empty-subtree charges into the instance chain too.
//!
//! [`QueryOptions::decrease_instance_limits_on_range_with_no_sub_elements`]:
//! super::query_options::QueryOptions::decrease_instance_limits_on_range_with_no_sub_elements

use std::fmt;

/// Mutable limit/offset state for one frame of the trusted query walk.
#[derive(Debug)]
pub struct QueryBudget {
    /// Remaining global result budget — the `SizedQuery::limit` chain.
    pub global: Option<u16>,
    /// Remaining per-instance budget — the tightest enclosing
    /// `Query::limit`, i.e. `min` of this frame's own cap and every
    /// ancestor instance's remaining cap. `None` on every query that
    /// carries no per-instance limits, in which case the engine
    /// behaves exactly as it did before the field existed.
    pub instance: Option<u16>,
    /// Remaining global offset — the `SizedQuery::offset` chain.
    pub offset: Option<u16>,
    /// Rows and empty-subtree charges this frame's subtree has charged
    /// against the global budget; reported to the parent frame for
    /// reconciliation.
    pub consumed: u16,
}

impl QueryBudget {
    /// A frame budget seeded from a `SizedQuery` plus the instance
    /// budget inherited from the enclosing frame (already reduced by
    /// everything consumed so far).
    pub fn new(global: Option<u16>, instance: Option<u16>, offset: Option<u16>) -> Self {
        QueryBudget {
            global,
            instance,
            offset,
            consumed: 0,
        }
    }

    /// `min` over two optional caps, treating `None` as unlimited —
    /// how an inherited instance budget combines with a frame's own
    /// `Query::limit`.
    pub fn min_caps(a: Option<u16>, b: Option<u16>) -> Option<u16> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    /// The tightest remaining budget — what a storage iterator or merk
    /// walk may still produce from here.
    pub fn effective_limit(&self) -> Option<u16> {
        match (self.global, self.instance) {
            (Some(global), Some(instance)) => Some(global.min(instance)),
            (Some(global), None) => Some(global),
            (None, instance) => instance,
        }
    }

    /// Whether any budget in scope is spent — the frame must stop
    /// producing results.
    pub fn is_exhausted(&self) -> bool {
        self.global == Some(0) || self.instance == Some(0)
    }

    /// Charge one result row: rows count against every budget.
    pub fn charge_row(&mut self) {
        if let Some(global) = self.global.as_mut() {
            *global = global.saturating_sub(1);
        }
        if let Some(instance) = self.instance.as_mut() {
            *instance = instance.saturating_sub(1);
        }
        self.consumed = self.consumed.saturating_add(1);
    }

    /// Charge one empty-subtree slot: always against the global budget
    /// (that charge is what bounds walks across many empty subtrees),
    /// and against the instance chain only when the caller opted in via
    /// `decrease_instance_limits_on_range_with_no_sub_elements`.
    pub fn charge_empty_subtree(&mut self, also_instance: bool) {
        if let Some(global) = self.global.as_mut() {
            *global = global.saturating_sub(1);
        }
        if also_instance && let Some(instance) = self.instance.as_mut() {
            *instance = instance.saturating_sub(1);
        }
        self.consumed = self.consumed.saturating_add(1);
    }
}

impl fmt::Display for QueryBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QueryBudget {{ global: {:?}, instance: {:?}, offset: {:?}, consumed: {} }}",
            self.global, self.instance, self.offset, self.consumed
        )
    }
}
