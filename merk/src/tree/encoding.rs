use anyhow::{anyhow, Error};
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostsExt, OperationCost,
};
use ed::{Decode, Encode};
use storage::StorageContext;

use super::Tree;

impl Tree {
    pub fn decode_raw(bytes: &[u8]) -> Result<Self, Error> {
        Decode::decode(bytes).map_err(|e| anyhow!("failed to decode a Tree structure ({})", e))
    }

    pub(crate) fn get<'db, S, K>(storage: &S, key: K) -> CostContext<Result<Option<Self>, Error>>
    where
        S: StorageContext<'db>,
        K: AsRef<[u8]>,
        Error: From<S::Error>,
    {
        let mut cost = OperationCost::default();
        let tree_bytes = cost_return_on_error!(&mut cost, storage.get(&key).map_err(|e| e.into()));

        let tree_opt = cost_return_on_error_no_add!(
            &cost,
            tree_bytes.map(|x| Tree::decode_raw(&x)).transpose()
        );

        Ok(if let Some(mut tree) = tree_opt {
            tree.set_key(key.as_ref().to_vec());
            Some(tree)
        } else {
            None
        })
        .wrap_with_cost(cost)
    }
}

impl Tree {
    #[inline]
    pub fn encode(&self) -> Vec<u8> {
        // operation is infallible so it's ok to unwrap
        Encode::encode(self).unwrap()
    }

    #[inline]
    pub fn encode_into(&self, dest: &mut Vec<u8>) {
        // operation is infallible so it's ok to unwrap
        Encode::encode_into(self, dest).unwrap()
    }

    #[inline]
    pub fn encoding_length(&self) -> usize {
        // operation is infallible so it's ok to unwrap
        Encode::encoding_length(self).unwrap()
    }

    #[inline]
    pub fn decode_into(&mut self, key: Vec<u8>, input: &[u8]) {
        // operation is infallible so it's ok to unwrap
        Decode::decode_into(self, input).unwrap();
        self.inner.kv.key = key;
    }

    #[inline]
    pub fn decode(key: Vec<u8>, input: &[u8]) -> Self {
        // operation is infallible so it's ok to unwrap
        // TODO: how said that its infallible?
        let mut tree: Self = Decode::decode(input).unwrap();
        tree.inner.kv.key = key;
        tree
    }
}

#[cfg(test)]
mod tests {
    use crate::tree::TreeFeatureType::BasicMerk;
    use super::{super::Link, *};

    #[test]
    fn encode_leaf_tree() {
        let tree = Tree::from_fields(vec![0], vec![1], [55; 32], None, None, BasicMerk).unwrap();
        assert_eq!(tree.encoding_length(), 68);
        assert_eq!(
            tree.encode(),
            vec![
                0, 0, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 32, 34, 236, 157, 87, 27, 167,
                116, 207, 158, 131, 208, 25, 73, 98, 245, 209, 227, 170, 26, 72, 212, 134, 166,
                126, 39, 98, 166, 199, 149, 144, 21, 1, 0
            ]
        );
    }

    #[test]
    #[should_panic]
    fn encode_modified_tree() {
        let tree = Tree::from_fields(
            vec![0],
            vec![1],
            [55; 32],
            Some(Link::Modified {
                pending_writes: 1,
                child_heights: (123, 124),
                tree: Tree::new(vec![2], vec![3], BasicMerk).unwrap(),
            }),
            None,
            BasicMerk,
        )
        .unwrap();
        tree.encode();
    }

    #[test]
    fn encode_loaded_tree() {
        let tree = Tree::from_fields(
            vec![0],
            vec![1],
            [55; 32],
            Some(Link::Loaded {
                hash: [66; 32],
                sum: None,
                child_heights: (123, 124),
                tree: Tree::new(vec![2], vec![3], BasicMerk).unwrap(),
            }),
            None,
            BasicMerk,
        )
        .unwrap();
        assert_eq!(
            tree.encode(),
            vec![
                1, 1, 2, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66,
                66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 123, 124, 0, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131,
                208, 25, 73, 98, 245, 209, 227, 170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199,
                149, 144, 21, 1, 0
            ]
        );
    }

    #[test]
    fn encode_uncommitted_tree() {
        let tree = Tree::from_fields(
            vec![0],
            vec![1],
            [55; 32],
            Some(Link::Uncommitted {
                hash: [66; 32],
                sum: None,
                child_heights: (123, 124),
                tree: Tree::new(vec![2], vec![3], BasicMerk).unwrap(),
            }),
            None,
            BasicMerk,
        )
        .unwrap();
        assert_eq!(
            tree.encode(),
            vec![
                1, 1, 2, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66,
                66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 123, 124, 0, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131,
                208, 25, 73, 98, 245, 209, 227, 170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199,
                149, 144, 21, 1, 0
            ]
        );
    }

    #[test]
    fn encode_reference_tree() {
        let tree = Tree::from_fields(
            vec![0],
            vec![1],
            [55; 32],
            Some(Link::Reference {
                hash: [66; 32],
                sum: None,
                child_heights: (123, 124),
                key: vec![2],
            }),
            None,
            BasicMerk,
        )
        .unwrap();
        assert_eq!(tree.encoding_length(), 104);
        assert_eq!(
            tree.encode(),
            vec![
                1, 1, 2, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66,
                66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 123, 124, 0, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131,
                208, 25, 73, 98, 245, 209, 227, 170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199,
                149, 144, 21, 1, 0
            ]
        );
    }

    #[test]
    fn decode_leaf_tree() {
        let bytes = vec![
            0, 0, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131,
            208, 25, 73, 98, 245, 209, 227, 170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199, 149,
            144, 21, 1, 0,
        ];
        let tree = Tree::decode(vec![0], bytes.as_slice());
        assert_eq!(tree.key(), &[0]);
        assert_eq!(tree.value(), &[1]);
        assert_eq!(tree.inner.feature_type, BasicMerk)
    }

    #[test]
    fn decode_reference_tree() {
        let bytes = vec![
            1, 1, 2, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66,
            66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 123, 124, 0, 55, 55, 55, 55, 55,
            55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
            55, 55, 55, 55, 55, 32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131, 208, 25, 73, 98,
            245, 209, 227, 170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199, 149, 144, 21, 1,
        ];
        let tree = Tree::decode(vec![0], bytes.as_slice());
        assert_eq!(tree.key(), &[0]);
        assert_eq!(tree.value(), &[1]);
        if let Some(Link::Reference {
            key,
            child_heights,
            hash,
            sum,
        }) = tree.link(true)
        {
            assert_eq!(*key, [2]);
            assert_eq!(*child_heights, (123u8, 124u8));
            assert_eq!(*hash, [66u8; 32]);
        } else {
            panic!("Expected Link::Reference");
        }
    }
}
