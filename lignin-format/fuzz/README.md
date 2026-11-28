# Fuzzing `lignin-format`

Rule B10 of `docs/lignindb/format.md`: a decoder never panics, overflows or
reads out of bounds, whatever its input. These targets check it with
libFuzzer (FMT-08 of the plan), and check on the way that what a decoder
accepts is what the writers write: decoding and then encoding gives the same
bytes back (B8), and the readers of one format agree with each other.

## Targets

| Target | Decoders |
| --- | --- |
| `frame` | `FileHeader::decode`, `RecordHeader::decode`, `read_record`, `FormatVersion::new`, `FileKind::from_code`, the fixed-width ids and `Checksum` |
| `manifest` | `Manifest::decode` |
| `segment` | `segment::decode_file_header`, `BlockTag::decode`, `Block::{read, decode, payload, copy_payload_to}`, `SegmentTrailer::decode`, `SegmentIndex::{decode_tail, blocks}` |
| `table` | `table::{decode, decode_file_header}`, `Decoder::{read, record}`, also on payloads without their frame and on checkpoints of thousands of links |
| `value_log` | `value_log::{scan, read_value, record_len}`, `ValueRecord::decode`, `ValuePointer::{decode, read}`, `ValuePlace::decode`, `PendingValue::new` |
| `flat_page` | `FlatPage::{decode, from_validated}` and every accessor of a leaf and a branch, checked or not |
| `tree_page` | `TreePage::{decode, from_validated, check, check_fragments}`, `TreeCell::decode`, every accessor, and `TreePageMut` on pages that decode (W5) |
| `delta` | `DeltaRun::decode` with `deltas` and `created`; `DeltaRun::apply`, `apply_chain` and `consolidate` on valid pages (A4) |
| `flat_run` | `FlatRun::decode` with `changes`; `FlatRun::apply` on valid pages |
| `wal` | `wal::{read_file_start, scan}`, the group readers, and `LogCursor::step` driven by `record_len`, which must end the valid part where `scan` does |
| `wal_record` | `wal::Record::decode`, `wal::record_len`, `read_logged_value`, `read_pending_value`, and a background change on the page it names |

Every input is a control byte and the bytes under test. Bit 0 of the control
byte keeps the checksums as they are. When it is clear, the target writes the
right checksum into every frame it finds before it decodes, so the fuzzer
reaches the decoders behind the checksums. The other bits are each target's
own: a page size, a limit, or which reader runs.

## Running

The targets need a nightly toolchain and `cargo-fuzz`. Overflow checks come
with `--debug-assertions`. The format crate has no unsafe code, so the
sanitizer is not needed for bounds; run with AddressSanitizer as well to cover
the dependencies.

```sh
cd lignin-format/fuzz
./seed.sh corpus-seeds
cargo +nightly fuzz build -s none --debug-assertions
cargo +nightly fuzz run -s none --debug-assertions tree_page corpus/tree_page corpus-seeds/tree_page \
    -- -fork=8 -rss_limit_mb=1024 -max_len=16384
```

## Golden files

`golden/v<N>/` holds a file of each kind, page and run of format version
*N*, written by `lignin-format/tests/golden_files.rs` from fixed contents.
That test holds every committed file to its contents and, except a segment,
whose LZ4 stream is not part of the format, to the bytes the writers write.
`seed.sh` builds the seed corpus of every target from them.

While version 1 is open (U10 of `format.md`), a deliberate change of a layout
rewrites the files with
`LIGNIN_WRITE_GOLDEN=1 cargo test -p lignin-format --test golden_files`; run the
test again without the variable to check them. Once a version is released its
files never change, and a new version adds its own directory next to them
(U9).

## What the fuzzer cannot reach

A value of the value log or of the log is cut into records of at most
2^27 - 14 bytes, and a commit group into parts of at most 2^27 - 4 bytes, and
every record but the last must be full. An input of 128 MiB is far beyond
what a fuzzer builds, so the paths that read a second record of a value or a
second part of a group are fuzzed only up to the check that refuses a short
first one. The unit tests of `value_log` and `wal` run those paths with small
limits.
