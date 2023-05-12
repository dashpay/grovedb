// MIT LICENSE
//
// Copyright (c) 2023 Dash Core Group
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

//! GroveDB subtree path manipulation library.

#![deny(missing_docs)]

mod subtree_path;
mod subtree_path_iter;
mod util;

pub use subtree_path::SubtreePath;
pub use subtree_path_iter::SubtreePathIter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::calculate_hash;

    #[test]
    fn test_hashes_are_equal() {
        let path_array = [
            b"one".to_vec(),
            b"two".to_vec(),
            b"three".to_vec(),
            b"four".to_vec(),
            b"five".to_vec(),
        ];
        let path_base_slice_vecs = SubtreePath::from(path_array.as_ref());
        let path_array = [
            b"one".as_ref(),
            b"two".as_ref(),
            b"three".as_ref(),
            b"four".as_ref(),
            b"five".as_ref(),
        ];
        let path_base_slice_slices = SubtreePath::from(path_array.as_ref());

        let path_array = [
            b"one".as_ref(),
            b"two".as_ref(),
            b"three".as_ref(),
            b"four".as_ref(),
            b"five".as_ref(),
            b"six".as_ref(),
        ];
        let path_base_slice_too_much = SubtreePath::from(path_array.as_ref());
        let path_base_unfinished = SubtreePath::from([b"one", b"two"].as_ref());
        let path_empty = SubtreePath::new();

        let path_derived_11 = path_empty.derive_child(b"one".as_ref());
        let path_derived_12 = path_derived_11.derive_child(b"two".as_ref());
        let path_derived_13 = path_derived_12.derive_child(b"three".as_ref());
        let path_derived_14 = path_derived_13.derive_child(b"four".to_vec());
        let path_derived_1 = path_derived_14.derive_child(b"five".as_ref());

        let (path_derived_2, _) = path_base_slice_too_much.derive_parent().unwrap();

        let path_derived_31 = path_base_unfinished.derive_child(b"three".to_vec());
        let path_derived_32 = path_derived_31.derive_child(b"four".as_ref());
        let path_derived_3 = path_derived_32.derive_child(b"five".as_ref());

        // Compare hashes
        let hash = calculate_hash(&path_base_slice_vecs);
        assert_eq!(calculate_hash(&path_base_slice_slices), hash);
        assert_eq!(calculate_hash(&path_derived_1), hash);
        assert_eq!(calculate_hash(&path_derived_2), hash);
        assert_eq!(calculate_hash(&path_derived_3), hash);
        // Check for equality
        assert_eq!(&path_base_slice_slices, &path_base_slice_vecs);
        assert_eq!(&path_derived_1, &path_base_slice_vecs);
        assert_eq!(&path_derived_2, &path_base_slice_vecs);
        assert_eq!(&path_derived_3, &path_base_slice_vecs);
    }

    #[test]
    fn test_is_root() {
        let path_empty = SubtreePath::<[u8; 0]>::from([].as_ref());
        assert!(path_empty.is_root());

        let path_derived = path_empty.derive_child(b"two".as_ref());
        assert!(path_derived.derive_parent().unwrap().0.is_root());

        let path_not_empty = SubtreePath::from([b"one"].as_ref());
        assert!(path_not_empty.derive_parent().unwrap().0.is_root());
    }
}
