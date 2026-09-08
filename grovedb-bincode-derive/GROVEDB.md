# GroveDB bincode derive

This package imports the derive macros from the published `bincode_derive`
**2.0.1** release as `grovedb-bincode-derive`. It accompanies
`grovedb-bincode`. Macro behavior and the default generated
`::bincode` paths are unchanged.

Use the runtime package's default `derive` feature, or alias this package as
`bincode_derive` when depending on the macros directly. The runtime dependency
should be aliased as `bincode`; upstream's `#[bincode(crate = "...")]`
attribute remains available for other aliases.

The original documentation is in [readme.md](readme.md), and the original MIT
license and copyright are in [LICENSE.md](LICENSE.md). The archive checksum and
source revision are recorded in `Cargo.toml` under `package.metadata.upstream`.
See the runtime package's `GROVEDB.md` for provenance and migration details.
