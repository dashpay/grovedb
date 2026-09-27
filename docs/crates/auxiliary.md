# Auxiliary Crates

## grovedb-path - Efficient Path Navigation

### Overview
The Path crate provides zero-copy path manipulation utilities for navigating GroveDB's hierarchical structure. It's designed to minimize allocations while providing ergonomic APIs for path operations.

### Core Components

#### SubtreePath
A non-owning view into a path:
```rust
pub struct SubtreePath<'a, B> {
    inner: SubtreePathInner<'a, B>,
}

enum SubtreePathInner<'a, B> {
    None,
    Iterator(Iter<'a, B>),
    Builder(&'a SubtreePathBuilder<B>),
}
```

**Features**:
- Cheap to clone (just copies references)
- Supports iteration over path segments
- Can be created from various sources

#### SubtreePathBuilder
Owned path representation:
```rust
pub struct SubtreePathBuilder<B = Vec<u8>> {
    pub(crate) segments: Vec<B>,
}
```

**Operations**:
- `push()`: Add segment to path
- `parent()`: Get parent path
- `derive_parent()`: Create new parent path
- `to_path()`: Convert to SubtreePath

### Usage Examples
```rust
// Create path from array
let path = SubtreePath::from(&[b"users", b"alice", b"documents"]);

// Build path dynamically
let mut builder = SubtreePathBuilder::new();
builder.push(b"users".to_vec());
builder.push(b"alice".to_vec());

// Iterate segments
for segment in path.iter() {
    println!("Segment: {:?}", segment);
}

// Get parent
let parent = path.derive_parent()?;
```

### Design Benefits
- Zero allocations for common operations
- Flexible input types (arrays, vectors, builders)
- Efficient parent/child navigation
- Display formatting for debugging

---

## grovedb-version - Protocol Version Management

### Overview
Manages versioning across GroveDB to ensure compatibility and enable protocol upgrades. Uses fine-grained version tracking for individual features.

### Version Structure
```rust
pub struct GroveVersion {
    pub protocol_version: u32,
    pub grovedb_versions: GroveDBVersions,
    pub merk_versions: MerkVersions,
}

pub struct GroveDBVersions {
    pub apply_batch: GroveDBApplyBatchVersions,
    pub operations: GroveDBOperationsVersions,
    pub element: GroveDBElementMethodVersions,
    // ... more categories
}
```

### Version Checking
```rust
// Compile-time version check
check_grovedb_v0_with_cost!(
    "insert",
    grove_version.grovedb_versions.operations.insert.insert
);

// Runtime version check
match grove_version.protocol_version {
    1 => handle_v1(),
    2 => handle_v2(),
    _ => return Err(Error::UnsupportedVersion),
}
```

### Key Features
- Hierarchical version organization
- Compile-time and runtime checks
- Smooth upgrade paths
- Feature-specific versioning
- Cost-aware version checks

---

## grovedb-epoch-based-storage-flags - Epoch-Based Storage Management

