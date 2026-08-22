# Costs - Resource Tracking and Accounting

## Overview

The Costs crate provides a unified interface for tracking computational and storage costs across GroveDB operations. It enables precise resource accounting, which is essential for blockchain applications where operations must be metered and paid for.

## Core Components

### OperationCost

The fundamental structure for tracking resource usage:

```rust
pub struct OperationCost {
    pub seek_count: u64,
    pub storage_cost: StorageCost,
    pub storage_loaded_bytes: u64,
    pub hash_node_calls: u64,
}
```

**Fields Explained**:
- `seek_count`: Number of database seek operations performed
- `storage_cost`: Detailed storage costs (see StorageCost below)
- `storage_loaded_bytes`: Total bytes loaded from storage
- `hash_node_calls`: Number of cryptographic hash operations

### StorageCost

Detailed storage cost tracking:

```rust
pub struct StorageCost {
    pub added_bytes: u64,
    pub replaced_bytes: u64,
    pub removed_bytes: u64,
}
```

**Cost Categories**:
- `added_bytes`: New data added to storage
- `replaced_bytes`: Existing data overwritten
- `removed_bytes`: Data deleted from storage

### CostResult

A wrapper type that pairs computation results with their costs:

```rust
pub type CostResult<T, E> = Result<ResultWithCost<T>, MappedCostErr<E>>;

pub struct ResultWithCost<T> {
    pub value: T,
    pub cost: OperationCost,
}
```

This pattern ensures costs are tracked throughout the call stack.

## Cost Calculation

### Storage Cost Formulas

The crate provides various formulas for calculating storage costs:

```rust
// Basic key-value storage cost
pub fn key_value_cost(key: &impl AsRef<[u8]>, value: &impl AsRef<[u8]>) -> u32 {
    KV::specialized_value_byte_cost_size(
        key.as_ref().len() as u32,
        value.as_ref().len() as u32,
        false,
    )
}

// Tree node with children
pub fn tree_cost_size(
    key_len: u32,
    value_len: u32,
    has_left_child: bool,
    has_right_child: bool,
) -> u32 {
    let flags_len = flags_len(key_len, value_len, has_left_child, has_right_child);
    let kv_len = key_len + value_len + flags_len;
    let hash_len = Hash::length() as u32;
    let left_len = if has_left_child { hash_len + 1 } else { 0 };
    let right_len = if has_right_child { hash_len + 1 } else { 0 };
    
    kv_len + left_len + right_len
}
```

### Special Tree Types

Different tree types have specific cost calculations:

#### Sum Trees
```rust
pub fn sum_tree_cost_size(
    key_len: u32,
    value_len: u32,
    has_left_child: bool,
    has_right_child: bool,
    is_sum_tree: bool,
) -> u32 {
    let base_cost = tree_cost_size(key_len, value_len, has_left_child, has_right_child);
    if is_sum_tree {
        base_cost + 8 // Additional 8 bytes for sum value
    } else {
        base_cost
    }
}
```

#### Count Trees
Count trees add overhead for maintaining element counts:
- Additional 8 bytes for count storage
- Propagation costs through parent trees

#### Append-only trees (BulkAppendTree, CommitmentTree, PrivateDocumentStore)

The append-only family writes three kinds of data rows: dense-buffer slots
(one per position, keys reused every epoch), the chunk blob an epoch is
compacted into (plus the MMR internal nodes), and — for the commitment
tree — the frontier, one value rewritten on every append. How those writes
are reported to the fee layer is version-gated
(`bulk_append_tree_versions.cost.append_storage_accounting`,
`commitment_tree_versions.cost.frontier_save_storage_accounting`):

