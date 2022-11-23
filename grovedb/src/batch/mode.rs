use std::collections::HashMap;
use merk::estimated_costs::average_case_costs::MerkAverageCaseInput;
use merk::estimated_costs::worst_case_costs::MerkWorstCaseInput;
use crate::batch::KeyInfoPath;

/// Batch Running Mode
#[derive(Clone, PartialEq)]
pub enum BatchRunMode {
    ExecuteMode,
    AverageCaseMode(HashMap<KeyInfoPath, MerkAverageCaseInput>),
    WorstCaseMode(HashMap<KeyInfoPath, MerkWorstCaseInput>),
}