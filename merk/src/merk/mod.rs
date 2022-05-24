pub mod chunks;
// TODO
// pub mod restore;
use std::{cell::Cell, cmp::Ordering, collections::LinkedList, fmt};

use anyhow::{anyhow, bail, Result};
use storage::{self, Batch, RawIterator, StorageContext};

use crate::{
    proofs::{encode_into, query::QueryItem, Op as ProofOp, Query},
    tree::{Commit, Fetch, Hash, Link, MerkBatch, Op, RefWalker, Tree, Walker, NULL_HASH},
};

const ROOT_KEY_KEY: &[u8] = b"root";

pub struct ProofConstructionResult {
    pub proof: Vec<u8>,
    pub limit: Option<u16>,
    pub offset: Option<u16>,
}

impl ProofConstructionResult {
    pub fn new(proof: Vec<u8>, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            proof,
            limit,
            offset,
        }
    }
}

pub struct ProofWithoutEncodingResult {
    pub proof: LinkedList<ProofOp>,
    pub limit: Option<u16>,
    pub offset: Option<u16>,
}

impl ProofWithoutEncodingResult {
    pub fn new(proof: LinkedList<ProofOp>, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            proof,
            limit,
            offset,
        }
    }
}

/// KVIterator allows you to lazily iterate over each kv pair of a subtree
pub struct KVIterator<'a, I: RawIterator> {
    raw_iter: I,
    query: &'a Query,
    left_to_right: bool,
    query_iterator: Box<dyn Iterator<Item = &'a QueryItem> + 'a>,
    current_query_item: Option<&'a QueryItem>,
}

impl<'a, I: RawIterator> KVIterator<'a, I> {
    pub fn new(raw_iter: I, query: &'a Query) -> Self {
        let mut iterator = KVIterator {
            raw_iter,
            query,
            left_to_right: query.left_to_right,
            current_query_item: None,
            query_iterator: query.directional_iter(query.left_to_right),
        };
        iterator.seek();
        iterator
    }

    /// Returns the current node the iter points to if it's valid for the given
    /// query item returns None otherwise
    fn get_kv(&mut self, query_item: &QueryItem) -> Option<(Vec<u8>, Vec<u8>)> {
        if query_item.iter_is_valid_for_type(&self.raw_iter, None, self.left_to_right) {
            let kv = (
                self.raw_iter
                    .key()
                    .expect("key must exist as iter is valid")
                    .to_vec(),
                self.raw_iter
                    .value()
                    .expect("value must exists as iter is valid")
                    .to_vec(),
            );
            if self.left_to_right {
                self.raw_iter.next()
            } else {
                self.raw_iter.prev()
            }
            Some(kv)
        } else {
            None
        }
    }

    /// Moves the iter to the start of the next query item
    fn seek(&mut self) {
        self.current_query_item = self.query_iterator.next();
        if let Some(query_item) = self.current_query_item {
            query_item.seek_for_iter(&mut self.raw_iter, self.left_to_right);
        }
    }
}

impl<'a, I: RawIterator> Iterator for KVIterator<'a, I> {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(query_item) = self.current_query_item {
            let kv_pair = self.get_kv(&query_item);

            if kv_pair.is_some() {
                return kv_pair;
            } else {
                self.seek();
                self.next()
            }
        } else {
            None
        }
    }
}

/// A handle to a Merkle key/value store backed by RocksDB.
pub struct Merk<S> {
    pub(crate) tree: Cell<Option<Tree>>,
    pub storage: S,
}

impl<S> fmt::Debug for Merk<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Merk").finish()
    }
}

