use std::collections::HashMap;

use merk::estimated_costs::{
    average_case_costs::{EstimatedLayerInformation},
    worst_case_costs::WorstCaseLayerInformation,
};

use crate::batch::KeyInfoPath;

/// Batch Running Mode
#[derive(Clone, PartialEq)]
pub enum BatchRunMode {
    ExecuteMode,
    AverageCaseMode(HashMap<KeyInfoPath, EstimatedLayerInformation>),
    WorstCaseMode(HashMap<KeyInfoPath, WorstCaseLayerInformation>),
}
