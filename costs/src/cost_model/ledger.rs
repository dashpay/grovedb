//! What the cost model handed out on this thread.
//!
//! Every `OperationCost` a function of the cost model returns to be
//! reported is added to a total, one per thread. A test of an engine adds
//! up the costs the engine reported and compares the two. They are equal
//! when the engine reported what the model gave it, all of it and nothing
//! else, whatever the engine's code looks like: a charge worked out
//! beside the model, or one of the model's that was dropped, shows as a
//! difference.
//!
//! The module is built with the feature `cost-model-ledger` only, which a
//! crate turns on for its tests.

use std::cell::RefCell;

use crate::OperationCost;

thread_local! {
    static ISSUED: RefCell<OperationCost> = RefCell::new(OperationCost::default());
}

/// The sum of the costs the model handed out on this thread since the last
/// [`take`], added up in the order they were handed out.
pub fn issued() -> OperationCost {
    ISSUED.with(|issued| issued.borrow().clone())
}

/// Returns [`issued`] and starts the total again from nothing.
pub fn take() -> OperationCost {
    ISSUED.with(RefCell::take)
}

pub(super) fn record(cost: &OperationCost) {
    ISSUED.with(|issued| *issued.borrow_mut() += cost.clone());
}
