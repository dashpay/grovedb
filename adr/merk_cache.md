# Merk Cache

An easy to use answer to GroveDB's implementation challenges.

Previous read: [[atomicity]].

Mandatory operation batching and deferred application, which are used for all GroveDB
updates, separate the state into two perspectives: one is the current state reflected in
the running transaction, and the other is a hypothetical state that will become real after
the batch is committed.

A common scenario is using the updated "state" for further operation processing, such
as propagating updated hashes upwards. Accessing the transaction state is not an option
in this case because we have just performed an update. However, we do have an in-memory
representation of the data we are working with: Merk trees. The updates made to them are
reflected in their structure, and for any missing data, they fall back to storage (the
transaction state in this case).

However, losing the handle to a Merk tree means losing access to the uncommitted state.
Reopening the Merk tree will only provide access to the transaction state, which does not
reflect the pending changes inside the storage batch. To solve this problem and to keep
Merks open as long as needed, we introduce `MerkCache`.

## Usage

### Working with a Merk tree

Since `MerkCache` manages the lifecycle of Merk handles, it replaces the previous approach
of opening a Merk instance using GroveDB’s internal helpers. Now, this is done via
`MerkCache::get_merk`.

To maintain control over the Merk tree’s lifecycle, `get_merk` returns a `MerkHandle`
instead of a direct `Merk` reference. For safety reasons (discussed in the next section),
`MerkHandle` cannot be dereferenced into `Merk`. Instead, the `MerkHandle::for_merk`
method is provided, which accepts a closure with a single argument -- a mutable reference
to the desired `Merk` instance. This allows data to be returned to the outer scope while
ensuring safe access.

Since the reference to `Merk` is unique, nested calls to `for_merk` that attempt to
access the same `Merk` instance simultaneously will result in a panic. Additionally, due
to implementation details, the parent `Merk` might already be in use behind the scenes,
further increasing the risk of a panic. Therefore, caution is advised when making nested
`for_merk` calls -- ideally, they should be avoided altogether.

On the other hand, holding multiple `MerkHandle` instances is not only safe but also
recommended, as it avoids additional lookups in `MerkCache`. However, if a handle is lost,
it is not an issue, since it can be retrieved again with a lookup when needed and without
reopening a `Merk` -- one of the reasons `MerkCache` was introduced in the first place.

### Subtree deletion

If a `Merk` is semantically deleted, `MerkCache` must be explicitly notified using the
`MerkCache::mark_deleted` method. This ensures that any subsequent attempt to retrieve
it will explicitly check the parent entry to determine whether it was reinserted. If it
wasn't, the operation will result in an error.

This operation requires a borrow, just like `for_merk`, so it should preferably not be
nested within one.

### Finalization

`MerkCache` receives a transaction upon initialization but no storage batch, as it relies
on the transaction state and evolves on top of it. Since no data is committed until
explicitly done at the end of the GroveDB operation, `MerkCache` finalizes its usage with
a delta of the transaction state. All modifications made to `MerkCache` are returned as
a storage batch via the `MerkCache::into_batch` method, which consumes the cache and,
through Rust lifetimes, renders all `MerkHandle`s inaccessible. The returned storage batch
can then be merged with the main batch, though this is not a concern of `MerkCache`.

## Implementation details

The concept of this type of cache is not new and was previously achieved in GroveDB
using a `HashMap`. However, due to its constraints, managing multiple `Merk` references
was problematic, as obtaining a mutable reference to a `HashMap` entry restricted access
to the entire `HashMap`. To work around this limitation, a hack was used: entries were
temporarily removed from the cache to detach ownership, then reinserted afterward. This
approach was inefficient and error-prone, as it introduced the risk of forgetting to
reinsert entries. Overall, it was a fragile, ad-hoc solution that required excessive
manual handling, and with the newest requirements (known as [[bidirectional_references]])
increasing the demand for caching, a better, reusable solution was needed.

A better solution is defined in the `merk_cache.rs` module under the GroveDB root, already
known as `MerkCache`.

### The HashMap "problem"

As stated above, taking a unique reference to an entry inside the `HashMap` requires a
unique reference to the entire structure itself. This is done for a reason: mutations to
the structure can trigger reallocations or other changes that would invalidate existing
references, and Rust protects us from this. However, when Rust's restrictions are too
strict, we can take matters into our own hands using `unsafe`. This is one of the reasons
why `MerkCache` has so few limitations -- it is tailored specifically to our task, whereas
standard collections are general and defensive, as they should be.

