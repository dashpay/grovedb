use serde::{Deserialize, Serialize};

/// Reference path variants
#[derive(Hash, Eq, PartialEq, Serialize, Deserialize, Clone)]
// TODO: Make this entire file more intiutive
pub enum ReferencePathType {
    AbsolutePathReference(Vec<Vec<u8>>),
    UpstreamRootHeightReference(u8, Vec<Vec<u8>>),
    UpstreamFromElementHeightReference(u8, Vec<Vec<u8>>),
    CousinReference(Vec<u8>),
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
        ReferencePathType::AbsolutePathReference(path) => path,
        ReferencePathType::UpstreamRootHeightReference(height_from_root, path) => {
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
        ReferencePathType::UpstreamFromElementHeightReference(height_from_element, path) => {
            let mut path_iter = path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();
            let current_path_iter = current_path.into_iter();
            let current_path_len = current_path_iter.len();
            // taking len - height_from_element from
            if usize::from(height_from_element + 1) > current_path_len {
                panic!("current path not enough");
            }
            let mut needed_path = current_path_iter
                .take(current_path_len - height_from_element as usize - 1)
                .collect::<Vec<_>>();
            needed_path.append(&mut path_iter);
            needed_path.iter().map(|x| x.to_vec()).collect::<Vec<_>>()
        }
        ReferencePathType::CousinReference(cousin_key) => {
            let mut current_path_as_vec = current_path.into_iter().collect::<Vec<_>>();
            let current_key = current_path_as_vec.pop().expect("confirmed has key");
            current_path_as_vec.pop(); // remove the cousin key
            current_path_as_vec.push(&cousin_key);
            current_path_as_vec.push(current_key);
            current_path_as_vec
                .iter()
                .map(|x| x.to_vec())
                .collect::<Vec<_>>()
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
    use merk::proofs::Query;
    use crate::{reference_path::{path_from_reference_path_type, ReferencePathType}, tests::{make_deep_tree, make_grovedb, TEST_LEAF}, Element, PathQuery};

    #[test]
    fn test_upstream_root_height_reference() {
        let ref1 =
            ReferencePathType::UpstreamRootHeightReference(1, vec![b"c".to_vec(), b"d".to_vec()]);
        let final_path =
            path_from_reference_path_type(ref1, vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref()]);
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );
    }

    #[test]
    fn test_upstream_from_element_height_reference() {
        let ref1 = ReferencePathType::UpstreamFromElementHeightReference(
            0,
            vec![b"c".to_vec(), b"d".to_vec()],
        );
        let final_path =
            path_from_reference_path_type(ref1, vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref()]);
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );
    }

    #[test]
    fn test_cousin_reference() {
        let ref1 = ReferencePathType::CousinReference(b"c".to_vec());
        let final_path = path_from_reference_path_type(
            ref1,
            vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref(), b"d".as_ref()],
        );
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );
    }

    #[test]
    fn test_query_many_with_different_reference_types() {
        let db = make_deep_tree();

        db.insert(
            [TEST_LEAF, b"innertree4"],
            b"ref1",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"innertree".to_vec(),
                b"key1".to_vec(),
            ])),
            None,
        )
        .unwrap()
        .expect("should insert successfully");

        db.insert(
            [TEST_LEAF, b"innertree4"],
            b"ref2",
            Element::new_reference(ReferencePathType::UpstreamRootHeightReference(0, vec![
                b"innertree".to_vec(),
                b"key1".to_vec(),
            ])),
            None,
        )
            .unwrap()
            .expect("should insert successfully");

        db.insert(
            [TEST_LEAF, b"innertree4"],
            b"ref3",
            Element::new_reference(ReferencePathType::UpstreamFromElementHeightReference(1, vec![
                b"innertree".to_vec(),
                b"key1".to_vec(),
            ])),
            None,
        )
            .unwrap()
            .expect("should insert successfully");

        // Query all the elements in Test Leaf
        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], query);
        let result = db.query(&path_query, None).unwrap().expect("should query items");
        dbg!(result);
    }
}
