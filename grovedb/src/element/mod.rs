//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

#[cfg(any(feature = "engine_generic", feature = "verify"))]
pub mod aggregate_sum_query;
#[cfg(feature = "engine_generic")]
/// Iterator utilities for traversing stored elements.
pub mod elements_iterator;
#[cfg(feature = "engine_generic")]
mod path_query_push;
#[cfg(feature = "engine_generic")]
mod path_query_push_args;
#[cfg(feature = "engine_generic")]
/// Query execution logic for elements, including path queries and sized queries.
pub mod query;
#[cfg(feature = "engine_generic")]
/// Per-frame limit/offset budget state for the trusted query walk.
pub mod query_budget;
/// Options for controlling query behavior.
pub mod query_options;

pub use grovedb_element::*;

#[cfg(feature = "engine_generic")]
mod local_offset_read;