pub type UseTreeMutResult = Result<Vec<(Vec<u8>, Option<Vec<u8>>)>>;

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
    <S as StorageContext<'db>>::Error: std::error::Error,
{
    pub fn open(storage: S) -> Result<Self> {
        let mut merk = Self {
            tree: Cell::new(None),
            storage,
        };
        merk.load_root()?;

        Ok(merk)
    }

    /// Deletes tree data
    pub fn clear(&mut self) -> Result<()> {
        let mut iter = self.storage.raw_iter();
        iter.seek_to_first();
        let mut to_delete = self.storage.new_batch();
        while iter.valid() {
            if let Some(key) = iter.key() {
                to_delete.delete(key)?;
            }
            iter.next();
        }
        self.storage.commit_batch(to_delete)?;
        self.tree.set(None);
        Ok(())
    }

    /// Gets an auxiliary value.
    pub fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.storage.get_aux(key)?)
    }

    /// Gets a value for the given key. If the key is not found, `None` is
    /// returned.
    ///
    /// Note that this is essentially the same as a normal RocksDB `get`, so
    /// should be a fast operation and has almost no tree overhead.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.get_node_fn(key, |node| node.value().to_vec())
    }

    /// Gets a hash of a node by a given key, `None` is returned in case
    /// when node not found by the key.
    pub fn get_hash(&self, key: &[u8]) -> Result<Option<[u8; 32]>> {
        self.get_node_fn(key, |node| node.hash())
    }

    /// Generic way to get a node's field
    fn get_node_fn<T, F>(&self, key: &[u8], f: F) -> Result<Option<T>>
    where
        F: FnOnce(&Tree) -> T,
    {
        self.use_tree(move |maybe_tree| {
            let mut cursor = match maybe_tree {
                None => return Ok(None), // empty tree
                Some(tree) => tree,
            };

            loop {
                if key == cursor.key() {
                    return Ok(Some(f(cursor)));
                }

                let left = key < cursor.key();
                let link = match cursor.link(left) {
                    None => return Ok(None), // not found
                    Some(link) => link,
                };

                let maybe_child = link.tree();
                match maybe_child {
                    None => {
                        // fetch from RocksDB
                        break Tree::get(&self.storage, key)
                            .map(|maybe_node| maybe_node.map(|node| f(&node)));
                    }
                    Some(child) => cursor = child, // traverse to child
                }
            }
        })
    }

    /// Returns the root hash of the tree (a digest for the entire store which
    /// proofs can be checked against). If the tree is empty, returns the null
    /// hash (zero-filled).
    pub fn root_hash(&self) -> Hash {
        self.use_tree(|tree| tree.map_or(NULL_HASH, |tree| tree.hash()))
    }

    /// Applies a batch of operations (puts and deletes) to the tree.
    ///
    /// This will fail if the keys in `batch` are not sorted and unique. This
    /// check creates some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `apply_unchecked` for a small performance
    /// gain.
    ///
    /// # Example
    /// ```
    /// # let mut store = merk::test_utils::TempMerk::new();
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0]))], &[]).unwrap();
    ///
    /// use merk::Op;
    ///
    /// let batch = &[
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6])), // puts value [4,5,6] to key[1,2,3]
    ///     (vec![4, 5, 6], Op::Delete),             // deletes key [4,5,6]
    /// ];
    /// store.apply::<_, Vec<_>>(batch, &[]).unwrap();
    /// ```
    pub fn apply<KB, KA>(&mut self, batch: &MerkBatch<KB>, aux: &MerkBatch<KA>) -> Result<()>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
    {
        // ensure keys in batch are sorted and unique
        let mut maybe_prev_key: Option<&KB> = None;
        for (key, _) in batch.iter() {
            if let Some(prev_key) = maybe_prev_key {
                match prev_key.as_ref().cmp(key.as_ref()) {
                    Ordering::Greater => bail!("Keys in batch must be sorted"),
                    Ordering::Equal => bail!("Keys in batch must be unique"),
                    _ => (),
                }
            }
            maybe_prev_key = Some(key);
        }

        unsafe { self.apply_unchecked(batch, aux) }
    }

    /// Applies a batch of operations (puts and deletes) to the tree.
    ///
    /// # Safety
    /// This is unsafe because the keys in `batch` must be sorted and unique -
    /// if they are not, there will be undefined behavior. For a safe version of
    /// this method which checks to ensure the batch is sorted and unique, see
    /// `apply`.
    ///
    /// # Example
    /// ```
    /// # let mut store = merk::test_utils::TempMerk::new();
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0]))], &[]).unwrap();
    ///
    /// use merk::Op;
    ///
    /// let batch = &[
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6])), // puts value [4,5,6] to key [1,2,3]
    ///     (vec![4, 5, 6], Op::Delete),             // deletes key [4,5,6]
    /// ];
    /// unsafe { store.apply_unchecked::<_, Vec<_>>(batch, &[]).unwrap() };
    /// ```
    pub unsafe fn apply_unchecked<KB, KA>(
        &mut self,
        batch: &MerkBatch<KB>,
        aux: &MerkBatch<KA>,
    ) -> Result<()>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
    {
        let (maybe_tree, deleted_keys) = {
            let maybe_walker = self
                .tree
                .take()
                .take()
                .map(|tree| Walker::new(tree, self.source()));

            Walker::apply_to(maybe_walker, batch, self.source())?
        };
        self.tree.set(maybe_tree);

        // commit changes to db
        self.commit(deleted_keys, aux)
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in the
    /// query, if the key is found in the store then the value will be proven to
    /// be in the tree. For each key in the query that does not exist in the
    /// tree, its absence will be proven by including boundary keys.
    ///
    /// The proof returned is in an encoded format which can be verified with
    /// `merk::verify`.
    ///
    /// This will fail if the keys in `query` are not sorted and unique. This
    /// check adds some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `prove_unchecked` for a small performance
    /// gain.
    pub fn prove(
        &self,
        query: Query,
        limit: Option<u16>,
        offset: Option<u16>,
    ) -> Result<ProofConstructionResult> {
        let left_to_right = query.left_to_right;
        let (proof, limit, offset) = self.prove_unchecked(query, limit, offset, left_to_right)?;

        let mut bytes = Vec::with_capacity(128);
        encode_into(proof.iter(), &mut bytes);
        Ok(ProofConstructionResult::new(bytes, limit, offset))
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in the
    /// query, if the key is found in the store then the value will be proven to
    /// be in the tree. For each key in the query that does not exist in the
    /// tree, its absence will be proven by including boundary keys.
    ///
    /// The proof returned is in an intermediate format to be later encoded
    ///
    /// This will fail if the keys in `query` are not sorted and unique. This
    /// check adds some overhead, so if you are sure your batch is sorted and
    /// unique you can use the unsafe `prove_unchecked` for a small performance
    /// gain.
    pub fn prove_without_encoding(
        &self,
        query: Query,
        limit: Option<u16>,
        offset: Option<u16>,
    ) -> Result<ProofWithoutEncodingResult> {
        let left_to_right = query.left_to_right;
        let (proof, limit, offset) = self.prove_unchecked(query, limit, offset, left_to_right)?;

        Ok(ProofWithoutEncodingResult::new(proof, limit, offset))
    }

    /// Creates a Merkle proof for the list of queried keys. For each key in
    /// the query, if the key is found in the store then the value will be
    /// proven to be in the tree. For each key in the query that does not
    /// exist in the tree, its absence will be proven by including
    /// boundary keys.
    /// The proof returned is in an encoded format which can be verified with
    /// `merk::verify`.
    ///
    /// This is unsafe because the keys in `query` must be sorted and unique -
    /// if they are not, there will be undefined behavior. For a safe version
    /// of this method which checks to ensure the batch is sorted and
    /// unique, see `prove`.
    pub fn prove_unchecked<Q, I>(
        &self,
        query: I,
        limit: Option<u16>,
        offset: Option<u16>,
        left_to_right: bool,
    ) -> Result<(LinkedList<ProofOp>, Option<u16>, Option<u16>)>
    where
        Q: Into<QueryItem>,
        I: IntoIterator<Item = Q>,
    {
        let query_vec: Vec<QueryItem> = query.into_iter().map(Into::into).collect();

        self.use_tree_mut(|maybe_tree| {
            let tree = maybe_tree.ok_or(anyhow!("Cannot create proof for empty tree"))?;

            let mut ref_walker = RefWalker::new(tree, self.source());
            let (proof, _, limit, offset, ..) =
                ref_walker.create_proof(query_vec.as_slice(), limit, offset, left_to_right)?;

            Ok((proof, limit, offset))
        })
    }

    pub fn commit<K>(&mut self, deleted_keys: LinkedList<Vec<u8>>, aux: &MerkBatch<K>) -> Result<()>
    where
        K: AsRef<[u8]>,
    {
        let mut batch = self.storage.new_batch();
        let mut to_batch = self.use_tree_mut(|maybe_tree| -> UseTreeMutResult {
            // TODO: concurrent commit
            if let Some(tree) = maybe_tree {
                // TODO: configurable committer
                let mut committer = MerkCommitter::new(tree.height(), 100);
                tree.commit(&mut committer)?;

                // update pointer to root node
                batch.put_root(ROOT_KEY_KEY, tree.key())?;

                Ok(committer.batch)
            } else {
                // empty tree, delete pointer to root
                batch.delete_root(ROOT_KEY_KEY)?;
                Ok(vec![])
            }
        })?;

        // TODO: move this to MerkCommitter impl?
        for key in deleted_keys {
            to_batch.push((key, None));
        }
        to_batch.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, maybe_value) in to_batch {
            if let Some(value) = maybe_value {
                batch.put(&key, &value)?;
            } else {
                batch.delete(&key)?;
            }
        }

        for (key, value) in aux {
            match value {
                Op::Put(value) => batch.put_aux(key, value)?,
                Op::PutReference(value, _) => batch.put_aux(key, value)?,
                Op::Delete => batch.delete_aux(key)?,
            };
        }

        // write to db
        self.storage.commit_batch(batch)?;

        Ok(())
    }

    pub fn walk<'s, T>(&'s self, f: impl FnOnce(Option<RefWalker<MerkSource<'s, S>>>) -> T) -> T {
        let mut tree = self.tree.take();
        let maybe_walker = tree
            .as_mut()
            .map(|tree| RefWalker::new(tree, self.source()));
        let res = f(maybe_walker);
        self.tree.set(tree);
        res
    }

    pub fn is_empty_tree(&self) -> bool {
        let mut iter = self.storage.raw_iter();
        iter.seek_to_first();

        !iter.valid()
    }

    fn source(&self) -> MerkSource<S> {
        MerkSource {
            storage: &self.storage,
        }
    }

    fn use_tree<T>(&self, f: impl FnOnce(Option<&Tree>) -> T) -> T {
        let tree = self.tree.take();
        let res = f(tree.as_ref());
        self.tree.set(tree);
        res
    }

    fn use_tree_mut<T>(&self, mut f: impl FnMut(Option<&mut Tree>) -> T) -> T {
        let mut tree = self.tree.take();
        let res = f(tree.as_mut());
        self.tree.set(tree);
        res
    }

    // pub(crate) fn set_root_key(&mut self, key: &[u8]) -> Result<()> {
    //     Ok(self.storage.put_root(ROOT_KEY_KEY, key)?)
    // }

    pub(crate) fn load_root(&mut self) -> Result<()> {
        if let Some(tree_root_key) = self.storage.get_root(ROOT_KEY_KEY)? {
            let tree = Tree::get(&self.storage, &tree_root_key)?;
            self.tree = Cell::new(tree);
        }
        Ok(())
    }
}

