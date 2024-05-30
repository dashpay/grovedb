// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Space efficient methods for referencing other elements in GroveDB

#[cfg(feature = "full")]
use std::fmt;

use bincode::{Decode, Encode};
#[cfg(feature = "full")]
use grovedb_visualize::visualize_to_vec;
#[cfg(feature = "full")]
use integer_encoding::VarInt;

#[cfg(feature = "full")]
use crate::Error;

#[cfg(any(feature = "full", feature = "verify"))]
#[cfg_attr(not(any(feature = "full", feature = "visualize")), derive(Debug))]
/// Reference path variants
#[derive(Hash, Eq, PartialEq, Encode, Decode, Clone)]
pub enum ReferencePathType {
    /// Holds the absolute path to the element the reference points to
    AbsolutePathReference(Vec<Vec<u8>>),

    /// This takes the first n elements from the current path and appends a new
    /// path to the subpath. If current path is [a, b, c, d] and we take the
    /// first 2 elements, subpath = [a, b] we can then append some other
    /// path [p, q] result = [a, b, p, q]
    UpstreamRootHeightReference(u8, Vec<Vec<u8>>),

    /// This discards the last n elements from the current path and appends a
    /// new path to the subpath. If current path is [a, b, c, d] and we
    /// discard the last element, subpath = [a, b, c] we can then append
    /// some other path [p, q] result = [a, b, c, p, q]
    UpstreamFromElementHeightReference(u8, Vec<Vec<u8>>),

    /// This swaps the immediate parent of the stored path with a provided key,
    /// retaining the key value. e.g. current path = [a, b, m, d] you can use
    /// the cousin reference to swap m with c to get [a, b, c, d]
    CousinReference(Vec<u8>),

    /// This swaps the immediate parent of the stored path with a path,
    /// retaining the key value. e.g. current path = [a, b, c, d] you can use
    /// the removed cousin reference to swap c with m and n to get [a, b, m, n,
    /// d]
    RemovedCousinReference(Vec<Vec<u8>>),

    /// This swaps the key with a new value, you use this to point to an element
    /// in the same tree.
    SiblingReference(Vec<u8>),
}

#[cfg(any(feature = "full", feature = "verify"))]
impl ReferencePathType {
    /// Given the reference path type and the current qualified path (path+key),
    /// this computes the absolute path of the item the reference is pointing to.
    pub fn absolute_path_using_current_qualified_path<B: AsRef<[u8]>>(self, current_qualified_path: &[B]) -> Result<Vec<Vec<u8>>, Error> {
        path_from_reference_qualified_path_type(self, current_qualified_path)
    }