| write | GROVE_V1..V3 (v0) | GROVE_V4 (v1, issue #822) |
|---|---|---|
| buffer slot, first epoch | key + value `added` | key + value `added` |
| buffer slot, epoch ≥ 2 | key + value `added` | value `replaced` (growth `added`, shrink not credited); key not charged |
| entry's chunk-blob share | — (blob charged at compaction) | entry bytes `added` at the entry's own append |
| compaction blob | key + whole blob `added` | entry bytes `replaced`, framing + key `added` |
| MMR internal nodes | `added` | `added` |
| frontier rewrite | key + value `added` every save | value `replaced`, growth `added`; first save fully `added` |

Under v0 every note is billed roughly twice over its life (slot + blob) and
the whole blob (≈ 630 KB at `chunk_power` 11) lands on one append per
epoch. Under v1 each entry's permanent bytes are charged once, at its own
append, and the rest of the churn is replacement; the sum of `added_bytes`
over an epoch matches the database growth. `removed_bytes` stays
`NoStorageRemoval` in both — a rolling buffer refunds nobody. Stored bytes,
roots and proofs are identical under both versions.

Mechanically: `BulkAppendTree` reads the committed value of a slot that
already holds one (judged against the count at open; never this session's
cache) before rewriting it — one billed seek plus the value's bytes — and
asks the dense tree for `SlotWriteAccounting::Overwrite { previous_value_len }`,
which attaches `KeyValueStorageCost::for_in_place_value_rewrite`; the MMR
`MmrStore` takes a `LeafValueStorageCost::PartlyPrepaid` policy that the
bulk tree feeds `chunk_blob_entry_bytes`; the `Result`-returning appends
report a `storage_accounting_cost` (the prepaid share plus the slot read)
for the caller to bill, while the `CostResult`-returning
`append_deferred_roots` already includes it in its cost.

## Cost Context

The `CostContext` trait provides functional combinators for cost-aware operations:

```rust
pub trait CostContext {
    fn add_cost(&mut self, cost: OperationCost);
    
    fn with_cost<C: Fn(&mut OperationCost)>(self, f: C) -> Self;
    
    fn flat_map_ok<U, F>(self, f: F) -> CostResult<U, Self::Error>
    where
        F: FnOnce(Self::Context) -> CostResult<U, Self::Error>;
        
    fn map_ok<U, F>(self, f: F) -> CostResult<U, Self::Error>
    where
        F: FnOnce(Self::Context) -> U;
}
```

### Usage Patterns

#### Accumulating Costs
```rust
let mut cost = OperationCost::default();

// Add seek cost
cost.seek_count += 1;

// Add storage cost
cost.storage_cost.added_bytes += key.len() as u64 + value.len() as u64;

// Add hash cost
cost.hash_node_calls += 1;
```

#### Chaining Operations
```rust
operation1()
    .flat_map_ok(|result1| {
        operation2(result1)
    })
    .map_ok(|result2| {
        transform(result2)
    })
    .add_cost(additional_cost)
```

## Cost Limits

The crate supports cost limits to prevent runaway operations:

```rust
pub struct CostLimit {
    pub seek_limit: Option<u64>,
    pub storage_limit: Option<u64>,
    pub hash_limit: Option<u64>,
}

impl OperationCost {
    pub fn check_limit(&self, limit: &CostLimit) -> Result<(), CostError> {
        if let Some(seek_limit) = limit.seek_limit {
            if self.seek_count > seek_limit {
                return Err(CostError::SeekLimitExceeded);
            }
        }
        // Check other limits...
        Ok(())
    }
}
```

## Integration with GroveDB

### Cost Flow

Costs flow through the entire GroveDB stack:

1. **Storage Layer**: Tracks seeks and bytes loaded
2. **Merk Layer**: Adds hash operations and tree costs
3. **GroveDB Layer**: Aggregates costs across subtrees
4. **Application Layer**: Uses costs for fee calculation

### Cost Sources

Different operations have different cost profiles:

#### Insert Operation
- Storage cost for new data
- Hash operations for tree updates
- Seeks to find insertion point
- Root hash propagation costs

#### Query Operation
- Seeks to traverse trees
- Bytes loaded from storage
- Hash verification costs
- Proof generation overhead

#### Delete Operation
- Seeks to find elements
- Storage removal costs
- Tree rebalancing costs
- Root hash updates

## Macros and Utilities

The crate provides helpful macros for cost handling:

```rust
// Early return with cost accumulation
cost_return_on_error!(result, cost_accumulator);

// Add cost to operation
cost_apply!(operation, cost);

// Conditional cost addition
cost_if!(condition, cost_to_add);
```

## Performance Considerations

### Cost Caching

To avoid recalculation:
- Tree nodes cache their storage costs
- Known costs are propagated upward
- Lazy recalculation on changes

### Batch Cost Optimization

Batch operations amortize costs:
- Single tree traversal for multiple operations
- Shared seek costs
- Deferred hash calculations

### Cost Estimation

Pre-calculate costs without execution:
```rust
pub fn estimate_operation_cost(
    operation: &Operation,
    current_state: &State,
) -> OperationCost {
    // Calculate costs based on operation type
    // and current database state
}
```

## Usage Examples

### Basic Cost Tracking
```rust
use grovedb_costs::{OperationCost, CostResult};

fn expensive_operation() -> CostResult<String, Error> {
    let mut cost = OperationCost::default();
    
    // Simulate database seek
    cost.seek_count += 1;
    
    // Simulate storage read
    cost.storage_loaded_bytes += 1024;
    
    // Simulate hash operation
    cost.hash_node_calls += 1;
    
    Ok(("result".to_string(), cost)).wrap_with_cost()
}
```

### Cost Accumulation
```rust
fn complex_operation() -> CostResult<(), Error> {
    let result1 = operation1()?;
    let result2 = operation2()?;
    
    // Costs automatically accumulated
    result1.flat_map_ok(|value1| {
        result2.map_ok(|value2| {
            combine(value1, value2)
        })
    })
}
```

### Cost Limits
```rust
fn limited_operation(limit: CostLimit) -> CostResult<Vec<Item>, Error> {
    let mut items = Vec::new();
    let mut total_cost = OperationCost::default();
    
    for key in keys {
        let (item, cost) = fetch_item(key)?;
        total_cost.add_assign(cost);
        
        // Check limits
        total_cost.check_limit(&limit)?;
        
        items.push(item);
    }
    
    Ok((items, total_cost)).wrap_with_cost()
}
```

## Design Philosophy

The Costs crate embodies several design principles:

1. **Transparency**: All operations report their resource usage
2. **Composability**: Costs flow through operation chains
3. **Precision**: Accurate byte-level storage accounting
4. **Flexibility**: Support for different cost models
5. **Performance**: Minimal overhead for cost tracking

## Future Enhancements

- Additional cost dimensions (CPU cycles, memory usage)
- Cost prediction models
- Historical cost analysis
- Dynamic cost adjustment
- Cost optimization hints
