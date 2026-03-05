//! Merk API enhancements for GroveDbg support

use grovedb_costs::CostsExt;
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{tree::kv::ValueDefinedCostType, CryptoHash, Error, Merk, TreeFeatureType};

impl<'a, S: StorageContext<'a>> Merk<S> {
    /// Fetches a node by key and returns its debug representation.
    pub fn get_node_dbg(&self, key: &[u8]) -> Result<Option<NodeDbg>, Error> {
        self.get_node_direct_fn(
            key,
            |tree| {
                NodeDbg {
                    key: tree.inner.key_as_slice().to_owned(),
                    value: tree.inner.value_as_slice().to_owned(),
                    left_child: tree.link(true).map(|link| link.key().to_owned()),
                    left_merk_hash: tree.link(true).map(|link| *link.hash()),
                    right_child: tree.link(false).map(|link| link.key().to_owned()),
                    right_merk_hash: tree.link(false).map(|link| *link.hash()),
                    value_hash: *tree.inner.kv.value_hash(),
                    kv_digest_hash: *tree.inner.kv.hash(),
                    feature_type: tree.inner.kv.feature_type(),
                }
                .wrap_with_cost(Default::default())
            },
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            GroveVersion::latest(),
        )
        .unwrap()
    }

    /// Fetches the root node and returns its debug representation.
    pub fn get_root_node_dbg(&self) -> Result<Option<NodeDbg>, Error> {
        Ok(self.use_tree(|tree_opt| {
            tree_opt.map(|tree| NodeDbg {
                key: tree.inner.key_as_slice().to_owned(),
                value: tree.inner.value_as_slice().to_owned(),
                left_child: tree.link(true).map(|link| link.key().to_owned()),
                left_merk_hash: tree.link(true).map(|link| *link.hash()),
                right_child: tree.link(false).map(|link| link.key().to_owned()),
                right_merk_hash: tree.link(false).map(|link| *link.hash()),
                value_hash: *tree.inner.kv.value_hash(),
                kv_digest_hash: *tree.inner.kv.hash(),
                feature_type: tree.inner.kv.feature_type(),
            })
        }))
    }
}

/// Debug representation of a Merk tree node.
#[derive(Debug)]
pub struct NodeDbg {
    /// The node's key.
    pub key: Vec<u8>,
    /// The node's value.
    pub value: Vec<u8>,
    /// Key of the left child node, if any.
    pub left_child: Option<Vec<u8>>,
    /// Merk hash of the left child, if any.
    pub left_merk_hash: Option<[u8; 32]>,
    /// Key of the right child node, if any.
    pub right_child: Option<Vec<u8>>,
    /// Merk hash of the right child, if any.
    pub right_merk_hash: Option<[u8; 32]>,
    /// Hash of the node's value.
    pub value_hash: CryptoHash,
    /// Combined key-value digest hash.
    pub kv_digest_hash: CryptoHash,
    /// The feature type of this node.
    pub feature_type: TreeFeatureType,
}
