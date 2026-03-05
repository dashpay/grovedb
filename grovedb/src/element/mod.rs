//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod aggregate_sum_query;
#[cfg(feature = "minimal")]
/// Iterator utilities for traversing stored elements.
pub mod elements_iterator;
#[cfg(feature = "minimal")]
mod path_query_push_args;
#[cfg(feature = "minimal")]
/// Query execution logic for elements, including path queries and sized queries.
pub mod query;
/// Options for controlling query behavior.
pub mod query_options;

pub use grovedb_element::*;
