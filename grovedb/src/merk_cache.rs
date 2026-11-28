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
    /// Subtree whose Merk was dropped because a closure that had it to
    /// write returned an error. A Merk whose write failed may not hold the
    /// tree that its store and the batch hold: a write that fails in its
    /// walk leaves it with no tree, and one that fails in its commit with a
    /// tree that was not written. Nothing reaches the subtree again through
    /// this cache, and an end of the cache that finds the subtree failed
    /// opens and writes no Merk.
    Failed,
}

/// The error of reaching a subtree again after a write of its Merk failed.
/// The error of that write ends the operation, so this one means that a
/// caller went on after it.
fn failed_merk() -> Error {
    Error::CorruptedCodeExecution("a Merk of the cache was reached after a write of it failed")
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
                let subtree = unsafe { &mut *cached.subtree.get() };
                if !matches!(subtree, Subtree::Failed) {
                    *subtree = Subtree::Deleted
                }
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
            Subtree::Failed => return Err(failed_merk()).wrap_with_cost(cost),
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
                Subtree::Failed => return Err(failed_merk()).wrap_with_cost(cost),
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

        // SAFETY, here and in the loop: the cache has itself to itself, so
        // no handle is alive but the one the loop takes for a parent, which
        // is gone by the next turn, and nothing else reaches an entry.
        let mut entries = self.merks.get_mut().values();
        if entries
            .any(|cached| matches!(unsafe { &*cached.as_ref().subtree.get() }, Subtree::Failed))
        {
            return Err(failed_merk()).wrap_with_cost(cost);
        }

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
    ///
    /// The closure may write the Merk, so an error of it drops the Merk from
    /// the cache: the handle and the cache then answer with an error for the
    /// subtree, and an end of the cache that is begun afterwards opens and
    /// writes no Merk. A closure that only reads goes through
    /// [`Self::read_merk`].
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
        self.borrow_merk(true, f)
    }

    /// Borrow Merk for a closure that only reads it. The shared borrow keeps
    /// the write methods of `Merk` out of the closure's reach, and an error
    /// of the closure leaves the Merk in the cache. The closures GroveDB
    /// passes read an element by its key or ask whether the Merk is empty,
    /// and neither changes the tree the Merk holds, whatever it returns.
    ///
    /// The borrow is exclusive and marks an operation boundary in the
    /// batch, as the one of [`Self::for_merk`] does.
    /// # Panics
    /// As [`Self::for_merk`].
    pub(crate) fn read_merk<T>(
        &mut self,
        f: impl FnOnce(&TxMerk<'db, E>) -> CostResult<T, Error>,
    ) -> CostResult<T, Error> {
        self.borrow_merk(false, |merk| f(merk))
    }

    fn borrow_merk<T>(
        &mut self,
        writes: bool,
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
        let merk = match subtree {
            Subtree::LoadedMerk(merk) => merk,
            gone => {
                // Release the borrow flag before bailing — otherwise every
                // subsequent `for_merk` on this handle would panic with a
                // phantom double-borrow.
                self.taken_handle.set(false);
                let error = match gone {
                    Subtree::Failed => failed_merk(),
                    _ => Error::InternalError("accessing subtree that was deleted".to_owned()),
                };
                return Err(error).wrap_with_cost(Default::default());
            }
        };

        let result = f(merk);

        if writes && result.value.is_err() {
            *subtree = Subtree::Failed;
        }
        self.taken_handle.set(false);

        result
    }
}

/// What the cache does with a Merk whose write failed, as a test that the
/// tests of each engine run.
#[cfg(test)]
pub(crate) mod failed_writes {
    use grovedb_costs::{CostResult, OperationCost};
    use grovedb_merk::element::{
        get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
    };
    use grovedb_path::SubtreePathBuilder;
    use grovedb_version::version::GroveVersion;

