//! Merk reference walker

#[cfg(feature = "minimal")]
use std::cmp::Ordering;

#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use super::{
    super::{Link, TreeNode},
    Fetch,
};
use crate::tree::kv::ValueDefinedCostType;
#[cfg(feature = "minimal")]
use crate::Error;

#[cfg(feature = "minimal")]
/// Allows read-only traversal of a `Tree`, fetching from the given source when
/// traversing to a pruned node. The fetched nodes are then retained in memory
/// until they (possibly) get pruned on the next commit.
///
/// Only finalized trees may be walked (trees which have had `commit` called
/// since the last update).
///
/// A walk that reads the values of few of its nodes, as the walk of a proof
/// does, says so with [`Self::values_on_demand`]. Its source may then leave
/// the value of a fetched node out ([`Fetch::fetch_without_value`]), and
/// the walk has it read where it asks ([`Self::load_value`]). A node that
/// came without its value is not retained in the tree: the walker of that
/// node owns it and what the walk fetches below it, and drops both when
/// the walk leaves the node.
pub struct RefWalker<'a, S>
where
    S: Fetch + Sized + Clone,
{
    tree: Walked<'a>,
    source: S,
    values_on_demand: bool,
}

#[cfg(feature = "minimal")]
/// The node a [`RefWalker`] stands on.
enum Walked<'a> {
    /// A node of the tree the walk was started on. A node fetched below it
    /// with its value is retained in that tree.
    Retained(&'a mut TreeNode),
    /// A node that was fetched without its value. Its walker owns it, and
    /// with it every node the walk fetches below it.
    Owned(TreeNode),
    /// A node below one that a walker owns.
    Lent(&'a mut TreeNode),
}

#[cfg(feature = "minimal")]
impl Walked<'_> {
    fn node(&self) -> &TreeNode {
        match self {
            Walked::Retained(node) | Walked::Lent(node) => node,
            Walked::Owned(node) => node,
        }
    }

    fn node_mut(&mut self) -> &mut TreeNode {
        match self {
            Walked::Retained(node) | Walked::Lent(node) => node,
            Walked::Owned(node) => node,
        }
    }
}

