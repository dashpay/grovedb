use itertools::Itertools;
use serde::{Deserialize, Serialize};

/// Reference path variants
#[derive(Hash, Eq, PartialEq, Serialize, Deserialize, Clone)]
pub enum ReferencePathType {
    AbsolutePath(Vec<Vec<u8>>),
    UpstreamRootHeight(u8, Vec<Vec<u8>>),
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
        ReferencePathType::UpstreamRootHeight(height_from_root, path) => {
            // TODO: Works but inefficient

            let mut path_iter = path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

            let current_path_iter = current_path.into_iter();
            if usize::from(height_from_root + 1) > current_path_iter.len() {
                panic!("current path not enough");
            }

            let mut needed_path = current_path_iter
                .take(height_from_root as usize + 1)
                .collect::<Vec<_>>();
            needed_path.append(&mut path_iter);
            needed_path.iter().map(|x| x.to_vec()).collect::<Vec<_>>()
        }
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

// TODO: Add tests here

#[cfg(test)]
mod tests {
    use crate::reference_path::{path_from_reference_path_type, ReferencePathType};

    #[test]
    fn test_upstream_root_height_reference() {
        let ref1 = ReferencePathType::UpstreamRootHeight(1, vec![b"c".to_vec(), b"d".to_vec()]);
        let final_path =
            path_from_reference_path_type(ref1, vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref()]);
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );
    }
}