    use super::MerkCache;
    use crate::{
        tests::faultable_reads::{
            with_failed_parent_read, with_one_failed_fetch, RefusedReads, INJECTED_READ_FAILURE,
        },
        Element, Engine, Error, GroveDb,
    };

    fn refused_by_the_store<T: std::fmt::Debug>(result: Result<T, Error>) {
        let error = result.expect_err("a read that the store refused");
        assert!(error.to_string().contains(INJECTED_READ_FAILURE), "{error}");
    }

    fn reached_after_a_failed_write<T: std::fmt::Debug>(result: Result<T, Error>) {
        let error = result.expect_err("a Merk whose write failed");
        assert!(matches!(error, Error::CorruptedCodeExecution(_)), "{error}");
    }

    /// A Merk of the cache whose store refuses a read inside a write: the
    /// write fails with the store's error, and from then on both handles
    /// of the subtree and the cache answer for it with an error, and the
    /// end of the cache fails before it opens or writes a Merk. A read
    /// that fails, or that finds nothing, leaves the Merk where it is.
    pub(crate) fn a_merk_whose_write_failed_is_dropped<E: Engine>() {
        let version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<RefusedReads<E>>::open_with_engine(dir.path()).expect("a database");
        let item = Element::new_item(b"value".to_vec());
        let none: &[&[u8]] = &[];
        let tree: &[&[u8]] = &[b"tree"];
        // The Merk of `tree` holds `b` with `a` and `c` under it, and `a`
        // is a tree. Beside it, `other` holds the tree `inner`.
        let other: &[&[u8]] = &[b"other"];
        let trees: [(&[&[u8]], &[u8]); 3] = [(none, b"tree"), (none, b"other"), (other, b"inner")];
        for (path, key) in trees {
            db.insert(path, key, Element::empty_tree(), None, None, version)
                .unwrap()
                .expect("a tree");
        }
        for (key, element) in [
            (b"b", item.clone()),
            (b"a", Element::empty_tree()),
            (b"c", item.clone()),
        ] {
            db.insert(tree, key, element, None, None, version)
                .unwrap()
                .expect("an element");
        }
        let root = db.root_hash(None, version).unwrap().expect("a root");

        let tx = db.start_transaction();
        let cache = MerkCache::<[u8; 0], _>::new(&db, &tx, version);
        let path = || SubtreePathBuilder::owned_from_iter([b"tree".as_slice()]);
        let below = || SubtreePathBuilder::owned_from_iter([b"tree".as_slice(), b"a"]);
        let read = |key: &'static [u8]| {
            move |merk: &super::TxMerk<'_, RefusedReads<E>>| {
                Element::get(merk, key, true, version).map_err(Error::MerkError)
            }
        };
        let mut handle = cache.get_merk(path()).unwrap().expect("the Merk");
        // A second handle of the subtree, taken before anything is done
        // through the first and used in turn with it.
        let mut beside = cache.get_merk(path()).unwrap().expect("the Merk again");

        // A read that finds nothing and a read that the store refuses are
        // errors that leave the Merk in the cache.
        let missing = handle.read_merk(read(b"nothing")).unwrap();
        assert!(matches!(missing, Err(Error::MerkError(_))), "{missing:?}");
        let refused = with_failed_parent_read(b"c", || handle.read_merk(read(b"c")).unwrap());
        refused_by_the_store(refused);
        let found = handle.read_merk(read(b"c")).unwrap();
        assert_eq!(found.expect("the element"), item);
        let found = handle.for_merk(|merk| read(b"c")(merk)).unwrap();
        assert_eq!(found.expect("the element"), item);
        let found = beside.read_merk(read(b"c")).unwrap();
        assert_eq!(found.expect("the element"), item);

        // Another Merk of the cache is written, two trees deep: its root
        // hash is owed to the Merk above it when the cache ends.
        let inner = SubtreePathBuilder::owned_from_iter([b"other".as_slice(), b"inner"]);
        let mut written = cache.get_merk(inner).unwrap().expect("the Merk");
        let write = written.for_merk(|merk| {
            item.insert(merk, b"e", None, version)
                .map_err(Error::MerkError)
        });
        write.unwrap().expect("a write");

        // A write whose walk is refused the fetch of `c`.
        let (written, fired) = with_one_failed_fetch(b"c", || {
            handle
                .for_merk(|merk| {
                    item.insert(merk, b"d", None, version)
                        .map_err(Error::MerkError)
                })
                .unwrap()
        });
        assert!(fired);
        refused_by_the_store(written);

        // Nothing reaches the Merk again: not a closure of the handle that
        // wrote or of the other one, not a new handle, not the open of a
        // Merk under it, and not after the subtree was marked as deleted.
        let reached = |_: &mut super::TxMerk<'_, RefusedReads<E>>| -> CostResult<(), Error> {
            panic!("the Merk was handed out after its write failed")
        };
        reached_after_a_failed_write(handle.for_merk(reached).unwrap());
        reached_after_a_failed_write(handle.read_merk(read(b"c")).unwrap());
        reached_after_a_failed_write(beside.for_merk(reached).unwrap());
        reached_after_a_failed_write(beside.read_merk(read(b"c")).unwrap());
        reached_after_a_failed_write(cache.get_merk(path()).unwrap().map(|_| ()));
        reached_after_a_failed_write(cache.get_merk(below()).unwrap().map(|_| ()));
        cache.mark_deleted(path());
        reached_after_a_failed_write(cache.get_merk(path()).unwrap().map(|_| ()));
        // And the end of the cache opens and writes nothing, which costs
        // nothing: the root hash of the Merk that was written is carried
        // nowhere.
        let ended = cache.into_batch();
        assert_eq!(ended.cost, OperationCost::default());
        reached_after_a_failed_write(ended.unwrap().map(|_| ()));

        // The refusal went with its scope, and the next operation makes the
        // write.
        drop(tx);
        db.insert(tree, b"d", item.clone(), None, None, version)
            .unwrap()
            .expect("the insert, made again");
        assert_ne!(db.root_hash(None, version).unwrap().expect("a root"), root);
    }
}