// impl Clone for Merk<S> {
//     fn clone(&self) -> Self {
//         let tree_clone = match self.tree.take() {
//             None => None,
//             Some(tree) => {
//                 let clone = tree.clone();
//                 self.tree.set(Some(tree));
//                 Some(clone)
//             }
//         };
//         Self {
//             tree: Cell::new(tree_clone),
//             storage: self.storage.clone(),
//         }
//     }
// }

// // TODO: get rid of Fetch/source and use GroveDB storage abstraction
#[derive(Debug)]
pub struct MerkSource<'s, S> {
    storage: &'s S,
}

impl<'s, S> Clone for MerkSource<'s, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
        }
    }
}

impl<'s, 'db, S> Fetch for MerkSource<'s, S>
where
    S: StorageContext<'db>,
{
    fn fetch(&self, link: &Link) -> Result<Tree> {
        Tree::get(self.storage, link.key())?.ok_or(anyhow!("Key not found"))
    }
}

struct MerkCommitter {
    batch: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    height: u8,
    levels: u8,
}

impl MerkCommitter {
    fn new(height: u8, levels: u8) -> Self {
        Self {
            batch: Vec::with_capacity(10000),
            height,
            levels,
        }
    }
}

impl Commit for MerkCommitter {
    fn write(&mut self, tree: &Tree) -> Result<()> {
        let mut buf = Vec::with_capacity(tree.encoding_length());
        tree.encode_into(&mut buf);
        self.batch.push((tree.key().to_vec(), Some(buf)));
        Ok(())
    }

