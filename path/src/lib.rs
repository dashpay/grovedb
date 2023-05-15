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
mod subtree_path_builder;
mod subtree_path_iter;
mod util;

pub use subtree_path::SubtreePath;
pub use subtree_path_builder::SubtreePathBuilder;
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
        let path_empty = SubtreePathBuilder::new();

        let path_derived_11 = path_empty.derive_owned_with_child(b"one".as_ref());
        let path_derived_12 = path_derived_11.derive_owned_with_child(b"two".as_ref());
        let path_derived_13 = path_derived_12.derive_owned_with_child(b"three".as_ref());
        let path_derived_14 = path_derived_13.derive_owned_with_child(b"four".to_vec());
        let path_derived_1 = path_derived_14.derive_owned_with_child(b"five".as_ref());

        let (path_derived_2, _) = path_base_slice_too_much.derive_parent().unwrap();

        let path_derived_31 = path_base_unfinished.derive_owned_with_child(b"three".to_vec());
        let path_derived_32 = path_derived_31.derive_owned_with_child(b"four".as_ref());
        let path_derived_3 = path_derived_32.derive_owned_with_child(b"five".as_ref());

        // Compare hashes
        let hash = calculate_hash(&path_base_slice_vecs);
        assert_eq!(calculate_hash(&path_base_slice_slices), hash);
        assert_eq!(calculate_hash(&path_derived_1), hash);
        assert_eq!(calculate_hash(&path_derived_2), hash);
        assert_eq!(calculate_hash(&path_derived_3), hash);
        // Check for equality
        let reference = path_base_slice_vecs;
        assert_eq!(&path_base_slice_slices, &reference);
        assert_eq!(&path_derived_1, &reference);
        assert_eq!(&path_derived_2, &reference);
        assert_eq!(&path_derived_3, &reference);
    }

    #[test]
    fn test_is_root() {
        let path_empty = SubtreePathBuilder::new();
        assert!(path_empty.is_root());

        let path_derived = path_empty.derive_owned_with_child(b"two".as_ref());
        assert!(path_derived.derive_parent().unwrap().0.is_root());

        let path_not_empty = SubtreePath::from([b"one"].as_ref());
        assert!(path_not_empty.derive_parent().unwrap().0.is_root());
    }

    #[test]
    fn test_complex_derivation() {
        // Append only operations:
        let base = SubtreePath::from([b"one", b"two"].as_ref());
        let with_child_1 = base.derive_owned_with_child(b"three".to_vec());
        let mut with_child_inplace = with_child_1.derive_owned_with_child(b"four");
        with_child_inplace.push_segment(b"five");
        with_child_inplace.push_segment(b"six");
        with_child_inplace.push_segment(b"seven");
        with_child_inplace.push_segment(b"eight");

        // `with_child_inplace` should be like (substituted for digits for short):
        // [1, 2] -> 3 -> [4, 5, 6, 7, 8]
        assert_eq!(
            with_child_inplace.reverse_iter().fold(0, |acc, _| acc + 1),
            8
        );

        // Now go to ancestors (note that intermediate derivations are dropped and
        // is still compiles, as [SubtreePathRef]s are intended for):
        let points_five = with_child_inplace
            .derive_parent()
            .unwrap()
            .0
            .derive_parent()
            .unwrap()
            .0
            .derive_parent()
            .unwrap()
            .0;

        let five_reference_slice = [
            b"one".as_ref(),
            b"two".as_ref(),
            b"three".as_ref(),
            b"four".as_ref(),
            b"five".as_ref(),
        ];
        let five_reference: SubtreePath<_> = five_reference_slice.as_ref().into();

        assert!(points_five
            .clone()
            .into_reverse_iter()
            .eq(five_reference.into_reverse_iter()));

        // And add a couple of other derivations
        let after_five_1 = points_five.derive_owned_with_child(b"four");
        let after_five_2 = after_five_1.derive_owned_with_child(b"twenty");
        let mut after_five_3 = after_five_2.derive_owned();
        after_five_3.push_segment(b"thirteen");
        after_five_3.push_segment(b"thirtyseven");

        // `after_five_3` should be like this:
        // [1, 2] -> 3 -> [4, 5, 6, 7, 8]
        //                    ^-> 4 -> 20 -> [13, 37]

        // Verify it behaves as a basic [SubtreePathRef] made from a slice.
        let reference_slice = [
            b"one".as_ref(),
            b"two".as_ref(),
            b"three".as_ref(),
            b"four".as_ref(),
            b"five".as_ref(),
            b"four".as_ref(),
            b"twenty".as_ref(),
            b"thirteen".as_ref(),
            b"thirtyseven".as_ref(),
        ];
        let reference: SubtreePath<_> = reference_slice.as_ref().into();

        assert_eq!(after_five_3.to_vec(), reference.to_vec());
        assert!(after_five_3
            .reverse_iter()
            .eq(reference.clone().into_reverse_iter()));
        assert_eq!(calculate_hash(&after_five_3), calculate_hash(&reference));
        assert_eq!(after_five_3, reference);
    }
}
