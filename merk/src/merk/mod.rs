pub mod chunks;
// TODO
// pub mod restore;
use std::{cell::Cell, cmp::Ordering, collections::LinkedList, fmt};

use anyhow::{anyhow, bail, Result};
use storage::{self, rocksdb_storage::PrefixedRocksDbStorage, Batch, RawIterator, Storage, Store};

use crate::{
    proofs::{encode_into, query::QueryItem, Query},
    tree::{Commit, Fetch, Hash, Link, MerkBatch, Op, RefWalker, Tree, Walker, NULL_HASH},
};

const ROOT_KEY_KEY: &[u8] = b"root";

/// A handle to a Merkle key/value store backed by RocksDB.
pub struct Merk<S>
where
    S: Storage,
{
    pub(crate) tree: Cell<Option<Tree>>,
    pub(crate) storage: S,
}

impl<S: Storage> fmt::Debug for Merk<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Merk").finish()
    }
}

pub type UseTreeMutResult = Result<Vec<(Vec<u8>, Option<Vec<u8>>)>>;

impl<S: Storage> Merk<S>
where
    <S as Storage>::Error: std::error::Error,
{
    pub fn open(storage: S) -> Result<Merk<S>> {
        let mut merk = Merk {
            tree: Cell::new(None),
            storage,
        };
        merk.load_root()?;

        Ok(merk)
    }

    /// Deletes tree data
    pub fn clear<'a>(&'a mut self, transaction: Option<&'a S::DBTransaction<'a>>) -> Result<()> {
        let mut iter = self.raw_iter(transaction);
        iter.seek_to_first();
        let mut to_delete = self.storage.new_batch(transaction)?;
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
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0]))], &[], None).unwrap();
    ///
    /// use merk::Op;
    ///
    /// let batch = &[
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6])), // puts value [4,5,6] to key[1,2,3]
    ///     (vec![4, 5, 6], Op::Delete),             // deletes key [4,5,6]
    /// ];
    /// store.apply::<_, Vec<_>>(batch, &[], None).unwrap();
    /// ```
    pub fn apply<'a: 'b, 'b, KB, KA>(
        &'a mut self,
        batch: &MerkBatch<KB>,
        aux: &MerkBatch<KA>,
        transaction: Option<&'b S::DBTransaction<'b>>,
    ) -> Result<()>
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

        unsafe { self.apply_unchecked(batch, aux, transaction) }
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
    /// # store.apply::<_, Vec<_>>(&[(vec![4,5,6], Op::Put(vec![0]))], &[], None).unwrap();
    ///
    /// use merk::Op;
    ///
    /// let batch = &[
    ///     (vec![1, 2, 3], Op::Put(vec![4, 5, 6])), // puts value [4,5,6] to key [1,2,3]
    ///     (vec![4, 5, 6], Op::Delete),             // deletes key [4,5,6]
    /// ];
    /// unsafe {
    ///     store
    ///         .apply_unchecked::<_, Vec<_>>(batch, &[], None)
    ///         .unwrap()
    /// };
    /// ```
    pub unsafe fn apply_unchecked<'a: 'b, 'b, KB, KA>(
        &'a mut self,
        batch: &MerkBatch<KB>,
        aux: &MerkBatch<KA>,
        transaction: Option<&'b S::DBTransaction<'b>>,
    ) -> Result<()>
    where
        KB: AsRef<[u8]>,
        KA: AsRef<[u8]>,
    {
        let maybe_walker = self
            .tree
            .take()
            .take()
            .map(|tree| Walker::new(tree, self.source()));

        let (maybe_tree, deleted_keys) = Walker::apply_to(maybe_walker, batch, self.source())?;
        self.tree.set(maybe_tree);

        // commit changes to db
        self.commit(deleted_keys, aux, transaction)
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
    pub fn prove(&self, query: Query, limit: Option<u16>, offset: Option<u16>) -> Result<Vec<u8>> {
        let left_to_right = query.left_to_right;
        self.prove_unchecked(query, limit, offset, left_to_right)
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
    ) -> Result<Vec<u8>>
    where
        Q: Into<QueryItem>,
        I: IntoIterator<Item = Q>,
    {
        let query_vec: Vec<QueryItem> = query.into_iter().map(Into::into).collect();

        self.use_tree_mut(|maybe_tree| {
            let tree = maybe_tree.ok_or(anyhow!("Cannot create proof for empty tree"))?;

            let mut ref_walker = RefWalker::new(tree, self.source());
            let (proof, ..) =
                ref_walker.create_proof(query_vec.as_slice(), limit, offset, left_to_right)?;

            let mut bytes = Vec::with_capacity(128);
            encode_into(proof.iter(), &mut bytes);
            Ok(bytes)
        })
    }

    pub fn commit<'a: 'b, 'b, K>(
        &'a mut self,
        deleted_keys: LinkedList<Vec<u8>>,
        aux: &MerkBatch<K>,
        transaction: Option<&'b S::DBTransaction<'b>>,
    ) -> Result<()>
    where
        K: AsRef<[u8]>,
    {
        let mut batch = self.storage.new_batch(transaction)?;
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
                Op::Delete => batch.delete_aux(key)?,
            };
        }

        // write to db
        self.storage.commit_batch(batch)?;

        Ok(())
    }

    pub fn walk<T>(&self, f: impl FnOnce(Option<RefWalker<MerkSource<S>>>) -> T) -> T {
        let mut tree = self.tree.take();
        let maybe_walker = tree
            .as_mut()
            .map(|tree| RefWalker::new(tree, self.source()));
        let res = f(maybe_walker);
        self.tree.set(tree);
        res
    }

    pub fn raw_iter<'a>(
        &'a self,
        transaction: Option<&'a S::DBTransaction<'a>>,
    ) -> S::RawIterator<'a> {
        self.storage.raw_iter(transaction)
    }

    pub fn is_empty_tree<'a>(&'a self, transaction: Option<&'a S::DBTransaction<'a>>) -> bool {
        let mut iter = self.raw_iter(transaction);
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

impl Clone for Merk<PrefixedRocksDbStorage> {
    fn clone(&self) -> Self {
        let tree_clone = match self.tree.take() {
            None => None,
            Some(tree) => {
                let clone = tree.clone();
                self.tree.set(Some(tree));
                Some(clone)
            }
        };
        Self {
            tree: Cell::new(tree_clone),
            storage: self.storage.clone(),
        }
    }
}

// TODO: get rid of Fetch/source and use GroveDB storage abstraction
#[derive(Debug)]
pub struct MerkSource<'a, S: Storage> {
    storage: &'a S,
}