### Overview
Records, in an element's flag bytes (`ElementFlags = Vec<u8>`), the epoch in which the element's storage was paid for and, optionally, the 32-byte identity that owns it. When an element grows in a later epoch, the new bytes are recorded against that epoch. When it shrinks or is deleted, the removed bytes are attributed back to the epochs that paid for them, newest first. The crate only keeps these records. Turning them into fees or refunds is up to the caller (Dash Platform's Drive).

GroveDB itself only uses this crate in tests (it is a dev-dependency). A client plugs it in through the flag callbacks that GroveDB's batch and delete APIs take (see [Integration with GroveDB](#integration-with-grovedb)).

### Storage Flag Types
```rust
// grovedb-epoch-based-storage-flags/src/lib.rs
pub enum StorageFlags {
    /// Type byte 0
    SingleEpoch(BaseEpoch),
    /// Type byte 1
    MultiEpoch(BaseEpoch, BTreeMap<EpochIndex, BytesAddedInEpoch>),
    /// Type byte 2
    SingleEpochOwned(BaseEpoch, OwnerId),
    /// Type byte 3
    MultiEpochOwned(BaseEpoch, BTreeMap<EpochIndex, BytesAddedInEpoch>, OwnerId),
}

// Private aliases, so callers write the underlying types:
type EpochIndex = u16;
type BaseEpoch = EpochIndex;
type BytesAddedInEpoch = u32;
type OwnerId = [u8; 32];
```

- **Base epoch**: the epoch the element was first stored in. It never changes when the element is updated.
- **Epoch map** (multi-epoch forms only): for each later epoch in which the element grew, how many bytes were added then. The map must not be empty (`serialize` debug-asserts this). Once no later epoch has bytes left, the combine functions switch the flags back to the single-epoch form.
- **Owner id** (owned forms only): the identity charged for, or refunded, the storage.

### Serialization
`serialize()` / `to_element_flags()` write, in this order:

1. The type byte (`0`–`3`, as above; also returned by `type_byte()`).
2. The 32-byte owner id, for the owned forms only. It comes *before* the base epoch, even though it is the last field of the enum variant.
3. The base epoch, 2 bytes big-endian.
4. For the multi-epoch forms, one record per map entry in ascending epoch order: the epoch index (2 bytes big-endian), then the bytes added in that epoch as an unsigned LEB128 varint (`integer-encoding`'s `VarInt` for `u32`, 1–5 bytes).

| Form | Size in bytes |
|---|---|
| `SingleEpoch` | 3 (`SINGLE_EPOCH_FLAGS_SIZE`) |
| `MultiEpoch` | 3 + records (at least 6) |
| `SingleEpochOwned` | 35 |
| `MultiEpochOwned` | 35 + records (at least 38) |

`serialized_size()` computes that length without serializing. `approximate_size(has_owner_id, Some((change_count, varint_len)))` gives the estimate used for cost estimation: `3 + 32·[owned] + change_count·(2 + varint_len)`.

`deserialize(bytes)` (also `from_slice`, `from_element_flags_ref`) returns:
- `Ok(None)` for empty bytes, which means the element has no storage flags.
- `Err(DeserializeUnknownStorageFlagsType)` for a type byte above 3.
- `Err(StorageFlagsWrongSize)` when a single-epoch form is not exactly 3 or 35 bytes, a multi-epoch form is shorter than 6 or 38 bytes, or a varint is truncated.

The decoder does not yet reject every non-canonical multi-epoch encoding, so only store bytes that `serialize()` produced.

### Combining Flags on Update
When an element is overwritten, the old flags (`self`) are combined with the flags the caller wrote for the new value (`rhs`):

```rust
pub fn combine_added_bytes(self, rhs: Self, added_bytes: u32, strategy: MergingOwnersStrategy) -> Result<Self, StorageFlagsError>;
pub fn combine_removed_bytes(self, rhs: Self, removed_bytes: &StorageRemovedBytes, strategy: MergingOwnersStrategy) -> Result<Self, StorageFlagsError>;
```

The result depends on how the two base epochs compare:
- **Same base epoch**: the epoch maps are merged, with `rhs` entries overwriting ours. `added_bytes` and `removed_bytes` are ignored.
- **`rhs` has a later base epoch**: the result keeps our base epoch.
  - `combine_added_bytes` adds `added_bytes` to the map entry for `rhs`'s base epoch, and returns `StorageFlagsOverflow` if the `u32` total overflows.
  - `combine_removed_bytes` subtracts the `SectionedStorageRemoval` amounts listed under our owner id (or `[0; 32]` if unowned) from the non-base epoch entries, skipping the base epoch's amount.
    - A removal that lists more than one identifier, or does not list ours, returns `MergingStorageFlagsFromDifferentOwners`.
    - Removing from an epoch that is not in the map returns `RemovingAtEpochWithNoAssociatedStorage`, and removing more than an entry holds returns `StorageFlagsOverflow`.
    - An entry left with `MINIMUM_NON_BASE_FLAGS_SIZE` (3) bytes or fewer is dropped.
    - If our flags are single-epoch, they are returned unchanged.
- **`rhs` has an earlier base epoch**: returns `MergingStorageFlagsWithDifferentBaseEpoch`.

`MergingOwnersStrategy` settles the case where both sides have different owner ids:
- `RaiseIssue` (the default) returns `MergingStorageFlagsFromDifferentOwners`.
- `UseOurs` keeps the old owner.
- `UseTheirs` takes the new owner, which models an ownership transfer.

If only one side has an owner, that owner is kept. `optional_combine_added_bytes` / `optional_combine_removed_bytes` take `Option<Self>` for the old flags and return `rhs` when there were none.

### Splitting Removed Bytes
```rust
pub fn split_storage_removed_bytes(&self, removed_key_bytes: u32, removed_value_bytes: u32)
    -> (StorageRemovedBytes, StorageRemovedBytes); // (key removal, value removal)
```
This works out which epochs, and which owner, a removal is attributed to. Each non-zero part is returned as `SectionedStorageRemoval`, keyed by the owner id (or `[0; 32]` if unowned) and then by epoch. A zero part is `NoStorageRemoval`.
- **Key bytes** are always taken from the base epoch. The key only goes away when the element is deleted.
- **Value bytes** are taken LIFO. The newest non-base epoch goes first, and each non-base epoch gives up at most `bytes_added - MINIMUM_NON_BASE_FLAGS_SIZE`. Whatever is still owed after all of them comes from the base epoch. Single-epoch flags take everything from the base epoch.

### GroveDB Callback Helpers
These work on raw `ElementFlags` and match the callback signatures GroveDB expects (after mapping the error type):
- `StorageFlags::update_element_flags(cost: &StorageCost, old_flags: Option<ElementFlags>, new_flags: &mut ElementFlags) -> Result<bool, StorageFlagsError>` rewrites `new_flags` according to `cost.transition_type()`, and returns whether it rewrote them:
  - **Bigger**: `combine_added_bytes` with `cost.added_bytes`.
  - **Smaller**: `combine_removed_bytes` with `cost.removed_bytes`.
  - **Same size**: keeps the old flags.
  - **Anything else, or no old flags**: leaves `new_flags` alone and returns `false`.

  It always merges with `MergingOwnersStrategy::UseTheirs`. When `old_flags` is `Some`, both the old and the new bytes must decode to flags, or it returns `RemovingFlagsError`.
- `StorageFlags::split_removal_bytes(flags: &mut ElementFlags, removed_key_bytes, removed_value_bytes)` decodes the flags and calls `split_storage_removed_bytes`. It returns `BasicStorageRemoval` for both parts if the element has no flags.

Other helpers include:
- Accessors: `new_single_epoch(epoch, Option<owner_id>)`, `base_epoch()`, `owner_id()`, `epoch_index_map()`, and `set_owner_id()` (no-op on unowned forms).
- `Option`/`Cow` conversions such as `map_some_element_flags_ref`, `map_to_some_element_flags` and `into_optional_cow`.

`StorageCost` and `StorageRemovedBytes` come from `grovedb-costs`. All errors are `grovedb_epoch_based_storage_flags::error::StorageFlagsError`.

### Usage Example
Dependencies: `grovedb-epoch-based-storage-flags` and `grovedb-costs`.

```rust
use std::collections::BTreeMap;

use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
use grovedb_epoch_based_storage_flags::{
    error::StorageFlagsError, MergingOwnersStrategy, StorageFlags,
};

fn main() -> Result<(), StorageFlagsError> {
    let owner = [7u8; 32];

    // An element first written in epoch 5 by `owner`.
    let written = StorageFlags::new_single_epoch(5, Some(owner));
    assert_eq!(written, StorageFlags::SingleEpochOwned(5, owner));

    // Wire form: type byte 2, owner id, base epoch (big-endian).
    let old_flags = written.to_element_flags();
    assert_eq!(old_flags.len(), 35);
    assert_eq!(old_flags[0], 2);
    assert_eq!(&old_flags[1..33], &owner);
    assert_eq!(&old_flags[33..], &5u16.to_be_bytes());
    assert_eq!(StorageFlags::deserialize(&old_flags)?, Some(written));
    assert_eq!(StorageFlags::deserialize(&[])?, None); // no flags

    // In epoch 8 the value grows by 100 bytes. The caller writes fresh
    // epoch-8 flags; `update_element_flags` folds the old flags in and
    // charges the 100 new bytes to epoch 8. Epoch 5 stays the base epoch.
    // (`replaced_bytes > 0` is what marks this as an update, not an insert.)
    let grow = StorageCost {
        added_bytes: 100,
        replaced_bytes: 60,
        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
    };
    let mut new_flags = StorageFlags::new_single_epoch(8, Some(owner)).to_element_flags();
    let changed = StorageFlags::update_element_flags(&grow, Some(old_flags), &mut new_flags)?;
    assert!(changed);
    let grown = StorageFlags::from_element_flags_ref(&new_flags)?.expect("flags present");
    assert_eq!(
        grown,
        StorageFlags::MultiEpochOwned(5, BTreeMap::from([(8, 100)]), owner)
    );
    // One record: 2-byte epoch index + 1-byte varint for 100.
    assert_eq!(new_flags.len(), 35 + 3);

    // In epoch 9 the value shrinks by 120 bytes. Removal is LIFO: epoch 8
    // gives up 100 - MINIMUM_NON_BASE_FLAGS_SIZE = 97 bytes and the other 23
    // come out of base epoch 5.
    let (key_removal, value_removal) = grown.split_storage_removed_bytes(0, 120);
    assert_eq!(key_removal, StorageRemovedBytes::NoStorageRemoval);
    let StorageRemovedBytes::SectionedStorageRemoval(by_owner) = &value_removal else {
        unreachable!("flagged elements always split into sections")
    };
    assert_eq!(by_owner[&owner].get(8), Some(&97));
    assert_eq!(by_owner[&owner].get(5), Some(&23));

    // Applying that removal leaves epoch 8 with only its 3 reserved bytes, so
    // the entry is dropped and the flags collapse back to the single-epoch form.
    let shrink = StorageCost {
        added_bytes: 0,
        replaced_bytes: 40,
        removed_bytes: value_removal,
    };
    let old_flags = new_flags;
    let mut new_flags = StorageFlags::new_single_epoch(9, Some(owner)).to_element_flags();
    StorageFlags::update_element_flags(&shrink, Some(old_flags), &mut new_flags)?;
    assert_eq!(
        StorageFlags::from_element_flags_ref(&new_flags)?,
        Some(StorageFlags::SingleEpochOwned(5, owner))
    );

    // Merging is directional: the newer flags may not have an older base
    // epoch, and different owners need an explicit strategy.
    let other_owner = [9u8; 32];
    assert!(matches!(
        StorageFlags::SingleEpoch(8).combine_added_bytes(
            StorageFlags::SingleEpoch(5),
            10,
            MergingOwnersStrategy::RaiseIssue,
        ),
        Err(StorageFlagsError::MergingStorageFlagsWithDifferentBaseEpoch(_))
    ));
    assert!(matches!(
        StorageFlags::SingleEpochOwned(5, owner).combine_added_bytes(
            StorageFlags::SingleEpochOwned(5, other_owner),
            10,
            MergingOwnersStrategy::RaiseIssue,
        ),
        Err(StorageFlagsError::MergingStorageFlagsFromDifferentOwners(_))
    ));
    assert_eq!(
        StorageFlags::SingleEpochOwned(5, owner).combine_added_bytes(
            StorageFlags::SingleEpochOwned(5, other_owner),
            10,
            MergingOwnersStrategy::UseTheirs,
        )?,
        StorageFlags::SingleEpochOwned(5, other_owner)
    );

    Ok(())
}
```

### Integration with GroveDB
Pass the two helpers as the flag callbacks of `GroveDb::apply_batch_with_element_flags_update` (and `apply_partial_batch_with_element_flags_update`). Pass `split_removal_bytes` alone to `delete_with_sectional_storage_function` and `delete_up_tree_while_empty_with_sectional_storage`:

```rust
db.apply_batch_with_element_flags_update(
    ops,
    None,
    |cost, old_flags, new_flags| {
        StorageFlags::update_element_flags(cost, old_flags, new_flags)
            .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
    },
    |flags, removed_key_bytes, removed_value_bytes| {
        StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
            .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
    },
    Some(&tx),
    grove_version,
)
.unwrap()?;
```

---

## grovedb-visualize - Debug Visualization

### Overview
Provides human-friendly visualization of GroveDB data structures for debugging and development. Intelligently formats byte arrays and tree structures.

### Core Components

#### Visualize Trait
```rust
pub trait Visualize {
    fn visualize<W: Write>(&self, drawer: Drawer<W>) -> Result<Drawer<W>>;
}
```

#### Drawer
Manages indentation and formatting:
```rust
pub struct Drawer<W: Write> {
    content: W,
    level: usize,
    indent: usize,
}
```

### Formatting Features
- Tree structure visualization with indentation
- Intelligent byte array display:
  - ASCII for printable characters
  - Hex for binary data
  - Mixed mode for partial ASCII
- Customizable indentation levels

### Usage Example
```rust
use grovedb_visualize::{Visualize, Drawer};

// Visualize a tree structure
let drawer = Drawer::new(std::io::stdout());
tree.visualize(drawer)?;

// Output:
// users
// ├── alice
// │   ├── balance: 100
// │   └── documents
// │       ├── doc1: [48 65 6c 6c 6f]
// │       └── doc2: "Hello World"
// └── bob
//     └── balance: 200
```

---

## Integration and Design Patterns

### Common Patterns Across Crates

1. **Zero-Copy Design**: Path crate avoids allocations
2. **Version Safety**: Explicit version checking throughout
3. **Cost Awareness**: All operations track resource usage
4. **Type Safety**: Strong typing with Rust's type system
5. **Error Propagation**: Consistent error handling

### Crate Interactions

```
GroveDB Core
    ├── Uses grovedb-path for navigation
    ├── Checks grovedb-version for compatibility
    ├── Tracks costs with grovedb-costs
    ├── Manages storage with epoch flags
    └── Debugs with grovedb-visualize
```

### Design Philosophy

The auxiliary crates follow consistent principles:
- **Modularity**: Each crate has a single, well-defined purpose
- **Performance**: Minimize allocations and overhead
- **Safety**: Use Rust's type system for correctness
- **Usability**: Provide ergonomic APIs
- **Extensibility**: Easy to add new features

These auxiliary crates provide essential functionality that makes GroveDB a complete, production-ready database system suitable for blockchain and other applications requiring authenticated data structures.
