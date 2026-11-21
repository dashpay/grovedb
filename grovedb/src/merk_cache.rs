//! Module dedicated to keep necessary Merks in memory.

use std::{
    cell::{Cell, UnsafeCell},
    collections::{btree_map::Entry, BTreeMap},
    ptr::NonNull,
};

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, get::ElementFetchFromStorageExtensions,
        tree_type::ElementTreeTypeExtensions,
    },
    Merk,
};
use grovedb_path::SubtreePathBuilder;
use grovedb_version::version::GroveVersion;

use crate::{engine::EngineBatch, Element, Engine, Error, GroveDb, Transaction};

type TxMerk<'db, E> = Merk<<E as Engine>::Subtree<'db>>;

/// Subtree that was put into the cache.
// A subtree of the cache already lives on the heap, in its `CachedSubtree`
// (required for pointer stability), so boxing the Merk again would only add
// an indirection.
#[allow(clippy::large_enum_variant)]
enum Subtree<'db, E: Engine> {
    /// Merk lazily loaded from backing storage.
    LoadedMerk(TxMerk<'db, E>),
    /// Subtree marked as deleted, this will prevent loading from backing storage
    /// which can be unaware of uncommitted deletion.
    Deleted,
}

/// A subtree of the cache, with the flag that says whether a closure has
/// it borrowed.
///
/// It is on the heap, where it stays for as long as the cache lives, and
/// from the moment it is put there it is reached through one pointer
/// ([`CachedMerkEntry`]) and through shared references made from that
/// pointer: by the cache, and by every handle of the subtree. The subtree
/// is behind a cell. A unique reference to it, or a write of it, is made
/// by a closure that holds the flag; by the cache after it has checked
/// that the flag is not held, in a borrow that ends before anything else
/// reaches the entry; and by the end of the cache, which has the cache to
/// itself. The box that owns an entry is not read or written through
/// again until the cache is dropped. The list of boxes may move a box
/// when it grows, which leaves the entry where it is.
struct CachedSubtree<'db, E: Engine> {
    taken: Cell<bool>,
    subtree: UnsafeCell<Subtree<'db, E>>,
}

impl<E: Engine> CachedSubtree<'_, E> {
    fn assert_not_taken(&self) {
        if self.taken.get() {
            panic!("Attempt to have double &mut borrow on Merk");
        }
    }
}

/// The pointer by which the cache reaches a [`CachedSubtree`], and from
/// which every handle of the subtree is made.
type CachedMerkEntry<'db, E> = NonNull<CachedSubtree<'db, E>>;

type Merks<'db, 'b, B, E> = BTreeMap<SubtreePathBuilder<'b, B>, CachedMerkEntry<'db, E>>;

/// Structure to keep subtrees open in memory for repeated access.
pub(crate) struct MerkCache<'db, 'tx: 'db, 'b, B: AsRef<[u8]>, E: Engine> {
    db: &'tx GroveDb<E>,
    pub(crate) version: &'db GroveVersion,
    merks: UnsafeCell<Merks<'db, 'b, B, E>>,
    // What `merks` points at. Before `batch`: the Merks borrow it and are
    // dropped first.
    subtrees: UnsafeCell<Vec<Box<CachedSubtree<'db, E>>>>,
    batch: Box<E::Batch>,
    external_batch: Option<&'db E::Batch>,
    tx: &'db Transaction<'tx, E>,
}