#[cfg(feature = "minimal")]
impl<'a, S> RefWalker<'a, S>
where
    S: Fetch + Sized + Clone,
{
    /// Creates a `RefWalker` with the given tree and source.
    pub fn new(tree: &'a mut TreeNode, source: S) -> Self {
        // TODO: check if tree has modified links, panic if so
        RefWalker {
            tree: Walked::Retained(tree),
            source,
            values_on_demand: false,
        }
    }

    /// Gets an immutable reference to the `Tree` wrapped by this `RefWalker`.
    pub fn tree(&self) -> &TreeNode {
        self.tree.node()
    }

    /// Makes this a walk that reads the value of a node only where it asks
    /// for it: from here on, [`Self::load_value`] comes before every read
    /// of the value of a node the walk came to, and a walk to a pruned
    /// child that is given no value-defined cost function fetches it with
    /// [`Fetch::fetch_without_value`]. The walkers of the children are
    /// walks of the same kind.
    ///
    /// On a source that fetches whole nodes nothing changes: the nodes are
    /// retained in the tree as they are without this.
    pub fn values_on_demand(&mut self) {
        self.values_on_demand = true;
    }

    /// Makes sure that the value of the walker's node is in memory. It is
    /// there already unless the node was fetched without it, and is then
    /// read through the source ([`Fetch::fetch_value`]).
    pub fn load_value(&mut self) -> CostResult<(), Error> {
        if !self.tree.node().value_absent() {
            return Ok(()).wrap_with_cost(Default::default());
        }
        self.source.fetch_value(self.tree.node_mut())
    }

    /// Traverses to the child on the given side (if any), fetching from the
    /// source if pruned. When fetching, the link is upgraded from
    /// `Link::Reference` to `Link::Loaded`, unless the node came without
    /// its value ([`Self::values_on_demand`]): the walker that is returned
    /// then owns it, and the link stays as it is.
    pub fn walk<V>(
        &mut self,
        left: bool,
        value_defined_cost_fn: Option<&V>,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<RefWalker<'_, S>>, Error>
    where
        V: Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
    {
        let link = match self.tree.node().link(left) {
            None => return Ok(None).wrap_with_cost(Default::default()),
            Some(link) => link,
        };

        let mut cost = OperationCost::default();
        let values_on_demand = self.values_on_demand;
        let retained = matches!(self.tree, Walked::Retained(_));
        match link {
            // Below a node the walk owns nothing is retained, so no
            // value-defined cost is asked for there.
            Link::Reference { .. }
                if values_on_demand && (value_defined_cost_fn.is_none() || !retained) =>
            {
                let child = cost_return_on_error!(
                    &mut cost,
                    self.source.fetch_without_value(link, grove_version)
                );
                if retained && child.value_absent() {
                    let child = RefWalker {
                        tree: Walked::Owned(child),
                        source: self.source.clone(),
                        values_on_demand,
                    };
                    return Ok(Some(child)).wrap_with_cost(cost);
                }
                cost_return_on_error_no_add!(
                    cost,
                    self.tree.node_mut().put_fetched_child(left, child)
                );
            }
            Link::Reference { .. } => {
                let load_res = self
                    .tree
                    .node_mut()
                    .load(left, &self.source, value_defined_cost_fn, grove_version)
                    .unwrap_add_cost(&mut cost);
                if let Err(e) = load_res {
                    return Err(e).wrap_with_cost(cost);
                }
            }
            Link::Modified { .. } => {
                return Err(Error::CorruptedState("cannot traverse Link::Modified"))
                    .wrap_with_cost(cost)
            }
            Link::Uncommitted { .. } | Link::Loaded { .. } => {}
        }

        let child = self.tree.node_mut().child_mut(left).unwrap();
        let child = RefWalker {
            tree: match retained {
                true => Walked::Retained(child),
                false => Walked::Lent(child),
            },
            source: self.source.clone(),
            values_on_demand,
        };
        Ok(Some(child)).wrap_with_cost(cost)
    }

    /// Finds the traversal path (sequence of left/right) to reach a specific
    /// key.
    ///
    /// Traverses the tree from the current position, comparing the target key
    /// with each node's key and recording the path taken until the key is
    /// found.
    ///
    /// # Arguments
    /// * `target_key` - The key to find
    /// * `grove_version` - The grove version for compatibility
    ///
    /// # Returns
    /// * `Ok(Some(path))` - If the key was found, with the traversal path
    ///   (true=left, false=right)
    /// * `Ok(None)` - If the key was not found
    pub fn find_key_path(
        &mut self,
        target_key: &[u8],
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<bool>>, Error> {
        let mut cost = OperationCost::default();
        let mut path = Vec::new();

        self.find_key_path_internal(target_key, grove_version, &mut path, &mut cost)
            .map(|found| if found { Some(path) } else { None })
            .wrap_with_cost(cost)
    }

    fn find_key_path_internal(
        &mut self,
        target_key: &[u8],
        grove_version: &GroveVersion,
        path: &mut Vec<bool>,
        cost: &mut OperationCost,
    ) -> Result<bool, Error> {
        let current_key = self.tree.node().key();
        match target_key.cmp(current_key) {
            Ordering::Equal => {
                // Found the key
                Ok(true)
            }
            Ordering::Less => {
                // Target is smaller, go left
                let maybe_left = self
                    .walk(
                        true,
                        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                        grove_version,
                    )
                    .unwrap_add_cost(cost);

                match maybe_left {
                    Ok(Some(mut left_walker)) => {
                        path.push(true);
                        left_walker.find_key_path_internal(target_key, grove_version, path, cost)
                    }
                    Ok(None) => Ok(false),
                    Err(e) => Err(e),
                }
            }
            Ordering::Greater => {
                // Target is larger, go right
                let maybe_right = self
                    .walk(
                        false,
                        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                        grove_version,
                    )
                    .unwrap_add_cost(cost);

                match maybe_right {
                    Ok(Some(mut right_walker)) => {
                        path.push(false);
                        right_walker.find_key_path_internal(target_key, grove_version, path, cost)
                    }
                    Ok(None) => Ok(false),
                    Err(e) => Err(e),
                }
            }
        }
    }
}

#[cfg(all(test, feature = "minimal"))]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

    use grovedb_costs::{CostResult, CostsExt, OperationCost};
    use grovedb_version::version::GroveVersion;

    use super::{Fetch, Link, RefWalker, TreeNode};
    use crate::{
        proofs::{encode_into, query::QueryItem},
        test_utils::{apply_memonly, seq_key},
        tree::{kv::ValueDefinedCostType, Op, PanicSource},
        Error,
        TreeFeatureType::BasicMerkNode,
    };

    const NODES: u64 = 31;
    const NO_COST_FN: Option<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;

    fn value_of(n: u64) -> Vec<u8> {
        format!("the value of {n}").into_bytes()
    }

    /// What a walk asked of its source.
    #[derive(Debug, PartialEq, Clone)]
    enum Asked {
        Whole(Vec<u8>),
        WithoutValue(Vec<u8>),
        Value(Vec<u8>),
    }

    /// The nodes of a committed tree, each as it is stored: with its links
    /// pruned. It hands them out whole, or without their values where a
    /// walk allows that, and keeps what it was asked.
    #[derive(Clone)]
    struct Stored {
        nodes: Rc<BTreeMap<Vec<u8>, Vec<u8>>>,
        leaves_values_out: bool,
        asked: Rc<RefCell<Vec<Asked>>>,
    }

    impl Stored {
        fn node(&self, key: &[u8]) -> TreeNode {
            TreeNode::decode_raw(
                &self.nodes[key],
                key.to_vec(),
                NO_COST_FN.copied(),
                GroveVersion::latest(),
            )
            .unwrap()
        }

        fn asked(&self) -> Vec<Asked> {
            self.asked.take()
        }
    }

    fn one_seek() -> OperationCost {
        OperationCost {
            seek_count: 1,
            ..Default::default()
        }
    }

    impl Fetch for Stored {
        fn fetch(
            &self,
            link: &Link,
            _value_defined_cost_fn: Option<
                &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
            >,
            _grove_version: &GroveVersion,
        ) -> CostResult<TreeNode, Error> {
            let mut asked = self.asked.borrow_mut();
            asked.push(Asked::Whole(link.key().to_vec()));
            Ok(self.node(link.key())).wrap_with_cost(one_seek())
        }

        fn fetch_without_value(
            &self,
            link: &Link,
            grove_version: &GroveVersion,
        ) -> CostResult<TreeNode, Error> {
            if !self.leaves_values_out {
                return self.fetch(link, NO_COST_FN, grove_version);
            }
            let mut asked = self.asked.borrow_mut();
            asked.push(Asked::WithoutValue(link.key().to_vec()));
            let node = self.node(link.key());
            let without = TreeNode::new_without_value(
                link.key().to_vec(),
                *node.kv_hash(),
                *node.value_hash(),
                node.feature_type(),
                node.link(true).cloned(),
                node.link(false).cloned(),
            );
            Ok(without).wrap_with_cost(one_seek())
        }

        fn fetch_value(&self, node: &mut TreeNode) -> CostResult<(), Error> {
            let mut asked = self.asked.borrow_mut();
            asked.push(Asked::Value(node.key().to_vec()));
            let value = self.node(node.key()).value_as_slice().to_vec();
            node.put_read_value(value);
            Ok(()).wrap_with_cost(Default::default())
        }
    }

    /// A tree of [`NODES`] nodes as a store holds it, and its root as a
    /// Merk holds it after it was opened: with both children pruned.
    fn stored(leaves_values_out: bool) -> (TreeNode, Stored) {
        let grove_version = GroveVersion::latest();
        let first = TreeNode::new(seq_key(0).to_vec(), value_of(0), None, BasicMerkNode).unwrap();
        let batch: Vec<_> = (1..NODES)
            .map(|n| (seq_key(n).to_vec(), Op::Put(value_of(n), BasicMerkNode)))
            .collect();
        let tree = apply_memonly(first, &batch, grove_version);
        let mut nodes = BTreeMap::new();
        let mut ahead = vec![&tree];
        while let Some(node) = ahead.pop() {
            nodes.insert(node.key().to_vec(), node.encode());
            ahead.extend(
                [true, false]
                    .into_iter()
                    .filter_map(|left| node.child(left)),
            );
        }
        assert_eq!(nodes.len() as u64, NODES);
        let source = Stored {
            nodes: Rc::new(nodes),
            leaves_values_out,
            asked: Rc::default(),
        };
        (source.node(tree.key()), source)
    }

    /// The keys from `walker`'s node down a side to a leaf.
    fn descend<S: Fetch + Clone>(walker: &mut RefWalker<'_, S>, left: bool) -> Vec<Vec<u8>> {
        let mut keys = vec![walker.tree().key().to_vec()];
        let child = walker.walk(left, NO_COST_FN, GroveVersion::latest());
        if let Some(mut child) = child.unwrap().unwrap() {
            keys.extend(descend(&mut child, left));
        }
        keys
    }

    fn proof_of(root: &mut TreeNode, source: &Stored, key: u64) -> (Vec<u8>, OperationCost) {
        let query = [QueryItem::Key(seq_key(key).to_vec())];
        let proved = RefWalker::new(root, source.clone()).create_proof(
            &query,
            None,
            true,
            GroveVersion::latest(),
        );
        let mut bytes = Vec::new();
        encode_into(proved.value.unwrap().0.iter(), &mut bytes);
        (bytes, proved.cost)
    }

    #[test]
    fn a_walk_keeps_the_whole_nodes_it_fetches_in_the_tree() {
        for values_on_demand in [false, true] {
            let (mut root, source) = stored(false);
            let mut walker = RefWalker::new(&mut root, source.clone());
            if values_on_demand {
                walker.values_on_demand();
            }
            let path = descend(&mut walker, true);
            assert!(path.len() > 3);
            // A value that is there is not asked for.
            walker.load_value().unwrap().unwrap();
            let fetched: Vec<Asked> = path[1..].iter().cloned().map(Asked::Whole).collect();
            assert_eq!(source.asked(), fetched);

            // The nodes are in the tree, and a second walk fetches none.
            let mut node = &root;
            for key in &path[1..] {
                node = node.child(true).expect("the child is kept");
                assert_eq!(node.key(), key);
                assert!(!node.value_absent());
            }
            assert_eq!(
                descend(&mut RefWalker::new(&mut root, source.clone()), true),
                path
            );
            assert_eq!(source.asked(), []);
        }
    }

    #[test]
    fn a_walk_that_reads_values_on_demand_keeps_no_node_that_came_without_its_value() {
        let (mut root, source) = stored(true);
        let mut walker = RefWalker::new(&mut root, source.clone());
        walker.values_on_demand();

        let child = walker.walk(true, NO_COST_FN, GroveVersion::latest());
        let mut child = child.unwrap().unwrap().expect("a child");
        let child_key = child.tree().key().to_vec();
        assert!(child.tree().value_absent());
        assert_eq!(source.asked(), [Asked::WithoutValue(child_key.clone())]);

        // What the walk fetches below such a node is kept with the node:
        // a second walk from it fetches nothing.
        let below = descend(&mut child, false);
        assert!(below.len() > 2);
        let fetched: Vec<Asked> = (below[1..].iter().cloned())
            .map(Asked::WithoutValue)
            .collect();
        assert_eq!(source.asked(), fetched);
        assert_eq!(descend(&mut child, false), below);
        assert_eq!(source.asked(), []);

        // The value is read where the walk asks for it, once.
        let n = u64::from_be_bytes(child_key.clone().try_into().unwrap());
        child.load_value().unwrap().unwrap();
        child.load_value().unwrap().unwrap();
        assert_eq!(source.asked(), [Asked::Value(child_key.clone())]);
        assert!(!child.tree().value_absent());
        assert_eq!(child.tree().value_as_slice(), value_of(n));

        // With a value-defined cost function the walk below a node it owns
        // is still one of nodes without values: none of them is kept.
        let cost_fn = |_: &[u8], _: &GroveVersion| None;
        let other = child.walk(true, Some(&cost_fn), GroveVersion::latest());
        let other_key = (other.unwrap().unwrap().expect("a child").tree().key()).to_vec();
        assert_eq!(source.asked(), [Asked::WithoutValue(other_key)]);
        drop(child);

        // The tree the walk began on holds no node of it: the root's links
        // are as they were, and the next walk fetches again.
        drop(walker);
        assert!(root.link(true).unwrap().is_reference());
        assert!(root.child(true).is_none());
        let mut walker = RefWalker::new(&mut root, source.clone());
        walker.values_on_demand();
        descend(&mut walker, true);
        assert_eq!(source.asked()[0], Asked::WithoutValue(child_key));
    }

    #[test]
    fn a_walk_that_is_given_a_cost_function_fetches_whole_nodes_into_the_tree() {
        let (mut root, source) = stored(true);
        let mut walker = RefWalker::new(&mut root, source.clone());
        walker.values_on_demand();
        let cost_fn = |_: &[u8], _: &GroveVersion| None;
        let child = walker.walk(false, Some(&cost_fn), GroveVersion::latest());
        let child_key = (child.unwrap().unwrap().expect("a child").tree().key()).to_vec();
        assert_eq!(source.asked(), [Asked::Whole(child_key.clone())]);
        drop(walker);
        assert_eq!(
            root.child(false).expect("the child is kept").key(),
            child_key
        );

        // And so does a walk that did not say that it reads values on
        // demand, whatever its source can leave out.
        let (mut root, source) = stored(true);
        let path = descend(&mut RefWalker::new(&mut root, source.clone()), false);
        let fetched: Vec<Asked> = path[1..].iter().cloned().map(Asked::Whole).collect();
        assert_eq!(source.asked(), fetched);
    }

    #[test]
    fn a_proof_reads_the_values_it_emits_and_no_other() {
        for key in 0..NODES {
            let (mut whole_root, whole) = stored(false);
            let (expected, expected_cost) = proof_of(&mut whole_root, &whole, key);
            let (mut root, source) = stored(true);
            let is_root = root.key() == seq_key(key);
            let (proof, cost) = proof_of(&mut root, &source, key);
            assert_eq!(proof, expected, "the proof of {key}");
            assert_eq!(cost, expected_cost, "the cost of the proof of {key}");

            // The same nodes are fetched, and one value is read: the
            // target's, unless it is the root, which the tree holds.
            let fetched = |asked: &[Asked]| -> Vec<Vec<u8>> {
                let key = |asked: &Asked| match asked {
                    Asked::Whole(key) | Asked::WithoutValue(key) => Some(key.clone()),
                    Asked::Value(_) => None,
                };
                asked.iter().filter_map(key).collect()
            };
            let (asked, asked_whole) = (source.asked(), whole.asked());
            assert_eq!(fetched(&asked), fetched(&asked_whole));
            let values: Vec<&Asked> = (asked.iter())
                .filter(|asked| matches!(asked, Asked::Value(_)))
                .collect();
            match is_root {
                true => assert!(values.is_empty(), "{values:?}"),
                false => assert_eq!(values, [&Asked::Value(seq_key(key).to_vec())]),
            }
            assert!(!asked.iter().any(|asked| matches!(asked, Asked::Whole(_))));
            assert!([true, false]
                .into_iter()
                .all(|left| root.child(left).is_none()));
        }
    }

    #[test]
    fn a_source_of_whole_nodes_reads_no_value_by_itself() {
        let mut node =
            TreeNode::new_without_value(vec![1], [2; 32], [3; 32], BasicMerkNode, None, None);
        assert!(node.value_absent());
        let refused = PanicSource {}.fetch_value(&mut node).unwrap();
        assert!(matches!(refused, Err(Error::CorruptedCodeExecution(_))));
        assert!(node.value_absent());
    }
}