The underlying implementation still uses a map structure (`BTreeMap` in our case, to
maintain ordered paths for subtrees), but it employs indirection, so when the structure's
memory alignment changes, it doesn't affect the actual Merks. Additionally, we don't
remove entries at all.

### Ensuring Safety

By avoiding the limitations of a map structure, we introduce a burden of unsafe code
that requires careful understanding and support.

The explanations will follow a bottom-up approach.

#### MerkHandle

```rust
/// Wrapper over `Merk` tree to manage unique borrow dynamically.
#[derive(Clone, Debug)]
pub(crate) struct MerkHandle<'db, 'c> {
    merk: *mut Subtree<'db>,
    taken_handle: &'c Cell<bool>,
}
```

Here, `Subtree` is an enum that represents either a `Merk` or a state marked as deleted
(note that the entry remains at the same place in memory and is valid regardless of its
"deletion" status).

`MerkHandle` is the only way to obtain a `&mut Merk` when using `MerkCache` via `for_merk`
method. The conversion from raw pointer to a unique reference that happens at the time of
this call of the subtree is safe due to the following reasons:

1. Lifetime `'c` is tied to `MerkCache`, ensuring its presence for the duration of the
   operation.
2. The ownership of `Subtree` is indirect, meaning that changes in the map structure (such
   as reallocations or memory movements) won't affect its memory address or validity.
3. `MerkCache` never frees this memory until the very end, either by consuming `self`
   or via a destructor that requires `&mut self`. Both operations are exclusive to `'c` of
   `MerkHandle` (from point 1).
4. The reference is unique because `taken_handle` is checked before conversion, and it is
   set during the `for_merk` call. Sharing the `Cell` reference is safe because the purpose
   of `Cell` is to provide interior mutability with shared access.

#### CachedMerkEntry

```rust
/// We store Merk on heap to preserve its location as well as borrow flag alongside.
type CachedMerkEntry<'db> = Box<(Cell<bool>, Subtree<'db>)>;
```

An immovable allocation that `MerkCache` owns and `MerkHandle` refers to. `MerkHandle`
holds a shared reference to this `Cell` and a pointer to `Subtree`, without taking a
reference unless needed.

#### Merks

```rust
type Merks<'db, 'b, B> = BTreeMap<SubtreePathBuilder<'b, B>, CachedMerkEntry<'db>>;
```

This is the heart of `MerkCache`, where paths are mapped to Merks wrapped with metadata.
The `BTreeMap` ensures that data is stored in order, with its ordering implementation
placing the longest paths first.

As `CachedMerkEntry` is a `Box`, this creates an indirection between the `Merks` memory
where it stores values and the actual data of the Merk.

Another implementation detail: `MerkCache` propagates hash updates from modified
subtrees to their parents up to the GroveDB root. This propagation happens during batch
finalization, and maintaining cached items in order greatly aids this process.

#### MerkCache

The final structure.

```rust
/// Structure to keep subtrees open in memory for repeated access.
pub(crate) struct MerkCache<'db, 'b, B: AsRef<[u8]>> {
    db: &'db GroveDb,
    pub(crate) version: &'db GroveVersion,
    batch: Box<StorageBatch>,
    tx: &'db Transaction<'db>,
    merks: UnsafeCell<Merks<'db, 'b, B>>,
}
```

Many of the fields are references, as `MerkCache` delegates storage interactions before
data gets cached.

The storage batch is owned because the result of `MerkCache` usage is a batch that it
builds over time. It also has a layer of indirection via `Box`, as cached Merks hold a
reference to the batch. This makes the entire `MerkCache` self-referential, requiring
indirection to ensure safety in case the cache value is moved.

Wrapping `Merks` in `UnsafeCell` is somewhat redundant.

We use lifetimes to enforce at compile time that `MerkHandle`s won't outlive the
cache, even though we don’t hold any direct references to it. A `&mut` reference on
`get_merk` call would make this borrow exclusive for the entire lifetime of the returned
`MerkHandle`, which is not an option since we want to have multiple `MerkHandle`s at the
same time. Therefore, it must initially go through a shared reference. However, with a
shared reference, we wouldn't be able to update the `BTreeMap` with new entries.

That's why `UnsafeCell` is needed -- any mutation that goes through a shared reference
must happen inside `UnsafeCell`, or it results in undefined behavior.
