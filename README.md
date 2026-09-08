# GroveDB

**Hierarchical Authenticated Data Structure Database**

A cryptographically verifiable database that organizes data as a "grove" of nested Merkle AVL trees (Merk). GroveDB combines key-value storage, secondary indexes, aggregate queries, and append-only structures under a single authenticated root hash. Clients can verify query results without holding the database.

**[Read the GroveDB Book](https://dashpay.github.io/grovedb/index.html)** — documentation covering architecture, element types, proofs, queries, and more, with translations into 16 languages.

| Branch | Tests | Coverage |
|--------|-------|----------|
| develop | [![Tests](https://github.com/dashpay/grovedb/actions/workflows/grovedb.yml/badge.svg?branch=develop)](https://github.com/dashpay/grovedb/actions) | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV)](https://codecov.io/gh/dashpay/grovedb) |

<details>
<summary>Per-Crate Coverage</summary>

| Crate | Coverage |
|-------|----------|
| grovedb | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV&component=grovedb-core)](https://codecov.io/gh/dashpay/grovedb/component/grovedb-core) |
| merk | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV&component=merk)](https://codecov.io/gh/dashpay/grovedb/component/merk) |
| storage | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV&component=storage)](https://codecov.io/gh/dashpay/grovedb/component/storage) |
| commitment-tree | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV&component=commitment-tree)](https://codecov.io/gh/dashpay/grovedb/component/commitment-tree) |
| mmr | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV&component=mmr)](https://codecov.io/gh/dashpay/grovedb/component/mmr) |
| bulk-append-tree | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV&component=bulk-append-tree)](https://codecov.io/gh/dashpay/grovedb/component/bulk-append-tree) |
| element | [![codecov](https://codecov.io/gh/dashpay/grovedb/branch/develop/graph/badge.svg?token=6Z6A6FT5HV&component=element)](https://codecov.io/gh/dashpay/grovedb/component/element) |

</details>

## Key Features

- **Hierarchical storage** — nest trees and authenticate their contents through one root hash
- **Secondary indexes and references** — index application fields through cross-tree references, with opt-in bidirectional references for update and deletion propagation
- **Unified queries** — express key selections, ranges, nested subqueries, per-instance limits, count-offset pagination, and sum-budget reads through `PathQuery`
- **Aggregates and ordered indexes** — count and sum trees, provable aggregate variants, and indexed trees supporting top-k, rank, and bounded queries ordered by count, sum, or average
- **Cryptographic proofs** — prove membership, absence, ranges, and supported aggregate and indexed queries; verify results against a trusted root hash
- **Transactions and batches** — apply atomic updates across the grove with explicit tracking of storage, seek, and hashing costs
- **Append-only structures** — Merkle mountain ranges, dense and bulk append trees, Sinsemilla commitment trees, and fixed-size opaque entries in `PrivateDocumentStore`
- **Client verification** — build proof verifiers separately from native RocksDB storage, including for WebAssembly clients

## Quick Start

Add both crates to your application's `Cargo.toml`:

```toml
[dependencies]
grovedb = "6.0"
grovedb-version = "6.0"
```

Save the following as `src/main.rs` and run `cargo run`. It creates a tree,
stores and reads an item, then generates and verifies a proof. Use a fresh
`my_db` directory.

```rust
use grovedb::{Element, GroveDb, PathQuery, Query};
use grovedb_version::version::GroveVersion;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = GroveDb::open("./my_db")?;
    let version = GroveVersion::latest();
    let root_path: &[&[u8]] = &[];
    let users_path: &[&[u8]] = &[b"users"];

    db.insert(
        root_path,
        b"users",
        Element::empty_tree(),
        None,
        None,
        version,
    )
    .value?;

    db.insert(
        users_path,
        b"alice",
        Element::new_item(b"Alice".to_vec()),
        None,
        None,
        version,
    )
    .value?;

    let alice = db.get(users_path, b"alice", None, version).value?;
    assert_eq!(alice, Element::new_item(b"Alice".to_vec()));

    let path_query = PathQuery::new_unsized(
        vec![b"users".to_vec()],
        Query::new_single_key(b"alice".to_vec()),
    );

    // This example trusts its local database. Remote clients obtain this
    // root independently, for example from an authenticated Platform block.
    let trusted_root = db.root_hash(None, version).value?;
    let proof = db.prove_query(&path_query, None, version).value?;
    let (root_hash, results) = GroveDb::verify_query(&proof, &path_query, version)?;

    assert_eq!(root_hash, trusted_root);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].2.as_ref(), Some(&alice));
    Ok(())
}
```

Database operations return a `CostContext` containing a result in `.value` and
measured work in `.cost`. The example uses `.value?` to propagate errors; an
application that accounts for execution costs can also accumulate `.cost`.

Use `GroveDb::run_path_query` and `GroveDb::verify_path_query` for the unified
read and verification interfaces across query shapes. See the
[query guide](https://dashpay.github.io/grovedb/unified-path-query.html) and
[per-instance limits](https://dashpay.github.io/grovedb/per-instance-limits.html).

## Client Verification

Applications that only verify proofs can disable the default storage features:

```toml
[dependencies]
grovedb = { version = "6.0", default-features = false, features = ["verify"] }
grovedb-version = "6.0"
```

Verification checks a proof against the requested query and returns its root
hash. Compare that hash with an independently trusted root before accepting the
results. This build supports verification without opening a RocksDB database.

## Upgrading to 6.0

GroveDB 6.0 includes public API changes since 5.0.1. Query merges are now fallible,
and query and operation-option structs have additional fields. Update callers
that construct these structs directly or match exhaustively on public enums.

GroveDB uses **`grovedb-bincode` 2.1.0**, maintained in this repository. Applications
that serialize or deserialize GroveDB types directly must use the fork's traits:

```toml
[dependencies]
bincode = { package = "grovedb-bincode", version = "=2.1.0" }
```

The default `derive` feature selects the matching `grovedb-bincode-derive` crate.
The fork has distinct Rust trait identities from upstream bincode. Its ordinary
encoding and decoding retain upstream 2.0.1 behavior; its new `DecodeUntrusted`
and `BorrowDecodeUntrusted` APIs provide opt-in allocation safeguards for external
input. See the [bincode integration guide](grovedb-bincode/GROVEDB.md) for direct
decoding, Serde opt-in, and configuration details. GroveDB's proof verification
entry points already use untrusted decoding.

V1 proofs now allow up to 65,535 immediate child layers, independently of the
128-level recursion limit. Older verifier binaries still reject layers wider
than 128 children, so update client verifiers alongside applications that serve
wide proofs. Decoding budgets still apply.

Package versions and runtime compatibility versions are separate. GroveDB 6.0
includes `GROVE_V4`; select the `GroveVersion` required by your application's
protocol. `GroveVersion::latest()` is convenient for new applications, while
consensus and replay code should select the required version explicitly.

## Building

Native database builds require a recent stable Rust toolchain, a C++ toolchain,
and libclang for RocksDB bindings.

```bash
cargo build --release
cargo test
cargo bench
```

To build just the proof-verification library:

```bash
cargo build -p grovedb --no-default-features --features verify
```

## Contributing

Install [pre-commit](https://pre-commit.com/) to catch formatting and lint issues before CI:

```bash
pip install pre-commit        # or: brew install pre-commit
pre-commit install             # fmt + typos on every commit
pre-commit install --hook-type pre-push  # clippy on push
```

## Architecture

GroveDB is organized around three layers:

1. **GroveDB Core** — coordinates the tree hierarchy, indexed and append-only structures, references, queries, proofs, and batch operations
2. **Merk** — self-balancing Merkle AVL tree with proof generation, cost tracking, and lazy loading
3. **Storage** — RocksDB abstraction with prefixed storage, transactions, and batching

Supporting workspace crates provide query types, elements, runtime version
selection, cost accounting, specialized trees, and the bincode fork. GroveDB
supports concurrent readers alongside a single writer; applications must
serialize write transactions.

For deep dives into each layer, see the [GroveDB Book](https://dashpay.github.io/grovedb/index.html).

## Academic Foundation

GroveDB implements concepts from [Database Outsourcing with Hierarchical Authenticated Data Structures](https://ia.cr/2015/351) (Etemad & Kupcu, 2015) — using a forest of Merkle AVL trees where each tree can contain other trees, solving the fundamental limitation of flat authenticated structures.

Built by [Dash Core Group](https://dashplatform.readme.io/docs/introduction-what-is-dash-platform) as the storage layer for Dash Platform.

## License

MIT — see [LICENSE.md](LICENSE.md).

## Links

- [GroveDB Book](https://dashpay.github.io/grovedb/index.html) — full documentation
- [GitHub Issues](https://github.com/dashpay/grovedb/issues)
- [Discord](https://discordapp.com/invite/PXbUxJB)
