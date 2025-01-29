//! Module dedicated to keep necessary Merks in memory.

use std::{
    cell::{Cell, UnsafeCell},
    collections::{btree_map::Entry, BTreeMap},
};

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt};
use grovedb_merk::Merk;
use grovedb_path::SubtreePathBuilder;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch};
use grovedb_version::version::GroveVersion;

use crate::{Element, Error, GroveDb, Transaction};

type TxMerk<'db> = Merk<PrefixedRocksDbTransactionContext<'db>>;

/// Subtree that was put into the cache.
#[derive(Debug)]
enum Subtree<'db> {
    /// Merk lazily loaded from backing storage.
    LoadedMerk(TxMerk<'db>),
    /// Subtee marked as deleted, this will prevent loading from backing storage
    /// which can be unaware of uncommited deletion.
    Deleted,
}

/// We store Merk on heap to preserve its location as well as borrow flag
/// alongside.
type CachedMerkEntry<'db> = Box<(Cell<bool>, Subtree<'db>)>;

type Merks<'db, 'b, B> = BTreeMap<SubtreePathBuilder<'b, B>, CachedMerkEntry<'db>>;

/// Structure to keep subtrees open in memory for repeated access.
pub(crate) struct MerkCache<'db, 'b, B: AsRef<[u8]>> {
    db: &'db GroveDb,
    pub(crate) version: &'db GroveVersion,
    batch: Box<StorageBatch>,
    tx: &'db Transaction<'db>,
    merks: UnsafeCell<Merks<'db, 'b, B>>,
}

impl<'db, 'b, B: AsRef<[u8]>> MerkCache<'db, 'b, B> {
    /// Initialize a new `MerkCache` instance
    pub(crate) fn new(
        db: &'db GroveDb,
        tx: &'db Transaction<'db>,
        version: &'db GroveVersion,
    ) -> Self {
        MerkCache {
            db,
            tx,
            version,
            merks: Default::default(),
            batch: Default::default(),
        }
    }

    pub(crate) fn mark_deleted(&self, path: SubtreePathBuilder<'b, B>) {
        // SAFETY: there are no other references to `merks` memory at the same time.
        // Note while it's possible to have direct references to actual Merk trees,
        // outside of the scope of this function, this map (`merks`) has
        // indirect connection to them through `Box`, thus there are no overlapping
        // references, and that is requirement of `UnsafeCell` we have there.
        let merks = unsafe {
            self.merks
                .get()
                .as_mut()
                .expect("`UnsafeCell` is never null")
        };

        merks
            .entry(path)
            .and_modify(|subtree| {
                if subtree.0.get() {
                    panic!("Attempt to have double &mut borrow on Merk");
                }
                subtree.1 = Subtree::Deleted
            })
            .or_insert(Box::new((Default::default(), Subtree::Deleted)));
    }

