# GroveDB

| Branch | Tests | Coverage |
|--------|-------|----------|
| master | [![Tests](https://github.com/dashevo/grovedb/workflows/CI/badge.svg?branch=master)](https://github.com/dashevo/grovedb/actions) | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV)](https://codecov.io/gh/dashpay/grovedb) |

**GroveDB: Hierarchical Authenticated Data Structure Database**

GroveDB is a high-performance, cryptographically verifiable database system that organizes data as a "grove" - a tree of trees. Built specifically for efficient secondary index queries, proofs, and blockchain applications, it provides the foundational storage layer for [Dash Platform](https://dashplatform.readme.io/docs/introduction-what-is-dash-platform) while being flexible enough for any application requiring authenticated data structures.

## Table of Contents

- [Key Features](#key-features)
- [Architecture Overview](#architecture-overview)
- [Core Concepts](#core-concepts)
- [Getting Started](#getting-started)
- [Usage Examples](#usage-examples)
- [Query System](#query-system)
- [Performance](#performance)
- [Documentation](#documentation)
- [Contributing](#contributing)

## Key Features

### 🌳 Hierarchical Tree-of-Trees Structure
- Organize data naturally in nested hierarchies
- Each subtree is a fully authenticated Merkle AVL tree
- Efficient navigation and organization of complex data

### 🔍 Efficient Secondary Index Queries
- Pre-computed secondary indices stored as subtrees
- O(log n) query performance on any indexed field
- No need to scan entire dataset for non-primary key queries

### 🔐 Cryptographic Proofs
- Generate proofs for any query result
- Supports membership, non-membership, and range proofs
- Minimal proof sizes through optimized algorithms
- Layer-by-layer verification from root to leaves

### 🚀 High Performance
- Built on RocksDB for reliable persistent storage
- Batch operations for atomic updates across multiple trees
- Intelligent caching system (MerkCache) for frequently accessed data
- Cost tracking for all operations

### 🔗 Advanced Reference System
- 7 types of references for complex data relationships
- Automatic reference following (configurable hop limits)
- Cycle detection prevents infinite loops
- Cross-tree data linking without duplication

### 📊 Built-in Aggregations
- Sum trees for automatic value totaling
- Count trees for element counting
- Combined count+sum trees
- Big sum trees for 256-bit integers

### 🌐 Cross-Platform Support
- Native Rust implementation
- Runs on x86, ARM (including Raspberry Pi), and WebAssembly
- Node.js bindings available
- C bindings coming soon

## Architecture Overview

GroveDB combines several innovative components:

```
┌────────────────────────────────────────────────────────┐
│                     GroveDB Core                       │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │   Element   │  │    Query     │  │     Proof     │  │
│  │   System    │  │    Engine    │  │   Generator   │  │
│  └─────────────┘  └──────────────┘  └───────────────┘  │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │   Batch     │  │  Reference   │  │   Version     │  │
│  │ Operations  │  │   Resolver   │  │  Management   │  │
│  └─────────────┘  └──────────────┘  └───────────────┘  │
└────────────────────────────────────────────────────────┘
┌────────────────────────────────────────────────────────┐
│                      Merk Layer                        │
│         (Merkle AVL Tree Implementation)               │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │  AVL Tree   │  │    Proof     │  │     Cost      │  │
│  │  Balancing  │  │    System    │  │   Tracking    │  │
│  └─────────────┘  └──────────────┘  └───────────────┘  │
└────────────────────────────────────────────────────────┘
┌────────────────────────────────────────────────────────┐
│                   Storage Layer                        │
│            (RocksDB Abstraction)                       │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │ Prefixed    │  │ Transaction  │  │    Batch      │  │
│  │  Storage    │  │   Support    │  │ Processing    │  │
│  └─────────────┘  └──────────────┘  └───────────────┘  │
└────────────────────────────────────────────────────────┘
```

### Component Details

1. **GroveDB Core**: Orchestrates multiple Merk trees into a unified hierarchical database
2. **Merk**: High-performance Merkle AVL tree implementation with proof generation
3. **Storage**: Abstract storage layer with RocksDB backend, supporting transactions and batching
4. **Costs**: Comprehensive resource tracking for all operations
5. **Version Management**: Protocol versioning for smooth upgrades

## Core Concepts

### Elements

GroveDB supports 8 element types:

```rust
// Basic storage
Element::Item(value, flags)           // Arbitrary bytes
Element::Reference(path, max_hops)    // Link to another element
Element::Tree(root_hash)             // Subtree container

// Aggregation types
Element::SumItem(value)              // Contributes to sum
Element::SumTree(root_hash, sum)     // Maintains sum of descendants
Element::BigSumTree(root_hash, sum)  // 256-bit sums
Element::CountTree(root_hash, count) // Element counting
Element::CountSumTree(root_hash, count, sum) // Combined
```

### Hierarchical Paths

Data is organized using paths:
```rust
// Path: ["users", "alice", "documents"]
db.insert(
    &["users", "alice"], 
    b"balance", 
    Element::new_item(b"100")
)?;
```

### Reference Types

Seven reference types enable complex relationships:
- `AbsolutePathReference`: Direct path from root
- `UpstreamRootHeightReference`: Go up N levels, then follow path
- `UpstreamFromElementHeightReference`: Relative to current element
- `CousinReference`: Same level, different branch
- `SiblingReference`: Same parent tree
- `UtilityReference`: Special system references

## Getting Started

### Requirements

- Rust 1.74+ (nightly)
- RocksDB dependencies

### Installation

Add to your `Cargo.toml`:
```toml
[dependencies]
grovedb = "3.0"
```

### Basic Setup

```rust
use grovedb::GroveDb;

// Open database
let db = GroveDb::open("./my_db")?;

// Create a tree structure
db.insert(&[], b"users", Element::new_tree(None), None, None)?;
db.insert(&[b"users"], b"alice", Element::new_tree(None), None, None)?;

// Insert data
db.insert(
    &[b"users", b"alice"], 
    b"age", 
    Element::new_item(b"30"),
    None,
    None
)?;

// Query data
let age = db.get(&[b"users", b"alice"], b"age", None)?;
```

## Usage Examples

### Creating Secondary Indexes

```rust
// Create user data
db.insert(&[b"users"], b"user1", Element::new_tree(None), None, None)?;
db.insert(&[b"users", b"user1"], b"age", Element::new_item(b"25"), None, None)?;
db.insert(&[b"users", b"user1"], b"city", Element::new_item(b"Boston"), None, None)?;

// Create indexes
db.insert(&[], b"indexes", Element::new_tree(None), None, None)?;
db.insert(&[b"indexes"], b"by_age", Element::new_tree(None), None, None)?;
db.insert(&[b"indexes"], b"by_city", Element::new_tree(None), None, None)?;

// Add references in indexes
db.insert(
    &[b"indexes", b"by_age"], 
    b"25_user1",
    Element::new_reference(ReferencePathType::absolute_path(vec![
        b"users".to_vec(), 
        b"user1".to_vec()
    ])),
    None,
    None
)?;
```

### Using Sum Trees

```rust
// Create account structure with balances
db.insert(&[], b"accounts", Element::new_sum_tree(None, 0), None, None)?;

// Add accounts with balances
db.insert(&[b"accounts"], b"alice", Element::new_sum_item(100), None, None)?;
db.insert(&[b"accounts"], b"bob", Element::new_sum_item(200), None, None)?;
db.insert(&[b"accounts"], b"charlie", Element::new_sum_item(150), None, None)?;

// Get total sum (automatically maintained)
let sum_tree = db.get(&[], b"accounts", None)?;
// sum_tree now contains Element::SumTree with sum = 450
```

### Batch Operations

```rust
use grovedb::batch::GroveDbOp;

let ops = vec![
    GroveDbOp::insert_op(vec![b"users"], b"alice", Element::new_tree(None)),
    GroveDbOp::insert_op(vec![b"users", b"alice"], b"name", Element::new_item(b"Alice")),
    GroveDbOp::insert_op(vec![b"users", b"alice"], b"age", Element::new_item(b"30")),
];

// Apply atomically
db.apply_batch(ops, None, None)?;
```

### Generating Proofs

```rust
use grovedb::query::PathQuery;

// Create a path query
let path_query = PathQuery::new_unsized(
    vec![b"users".to_vec()],
    Query::new_range_full(),
);

// Generate proof
let proof = db.prove_query(&path_query, None, None)?;

// Verify proof independently
let (root_hash, results) = GroveDb::verify_query(proof.as_slice(), &path_query)?;
```

## Query System

### Basic Queries

```rust
// Get all items in a subtree
let query = Query::new_range_full();
let path_query = PathQuery::new_unsized(vec![b"users".to_vec()], query);
let results = db.query(&path_query, false, false, None)?;
```

### Range Queries

```rust
// Get users with names from "A" to "M"
let mut query = Query::new();
query.insert_range(b"A".to_vec()..b"N".to_vec());

let path_query = PathQuery::new_unsized(vec![b"users".to_vec()], query);
let results = db.query(&path_query, false, false, None)?;
```

### Complex Queries with Subqueries

```rust
// Get all users and their documents
let mut query = Query::new_with_subquery_key(b"documents".to_vec());
let path_query = PathQuery::new_unsized(vec![b"users".to_vec()], query);
let results = db.query(&path_query, false, false, None)?;
```

### Query Types

GroveDB supports 10 query item types:
- `Key(key)` - Exact key match
- `Range(start..end)` - Exclusive range
- `RangeInclusive(start..=end)` - Inclusive range
- `RangeFull(..)` - All keys
- `RangeFrom(start..)` - From key onwards
- `RangeTo(..end)` - Up to key
- `RangeToInclusive(..=end)` - Up to and including key
- `RangeAfter(prev..)` - After specific key
- `RangeAfterTo(prev..end)` - After key up to end
- `RangeAfterToInclusive(prev..=end)` - After key up to and including end

### Advanced Query Features (v2+)

**Parent Tree Inclusion**: When performing subqueries, you can include the parent tree element itself in the results:

```rust
let mut query = Query::new();
query.insert_key(b"users".to_vec());
query.set_subquery(Query::new_range_full());
query.add_parent_tree_on_subquery = true;  // Include parent tree

let path_query = PathQuery::new_unsized(vec![], query);
let results = db.query(&path_query, false, false, None)?;
// Results include both the "users" tree element AND its contents
```

This is particularly useful for count trees and sum trees where you want both the aggregate value and the individual elements.

## Performance

### Benchmarks

Performance on different hardware:

| Hardware | Full Test Suite |
|----------|----------------|
| Raspberry Pi 4 | 2m 58s |
| AMD Ryzen 5 1600AF | 34s |
| AMD Ryzen 5 3600 | 26s |
| Apple M1 Pro | 19s |

### Optimization Features

1. **MerkCache**: Keeps frequently accessed subtrees in memory
2. **Batch Operations**: Process multiple updates in single transaction
3. **Cost Tracking**: Monitor resource usage for optimization
4. **Lazy Loading**: Load only required tree nodes
5. **Prefix Iteration**: Efficient subtree traversal

## Documentation

### Detailed Documentation

- [Merk - Merkle AVL Tree](docs/crates/merk.md)
- [Storage Abstraction Layer](docs/crates/storage.md)
- [GroveDB Core](docs/crates/grovedb.md)
- [Cost Tracking System](docs/crates/costs.md)
- [Auxiliary Crates](docs/crates/auxiliary.md)

### Examples

See the [examples](examples/) directory for:
- Basic CRUD operations
- Secondary indexing patterns
- Reference usage
- Batch operations
- Proof generation and verification

## Building from Source

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone repository
git clone https://github.com/dashevo/grovedb.git
cd grovedb

# Build
cargo build --release

# Run tests
cargo test

# Run benchmarks
cargo bench
```

## Debug Visualization

GroveDB includes a web-based visualizer for debugging:

```rust
let db = Arc::new(GroveDb::open("db")?);
db.start_visualizer(10000); // Port 10000

// Visit http://localhost:10000 in your browser
```

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Development Setup

1. Fork the repository
2. Create a feature branch
3. Write tests for new functionality
4. Ensure all tests pass
5. Submit a pull request

### Testing

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run with verbose output
cargo test -- --nocapture
```

## License

GroveDB is licensed under the MIT License. See [LICENSE](LICENSE) for details.

## Support

- [GitHub Issues](https://github.com/dashevo/grovedb/issues)
- [Discord](https://discord.gg/dash)
- [Documentation](https://dashplatform.readme.io)

## Acknowledgments

GroveDB is built on research including:
- [Database Outsourcing with Hierarchical Authenticated Data Structures](https://ia.cr/2015/351)
- AVL tree algorithms for self-balancing
- Merkle tree cryptographic proofs

Special thanks to the Dash Core Group and all contributors.
