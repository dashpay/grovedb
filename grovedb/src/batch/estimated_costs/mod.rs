//! Estimated costs

#[cfg(feature = "full")]
use std::collections::HashMap;

#[cfg(feature = "full")]
use grovedb_merk::estimated_costs::{
    average_case_costs::EstimatedLayerInformation, worst_case_costs::WorstCaseLayerInformation,
};

#[cfg(feature = "full")]
use crate::batch::KeyInfoPath;

#[cfg(feature = "full")]
pub mod average_case_costs;
#[cfg(feature = "full")]
pub mod worst_case_costs;

/// Estimated costs types
#[cfg(feature = "full")]
pub enum EstimatedCostsType {
    /// Average cast estimated costs type
    AverageCaseCostsType(HashMap<KeyInfoPath, EstimatedLayerInformation>),
    /// Worst case estimated costs type
    WorstCaseCostsType(HashMap<KeyInfoPath, WorstCaseLayerInformation>),
}
