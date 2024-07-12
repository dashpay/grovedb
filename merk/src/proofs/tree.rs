//! Tree proofs

#[cfg(feature = "full")]
use std::fmt::Debug;

#[cfg(any(feature = "full", feature = "verify"))]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostResult, CostsExt,
    OperationCost,
};

#[cfg(any(feature = "full", feature = "verify"))]
use super::{Node, Op};
#[cfg(any(feature = "full", feature = "verify"))]
use crate::tree::{combine_hash, kv_digest_to_kv_hash, kv_hash, node_hash, value_hash, NULL_HASH};
#[cfg(any(feature = "full", feature = "verify"))]
use crate::{error::Error, tree::CryptoHash};
#[cfg(feature = "full")]
use crate::{
    proofs::chunk::chunk::{LEFT, RIGHT},
    Link,
    TreeFeatureType::SummedMerkNode,
};

#[cfg(any(feature = "full", feature = "verify"))]
/// Contains a tree's child node and its hash. The hash can always be assumed to
/// be up-to-date.
#[derive(Debug)]
pub struct Child {
    /// Tree
    pub tree: Box<Tree>,
    /// Hash
    pub hash: CryptoHash,
}

impl Child {
    #[cfg(feature = "full")]
    pub fn as_link(&self) -> Link {
        let (key, sum) = match &self.tree.node {
            Node::KV(key, _) | Node::KVValueHash(key, ..) => (key.as_slice(), None),
            Node::KVValueHashFeatureType(key, _, _, feature_type) => {
                let sum_value = match feature_type {
                    SummedMerkNode(sum) => Some(*sum),
                    _ => None,
                };
                (key.as_slice(), sum_value)
            }
            // for the connection between the trunk and leaf chunks, we don't
            // have the child key so we must first write in an empty one. once
            // the leaf gets verified, we can write in this key to its parent
            _ => (&[] as &[u8], None),
        };

        Link::Reference {
            hash: self.hash,
            sum,
            child_heights: (
                self.tree.child_heights.0 as u8,
                self.tree.child_heights.1 as u8,
            ),
            key: key.to_vec(),
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
/// A binary tree data structure used to represent a select subset of a tree
/// when verifying Merkle proofs.
#[derive(Debug)]
pub struct Tree {
    /// Node
    pub node: Node,
    /// Left
    pub left: Option<Child>,
    /// Right
    pub right: Option<Child>,
    /// Height
    pub height: usize,
    /// Child Heights
    pub child_heights: (usize, usize),
}

#[cfg(any(feature = "full", feature = "verify"))]
impl From<Node> for Tree {
    /// Creates a childless tree with the target node as the `node` field.
    fn from(node: Node) -> Self {
        Self {
            node,
            left: None,
            right: None,
            height: 1,
            child_heights: (0, 0),
        }
    }
}

#[cfg(feature = "full")]
impl PartialEq for Tree {
    /// Checks equality for the root hashes of the two trees.
    fn eq(&self, other: &Self) -> bool {
        self.hash() == other.hash()
    }
}

impl Tree {
    /// Gets or computes the hash for this tree node.
    #[cfg(any(feature = "full", feature = "verify"))]
    pub fn hash(&self) -> CostContext<CryptoHash> {
        fn compute_hash(tree: &Tree, kv_hash: CryptoHash) -> CostContext<CryptoHash> {
            node_hash(&kv_hash, &tree.child_hash(true), &tree.child_hash(false))
        }

        match &self.node {
            Node::Hash(hash) => (*hash).wrap_with_cost(Default::default()),
            Node::KVHash(kv_hash) => compute_hash(self, *kv_hash),
            Node::KV(key, value) => kv_hash(key.as_slice(), value.as_slice())
                .flat_map(|kv_hash| compute_hash(self, kv_hash)),
            Node::KVValueHash(key, _, value_hash)
            | Node::KVValueHashFeatureType(key, _, value_hash, _) => {
                // TODO: add verification of the value
                kv_digest_to_kv_hash(key.as_slice(), value_hash)
                    .flat_map(|kv_hash| compute_hash(self, kv_hash))
            }
            Node::KVDigest(key, value_hash) => kv_digest_to_kv_hash(key, value_hash)
                .flat_map(|kv_hash| compute_hash(self, kv_hash)),
            Node::KVRefValueHash(key, referenced_value, node_value_hash) => {
                let mut cost = OperationCost::default();
                let referenced_value_hash =
                    value_hash(referenced_value.as_slice()).unwrap_add_cost(&mut cost);
                let combined_value_hash = combine_hash(node_value_hash, &referenced_value_hash)
                    .unwrap_add_cost(&mut cost);

                kv_digest_to_kv_hash(key.as_slice(), &combined_value_hash)
                    .flat_map(|kv_hash| compute_hash(self, kv_hash))
            }
        }
    }

    /// Creates an iterator that yields the in-order traversal of the nodes at
    /// the given depth.
    #[cfg(feature = "full")]
    pub fn layer(&self, depth: usize) -> LayerIter {
        LayerIter::new(self, depth)
    }

    /// Consumes the `Tree` and does an in-order traversal over all the nodes in
    /// the tree, calling `visit_node` for each.
    #[cfg(feature = "full")]
    pub fn visit_nodes<F: FnMut(Node)>(mut self, visit_node: &mut F) {
        if let Some(child) = self.left.take() {
            child.tree.visit_nodes(visit_node);
        }

        let maybe_right_child = self.right.take();
        visit_node(self.node);

        if let Some(child) = maybe_right_child {
            child.tree.visit_nodes(visit_node);
        }
    }

    /// Does an in-order traversal over references to all the nodes in the tree,
    /// calling `visit_node` for each.
    #[cfg(feature = "full")]
    pub fn visit_refs<F: FnMut(&Self) -> Result<(), Error>>(
        &self,
        visit_node: &mut F,
    ) -> Result<(), Error> {
        if let Some(child) = &self.left {
            child.tree.visit_refs(visit_node)?;
        }

        visit_node(self)?;

        if let Some(child) = &self.right {
            child.tree.visit_refs(visit_node)?;
        }
        Ok(())
    }

    #[cfg(feature = "full")]
    /// Does an in-order traversal over references to all the nodes in the tree,
    /// calling `visit_node` for each with the current traversal path.
    pub fn visit_refs_track_traversal_and_parent<
        F: FnMut(&Self, &mut Vec<bool>, Option<&[u8]>) -> Result<(), Error>,
    >(
        &self,
        base_traversal_instruction: &mut Vec<bool>,
        parent_key: Option<&[u8]>,
        visit_node: &mut F,
    ) -> Result<(), Error> {
        if let Some(child) = &self.left {
            base_traversal_instruction.push(LEFT);
            child.tree.visit_refs_track_traversal_and_parent(
                base_traversal_instruction,
                Some(self.key()),
                visit_node,
            )?;
            base_traversal_instruction.pop();
        }

        visit_node(self, base_traversal_instruction, parent_key)?;

        if let Some(child) = &self.right {
            base_traversal_instruction.push(RIGHT);
            child.tree.visit_refs_track_traversal_and_parent(
                base_traversal_instruction,
                Some(self.key()),
                visit_node,
            )?;
            base_traversal_instruction.pop();
        }

        Ok(())
    }

    /// Returns an immutable reference to the child on the given side, if any.
    #[cfg(any(feature = "full", feature = "verify"))]
    pub const fn child(&self, left: bool) -> Option<&Child> {
        if left {
            self.left.as_ref()
        } else {
            self.right.as_ref()
        }
    }

    /// Returns a mutable reference to the child on the given side, if any.
    #[cfg(any(feature = "full", feature = "verify"))]
    pub(crate) fn child_mut(&mut self, left: bool) -> &mut Option<Child> {
        if left {
            &mut self.left
        } else {
            &mut self.right
        }
    }

    /// Attaches the child to the `Tree`'s given side. Panics if there is
    /// already a child attached to this side.
    #[cfg(any(feature = "full", feature = "verify"))]
    pub(crate) fn attach(&mut self, left: bool, child: Self) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if self.child(left).is_some() {
            return Err(Error::CorruptedCodeExecution(
                "Tried to attach to left child, but it is already Some",
            ))
            .wrap_with_cost(cost);
        }

        self.height = self.height.max(child.height + 1);

        // update child height
        if left {
            self.child_heights.0 = child.height;
        } else {
            self.child_heights.1 = child.height;
        }

        let hash = child.hash().unwrap_add_cost(&mut cost);
        let tree = Box::new(child);
        *self.child_mut(left) = Some(Child { tree, hash });

        Ok(()).wrap_with_cost(cost)
    }

    /// Returns the already-computed hash for this tree node's child on the
    /// given side, if any. If there is no child, returns the null hash
    /// (zero-filled).
    #[cfg(any(feature = "full", feature = "verify"))]
    #[inline]
    const fn child_hash(&self, left: bool) -> CryptoHash {
        match self.child(left) {
            Some(c) => c.hash,
            _ => NULL_HASH,
        }
    }

    /// Consumes the tree node, calculates its hash, and returns a `Node::Hash`
    /// variant.
    #[cfg(any(feature = "full", feature = "verify"))]
    fn into_hash(self) -> CostContext<Self> {
        self.hash().map(|hash| Node::Hash(hash).into())
    }

    #[cfg(feature = "full")]
    pub(crate) fn key(&self) -> &[u8] {
        match self.node {
            Node::KV(ref key, _)
            | Node::KVValueHash(ref key, ..)
            | Node::KVRefValueHash(ref key, ..)
            | Node::KVValueHashFeatureType(ref key, ..) => key,
            _ => panic!("Expected node to be type KV"),
        }
    }

    #[cfg(feature = "full")]
    pub(crate) fn sum(&self) -> Option<i64> {
        match self.node {
            Node::KVValueHashFeatureType(.., feature_type) => match feature_type {
                SummedMerkNode(sum) => Some(sum),
                _ => None,
            },
            _ => panic!("Expected node to be type KVValueHashFeatureType"),
        }
    }
}

#[cfg(feature = "full")]
/// `LayerIter` iterates over the nodes in a `Tree` at a given depth. Nodes are
/// visited in order.
pub struct LayerIter<'a> {
    stack: Vec<(&'a Tree, usize)>,
    depth: usize,
}

#[cfg(feature = "full")]
impl<'a> LayerIter<'a> {
    /// Creates a new `LayerIter` that iterates over `tree` at the given depth.
    fn new(tree: &'a Tree, depth: usize) -> Self {
        let mut iter = LayerIter {
            stack: Vec::with_capacity(depth),
            depth,
        };

        iter.stack.push((tree, 0));
        iter
    }
}

#[cfg(feature = "full")]
impl<'a> Iterator for LayerIter<'a> {
    type Item = &'a Tree;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((item, item_depth)) = self.stack.pop() {
            if item_depth != self.depth {
                if let Some(right_child) = item.child(false) {
                    self.stack.push((&right_child.tree, item_depth + 1))
                }
                if let Some(left_child) = item.child(true) {
                    self.stack.push((&left_child.tree, item_depth + 1))
                }
            } else {
                return Some(item);
            }
        }

        None
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Executes a proof by stepping through its operators, modifying the
/// verification stack as it goes. The resulting stack item is returned.
///
/// If the `collapse` option is set to `true`, nodes will be hashed and pruned
/// from memory during execution. This results in the minimum amount of memory
/// usage, and the returned `Tree` will only contain a single node of type
/// `Node::Hash`. If `false`, the returned `Tree` will contain the entire
/// subtree contained in the proof.
///
/// `visit_node` will be called once for every push operation in the proof, in
/// key-order. If `visit_node` returns an `Err` result, it will halt the
/// execution and `execute` will return the error.
pub(crate) fn execute<I, F>(ops: I, collapse: bool, mut visit_node: F) -> CostResult<Tree, Error>
where
    I: IntoIterator<Item = Result<Op, Error>>,
    F: FnMut(&Node) -> Result<(), Error>,
{
    let mut cost = OperationCost::default();

    let mut stack: Vec<Tree> = Vec::with_capacity(32);
    let mut maybe_last_key = None;

    fn try_pop(stack: &mut Vec<Tree>) -> Result<Tree, Error> {
        stack
            .pop()
            .ok_or_else(|| Error::InvalidProofError("Stack underflow".to_string()))
    }

    for op in ops {
        match cost_return_on_error_no_add!(&cost, op) {
            Op::Parent => {
                let (mut parent, child) = (
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                );
                cost_return_on_error!(
                    &mut cost,
                    parent.attach(
                        true,
                        if collapse {
                            child.into_hash().unwrap_add_cost(&mut cost)
                        } else {
                            child
                        },
                    )
                );
                stack.push(parent);
            }
            Op::Child => {
                let (child, mut parent) = (
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                );
                cost_return_on_error!(
                    &mut cost,
                    parent.attach(
                        false,
                        if collapse {
                            child.into_hash().unwrap_add_cost(&mut cost)
                        } else {
                            child
                        }
                    )
                );
                stack.push(parent);
            }
            Op::ParentInverted => {
                let (mut parent, child) = (
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                );
                cost_return_on_error!(
                    &mut cost,
                    parent.attach(
                        false,
                        if collapse {
                            child.into_hash().unwrap_add_cost(&mut cost)
                        } else {
                            child
                        },
                    )
                );
                stack.push(parent);
            }
            Op::ChildInverted => {
                let (child, mut parent) = (
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                    cost_return_on_error_no_add!(&cost, try_pop(&mut stack)),
                );
                cost_return_on_error!(
                    &mut cost,
                    parent.attach(
                        true,
                        if collapse {
                            child.into_hash().unwrap_add_cost(&mut cost)
                        } else {
                            child
                        },
                    )
                );
                stack.push(parent);
            }
            Op::Push(node) => {
                if let Node::KV(key, _)
                | Node::KVValueHashFeatureType(key, ..)
                | Node::KVRefValueHash(key, ..) = &node
                {
                    // keys should always increase
                    if let Some(last_key) = &maybe_last_key {
                        if key <= last_key {
                            return Err(Error::InvalidProofError(
                                "Incorrect key ordering".to_string(),
                            ))
                            .wrap_with_cost(cost);
                        }
                    }

                    maybe_last_key = Some(key.clone());
                }

                cost_return_on_error_no_add!(&cost, visit_node(&node));

                let tree: Tree = node.into();
                stack.push(tree);
            }
            Op::PushInverted(node) => {
                if let Node::KV(key, _)
                | Node::KVValueHashFeatureType(key, ..)
                | Node::KVRefValueHash(key, ..) = &node
                {
                    // keys should always decrease
                    if let Some(last_key) = &maybe_last_key {
                        if key >= last_key {
                            return Err(Error::InvalidProofError(
                                "Incorrect key ordering inverted".to_string(),
                            ))
                            .wrap_with_cost(cost);
                        }
                    }

                    maybe_last_key = Some(key.clone());
                }

                cost_return_on_error_no_add!(&cost, visit_node(&node));

                let tree: Tree = node.into();
                stack.push(tree);
            }
        }
    }

    if stack.len() != 1 {
        return Err(Error::InvalidProofError(
            "Expected proof to result in exactly one stack item".to_string(),
        ))
        .wrap_with_cost(cost);
    }

    let tree = stack.pop().unwrap();

    if tree.child_heights.0.max(tree.child_heights.1)
        - tree.child_heights.0.min(tree.child_heights.1)
        > 1
    {
        return Err(Error::InvalidProofError(
            "Expected proof to result in a valid avl tree".to_string(),
        ))
        .wrap_with_cost(cost);
    }

    Ok(tree).wrap_with_cost(cost)
}

#[cfg(feature = "full")]
#[cfg(test)]
mod test {
    use super::{super::*, Tree as ProofTree, *};

    fn make_7_node_prooftree() -> ProofTree {
        let make_node = |i| -> super::super::tree::Tree { Node::KV(vec![i], vec![]).into() };

        let mut tree = make_node(3);
        let mut left = make_node(1);
        left.attach(true, make_node(0)).unwrap().unwrap();
        left.attach(false, make_node(2)).unwrap().unwrap();
        let mut right = make_node(5);
        right.attach(true, make_node(4)).unwrap().unwrap();
        right.attach(false, make_node(6)).unwrap().unwrap();
        tree.attach(true, left).unwrap().unwrap();
        tree.attach(false, right).unwrap().unwrap();

        tree
    }

    #[test]
    fn height_counting() {
        fn recurse(tree: &super::Tree, expected_height: usize) {
            assert_eq!(tree.height, expected_height);
            if let Some(l) = tree.left.as_ref() {
                recurse(&l.tree, expected_height - 1);
            }
            if let Some(r) = tree.right.as_ref() {
                recurse(&r.tree, expected_height - 1);
            }
        }

        let tree = make_7_node_prooftree();
        recurse(&tree, 3);
    }

    #[test]
    fn layer_iter() {
        let tree = make_7_node_prooftree();

        let assert_node = |node: &Tree, i| match node.node {
            Node::KV(ref key, _) => assert_eq!(key[0], i),
            _ => unreachable!(),
        };

        let mut iter = tree.layer(0);
        assert_node(iter.next().unwrap(), 3);
        assert!(iter.next().is_none());

        let mut iter = tree.layer(1);
        assert_node(iter.next().unwrap(), 1);
        assert_node(iter.next().unwrap(), 5);
        assert!(iter.next().is_none());

        let mut iter = tree.layer(2);
        assert_node(iter.next().unwrap(), 0);
        assert_node(iter.next().unwrap(), 2);
        assert_node(iter.next().unwrap(), 4);
        assert_node(iter.next().unwrap(), 6);
        assert!(iter.next().is_none());
    }

    #[test]
    fn visit_nodes() {
        let tree = make_7_node_prooftree();

        let assert_node = |node: Node, i| match node {
            Node::KV(ref key, _) => assert_eq!(key[0], i),
            _ => unreachable!(),
        };

        let mut visited = vec![];
        tree.visit_nodes(&mut |node| visited.push(node));

        let mut iter = visited.into_iter();
        for i in 0..7 {
            assert_node(iter.next().unwrap(), i);
        }
        assert!(iter.next().is_none());
    }

    #[test]
    fn execute_non_avl_tree() {
        let non_avl_tree_proof = vec![
            Op::Push(Node::KV(vec![1], vec![1])),
            Op::Push(Node::KV(vec![2], vec![2])),
            Op::Parent,
            Op::Push(Node::KV(vec![3], vec![3])),
            Op::Parent,
        ];
        let execution_result =
            execute(non_avl_tree_proof.into_iter().map(Ok), false, |_| Ok(())).unwrap();
        assert!(execution_result.is_err());
    }

    #[test]
    fn child_to_link() {
        let basic_merk_tree = vec![
            Op::Push(Node::KV(vec![1], vec![1])),
            Op::Push(Node::KV(vec![2], vec![2])),
            Op::Parent,
            Op::Push(Node::KV(vec![3], vec![3])),
            Op::Child,
        ];
        let tree = execute(basic_merk_tree.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .unwrap();

        let left_link = tree.left.as_ref().unwrap().as_link();
        let right_link = tree.right.as_ref().unwrap().as_link();

        assert_eq!(
            left_link,
            Link::Reference {
                hash: tree.left.as_ref().map(|node| node.hash).unwrap(),
                sum: None,
                child_heights: (0, 0),
                key: vec![1]
            }
        );

        assert_eq!(
            right_link,
            Link::Reference {
                hash: tree.right.as_ref().map(|node| node.hash).unwrap(),
                sum: None,
                child_heights: (0, 0),
                key: vec![3]
            }
        );

        let sum_merk_tree = vec![
            Op::Push(Node::KVValueHashFeatureType(
                vec![1],
                vec![1],
                [0; 32],
                SummedMerkNode(3),
            )),
            Op::Push(Node::KVValueHashFeatureType(
                vec![2],
                vec![2],
                [0; 32],
                SummedMerkNode(1),
            )),
            Op::Parent,
            Op::Push(Node::KVValueHashFeatureType(
                vec![3],
                vec![3],
                [0; 32],
                SummedMerkNode(1),
            )),
            Op::Child,
        ];
        let tree = execute(sum_merk_tree.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .unwrap();

        let left_link = tree.left.as_ref().unwrap().as_link();
        let right_link = tree.right.as_ref().unwrap().as_link();

        assert_eq!(
            left_link,
            Link::Reference {
                hash: tree.left.as_ref().map(|node| node.hash).unwrap(),
                sum: Some(3),
                child_heights: (0, 0),
                key: vec![1]
            }
        );

        assert_eq!(
            right_link,
            Link::Reference {
                hash: tree.right.as_ref().map(|node| node.hash).unwrap(),
                sum: Some(1),
                child_heights: (0, 0),
                key: vec![3]
            }
        );
    }
}
