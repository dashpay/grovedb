//! Merk API enhancements for GroveDbg support

use grovedb_costs::CostsExt;
use grovedb_storage::StorageContext;

use crate::{tree::kv::ValueDefinedCostType, Error, Merk};

impl<'a, S: StorageContext<'a>> Merk<S> {
    pub fn get_node_dbg(&self, key: &[u8]) -> Result<Option<NodeDbg>, Error> {
        self.get_node_direct_fn(
            key,
            |tree| {
                NodeDbg {
                    key: tree.inner.key_as_slice().to_owned(),
                    value: tree.inner.value_as_slice().to_owned(),
                    left_child: tree.link(true).map(|link| link.key().to_owned()),
                    right_child: tree.link(false).map(|link| link.key().to_owned()),
                }
                .wrap_with_cost(Default::default())
            },
            None::<fn(&[u8]) -> Option<ValueDefinedCostType>>, // I wish I knew why
        )
        .unwrap()
    }

    pub fn get_root_node_dbg(&self) -> Result<Option<NodeDbg>, Error> {
        Ok(self.use_tree(|tree_opt| {
            tree_opt.map(|tree| NodeDbg {
                key: tree.inner.key_as_slice().to_owned(),
                value: tree.inner.value_as_slice().to_owned(),
                left_child: tree.link(true).map(|link| link.key().to_owned()),
                right_child: tree.link(false).map(|link| link.key().to_owned()),
            })
        }))
    }
}

#[derive(Debug)]
pub struct NodeDbg {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub left_child: Option<Vec<u8>>,
    pub right_child: Option<Vec<u8>>,
}
