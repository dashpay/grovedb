//! What `tests` is where no storage engine is compiled in: the store that
//! fails reads, which the library reads through in every test build, and
//! the tests of the estimated costs that open no database.

#[path = "tests/faultable_reads.rs"]
pub(crate) mod faultable_reads;

#[cfg(feature = "estimated_costs")]
#[path = "tests/delete_cost_estimation_tests.rs"]
mod delete_cost_estimation_tests;
#[cfg(feature = "estimated_costs")]
#[path = "tests/estimated_costs_average_case_tests.rs"]
mod estimated_costs_average_case_tests;
#[cfg(feature = "estimated_costs")]
#[path = "tests/estimated_costs_worst_case_tests.rs"]
mod estimated_costs_worst_case_tests;
#[cfg(feature = "estimated_costs")]
#[path = "tests/get_cost_estimator_tests.rs"]
mod get_cost_estimator_tests;