    /// Open Merk using data from parent subtree, returning errors in case
    /// parent element isn't a subtree.
    ///
    /// If there is no parent subtree in the cache `None` will be returned
    /// instead of Merk.
    fn try_open_merk_using_cached_parent<'m>(
        &self,
        merks: &'m Merks<'db, 'b, B>,
        batch: &'db StorageBatch,
        path: SubtreePathBuilder<'b, B>,
    ) -> CostResult<Option<TxMerk<'db>>, Error> {
        let Some((parent_merk, parent_key)) =
            path.derive_parent_owned()
                .and_then(|(parent_path, parent_key)| {
                    merks
                        .get(&parent_path)
                        .map(|parent_merk| (parent_merk, parent_key))
                })
        else {
            return Ok(None).wrap_with_cost(Default::default());
        };

        if parent_merk.0.get() {
            panic!("Attempt to have double &mut borrow on Merk");
        }

        let mut cost = Default::default();
        let merk = match &parent_merk.1 {
            Subtree::LoadedMerk(merk) => {
                if let Some((root_key, tree_type)) = cost_return_on_error!(
                    &mut cost,
                    Element::get(&merk, parent_key, true, &self.version)
                        .map_ok(|element| element.root_key_and_tree_type_owned())
                ) {
                    let storage = self
                        .db
                        .db
                        .get_transactional_storage_context((&path).into(), Some(batch), self.tx)
                        .unwrap_add_cost(&mut cost);
                    cost_return_on_error!(
                        &mut cost,
                        Merk::open_layered_with_root_key(
                            storage,
                            root_key,
                            tree_type,
                            Some(&Element::value_defined_cost_for_serialized_value),
                            self.version,
                        )
                        .map_err(Into::into)
                    )
                } else {
                    return Err(Error::CorruptedData("parent must be a tree".to_owned()))
                        .wrap_with_cost(cost);
                }
            }
            Subtree::Deleted => {
                return Err(Error::CorruptedData("parent was deleted".to_owned()))
                    .wrap_with_cost(cost)
            }
        };

        Ok(Some(merk)).wrap_with_cost(cost)
    }

    /// Gets a smart pointer to a cached Merk or opens one if needed.
    pub(crate) fn get_merk<'c>(
        &'c self,
        path: SubtreePathBuilder<'b, B>,
    ) -> CostResult<MerkHandle<'db, 'c>, Error> {
        let mut cost = Default::default();

        // SAFETY: there are no other references to `merks` memory at the same time.
        // Note while it's possible to have direct references to actual Merk trees,
        // outside of the scope of this function, this map (`merks`) has
        // indirect connection to them through `Box`, thus there are no overlapping
        // references, and that is requirement of `UnsafeCell` we have there.
        let merks = unsafe {
            self.merks
                .get()
                .as_mut()
                .expect("`UnsafeCell` is never null")
        };

        // SAFETY: batch is allocated on the heap and we use only shared
        // references, so as long as the `Box` allocation
        // outlives those references we're safe,
        // and it will outlive because Merks are dropped first.
        let batch = unsafe {
            (&*self.batch as *const StorageBatch)
                .as_ref()
                .expect("`Box` is never null")
        };

        // Getting mutable reference for subtree with lifetime unlinked from the rest
        // of Merks map.
        // SAFETY: we use borrow flag to ensure only one mutable reference to subtree
        // memory will present. As for the rest: `MerkCache` guarantees
        // subtrees to stay at their places through the whole lifetime of the cache
        // structure using indirection via Box and not allowing actual deletions.
        let boxed_flag_merk = if let Some(cached_subtree) = unsafe {
            merks.get_mut(&path).map(|b: &mut Box<_>| {
                (&mut (**b) as *mut (Cell<bool>, Subtree<'db>))
                    .as_mut()
                    .expect("box is never null")
            })
        } {
            // While we can be certain that no one can conflict for Box memory of flag and
            // subtree's pointer, the subtree itself can be referred from
            // outside and we have to check the flag:
            if cached_subtree.0.get() {
                panic!("Attempt to have double &mut borrow on Merk");
            }

            match cached_subtree.1 {
                // Cache hit, all good:
                Subtree::LoadedMerk(_) => {}
                // Cache hit, but marked as deleted, need to look at the parent to see whether it
                // was re-inserted:
                Subtree::Deleted => {
                    match cost_return_on_error!(
                        &mut cost,
                        self.try_open_merk_using_cached_parent(merks, batch, path)
                    ) {
                        Some(merk) => {
                            // Parent data indicates that Merk was re-inserted
                            cached_subtree.1 = Subtree::LoadedMerk(merk);
                        }
                        None => {
                            // This should not happen: subtree is marked as deleted,
                            // but no operations on parent are performed (element deletion is
                            // required as well by GroveDb
                            // structure requirements)
                            return Err(Error::InternalError(
                                "Subtree is marked as deleted, but parent wasn't updated"
                                    .to_owned(),
                            ))
                            .wrap_with_cost(cost);
                        }
                    }
                }
            }

            cached_subtree
        } else {
            // Cache miss, Merk needs to be loaded, either from the storage or from the
            // cached parent if it is present:
            match cost_return_on_error!(
                &mut cost,
                self.try_open_merk_using_cached_parent(merks, batch, path.clone())
            ) {
                Some(merk) => match merks.entry(path) {
                    Entry::Vacant(e) => {
                        e.insert(Box::new((Default::default(), Subtree::LoadedMerk(merk))))
                    }
                    Entry::Occupied(e) => {
                        // This cannot happen since it's a cache miss branch, but whatever
                        let res = e.into_mut();
                        res.1 = Subtree::LoadedMerk(merk);
                        res
                    }
                },
                None => {
                    // No cached merk nor parent, going into storage:
                    merks.entry(path.clone()).or_insert(Box::new((
                        Default::default(),
                        Subtree::LoadedMerk(cost_return_on_error!(
                            &mut cost,
                            self.db.open_transactional_merk_at_path(
                                (&path).into(),
                                self.tx,
                                Some(batch),
                                &self.version,
                            )
                        )),
                    )))
                }
            }
        };

        // As long as we're not making mutable references out of shared references we're
        // good:
        let taken_handle_ref: *const Cell<bool> = &boxed_flag_merk.0 as *const _;
        let merk_ptr: *mut Subtree<'db> = &mut boxed_flag_merk.1 as *mut _;

        // SAFETY: `MerkHandle` contains two references to the heap allocated memory,
        // and we want to be sure that the referenced data will outlive those
        // references plus borrowing rules aren't violated (one `&mut` or many
        // `&` with no `&mut` at a time).
        //
        // To make sure changes to the map won't affect existing borrows we have an
        // indirection in a form of `Box`, that allows us to move and update
        // `MerkCache` with new subtrees and possible reallocations without breaking
        // `MerkHandle`'s references. We use `UnsafeCell` to connect lifetimes and check
        // in compile time that `MerkHandle`s won't outlive the cache, even though we
        // don't hold any references to it, but `&mut` reference would make this borrow
        // exclusive for the whole time of `MerkHandle`, so it shall go intially through
        // a shared reference.
        //
        // Borrowing rules are covered using a borrow flag of each Merk:
        // 1. Borrow flag's reference points to a heap allocated memory and will remain
        //    valid. Since the reference is shared and no need to obtain a `&mut`
        //    reference this part of the memory is covered.
        // 2. For the same reason the Merk's pointer can be converted to a reference,
        //    because the memory behind the `Box` is valid and `MerkHandle` can't
        //    outlive it since we use lifetime parameters.
        // 3. We can get unique reference out of that pointer safely because of
        //    borrowing flag.
        Ok(unsafe {
            MerkHandle {
                merk: merk_ptr,
                taken_handle: taken_handle_ref
                    .as_ref()
                    .expect("`Box` contents are never null"),
            }
        })
        .wrap_with_cost(cost)
    }

    /// Consumes `MerkCache` into accumulated batch of uncommited operations
    /// with subtrees' root hash  propagation done.
    pub(crate) fn into_batch(mut self) -> CostResult<Box<StorageBatch>, Error> {
        let mut cost = Default::default();
        cost_return_on_error!(&mut cost, self.propagate_subtrees());

        // SAFETY: By this time all subtrees are taken and dropped during
        // propagation, so there are no more references to the batch and in can be
        // safely released into the world.
        Ok(self.batch).wrap_with_cost(cost)
    }

    fn propagate_subtrees(&mut self) -> CostResult<(), Error> {
        let mut cost = Default::default();

        // This relies on [SubtreePath]'s ordering implementation to put the deepest
        // path's first.
        while let Some((path, flag_and_merk)) = self.merks.get_mut().pop_first() {
            let Subtree::LoadedMerk(merk) = flag_and_merk.1 else {
                continue;
            };
            if let Some((parent_path, parent_key)) = path.derive_parent_owned() {
                let mut parent_merk = cost_return_on_error!(&mut cost, self.get_merk(parent_path));

                let (root_hash, root_key, aggregate_data) = cost_return_on_error!(
                    &mut cost,
                    merk.root_hash_key_and_aggregate_data()
                        .map_err(Error::MerkError)
                );
                cost_return_on_error!(
                    &mut cost,
                    parent_merk.for_merk(|m| GroveDb::update_tree_item_preserve_flag(
                        m,
                        parent_key,
                        root_key,
                        root_hash,
                        aggregate_data,
                        self.version,
                    ))
                );
            }
        }

        Ok(()).wrap_with_cost(cost)
    }
}

/// Wrapper over `Merk` tree to manage unqiue borrow dynamically.
#[derive(Clone, Debug)]
pub(crate) struct MerkHandle<'db, 'c> {
    merk: *mut Subtree<'db>,
    taken_handle: &'c Cell<bool>,
}

impl<'db> MerkHandle<'db, '_> {
    /// Borrow Merk exclusively to perform provided closure on it.
    /// # Panics
    /// *Rule of thumb: don't use nested `for_merk`*.
    /// Nested usage of `for_merk` can cause a panic in situations involving
    /// double borrowing, as there is no mechanism to prevent multiple
    /// `MerkHandle`s from targeting the same Merk. A less obvious scenario
    /// occurs when there is an implicit peek into a parent Merk to open
    /// another Merk, which might already be inside a `for_merk` call for the
    /// parent. Although such cases can generally lead to panics, they
    /// remain memory-safe due to the checks in place. If necessary, these
    /// nested `for_merk` calls are still available for use.
    pub(crate) fn for_merk<T>(
        &mut self,
        f: impl FnOnce(&mut TxMerk<'db>) -> CostResult<T, Error>,
    ) -> CostResult<T, Error> {
        if self.taken_handle.get() {
            panic!("Attempt to have double &mut borrow on Merk");
        }

        self.taken_handle.set(true);

        // SAFETY: here we want to have `&mut` reference to Merk out of a pointer, there
        // is a checklist for that:
        // 1. Memory is valid, because `MerkHandle` can't outlive `MerkCache` and heap
        //    allocated Merks stay at their place for the whole `MerkCache` lifetime.
        // 2. No other references exist because of `taken_handle` check above.
        let subtree = unsafe { self.merk.as_mut().expect("`Box` contents are never null") };
        let Subtree::LoadedMerk(merk) = subtree else {
            return Err(Error::InternalError(
                "accessing subtree that was deleted".to_owned(),
            ))
            .wrap_with_cost(Default::default());
        };

        let result = f(merk);

        self.taken_handle.set(false);

        result
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::CostsExt;
    use grovedb_path::{SubtreePath, SubtreePathBuilder};
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;

    use super::MerkCache;
    use crate::{
        tests::{make_deep_tree, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    #[test]
    #[should_panic]
    fn cant_borrow_twice() {
        let version = GroveVersion::latest();
        let db = make_test_grovedb(&version);
        let tx = db.start_transaction();

        let cache = MerkCache::new(&db, &tx, version);

        let mut merk1 = cache
            .get_merk(SubtreePath::empty().derive_owned())
            .unwrap()
            .unwrap();
        let mut merk2 = cache
            .get_merk(SubtreePath::empty().derive_owned())
            .unwrap()
            .unwrap();

        merk1
            .for_merk(|_m1| {
                merk2.for_merk(|_m2| {
                    // this shouldn't happen
                    Ok(()).wrap_with_cost(Default::default())
                })
            })
            .unwrap()
            .unwrap();
    }

    #[test]
    #[should_panic]
    fn cant_borrow_parent_twice() {
        let version = GroveVersion::latest();
        let db = make_test_grovedb(&version);
        let tx = db.start_transaction();

        let cache = MerkCache::new(&db, &tx, version);

        let mut merk1 = cache
            .get_merk(SubtreePath::empty().derive_owned())
            .unwrap()
            .unwrap();
        // Opening child requires taking a peek into parent, but we're already inside of
        // `for_merk` of parent
        let mut _merk2 = merk1
            .for_merk(|_m1| cache.get_merk(SubtreePath::empty().derive_owned_with_child(b"nested")))
            .unwrap()
            .unwrap();
    }

    #[test]
    fn can_use_non_overlapping_for_merk() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();

        let cache = MerkCache::new(&db, &tx, version);

        let mut merk1 = cache
            .get_merk(SubtreePath::empty().derive_owned())
            .unwrap()
            .unwrap();
        let mut _merk2 = merk1
            .for_merk(|_m1| {
                cache.get_merk(SubtreePathBuilder::owned_from_iter([
                    TEST_LEAF,
                    b"innertree",
                ]))
            })
            .unwrap()
            .unwrap();
    }

    #[test]
    fn cant_open_merk_with_deleted_parent() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();

        let cache = MerkCache::new(&db, &tx, version);

        cache.mark_deleted(SubtreePathBuilder::new());

        assert!(matches!(
            cache
                .get_merk(SubtreePathBuilder::owned_from_iter([TEST_LEAF]))
                .unwrap(),
            Err(Error::CorruptedData(_))
        ));
    }

    #[test]
    fn subtrees_are_propagated() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();

        let path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let item = Element::new_item(b"hello".to_vec());

        let no_propagation_ops_count = {
            let batch = StorageBatch::new();

            let mut merk = db
                .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), &version)
                .unwrap()
                .unwrap();

            item.insert(&mut merk, b"k1", None, &version)
                .unwrap()
                .unwrap();

            batch.len()
        };

        let cache = MerkCache::new(&db, &tx, version);

        let mut merk = cache.get_merk(path.derive_owned()).unwrap().unwrap();

        merk.for_merk(|m| item.insert(m, b"k1", None, &version))
            .unwrap()
            .unwrap();

        drop(merk);

        assert!(cache.into_batch().unwrap().unwrap().len() > no_propagation_ops_count);
    }
}