#[cfg(all(test, grovedb_test_engine))]
mod tests {
    use crate::tests::SuiteBatch;
    use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, CostsExt};
    use grovedb_merk::{
        element::{
            delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
            insert::ElementInsertToStorageExtensions,
        },
        TreeType,
    };
    use grovedb_path::{SubtreePath, SubtreePathBuilder};
    use grovedb_version::version::GroveVersion;

    use super::MerkCache;
    use crate::{
        engine::Engine,
        tests::{make_deep_tree, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    #[test]
    fn a_merk_whose_write_failed_is_dropped() {
        super::failed_writes::a_merk_whose_write_failed_is_dropped::<crate::engine::DefaultEngine>(
        );
    }

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
        let root_before = db.root_hash(Some(&tx), version).unwrap().unwrap();

        // An engine whose batch counts its operations shows the propagation
        // as more of them. LigninDB's batch gives no count.
        #[cfg(any(grovedb_test_engine = "rocksdb", grovedb_test_engine = "pinned-reads"))]
        let no_propagation_ops_count = {
            let batch = SuiteBatch::default();

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

        let batch = cache.into_batch().unwrap().unwrap();
        #[cfg(any(grovedb_test_engine = "rocksdb", grovedb_test_engine = "pinned-reads"))]
        assert!(batch.len() > no_propagation_ops_count);

        // On every engine the batch holds the writes up to the root: applied,
        // it moves the root hash.
        db.db
            .apply_batch(*batch, &tx)
            .unwrap()
            .expect("the batch of the cache applies");
        assert_ne!(
            db.root_hash(Some(&tx), version).unwrap().unwrap(),
            root_before
        );
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
