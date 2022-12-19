#[cfg(feature = "full")]
use std::collections::HashMap;

#[cfg(feature = "full")]
use merk::estimated_costs::{
    average_case_costs::EstimatedLayerInformation, worst_case_costs::WorstCaseLayerInformation,
};

#[cfg(feature = "full")]
use crate::batch::KeyInfoPath;

#[cfg(feature = "full")]
pub mod average_case_costs;
#[cfg(feature = "full")]
pub mod worst_case_costs;

#[cfg(feature = "full")]
pub enum EstimatedCostsType {
    AverageCaseCostsType(HashMap<KeyInfoPath, EstimatedLayerInformation>),
    WorstCaseCostsType(HashMap<KeyInfoPath, WorstCaseLayerInformation>),
}