impl<'a, S: Storage> Clone for MerkSource<'a, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
        }
    }
}

impl<'a, S: Storage> Fetch for MerkSource<'a, S>
where
    //    crate::error::Error: From<<S as
    // Storage>::Error>,
    <S as Storage>::Error: std::error::Error,
{
    fn fetch(&self, link: &Link) -> Result<Tree> {
        Tree::get(&self.storage, link.key())?.ok_or(anyhow!("Key not found"))
    }
}

struct MerkCommitter {
    batch: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    height: u8,
    levels: u8,
}

impl MerkCommitter {
    fn new(height: u8, levels: u8) -> Self {
        MerkCommitter {
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
    use storage::{
        rocksdb_storage::{
            default_rocksdb, PrefixedRocksDbStorage, RawPrefixedTransactionalIterator,
        },
        RawIterator,
    };
    use tempdir::TempDir;

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
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .expect("apply failed");

        assert_invariants(&merk);
        assert_eq!(
            merk.root_hash(),
            [
                99, 81, 104, 29, 169, 195, 53, 48, 134, 74, 250, 47, 77, 121, 157, 227, 139, 241,
                250, 216, 78, 87, 152, 116, 252, 116, 132, 16, 150, 163, 107, 30
            ]
        );
    }

    #[test]
    fn insert_uncached() {
        let batch_size = 20;
        let mut merk = TempMerk::new();

        let batch = make_batch_seq(0..batch_size);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .expect("apply failed");
        assert_invariants(&merk);

        let batch = make_batch_seq(batch_size..(batch_size * 2));
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .expect("apply failed");
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
            merk.apply::<_, Vec<_>>(&batch, &[], None)
                .expect("apply failed");
        }
    }

