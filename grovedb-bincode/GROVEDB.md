# GroveDB bincode

This workspace package imports the published `bincode` **2.0.1** release as
`grovedb-bincode`, together with `bincode_derive` **2.0.1** as
`grovedb-bincode-derive`. The import preserves the upstream encoder, decoder,
derive macros, wire format, features, tests, benchmarks, specification, and MIT
license. It introduces no decoding hardening or new resource limits.

The original documentation remains in [readme.md](readme.md) and [docs](docs/).
The original copyright and license remain in [LICENSE.md](LICENSE.md).

## Provenance

The sources come from the crates.io release archives, not the latest upstream
branch. Both archives record Git revision
`4673360aa638b2b907dff24538de54f258d157da` in `.cargo_vcs_info.json`.

| Original package | Archive SHA-256 |
| --- | --- |
| `bincode-2.0.1.crate` | `36eaf5d7b090263e8150820482d5d93cd964a81e4019913c972f4edcc6edb740` |
| `bincode_derive-2.0.1.crate` | `bf95709a440f45e986983918d0e8a1f30a9b1df04918fc828670606804ac3c09` |

Cargo manifests are adapted to the GroveDB workspace and package names. The
release's generated manifests, lockfiles, and Cargo cache metadata are omitted.
`unty` and `virtue` remain external dependencies with their upstream requirements.
Package-local lint allowances cover existing upstream deprecation/lifetime
warnings and compile-only test fixtures. Narrow pre-commit exclusions preserve
the original comments, whitespace, and documentation line endings.

## Depending on the fork

Keep the `bincode` dependency alias so existing imports and generated derive
paths continue to work:

```toml
[dependencies]
bincode = { package = "grovedb-bincode", version = "=2.0.1" }
```

The default `derive` feature uses the matching local derive package. Code that
depends on the macros separately can use:

```toml
bincode_derive = { package = "grovedb-bincode-derive", version = "=2.0.1" }
```

Although the bytes are unchanged, these are new Cargo packages with distinct
Rust trait identities. An application using upstream `bincode::Encode` or
`bincode::Decode` on GroveDB types must switch to this fork too. A dependency
alias does not make implementations interchangeable with upstream bincode.
Publish the derive package, then the runtime package, before publishing GroveDB
packages that depend on them. Their versions are independent of GroveDB's version.

## Validation

The original release tests are retained. The additional
`upstream_compatibility` test compares both implementations directly, using
upstream 2.0.1 only as a development dependency. It checks identical bytes and
cross-decoding with both byte orders and both integer encodings.

```sh
cargo test -p grovedb-bincode -p grovedb-bincode-derive --all-features
```

CI runs these tests, Clippy, formatting, and no-std feature checks in a separate
`Bincode` job only when files in `grovedb-bincode/` or `grovedb-bincode-derive/`
change. The regular workspace CI jobs exclude both packages as direct targets,
and both packages are excluded from coverage reports and Codecov. GroveDB's
tests still compile and exercise the fork as a dependency.
