use std::collections::HashMap;

use merk::estimated_costs::average_case_costs::EstimatedLayerInformation;

use crate::batch::KeyInfoPath;

pub mod average_case_costs;
pub mod worst_case_costs;

pub enum EstimatedCostsType {
    AverageCaseCostsType(HashMap<KeyInfoPath, EstimatedLayerInformation>),
    WorstCaseCostsType,
}