    #[test]
    fn actual_deletes() {
        let mut merk = TempMerk::new();

        let batch = make_batch_rand(10, 1);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .expect("apply failed");

        let key = batch.first().unwrap().0.clone();
        merk.apply::<_, Vec<_>>(&[(key.clone(), Op::Delete)], &[], None)
            .unwrap();

        let value = merk.inner.get(key.as_slice()).unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn aux_data() {
        let mut merk = TempMerk::new();
        merk.apply::<Vec<_>, _>(&[], &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))], None)
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
            None,
        )
        .expect("apply failed");

        // make enough changes so that main column family gets auto-flushed
        for i in 0..250 {
            merk.apply::<_, Vec<_>>(&make_batch_seq(i * 2_000..(i + 1) * 2_000), &[], None)
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
        merk.apply::<_, Vec<_>>(&[(vec![5, 5, 5], Op::Put(vec![]))], &[], None)
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
            None,
        )
        .unwrap();
        assert!(merk.get(&[3, 3, 3]).unwrap().is_none());
    }

    #[test]
    fn reopen() {
        fn collect(
            mut node: RefWalker<MerkSource<PrefixedRocksDbStorage>>,
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

        let tmp_dir = TempDir::new("test_reopen").expect("cannot open tempdir");

        let original_nodes = {
            let db = default_rocksdb(tmp_dir.path());
            let mut merk =
                Merk::open(PrefixedRocksDbStorage::new(db, Vec::new()).unwrap()).unwrap();
            let batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
                .unwrap();
            let mut tree = merk.tree.take().unwrap();
            let walker = RefWalker::new(&mut tree, merk.source());

            let mut nodes = vec![];
            collect(walker, &mut nodes);
            nodes
        };

        let db = default_rocksdb(tmp_dir.path());
        let merk = Merk::open(PrefixedRocksDbStorage::new(db, Vec::new()).unwrap()).unwrap();
        let mut tree = merk.tree.take().unwrap();
        let walker = RefWalker::new(&mut tree, merk.source());

        let mut reopen_nodes = vec![];
        collect(walker, &mut reopen_nodes);

        assert_eq!(reopen_nodes, original_nodes);
    }

    #[test]
    fn reopen_iter() {
        fn collect(
            iter: &mut RawPrefixedTransactionalIterator,
            nodes: &mut Vec<(Vec<u8>, Vec<u8>)>,
        ) {
            while iter.valid() {
                nodes.push((iter.key().unwrap().to_vec(), iter.value().unwrap().to_vec()));
                iter.next();
            }
        }
        let tmp_dir = TempDir::new("reopen_iter_test").expect("cannot open tempdir");

        let original_nodes = {
            let db = default_rocksdb(tmp_dir.path());
            let mut merk =
                Merk::open(PrefixedRocksDbStorage::new(db, Vec::new()).unwrap()).unwrap();
            let batch = make_batch_seq(1..10_000);
            merk.apply::<_, Vec<_>>(batch.as_slice(), &[], None)
                .unwrap();

            let mut nodes = vec![];
            collect(&mut merk.raw_iter(None), &mut nodes);
            nodes
        };
        let db = default_rocksdb(tmp_dir.path());
        let merk = Merk::open(PrefixedRocksDbStorage::new(db, Vec::new()).unwrap()).unwrap();

        let mut reopen_nodes = vec![];
        collect(&mut merk.raw_iter(None), &mut reopen_nodes);

        assert_eq!(reopen_nodes, original_nodes);
    }
}