    fn prune(&self, tree: &Tree) -> (bool, bool) {
        // keep N top levels of tree
        let prune = (self.height - tree.height()) >= self.levels;
        (prune, prune)
    }
}

#[cfg(test)]
mod test {
    use std::iter::empty;

    use storage::{
        rocksdb_storage::{PrefixedRocksDbStorageContext, RocksDbStorage},
        RawIterator, Storage, StorageContext,
    };
    use tempfile::TempDir;

    use super::{Merk, MerkSource, RefWalker};
    use crate::{test_utils::*, Op};

    // TODO: Close and then reopen test

    fn assert_invariants(merk: &TempMerk) {
        merk.use_tree(|maybe_tree| {
            let tree = maybe_tree.expect("expected tree");
            assert_tree_invariants(tree);
        })
    }

    #[test]
    fn simple_insert_apply() {
        let batch_size = 20;
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..batch_size);
        merk.apply::<_, Vec<_>>(&batch, &[]).expect("apply failed");

        assert_invariants(&merk);
        assert_eq!(
            merk.root_hash(),
            [
                126, 168, 96, 201, 59, 225, 123, 33, 206, 154, 87, 23, 139, 143, 136, 52, 103, 9,
                218, 90, 71, 153, 240, 47, 227, 168, 1, 104, 239, 237, 140, 147
            ]
        );
    }

    #[test]
    fn insert_uncached() {
        let batch_size = 20;
        let mut merk = TempMerk::new();

        let batch = make_batch_seq(0..batch_size);
        merk.apply::<_, Vec<_>>(&batch, &[]).expect("apply failed");
        assert_invariants(&merk);

        let batch = make_batch_seq(batch_size..(batch_size * 2));
        merk.apply::<_, Vec<_>>(&batch, &[]).expect("apply failed");
        assert_invariants(&merk);
    }

    #[test]
    fn insert_rand() {
        let tree_size = 40;
        let batch_size = 4;
        let mut merk = TempMerk::new();

        for i in 0..(tree_size / batch_size) {
            println!("i:{}", i);
            let batch = make_batch_rand(batch_size, i);
            merk.apply::<_, Vec<_>>(&batch, &[]).expect("apply failed");
        }
    }

    #[test]
    fn actual_deletes() {
        let mut merk = TempMerk::new();

        let batch = make_batch_rand(10, 1);
        merk.apply::<_, Vec<_>>(&batch, &[]).expect("apply failed");

        let key = batch.first().unwrap().0.clone();
        merk.apply::<_, Vec<_>>(&[(key.clone(), Op::Delete)], &[])
            .unwrap();

        let value = merk.storage.get(key.as_slice()).unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn aux_data() {
        let mut merk = TempMerk::new();
        merk.apply::<Vec<_>, _>(&[], &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))])
            .expect("apply failed");
        let val = merk.get_aux(&[1, 2, 3]).unwrap();
        assert_eq!(val, Some(vec![4, 5, 6]));
    }

    #[test]
    fn simulated_crash() {
        let mut merk = CrashMerk::open().expect("failed to open merk");

        merk.apply::<_, Vec<_>>(
            &[(vec![0], Op::Put(vec![1]))],
            &[(vec![2], Op::Put(vec![3]))],
        )
        .expect("apply failed");

        // make enough changes so that main column family gets auto-flushed
        for i in 0..250 {
            merk.apply::<_, Vec<_>>(&make_batch_seq(i * 2_000..(i + 1) * 2_000), &[])
                .expect("apply failed");
        }
        merk.crash();

        assert_eq!(merk.get_aux(&[2]).unwrap(), Some(vec![3]));
    }

    #[test]
    fn get_not_found() {
        let mut merk = TempMerk::new();

        // no root
        assert!(merk.get(&[1, 2, 3]).unwrap().is_none());

        // cached
        merk.apply::<_, Vec<_>>(&[(vec![5, 5, 5], Op::Put(vec![]))], &[])
            .unwrap();
        assert!(merk.get(&[1, 2, 3]).unwrap().is_none());

        // uncached
        merk.apply::<_, Vec<_>>(
            &[
                (vec![0, 0, 0], Op::Put(vec![])),
                (vec![1, 1, 1], Op::Put(vec![])),
                (vec![2, 2, 2], Op::Put(vec![])),
            ],
            &[],
        )
        .unwrap();
        assert!(merk.get(&[3, 3, 3]).unwrap().is_none());
    }

    #[test]
    fn reopen() {
        fn collect(
            mut node: RefWalker<MerkSource<PrefixedRocksDbStorageContext>>,
            nodes: &mut Vec<Vec<u8>>,
        ) {
            nodes.push(node.tree().encode());
            if let Some(c) = node.walk(true).unwrap() {
                collect(c, nodes);
            }
            if let Some(c) = node.walk(false).unwrap() {
                collect(c, nodes);
            }
        }

        let tmp_dir = TempDir::new().expect("cannot open tempdir");

        let original_nodes = {
            let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
                .expect("cannot open rocksdb storage");
            let mut merk =
                Merk::open(storage.get_storage_context(empty())).expect("cannot open merk");
            let batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(batch.as_slice(), &[]).unwrap();
            let mut tree = merk.tree.take().unwrap();
            let walker = RefWalker::new(&mut tree, merk.source());

            let mut nodes = vec![];
            collect(walker, &mut nodes);
            nodes
        };

        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open(storage.get_storage_context(empty())).expect("cannot open merk");
        let mut tree = merk.tree.take().unwrap();
        let walker = RefWalker::new(&mut tree, merk.source());

        let mut reopen_nodes = vec![];
        collect(walker, &mut reopen_nodes);

        assert_eq!(reopen_nodes, original_nodes);
    }

    type PrefixedStorageIter<'db, 'ctx> =
        &'ctx mut <PrefixedRocksDbStorageContext<'db> as StorageContext<'db>>::RawIterator;

    #[test]
    fn reopen_iter() {
        fn collect<'db, 'ctx>(
            iter: PrefixedStorageIter<'db, 'ctx>,
            nodes: &mut Vec<(Vec<u8>, Vec<u8>)>,
        ) {
            while iter.valid() {
                nodes.push((iter.key().unwrap().to_vec(), iter.value().unwrap().to_vec()));
                iter.next();
            }
        }
        let tmp_dir = TempDir::new().expect("cannot open tempdir");

        let original_nodes = {
            let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
                .expect("cannot open rocksdb storage");
            let mut merk =
                Merk::open(storage.get_storage_context(empty())).expect("cannot open merk");
            let batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(batch.as_slice(), &[]).unwrap();

            let mut nodes = vec![];
            collect(&mut merk.storage.raw_iter(), &mut nodes);
            nodes
        };
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open(storage.get_storage_context(empty())).expect("cannot open merk");

        let mut reopen_nodes = vec![];
        collect(&mut merk.storage.raw_iter(), &mut reopen_nodes);

        assert_eq!(reopen_nodes, original_nodes);
    }
}