impl<'db, 'tx: 'db, 'b, B: AsRef<[u8]>, E: Engine> MerkCache<'db, 'tx, 'b, B, E> {
    /// Initialize a new `MerkCache` instance
    pub(crate) fn new(
        db: &'tx GroveDb<E>,
        tx: &'db Transaction<'tx, E>,
        version: &'db GroveVersion,
    ) -> Self {
        MerkCache {
            db,
            tx,
            version,
            merks: Default::default(),
            subtrees: Default::default(),
            batch: Default::default(),
            external_batch: None,
        }
    }

    /// Seed a cache with an already observed Merk. Its storage batch is owned
    /// by the caller and outlives the cache. All paths and operation boundaries
    /// use that same batch, preserving ordering between reference mutations.
    pub(crate) fn with_prepared_merk(
        db: &'tx GroveDb<E>,
        tx: &'db Transaction<'tx, E>,
        version: &'db GroveVersion,
        path: SubtreePathBuilder<'b, B>,
        merk: TxMerk<'db, E>,
        batch: &'db E::Batch,
    ) -> Self {
        let mut cache = Self::new(db, tx, version);
        cache.external_batch = Some(batch);
        let entry = cache.keep(Subtree::LoadedMerk(merk));
        cache.merks.get_mut().insert(path, entry);
        cache
    }

    /// Puts a subtree on the heap, where it stays for as long as the cache
    /// lives, and gives the pointer by which it is reached from then on.
    fn keep(&self, subtree: Subtree<'db, E>) -> CachedMerkEntry<'db, E> {
        // SAFETY: the list of entries is reached here alone, and no reference
        // to it or to one of its boxes leaves this function. The pointer is
        // made from a shared reference to the entry, whose flag and subtree
        // are both behind cells.
        let subtrees = unsafe { &mut *self.subtrees.get() };
        subtrees.push(Box::new(CachedSubtree {
            taken: Cell::new(false),
            subtree: UnsafeCell::new(subtree),
        }));
        NonNull::from(&**subtrees.last().expect("an entry was pushed"))
    }

    pub(crate) fn mark_deleted(&self, path: SubtreePathBuilder<'b, B>) {
        // SAFETY: there are no other references to `merks` memory at the same time.
        // Note while it's possible to have direct references to actual Merk trees,
        // outside of the scope of this function, this map (`merks`) holds
        // pointers to their entries and nothing of them, thus there are no
        // overlapping references, and that is requirement of `UnsafeCell` we
        // have there.
        let merks = unsafe {
            self.merks
                .get()
                .as_mut()
                .expect("`UnsafeCell` is never null")
        };

        match merks.entry(path) {
            Entry::Occupied(cached) => {
                // SAFETY: an entry lives as long as the cache. The flag is
                // not set, so no closure has the subtree, and no other
                // reference to it lives across a call.
                let cached = unsafe { cached.get().as_ref() };
                cached.assert_not_taken();
                unsafe { *cached.subtree.get() = Subtree::Deleted };
            }
            Entry::Vacant(vacant) => {
                vacant.insert(self.keep(Subtree::Deleted));
            }
        }
    }

    /// Open Merk using data from parent subtree, returning errors in case
    /// parent element isn't a subtree.
    ///
    /// If there is no parent subtree in the cache `None` will be returned
    /// instead of Merk.
    fn try_open_merk_using_cached_parent<'m>(
        &self,
        merks: &'m Merks<'db, 'b, B, E>,
        batch: &'db E::Batch,
        path: SubtreePathBuilder<'b, B>,
    ) -> CostResult<Option<TxMerk<'db, E>>, Error> {
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

        // SAFETY: an entry lives as long as the cache. The flag is not set,
        // so no closure has the parent, and the borrow of its subtree ends
        // before the cache is reached again.
        let parent_merk = unsafe { parent_merk.as_ref() };
        parent_merk.assert_not_taken();

        let mut cost = Default::default();
        let merk = match unsafe { &*parent_merk.subtree.get() } {
            Subtree::LoadedMerk(merk) => {
                if let Some((root_key, tree_type)) = cost_return_on_error!(
                    &mut cost,
                    Element::get(merk, parent_key, true, self.version)
                        .map_ok(|element| element.root_key_and_tree_type_owned())
                        .map_err(|e| match e {
                            grovedb_merk::error::Error::PathKeyNotFound(s) =>
                                Error::PathKeyNotFound(s),
                            e => Error::MerkError(e),
                        })
                ) {
                    let storage = self
                        .db
                        .db
                        .subtree((&path).into(), Some(batch), self.tx)
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
                return Err(Error::MerkCacheSubtreeDeleted("parent was deleted"))
                    .wrap_with_cost(cost)
            }
        };

        Ok(Some(merk)).wrap_with_cost(cost)
    }

    /// Gets a smart pointer to a cached Merk or opens one if needed.
    pub(crate) fn get_merk<'c>(
        &'c self,
        path: SubtreePathBuilder<'b, B>,
    ) -> CostResult<MerkHandle<'db, 'c, E>, Error> {
        let mut cost = Default::default();

        // SAFETY: there are no other references to `merks` memory at the same time.
        // Note while it's possible to have direct references to actual Merk trees,
        // outside of the scope of this function, this map (`merks`) holds
        // pointers to their entries and nothing of them, thus there are no
        // overlapping references, and that is requirement of `UnsafeCell` we
        // have there.
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
        let batch = self.external_batch.unwrap_or_else(|| unsafe {
            (&*self.batch as *const E::Batch)
                .as_ref()
                .expect("`Box` is never null")
        });

        // SAFETY, for each use of an entry below: an entry is on the heap for
        // as long as the cache lives, and only `propagate_subtrees`, which
        // has the cache to itself, takes its pointer out of the map. It is
        // reached through shared references made from that pointer, and its
        // subtree is read or written here only once the flag has said that
        // no closure has it.
        let cached = if let Some(cached) = merks.get(&path).copied() {
            let entry = unsafe { cached.as_ref() };
            // While we can be certain that no one can conflict for the memory of flag and
            // subtree's pointer, the subtree itself can be referred from
            // outside and we have to check the flag:
            entry.assert_not_taken();

            let deleted = match unsafe { &*entry.subtree.get() } {
                // Cache hit, all good:
                Subtree::LoadedMerk(_) => false,
                // Cache hit, but marked as deleted, need to look at the parent to see whether it
                // was re-inserted:
                Subtree::Deleted => true,
            };
            if deleted {
                match cost_return_on_error!(
                    &mut cost,
                    self.try_open_merk_using_cached_parent(merks, batch, path)
                ) {
                    Some(merk) => {
                        // Parent data indicates that Merk was re-inserted
                        unsafe { *entry.subtree.get() = Subtree::LoadedMerk(merk) };
                    }
                    None => {
                        // This should not happen: subtree is marked as deleted,
                        // but no operations on parent are performed (element deletion is
                        // required as well by GroveDb
                        // structure requirements)
                        return Err(Error::InternalError(
                            "Subtree is marked as deleted, but parent wasn't updated".to_owned(),
                        ))
                        .wrap_with_cost(cost);
                    }
                }
            }

            cached
        } else {
            // Cache miss, Merk needs to be loaded, either from the storage or from the
            // cached parent if it is present:
            match cost_return_on_error!(
                &mut cost,
                self.try_open_merk_using_cached_parent(merks, batch, path.clone())
            ) {
                Some(merk) => match merks.entry(path) {
                    Entry::Vacant(e) => *e.insert(self.keep(Subtree::LoadedMerk(merk))),
                    Entry::Occupied(e) => {
                        // This cannot happen since it's a cache miss branch, but whatever
                        let cached = *e.get();
                        let entry = unsafe { cached.as_ref() };
                        entry.assert_not_taken();
                        unsafe { *entry.subtree.get() = Subtree::LoadedMerk(merk) };
                        cached
                    }
                },
                None => {
                    // No cached merk nor parent, going into storage:
                    let merk = cost_return_on_error!(
                        &mut cost,
                        self.db.open_transactional_merk_at_path(
                            (&path).into(),
                            self.tx,
                            Some(batch),
                            self.version,
                        )
                    );
                    *merks
                        .entry(path)
                        .or_insert_with(|| self.keep(Subtree::LoadedMerk(merk)))
                }
            }
        };

        // SAFETY: `MerkHandle` contains a pointer and a reference to the heap
        // allocated entry, and we want to be sure that the referenced data will
        // outlive them plus borrowing rules aren't violated (one `&mut` or many
        // `&` with no `&mut` at a time).
        //
        // To make sure changes to the map won't affect existing borrows the map
        // holds a pointer to each entry and the entry itself is on the heap, that
        // allows us to move and update `MerkCache` with new subtrees and possible
        // reallocations without breaking `MerkHandle`'s references. We use
        // `UnsafeCell` to connect lifetimes and check in compile time that
        // `MerkHandle`s won't outlive the cache, even though we don't hold any
        // references to it, but `&mut` reference would make this borrow exclusive
        // for the whole time of `MerkHandle`, so it shall go initially through a
        // shared reference.
        //
        // Borrowing rules are covered using a borrow flag of each Merk:
        // 1. Borrow flag's reference points to a heap allocated memory and will remain
        //    valid. Since the reference is shared and no need to obtain a `&mut`
        //    reference this part of the memory is covered.
        // 2. For the same reason the pointer to the subtree's cell stays valid,
        //    because the entry is on the heap for as long as the cache lives and
        //    `MerkHandle` can't outlive it since we use lifetime parameters. Every
        //    handle of one subtree takes its pointer from the cell through a
        //    shared reference to the entry, so making one handle does not end
        //    the use of another.
        // 3. We can get unique reference out of that pointer safely because of
        //    borrowing flag.
        let entry = unsafe { cached.as_ref() };
        Ok(MerkHandle {
            merk: entry.subtree.get(),
            taken_handle: &entry.taken,
            batch: self.external_batch.unwrap_or(&self.batch),
        })
        .wrap_with_cost(cost)
    }

    /// Consumes `MerkCache` into accumulated batch of uncommitted operations
    /// with subtrees' root hash propagation done.
    #[cfg(test)]
    pub(crate) fn into_batch(mut self) -> CostResult<Box<E::Batch>, Error> {
        let mut cost = Default::default();
        cost_return_on_error!(&mut cost, self.propagate_subtrees());

        // SAFETY: By this time all Merks are taken out of their entries and
        // dropped during propagation, so there are no more references to the
        // batch and in can be safely released into the world.
        Ok(self.batch).wrap_with_cost(cost)
    }

    fn propagate_subtrees(&mut self) -> CostResult<(), Error> {
        let mut cost = Default::default();

        // SAFETY, in the loop: the cache has itself to itself, so no handle is
        // alive but the one the loop takes for a parent, which is gone by the
        // next turn, and nothing else reaches an entry.
        //
        // This relies on [SubtreePath]'s ordering implementation to put the deepest
        // path's first.
        while let Some((path, cached)) = self.merks.get_mut().pop_first() {
            let subtree = unsafe { &mut *cached.as_ref().subtree.get() };
            let Subtree::LoadedMerk(merk) = std::mem::replace(subtree, Subtree::Deleted) else {
                continue;
            };

            if let Some((parent_path, parent_key)) = path.derive_parent_owned() {
                // Error handling here ensures that it is not a major issue if the
                // parent Merk was marked as deleted. `MerkCache` is not responsible for
                // determining how a subtree ended up deleted, especially when some of
                // its child subtrees still have changes. This situation can arise when
                // a more efficient deletion process occurs outside the cache without
                // spending extra time marking entries within it, but still marking the
                // root of deletion as gone to prevent further propagations and wrong
                // re-insertions.
                let mut parent_merk = match self.get_merk(parent_path).unwrap_add_cost(&mut cost) {
                    Ok(merk) => merk,
                    Err(Error::MerkCacheSubtreeDeleted(_)) => continue,
                    // The parent element is already gone from ITS parent (a
                    // recursive deletion removed it without marking every
                    // descendant in this cache) — same situation as the
                    // explicit deleted marker above, so propagate nothing.
                    Err(Error::PathKeyNotFound(_)) => continue,
                    Err(e) => return Err(e).wrap_with_cost(cost),
                };

                let (root_hash, root_key, aggregate_data) = cost_return_on_error!(
                    &mut cost,
                    merk.root_hash_key_and_aggregate_data()
                        .map_err(Error::MerkError)
                );
                cost_return_on_error!(
                    &mut cost,
                    parent_merk.for_merk(|m| {
                        // This reference-cache executor does not mirror secondary
                        // rows. Refuse before commit; full batches support the
                        // indexed propagation path and can perform this mutation.
                        if m.tree_type.is_indexed_primary() {
                            return Err(Error::NotSupported(
                                "live backward-reference maintenance below indexed primaries requires a full batch".to_owned(),
                            )).wrap_with_cost(Default::default());
                        }
                        crate::update_tree_item_preserve_flag(
                            m, parent_key, root_key, root_hash, aggregate_data, self.version,
                        )
                    })
                );
            }
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Finalize automatic maintenance into the caller-owned batch.
    pub(crate) fn finish_prepared(mut self) -> CostResult<(), Error> {
        self.propagate_subtrees()
    }
}

/// Wrapper over `Merk` tree to manage unique borrow dynamically.
pub(crate) struct MerkHandle<'db, 'c, E: Engine> {
    merk: *mut Subtree<'db, E>,
    taken_handle: &'c Cell<bool>,
    /// The cache's shared batch, so every `for_merk` call can mark itself
    /// as a new operation (see [`StorageBatch::next_operation`]).
    batch: &'c E::Batch,
}

impl<'db, E: Engine> MerkHandle<'db, '_, E> {
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
        f: impl FnOnce(&mut TxMerk<'db, E>) -> CostResult<T, Error>,
    ) -> CostResult<T, Error> {
        if self.taken_handle.get() {
            panic!("Attempt to have double &mut borrow on Merk");
        }

        self.taken_handle.set(true);

        // Every closure is one Merk operation sharing the cache's batch.
        // Marking the boundary lets a later operation's delete supersede an
        // earlier operation's put of the same key (a cascade deleting a node
        // that the preceding delete's rebalancing just rewrote), while the
        // put-wins rule Merk's rebalancing relies on still holds within the
        // operation.
        self.batch.next_operation();

        // SAFETY: here we want to have `&mut` reference to Merk out of a pointer, there
        // is a checklist for that:
        // 1. Memory is valid, because `MerkHandle` can't outlive `MerkCache` and heap
        //    allocated Merks stay at their place for the whole `MerkCache` lifetime.
        // 2. No other references exist because of `taken_handle` check above.
        let subtree = unsafe { self.merk.as_mut().expect("a cell's pointer is never null") };
        let Subtree::LoadedMerk(merk) = subtree else {
            // Release the borrow flag before bailing — otherwise every
            // subsequent `for_merk` on this handle would panic with a
            // phantom double-borrow.
            self.taken_handle.set(false);
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

#[cfg(all(test, grovedb_test_engine))]
mod tests {
    use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, CostsExt};
    use grovedb_merk::{
        element::{
            delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
            insert::ElementInsertToStorageExtensions,
        },
        TreeType,
    };
    use grovedb_path::{SubtreePath, SubtreePathBuilder};
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;

    use super::MerkCache;
    use crate::{
        tests::{make_deep_tree, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    #[test]
    fn two_handles_of_one_subtree_are_used_in_turn() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(version);
        let tx = db.start_transaction();
        let cache = MerkCache::<[u8; 0], _>::new(&db, &tx, version);
        let path = || SubtreePathBuilder::owned_from_iter([TEST_LEAF, b"innertree"]);
        let item = Element::new_item(b"hello".to_vec());
        type Cached<'db> = super::TxMerk<'db, crate::engine::DefaultEngine>;
        let write = |key: &'static [u8]| {
            let item = item.clone();
            move |merk: &mut Cached<'_>| {
                item.insert(merk, key, None, version)
                    .map_err(Error::MerkError)
            }
        };
        let holds = |key: &'static [u8]| {
            let item = item.clone();
            move |merk: &mut Cached<'_>| {
                Element::get(merk, key, true, version)
                    .map_ok(|found| found == item)
                    .map_err(Error::MerkError)
            }
        };

        // Each handle is taken while the ones before it are alive, and an
        // older one is used after a newer one was taken and used.
        let mut first = cache.get_merk(path()).unwrap().unwrap();
        let mut second = cache.get_merk(path()).unwrap().unwrap();
        first.for_merk(write(b"k1")).unwrap().unwrap();
        assert!(second.for_merk(holds(b"k1")).unwrap().unwrap());
        let mut third = cache.get_merk(path()).unwrap().unwrap();
        third.for_merk(write(b"k2")).unwrap().unwrap();
        assert!(first.for_merk(holds(b"k2")).unwrap().unwrap());
        second.for_merk(write(b"k3")).unwrap().unwrap();
        assert!(third.for_merk(holds(b"k3")).unwrap().unwrap());

        // The subtree is marked as deleted under the three, and each of
        // them finds it gone.
        cache.mark_deleted(path());
        for handle in [&mut first, &mut second, &mut third] {
            let gone = handle.for_merk(holds(b"k1")).unwrap();
            assert!(matches!(gone, Err(Error::InternalError(_))), "{gone:?}");
        }
    }

    #[test]
    #[should_panic]
    fn cant_borrow_twice() {
        let version = GroveVersion::latest();
        let db = make_test_grovedb(version);
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
        let db = make_test_grovedb(version);
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
        let db = make_deep_tree(version);
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
        let db = make_deep_tree(version);
        let tx = db.start_transaction();

        let cache = MerkCache::new(&db, &tx, version);

        cache.mark_deleted(SubtreePathBuilder::new());

        assert!(matches!(
            cache
                .get_merk(SubtreePathBuilder::owned_from_iter([TEST_LEAF]))
                .unwrap(),
            Err(Error::MerkCacheSubtreeDeleted(_))
        ));
    }

    #[test]
    fn subtrees_are_propagated() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(version);
        let tx = db.start_transaction();

        let path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let item = Element::new_item(b"hello".to_vec());

        let no_propagation_ops_count = {
            let batch = StorageBatch::new();

            let mut merk = db
                .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), version)
                .unwrap()
                .unwrap();

            item.insert(&mut merk, b"k1", None, version)
                .unwrap()
                .unwrap();

            batch.len()
        };

        let cache = MerkCache::new(&db, &tx, version);

        let mut merk = cache.get_merk(path.derive_owned()).unwrap().unwrap();

        merk.for_merk(|m| {
            item.insert(m, b"k1", None, version)
                .map_err(Error::MerkError)
        })
        .unwrap()
        .unwrap();

        drop(merk);

        assert!(cache.into_batch().unwrap().unwrap().len() > no_propagation_ops_count);
    }

    #[test]
    fn deleted_subtree_can_be_reinserted() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(version);
        let tx = db.start_transaction();
        let cache = MerkCache::<[u8; 0], _>::new(&db, &tx, version);

        let mut parent_merk = cache
            .get_merk(SubtreePathBuilder::owned_from_iter([TEST_LEAF]))
            .unwrap()
            .unwrap();

        // Delete child subtree element
        parent_merk
            .for_merk(|m| {
                Element::delete_with_sectioned_removal_bytes(
                    m,
                    b"innertree",
                    None,
                    true,
                    TreeType::NormalTree,
                    &mut |_, removed_key_bytes, removed_value_bytes| {
                        Ok((
                            StorageRemovedBytes::BasicStorageRemoval(removed_key_bytes),
                            StorageRemovedBytes::BasicStorageRemoval(removed_value_bytes),
                        ))
                    },
                    version,
                )
                .map_err(Error::MerkError)
            })
            .unwrap()
            .unwrap();

        // Mark child subtree as deleted in cache (should be done by deletion in
        // GroveDb, but we keep it local for now, emulating the logic)
        cache.mark_deleted(SubtreePathBuilder::owned_from_iter([
            TEST_LEAF,
            b"innertree",
        ]));

        // Attempt to open merk that was deleted shall fail
        assert!(matches!(
            cache
                .get_merk(SubtreePathBuilder::owned_from_iter([
                    TEST_LEAF,
                    b"innertree"
                ]))
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));

        // Let's insert empty tree at that old place in parent
        parent_merk
            .for_merk(|m| {
                Element::empty_tree()
                    .insert(m, b"innertree", None, version)
                    .map_err(Error::MerkError)
            })
            .unwrap()
            .unwrap();

        // Shall be able to have a merk handle now (note it won't be empty, because we
        // applied no operations to clean it up):
        assert!(cache
            .get_merk(SubtreePathBuilder::owned_from_iter([
                TEST_LEAF,
                b"innertree",
            ]))
            .unwrap()
            .is_ok());
    }

    #[test]
    fn open_subtree_checks_on_parent() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(version);
        let tx = db.start_transaction();

        let cache = MerkCache::<[u8; 0], _>::new(&db, &tx, version);
        let cost_with_no_cached_parent = cache
            .get_merk(SubtreePathBuilder::owned_from_iter([
                TEST_LEAF,
                b"innertree",
            ]))
            .cost;

        // This time with fresh cache we load parent first:

        let cache = MerkCache::<[u8; 0], _>::new(&db, &tx, version);

        cache
            .get_merk(SubtreePathBuilder::owned_from_iter([TEST_LEAF]))
            .unwrap()
            .unwrap();

        let cost_with_cached_parent = cache
            .get_merk(SubtreePathBuilder::owned_from_iter([
                TEST_LEAF,
                b"innertree",
            ]))
            .cost;

        assert!(
            cost_with_cached_parent.storage_loaded_bytes
                < cost_with_no_cached_parent.storage_loaded_bytes
        );
    }
}
