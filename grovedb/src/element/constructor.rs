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

//! Constructor
//! Functions for setting an element's type

#[cfg(feature = "full")]
use crate::{
    element::{MaxReferenceHop, SumValue},
    reference_path::ReferencePathType,
    Element, ElementFlags,
};

impl Element {
    #[cfg(feature = "full")]
    /// Set element to default empty tree without flags
    // TODO: improve API to avoid creation of Tree elements with uncertain state
    pub fn empty_tree() -> Self {
        Element::new_tree(Default::default())
    }

    #[cfg(feature = "full")]
    /// Set element to default empty tree with flags
    pub fn empty_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_tree_with_flags(Default::default(), flags)
    }

    #[cfg(feature = "full")]
    /// Set element to default empty sum tree without flags
    pub fn empty_sum_tree() -> Self {
        Element::new_sum_tree(Default::default())
    }

    #[cfg(feature = "full")]
    /// Set element to default empty sum tree with flags
    pub fn empty_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_sum_tree_with_flags(Default::default(), flags)
    }

    #[cfg(feature = "full")]
    /// Set element to an item without flags
    pub fn new_item(item_value: Vec<u8>) -> Self {
        Element::Item(item_value, None)
    }

    #[cfg(feature = "full")]
    /// Set element to an item with flags
    pub fn new_item_with_flags(item_value: Vec<u8>, flags: Option<ElementFlags>) -> Self {
        Element::Item(item_value, flags)
    }

    #[cfg(feature = "full")]
    /// Set element to a sum item without flags
    pub fn new_sum_item(value: i64) -> Self {
        Element::SumItem(value, None)
    }

    #[cfg(feature = "full")]
    /// Set element to a sum item with flags
    pub fn new_sum_item_with_flags(value: i64, flags: Option<ElementFlags>) -> Self {
        Element::SumItem(value, flags)
    }

    #[cfg(feature = "full")]
    /// Set element to a reference without flags
    pub fn new_reference(reference_path: ReferencePathType) -> Self {
        Element::Reference(reference_path, None, None)
    }

    #[cfg(feature = "full")]
    /// Set element to a reference with flags
    pub fn new_reference_with_flags(
        reference_path: ReferencePathType,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Reference(reference_path, None, flags)
    }

    #[cfg(feature = "full")]
    /// Set element to a reference with hops, no flags
    pub fn new_reference_with_hops(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
    ) -> Self {
        Element::Reference(reference_path, max_reference_hop, None)
    }

    #[cfg(feature = "full")]
    /// Set element to a reference with max hops and flags
    pub fn new_reference_with_max_hops_and_flags(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Reference(reference_path, max_reference_hop, flags)
    }

    #[cfg(feature = "full")]
    /// Set element to a tree without flags
    pub fn new_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::Tree(maybe_root_key, None)
    }

    #[cfg(feature = "full")]
    /// Set element to a tree with flags
    pub fn new_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Tree(maybe_root_key, flags)
    }

    #[cfg(feature = "full")]
    /// Set element to a sum tree without flags
    pub fn new_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::SumTree(maybe_root_key, 0, None)
    }

    #[cfg(feature = "full")]
    /// Set element to a sum tree with flags
    pub fn new_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::SumTree(maybe_root_key, 0, flags)
    }

    #[cfg(feature = "full")]
    /// Set element to a sum tree with flags and sum value
    pub fn new_sum_tree_with_flags_and_sum_value(
        maybe_root_key: Option<Vec<u8>>,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::SumTree(maybe_root_key, sum_value, flags)
    }
}
