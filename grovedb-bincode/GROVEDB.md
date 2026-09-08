# GroveDB bincode

This workspace package started from the published `bincode` **2.0.1** release as
`grovedb-bincode`, together with `bincode_derive` **2.0.1** as
`grovedb-bincode-derive`. The initial import preserved the upstream encoder, decoder,
derive macros, wire format, features, tests, benchmarks, specification, and MIT
license. Runtime version **2.1.0** adds explicit untrusted decoding APIs below;
the derive package is also **2.1.0** with explicit untrusted derives.

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
bincode = { package = "grovedb-bincode", version = "=2.1.0" }
```

The default `derive` feature uses the matching local derive package. Code that
depends on the macros separately can use:

```toml
bincode_derive = { package = "grovedb-bincode-derive", version = "=2.1.0" }
```

Although the bytes are unchanged, these are new Cargo packages with distinct
Rust trait identities. An application using upstream `bincode::Encode` or
`bincode::Decode` on GroveDB types must switch to this fork too. A dependency
alias does not make implementations interchangeable with upstream bincode.
Publish the derive package, then the runtime package, before publishing GroveDB
packages that depend on them. Their versions are independent of GroveDB's version.

## Opt-in untrusted decoding in 2.1.0

Ordinary `decode_*` / `borrow_decode_*` functions and decoder constructors retain
upstream 2.0.1 behavior, including eager allocation, Serde size hints, and reader
errors. Use the new APIs when input is untrusted:

```rust
#[derive(bincode::Encode, bincode::DecodeUntrusted)]
struct ClientMessage { payload: Vec<u8> }

let config = bincode::config::standard().with_limit::<1048576>();
let (value, consumed): (ClientMessage, usize) =
    bincode::decode_from_slice_untrusted(&[3, 1, 2, 3], config)?;
```

The native API includes `decode_from_slice_untrusted`,
`borrow_decode_from_slice_untrusted`, `decode_from_reader_untrusted`, and
`decode_from_std_read_untrusted`. Slice and standard-reader functions also have
`_with_context` variants. Advanced callers can use `DecoderImpl::new_untrusted`.
The functions require independent `DecodeUntrusted` / `BorrowDecodeUntrusted`
traits. Deriving `DecodeUntrusted` implements both new traits, without implementing
ordinary `Decode`. Derive both when a type should support both APIs. Borrowed
client types can derive `BorrowDecodeUntrusted`. Generated fields and generic
containers recursively require and call the new traits; there is no blanket
implementation for arbitrary ordinary decoders.

Allocation safeguards live in the untrusted collection implementations. Ordinary
`Decode` and `BorrowDecode` retain upstream allocation behavior even when explicitly
called on a decoder constructed with `new_untrusted`. Manual implementations must
call the untrusted traits for nested fields; there is no decoder-wide mode switch.
The sealed decoder capability survives mutable references and `with_context`.
New trait methods accept only an `UntrustedDecoder`, so
passing an ordinary decoder is a compile-time error. Existing decode-context,
custom-bound, and renamed-crate derive attributes are supported. Custom native
implementations must explicitly implement the new traits and retain their own
domain checks and resource constraints.

The Serde module provides matching untrusted slice/reader functions,
`seed_decode_from_slice_untrusted`, and untrusted constructors on
`OwnedSerdeDecoder` / `BorrowedSerdeDecoder`. These constructors return dedicated
untrusted decoder types with constrained `decode` / `decode_seed` methods, not a
raw Serde deserializer. Custom Serde types implement `DeserializeUntrusted<'de>`;
custom seeds implement `DeserializeSeedUntrusted<'de>`. This explicitly opts in
the entire Serde graph, which Serde's own dispatch cannot check recursively.
Library container implementations require their contents to opt in. `Compat`,
`BorrowCompat`, and derived `#[bincode(with_serde)]` fields require the same opt-in
and select the private untrusted Serde adapter policy. Ordinary compatibility
wrappers select the ordinary policy, independently of decoder capability.

Untrusted native collection implementations do not reserve vector or
hash-collection storage from an unverified length header. Byte vectors allocate after a reader
can show the bytes, or after bounded chunks have been read. Other vectors and
hash collections grow after a complete value or key/value pair has decoded.
Fallible reservations return `DecodeError::LimitExceeded`. Types implemented
through these vectors (including strings, boxed slices, and vector deques)
inherit the checks. Both untrusted Serde adapters omit sequence/map size hints
so visitors cannot mistake a length declaration for a verified allocation size.

Encoding is unchanged. Successfully decoded values and consumed-byte counts
remain compatible with upstream 2.0.1, including noncanonical integer encodings
and configured limit accounting. Hash collection iteration order is unspecified
and may differ after untrusted decoding. On malformed input, an untrusted chunked
reader may consume earlier chunks before returning an error, and its missing-byte estimate may
describe only the failing chunk.

This is not a universal memory or CPU budget: zero-wire types remain valid,
`with_no_limit()` still disables the configured limit, and custom decoders or
Serde visitors control their own allocations. GroveDB's Element wrapper rules
remain in `grovedb-element`, where nested wrappers are rejected before descent.
GroveDB's custom parsers share wire schemas and domain validation while explicitly
selecting ordinary or untrusted field decoding and recursion. Query collection
allocation is also selected by the trait implementation.
GroveDB's Element, backward-reference, and proof decoding boundaries explicitly
select the untrusted APIs. Downstream callers decoding GroveDB types directly
must also select these APIs when handling untrusted bytes.

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