    /// Given the reference path type, the current path and the terminal key, this
    /// computes the absolute path of the item the reference is pointing to.
    pub fn absolute_path<B: AsRef<[u8]>>(self,     current_path: &[B],
                                         current_key: Option<&[u8]>) -> Result<Vec<Vec<u8>>, Error> {
        path_from_reference_path_type(self, current_path, current_key)
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl fmt::Debug for ReferencePathType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        visualize_to_vec(&mut v, self);

        f.write_str(&String::from_utf8_lossy(&v))
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Given the reference path type and the current qualified path (path+key),
/// this computes the absolute path of the item the reference is pointing to.
pub fn path_from_reference_qualified_path_type<B: AsRef<[u8]>>(
    reference_path_type: ReferencePathType,
    current_qualified_path: &[B],
) -> Result<Vec<Vec<u8>>, Error> {
    match current_qualified_path.split_last() {
        None => Err(Error::CorruptedPath(
            "qualified path should always have an element",
        )),
        Some((key, path)) => {
            path_from_reference_path_type(reference_path_type, path, Some(key.as_ref()))
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Given the reference path type, the current path and the terminal key, this
/// computes the absolute path of the item the reference is pointing to.
pub fn path_from_reference_path_type<B: AsRef<[u8]>>(
    reference_path_type: ReferencePathType,
    current_path: &[B],
    current_key: Option<&[u8]>,
) -> Result<Vec<Vec<u8>>, Error> {
    match reference_path_type {
        // No computation required, we already know the absolute path
        ReferencePathType::AbsolutePathReference(path) => Ok(path),

        // Take the first n elements from current path, append new path to subpath
        ReferencePathType::UpstreamRootHeightReference(no_of_elements_to_keep, mut path) => {
            let current_path_iter = current_path.iter();
            if usize::from(no_of_elements_to_keep) > current_path_iter.len() {
                return Err(Error::InvalidInput(
                    "reference stored path cannot satisfy reference constraints",
                ));
            }
            let mut subpath_as_vec = current_path_iter
                .take(no_of_elements_to_keep as usize)
                .map(|x| x.as_ref().to_vec())
                .collect::<Vec<_>>();
            subpath_as_vec.append(&mut path);
            Ok(subpath_as_vec)
        }

        // Discard the last n elements from current path, append new path to subpath
        ReferencePathType::UpstreamFromElementHeightReference(
            no_of_elements_to_discard_from_end,
            mut path,
        ) => {
            let current_path_iter = current_path.iter();
            let current_path_len = current_path_iter.len();
            if usize::from(no_of_elements_to_discard_from_end) > current_path_len {
                return Err(Error::InvalidInput(
                    "reference stored path cannot satisfy reference constraints",
                ));
            }

            let mut subpath_as_vec = current_path_iter
                .take(current_path_len - no_of_elements_to_discard_from_end as usize)
                .map(|x| x.as_ref().to_vec())
                .collect::<Vec<_>>();
            subpath_as_vec.append(&mut path);
            Ok(subpath_as_vec)
        }

        // Pop child, swap parent, reattach child
        ReferencePathType::CousinReference(cousin_key) => {
            let mut current_path_as_vec = current_path
                .iter()
                .map(|p| p.as_ref().to_vec())
                .collect::<Vec<Vec<u8>>>();
            if current_path_as_vec.is_empty() {
                return Err(Error::InvalidInput(
                    "reference stored path cannot satisfy reference constraints",
                ));
            }
            let current_key = match current_key {
                None => Err(Error::InvalidInput("cousin reference must supply a key")),
                Some(k) => Ok(k.to_vec()),
            }?;

            current_path_as_vec.pop();
            current_path_as_vec.push(cousin_key);
            current_path_as_vec.push(current_key);
            Ok(current_path_as_vec)
        }

        // Pop child, swap parent, reattach child
        ReferencePathType::RemovedCousinReference(mut cousin_path) => {
            let mut current_path_as_vec = current_path
                .iter()
                .map(|p| p.as_ref().to_vec())
                .collect::<Vec<Vec<u8>>>();
            if current_path_as_vec.is_empty() {
                return Err(Error::InvalidInput(
                    "reference stored path cannot satisfy reference constraints",
                ));
            }
            let current_key = match current_key {
                None => Err(Error::InvalidInput("cousin reference must supply a key")),
                Some(k) => Ok(k.to_vec()),
            }?;

            current_path_as_vec.pop();
            current_path_as_vec.append(&mut cousin_path);
            current_path_as_vec.push(current_key);
            Ok(current_path_as_vec)
        }

        // Pop child, attach new child
        ReferencePathType::SiblingReference(sibling_key) => {
            let mut current_path_as_vec = current_path
                .iter()
                .map(|p| p.as_ref().to_vec())
                .collect::<Vec<Vec<u8>>>();
            current_path_as_vec.push(sibling_key);
            Ok(current_path_as_vec)
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl ReferencePathType {
    /// Serialized size
    pub fn serialized_size(&self) -> usize {
        match self {
            ReferencePathType::AbsolutePathReference(path)
            | ReferencePathType::RemovedCousinReference(path) => {
                1 + path
                    .iter()
                    .map(|inner| {
                        let inner_len = inner.len();
                        inner_len + inner_len.required_space()
                    })
                    .sum::<usize>()
            }
            ReferencePathType::UpstreamRootHeightReference(_, path)
            | ReferencePathType::UpstreamFromElementHeightReference(_, path) => {
                1 + 1
                    + path
                        .iter()
                        .map(|inner| {
                            let inner_len = inner.len();
                            inner_len + inner_len.required_space()
                        })
                        .sum::<usize>()
            }
            ReferencePathType::CousinReference(path)
            | ReferencePathType::SiblingReference(path) => {
                1 + path.len() + path.len().required_space()
            }
        }
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::Query;

    use crate::{
        reference_path::{path_from_reference_path_type, ReferencePathType},
        tests::{make_deep_tree, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    #[test]
    fn test_upstream_root_height_reference() {
        let stored_path = vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref()];
        // selects the first 2 elements from the stored path and appends the new path.
        let ref1 =
            ReferencePathType::UpstreamRootHeightReference(2, vec![b"c".to_vec(), b"d".to_vec()]);
        let final_path = path_from_reference_path_type(ref1, &stored_path, None).unwrap();
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );
    }

    #[test]
    fn test_upstream_from_element_height_reference() {
        let stored_path = vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref()];
        // discards the last element from the stored_path
        let ref1 = ReferencePathType::UpstreamFromElementHeightReference(
            1,
            vec![b"c".to_vec(), b"d".to_vec()],
        );
        let final_path = path_from_reference_path_type(ref1, &stored_path, None).unwrap();
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );
    }

    #[test]
    fn test_cousin_reference_no_key() {
        let stored_path = vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref()];
        // Replaces the immediate parent (in this case b) with the given key (c)
        let ref1 = ReferencePathType::CousinReference(b"c".to_vec());
        let final_path = path_from_reference_path_type(ref1, &stored_path, None);
        assert!(final_path.is_err());
    }

    #[test]
    fn test_cousin_reference() {
        let stored_path = vec![b"a".as_ref(), b"b".as_ref()];
        let key = b"m".as_ref();
        // Replaces the immediate parent (in this case b) with the given key (c)
        let ref1 = ReferencePathType::CousinReference(b"c".to_vec());
        let final_path = path_from_reference_path_type(ref1, &stored_path, Some(key)).unwrap();
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"c".to_vec(), b"m".to_vec()]
        );
    }

    #[test]
    fn test_removed_cousin_reference_no_key() {
        let stored_path = vec![b"a".as_ref(), b"b".as_ref(), b"m".as_ref()];
        // Replaces the immediate parent (in this case b) with the given key (c)
        let ref1 = ReferencePathType::RemovedCousinReference(vec![b"c".to_vec(), b"d".to_vec()]);
        let final_path = path_from_reference_path_type(ref1, &stored_path, None);
        assert!(final_path.is_err());
    }

    #[test]
    fn test_removed_cousin_reference() {
        let stored_path = vec![b"a".as_ref(), b"b".as_ref()];
        let key = b"m".as_ref();
        // Replaces the immediate parent (in this case b) with the given key (c)
        let ref1 = ReferencePathType::RemovedCousinReference(vec![b"c".to_vec(), b"d".to_vec()]);
        let final_path = path_from_reference_path_type(ref1, &stored_path, Some(key)).unwrap();
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"c".to_vec(), b"d".to_vec(), b"m".to_vec()]
        );
    }

    #[test]
    fn test_sibling_reference() {
        let stored_path = vec![b"a".as_ref(), b"b".as_ref()];
        let key = b"m".as_ref();
        let ref1 = ReferencePathType::SiblingReference(b"c".to_vec());
        let final_path = path_from_reference_path_type(ref1, &stored_path, Some(key)).unwrap();
        assert_eq!(
            final_path,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
    }

    #[test]
    fn test_query_many_with_different_reference_types() {
        let db = make_deep_tree();

        db.insert(
            [TEST_LEAF, b"innertree4"].as_ref(),
            b"ref1",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"innertree".to_vec(),
                b"key1".to_vec(),
            ])),
            None,
            None,
        )
        .unwrap()
        .expect("should insert successfully");

        db.insert(
            [TEST_LEAF, b"innertree4"].as_ref(),
            b"ref2",
            Element::new_reference(ReferencePathType::UpstreamRootHeightReference(
                1,
                vec![b"innertree".to_vec(), b"key1".to_vec()],
            )),
            None,
            None,
        )
        .unwrap()
        .expect("should insert successfully");

        db.insert(
            [TEST_LEAF, b"innertree4"].as_ref(),
            b"ref3",
            Element::new_reference(ReferencePathType::UpstreamFromElementHeightReference(
                1,
                vec![b"innertree".to_vec(), b"key1".to_vec()],
            )),
            None,
            None,
        )
        .unwrap()
        .expect("should insert successfully");

        // Query all the elements in Test Leaf
        let mut query = Query::new();
        query.insert_all();
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], query);
        let result = db
            .query_item_value(&path_query, true, true, true, None)
            .unwrap()
            .expect("should query items");
        assert_eq!(result.0.len(), 5);
        assert_eq!(
            result.0,
            vec![
                b"value4".to_vec(),
                b"value5".to_vec(),
                b"value1".to_vec(),
                b"value1".to_vec(),
                b"value1".to_vec()
            ]
        );

        let proof = db
            .prove_query(&path_query)
            .unwrap()
            .expect("should generate proof");
        let (hash, result) =
            GroveDb::verify_query_raw(&proof, &path_query).expect("should verify proof");
        assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
        assert_eq!(result.len(), 5);
    }
}
