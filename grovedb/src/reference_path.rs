use serde::{Deserialize, Serialize};

/// Reference path variants
#[derive(Hash, Eq, PartialEq, Serialize, Deserialize, Clone)]
pub enum ReferencePathType {
    AbsolutePath(Vec<Vec<u8>>),
}

pub fn path_from_reference_path_type<'p, P>(
    reference_path_type: ReferencePathType,
    current_path: P,
) -> Vec<Vec<u8>>
where
    P: IntoIterator<Item = &'p [u8]>,
    <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
{
    return match reference_path_type {
        ReferencePathType::AbsolutePath(path) => path,
    };
}

// pub struct ReferencePath {
//     pub path: Vec<Vec<u8>>,
// }
//
// impl ReferencePath {
//     pub fn from_reference_path_type(reference_path_type: ReferencePathType)
// -> Self {         return match reference_path_type {
//             ReferencePathType::AbsolutePath(path) => {
//                 Self {
//                     path
//                 }
//             }
//         }
//     }
// }
