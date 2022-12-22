#[cfg(feature = "full")]
use std::collections::HashMap;

#[cfg(feature = "full")]
use merk::estimated_costs::{
    average_case_costs::EstimatedLayerInformation, worst_case_costs::WorstCaseLayerInformation,
};

#[cfg(feature = "full")]
use crate::batch::KeyInfoPath;

#[cfg(feature = "full")]
/// Batch Running Mode
#[derive(Clone, PartialEq, Eq)]
pub enum BatchRunMode {
    Execute,
    AverageCase(HashMap<KeyInfoPath, EstimatedLayerInformation>),
    WorstCase(HashMap<KeyInfoPath, WorstCaseLayerInformation>),
}
