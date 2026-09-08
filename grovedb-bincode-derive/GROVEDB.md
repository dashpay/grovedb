# GroveDB bincode derive

This package imports the derive macros from the published `bincode_derive`
**2.0.1** release as `grovedb-bincode-derive`. It accompanies
`grovedb-bincode`. Version **2.1.0** adds `DecodeUntrusted` and
`BorrowDecodeUntrusted`. Ordinary derives and default generated `::bincode` paths
retain their upstream behavior.

`#[derive(DecodeUntrusted)]` emits only the untrusted owned and borrowed traits.
Its fields must support the corresponding untrusted trait, and generated methods
accept only decoders with the untrusted policy enabled. Borrowing client types
can derive `BorrowDecodeUntrusted`. Derive ordinary `Decode` separately when both
APIs are needed. Existing context, custom-bound, renamed-crate, and Serde-field
attributes are supported; Serde fields require an explicit
`bincode::serde::DeserializeUntrusted` implementation for their Serde graph.

Use the runtime package's default `derive` feature, or alias this package as
`bincode_derive` when depending on the macros directly. The runtime dependency
should be aliased as `bincode`; upstream's `#[bincode(crate = "...")]`
attribute remains available for other aliases.

The original documentation is in [readme.md](readme.md), and the original MIT
license and copyright are in [LICENSE.md](LICENSE.md). The archive checksum and
source revision are recorded in `Cargo.toml` under `package.metadata.upstream`.
See the runtime package's `GROVEDB.md` for provenance and migration details.
