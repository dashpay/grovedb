# GroveDB

**Hierarchical Authenticated Data Structure Database**

A high-performance, cryptographically verifiable database that organizes data as a "grove" — a forest of Merkle AVL trees (Merk). Enables efficient queries on any indexed field while maintaining cryptographic proofs throughout the hierarchy.

**[Read the GroveDB Book](https://dashpay.github.io/grovedb/index.html)** — comprehensive documentation covering architecture, element types, proofs, queries, and more. Available in 16 languages.

| Branch | Tests | Coverage |
|--------|-------|----------|
| master | [![Tests](https://github.com/dashpay/grovedb/actions/workflows/grovedb.yml/badge.svg?branch=master)](https://github.com/dashpay/grovedb/actions) | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV)](https://codecov.io/gh/dashpay/grovedb) |

<details>
<summary>Per-Crate Coverage</summary>

| Crate | Coverage |
|-------|----------|
| grovedb | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV&component=grovedb-core)](https://codecov.io/gh/dashpay/grovedb/component/grovedb-core) |
| merk | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV&component=merk)](https://codecov.io/gh/dashpay/grovedb/component/merk) |
| storage | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV&component=storage)](https://codecov.io/gh/dashpay/grovedb/component/storage) |
| commitment-tree | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV&component=commitment-tree)](https://codecov.io/gh/dashpay/grovedb/component/commitment-tree) |
| mmr | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV&component=mmr)](https://codecov.io/gh/dashpay/grovedb/component/mmr) |
| bulk-append-tree | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV&component=bulk-append-tree)](https://codecov.io/gh/dashpay/grovedb/component/bulk-append-tree) |
| element | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/master/graph/badge.svg?token=6Z6A6FT5HV&component=element)](https://codecov.io/gh/dashpay/grovedb/component/element) |

</details>

## Key Features

- **Hierarchical tree-of-trees** — organize data in nested Merk trees with a single root hash authenticating everything
- **Efficient secondary indexes** — pre-computed index trees give O(log n) queries on any field
- **Cryptographic proofs** — membership, non-membership, and range proofs with minimal size
- **7 reference types** — cross-tree linking without data duplication
- **Built-in aggregations** — sum trees, count trees, big sum trees, and combined variants
- **Batch operations** — atomic updates across multiple trees
- **Append-only structures** — MMR trees, bulk append trees, commitment trees (Sinsemilla/Halo 2)
- **Cross-platform** — x86, ARM, WebAssembly

## Quick Start

```toml
[dependencies]
grovedb = "3.0"
```

```rust
use grovedb::{GroveDb, Element};
use grovedb_version::version::GroveVersion;

let db = GroveDb::open("./my_db")?;
let v = GroveVersion::latest();

// Create trees
db.insert(&[], b"users", Element::new_tree(None), None, None, v)?;
db.insert(&[b"users"], b"alice", Element::new_tree(None), None, None, v)?;

// Insert data
db.insert(&[b"users", b"alice"], b"age", Element::new_item(b"30"), None, None, v)?;

// Query
let age = db.get(&[b"users", b"alice"], b"age", None, v)?;

// Generate and verify proofs
let path_query = PathQuery::new_unsized(vec![b"users".to_vec()], Query::new_range_full());
let proof = db.prove_query(&path_query, None, None, v)?;
let (root_hash, results) = GroveDb::verify_query(&proof, &path_query, v)?;
```

## Building

```bash
cargo build --release
cargo test
cargo bench
```

## Architecture

GroveDB is built in three layers:

1. **GroveDB Core** — orchestrates multiple Merk trees, elements, references, queries, proofs, and batch operations
2. **Merk** — self-balancing Merkle AVL tree with proof generation, cost tracking, and lazy loading
3. **Storage** — RocksDB abstraction with prefixed storage, transactions, and batching

For deep dives into each layer, see the [GroveDB Book](https://dashpay.github.io/grovedb/index.html).

## Academic Foundation

GroveDB implements concepts from [Database Outsourcing with Hierarchical Authenticated Data Structures](https://ia.cr/2015/351) (Etemad & Kupcu, 2015) — using a forest of Merkle AVL trees where each tree can contain other trees, solving the fundamental limitation of flat authenticated structures.

Built by [Dash Core Group](https://dashplatform.readme.io/docs/introduction-what-is-dash-platform) as the storage layer for Dash Platform.

## License

MIT — see [LICENSE](LICENSE).

## Links

- [GroveDB Book](https://dashpay.github.io/grovedb/index.html) — full documentation
- [GitHub Issues](https://github.com/dashpay/grovedb/issues)
- [Discord](https://discordapp.com/invite/PXbUxJB)
