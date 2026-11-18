# LigninDB on-disk format: segments and the manifest

This is the specification for task FMT-06 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines two files:

- a **segment**, `seg/<n>.seg`: blocks of page images and page deltas, and
  the footer index of a sealed segment (section 2)
- the **manifest**, `MANIFEST`: the store's page size, which files are
  live, how much of each is valid, the page-table checkpoint and the
  database format version (section 3)

It builds on [format.md](format.md) (FMT-01) and uses its rule numbers (B1 to
B11, C1 to C7, R1 to R7, H1 to H5, V1 to V5, U1 to U10). The code is
`lignin-format::segment` and `lignin-format::manifest`. Section 5 lists the
choices made here that the plan left open, and section 6 maps each rule to
its tests.

## 1. Scope

This document fixes the bytes of a segment and of the manifest. It does not
say:

- what a block's payload holds. A page image is defined by
  [tree-page.md](tree-page.md) (tree pages) and [flat-page.md](flat-page.md)
  (flat pages), a delta by FMT-03.
- when a segment is sealed, how blocks are located, or how the manifest is
  replaced on disk (STORE-02, STORE-03).
- the layout of a page-table checkpoint (STORE-03), a log (FMT-05) or a value
  log (FMT-07). The manifest only names those files.

Both formats are pure functions over bytes. Neither reads a file.

## 2. Segments

### 2.1 File layout

```text
file header     20 bytes: kind 3 (segment), sequence = the file id
block           zero or more, one after another with no gap
index record    sealed segments only
trailer         sealed segments only: the last 20 valid bytes
```

**S1. A segment is its header, then blocks.** The first block starts at
offset 20. A segment is written by appending blocks (B11).

**S2. Sealing appends a footer.** The footer is the index record followed by
the trailer. Nothing is appended after it.

**S3. A segment does not say how long it is, or whether it is sealed.** The
manifest does (section 3.5): it gives the segment's valid length and its
state. Bytes past the valid length are not part of the segment, whatever they
hold. This is what lets a checkpoint share a file that the live database is
still appending to: the same file can be unsealed and shorter for the
checkpoint, and sealed and longer for the live database.

### 2.2 Record frame

Every record after the file header has the same 8-byte frame.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | tag, a `u32`: bits 0 to 26 are `len`, the body length in bytes; bits 27 to 31 are the record kind |
| 4 | 4 | checksum (format.md section 4) of bytes 0 to 3 followed by the body |
| 8 | `len` | body |

**S4. The frame is the default frame with the kind in its spare bits.** A
default frame's length is at most 2^27 − 4 (format.md 5.1), so the top five
bits of its length field are always zero. Here they carry the record kind.
The body is at most 2^27 − 4 bytes, which keeps the checksum within C5.

**S5. Record kinds.**

| Kind | Record | Body |
| --- | --- | --- |
| 1 | base image, raw | the payload |
| 2 | base image, LZ4 | `raw_len` (`u32`), then an LZ4 block |
| 3 | delta run, raw | the payload |
| 4 | delta run, LZ4 | `raw_len` (`u32`), then an LZ4 block |
| 5 | index | 8 bytes for each block |
| 6 | trailer | 12 bytes |

Kind 0 is invalid (B6). So zero-filled bytes are not a segment record, and
neither is a record in the default frame. Every other code is an error
(`UnknownRecordKind`, R6). A kind that is defined but stands where it is not
allowed is `SegmentError::UnexpectedRecordKind`.

**S6. Order of checks.** A reader checks a record in this order:

1. 8 bytes are there: else `Truncated`
2. `len` is within the caller's maximum: else `RecordTooLong`
3. `len` bytes follow the frame: else `Truncated`
4. the checksum: else `ChecksumMismatch`
5. the kind
6. the fields of the body

The kind comes after the checksum so that a flipped bit in it is reported as
damage (C7).

### 2.3 Blocks

**S7. A block holds one payload**: the base image of one page, or one run of
deltas of one page. The payload is opaque here, and it is stored at its
actual length with no padding.

**S8. The block's checksum covers the bytes as stored, and it is the only
checksum over a page image.** A reader verifies a block before it
decompresses it (C7). Neither a tree page image nor a flat page image has a
checksum of its own (TP3 of `tree-page.md`, P12 of `flat-page.md`), so the
block is what detects a damaged image. What it does not cover is the
decompressor: a reader finds a wrong decompression by its length (S12) and
by the page decoder's structural checks, not by a checksum.

**S9. Raw.** The body is the payload. The block is the payload plus 8 bytes.

**S10. LZ4.** The body is `raw_len`, the payload's length once decompressed,
followed by the payload compressed in the LZ4 block format, with no
dictionary and no frame.

**S11. LZ4 only when it is smaller.** An LZ4 block is valid only if its body
is shorter than `raw_len`, that is, only if the whole block is smaller than
the same payload stored raw. A reader rejects any other
(`SegmentError::CompressedBlockNotSmaller`). So no block is more than 8 bytes
longer than its payload, whatever the payload. A writer may store any payload
raw.

**S12. What a reader accepts as LZ4.** Any stream of LZ4 sequences that uses
every stored byte and decompresses to exactly `raw_len` bytes. The rules the
LZ4 specification gives a compressor for the end of a block are not checked.
Anything else is `SegmentError::Decompression`.

**S13. One exception to B8.** Different LZ4 streams can decompress to the
same payload. Which one a writer produces is not part of the format, and it
may change with the compressor. So decoding what was encoded always gives the
payload back, but encoding a decoded LZ4 block need not give the same bytes
back. Everything else in a segment has one encoding.

**S14. A block does not name its page or its place.** File space is never
reused, so an old block cannot turn up where a new one is expected (format.md
section 5). The pointer that leads to a block, a file id, an offset and a
length, comes from the page table. The page id of each block is in the footer
index (2.4).

**S15. A block is read in one of two ways.**

- *By pointer.* The reader has exactly the block's bytes. More or fewer bytes
  than the frame's `len` says is an error (`SegmentError::LengthMismatch`,
  `Truncated`).
- *By walking.* The frame gives each block's length, so a segment with no
  footer can be walked from offset 20 to its valid length.

In both, the caller passes the longest payload it accepts, and nothing longer
is read or allocated (B10).

Example: the payload `lignin` (6 bytes) as a raw base image is the 14 bytes

```text
06 00 00 08  e6 e3 7a 08  6c 69 67 6e 69 6e
```

and the 32 bytes `abcd` × 8 as an LZ4 base image are the 26 bytes

```text
12 00 00 10              tag: kind 2, len 18
43 16 a8 97              checksum
20 00 00 00              raw_len 32
4f 61 62 63 64 04 00 04  4 literals "abcd", then a match of 23 at offset 4
50 64 61 62 63 64        5 literals "dabcd"
```

### 2.4 Footer

**S16. The index record** (kind 5) has one 8-byte entry for each block, in
file order:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | page id of the block's page (never 0) |
| 4 | 4 | the block's tag, exactly as in its frame |

**S17. Offsets are not stored.** Block 0 starts at offset 20, and each block
starts where the one before it ends, 8 bytes plus its `len` further on. The
index record starts where the last block ends.

**S18. The trailer** (kind 6) is the last 20 bytes of a sealed segment. Its
12-byte body:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | index offset: the file offset of the index record, `u64` |
| 8 | 4 | block count, `u32` |

**S19. A reader checks all of it**, so a run of blocks has exactly one footer
(B8):

- index offset + 8 + 8 × block count + 20 is the segment's valid length
- the index body is 8 × block count bytes
- every entry has a page id (`MissingId`) and the tag of a block
- 20 plus the lengths of all blocks is the index offset

A mismatch is `SegmentError::LengthMismatch`.

**S20. A segment holds at most 2^24 − 1 blocks.** That is what one index
record can list within C5. A writer seals a segment before then.

**S21. Reading a footer.** The reader takes the last bytes of the valid
length. The trailer is the last 20 of them and says how long the index record
is. If the reader had too few bytes, the error says how many it needs
(`Truncated`), so it can try a fixed amount first and read again only for a
large index.

Example: `seg/00000310.seg` with a raw base image of page 7 (`lignin`), a raw
delta run of page 7 (`d1`) and the LZ4 base image above for page 9 is 122
bytes. Its blocks are at offsets 20, 34 and 44, and its footer, at offset 70,
is

```text
18 00 00 28  63 fe 54 81              index: kind 5, len 24
07 00 00 00  06 00 00 08              page 7, base raw, len 6
07 00 00 00  02 00 00 18              page 7, delta raw, len 2
09 00 00 00  12 00 00 10              page 9, base LZ4, len 18
0c 00 00 30  ea 3c f0 00              trailer: kind 6, len 12
46 00 00 00 00 00 00 00  03 00 00 00  index offset 70, 3 blocks
```

### 2.5 The file header

**S22. A reader checks the header against the name.** The header must decode
(H4), its kind must be segment (`SegmentError::NotASegment`) and its sequence
must be the id in the file's name (`SegmentError::WrongFileId`, H3). The
format version it carries is the version of every record in the file (R5).

## 3. The manifest

### 3.1 File layout

```text
file header   20 bytes: kind 1 (manifest), sequence 0
one record    in the default frame; its payload is the manifest body
```

**M1. One record, and nothing after it.** The manifest is never appended to.
The store replaces the whole file (B11, STORE-02).

**M2. The format version in the file header is the database format version**
(V3). The body has no version field of its own.

**M3. The sequence field of the file header is 0** (H3). The manifest is not
numbered and nothing counts its replacements. A reader rejects any other
value (`ManifestError::NonZeroSequence`).

**M4. A build writes the manifest at its current version** (U1), whatever
version it read the manifest at. That is the first step of an upgrade (U3).

### 3.2 Body

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | page size in bytes, `u32`: 8,192 to 65,536 |
| 4 | 4 | last file id used for a log file, or 0 |
| 8 | 4 | last file id used for a segment, or 0 |
| 12 | 4 | last file id used for a value-log file, or 0 |
| 16 | 4 | last file id used for a page-table checkpoint, or 0 |
| 20 | 4 | page-table checkpoint: file id, or 0 for none |
| 24 | 8 | page-table checkpoint: valid length, `u64` |
| 32 | 8 | page-table checkpoint: commit version it covers, `u64` |
| 40 | 16 each | one entry for each live file |

**M5. The number of entries is not stored.** It is the rest of the body
divided by 16. A body that is shorter than 40 bytes, or whose rest is not a
whole number of entries, is an error (`ManifestError::LengthMismatch`).

**M6. A manifest names at most 2^20 files.**

**M17. The manifest records the store's page size.** A store has one page
size, for tree pages and flat pages alike, and no page image holds it (TP4
of `tree-page.md`, P10 of `flat-page.md`). It is what bounds how long a page
may grow in a buffer and how large one cell may be, so pages written under
one size cannot be served under a smaller one. The field is the size in
bytes, and the valid values are those of `PageSize`: 8,192 to 65,536,
every value in between included. A reader rejects any other
(`ManifestError::InvalidPageSize`), so a manifest that decodes always has a
page size a store can have.

**M18. The page size never changes.** The first manifest of a database
directory sets it, and every later manifest of that directory repeats it.
A checkpoint has the page size of the database it was made from. This is a
rule between two manifests, so the format cannot check it: the store
refuses a replacement that changes the page size, and refuses to open a
directory with another page size than its manifest records (STORE-02,
`ManifestStore::update` and `ManifestStore::check_page_size`). Changing the
page size of a database means writing every page again, into a new
database.

### 3.3 Last file ids

**M7. For each numbered kind, the manifest keeps the highest id the database
directory has ever given to a file of that kind**, or 0 if it never created
one. A file id is never reused, even after its file is deleted (format.md
3.3), and this field is what remembers the ids of deleted files.

**M8. Every id in the manifest is at most the last id of its kind.** A store
gives a new file a higher id than the last one.

### 3.4 Page-table checkpoint

**M9. The manifest names one page-table checkpoint**, `table/<n>.pt`, by its
file id, the length of the file that is valid, and the commit version it
covers. What "covers" means for replay, and the layout of the file, are the
page table's to define (STORE-03).

**M10. A database with no checkpoint yet** stores file id 0, valid length 0
and the "no version" pattern, all ones. No other mix of set and unset fields
is valid.

**M11. The checkpoint has no state.** It is written whole before the manifest
names it, and its valid length is at least 20.

### 3.5 Live files

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 2 | file kind, as in the file header: 2 log, 3 segment, 4 value log |
| 2 | 1 | state: 1 open, 2 closed, 3 sealed |
| 3 | 1 | reserved, 0 (B7) |
| 4 | 4 | file id (never 0) |
| 8 | 8 | valid length, `u64` |

**M12. Entries are in strictly ascending order of kind code, then file id.**
So a file is named once, and a set of files has one encoding
(`ManifestError::EntriesOutOfOrder`).

**M13. Valid length.** Every byte of the file below its valid length belongs
to a whole record (R1). It is at least 20: a manifest names a file only once
its header is written (H5).

**M14. Who checks a valid length.** It is a statement about another file, and
this format reads no files. A manifest reader checks what the manifest alone
can show: the length is at least 20; a segment's is 20 or at least 28, since
its smallest record is 8 bytes; a sealed segment's is at least 48, which is a
header, an empty index record and a trailer. Whether the file really holds
whole records up to that length is checked by whoever reads the file. For a
segment that is a walk of its blocks, or its footer read against the valid
length (S19); both fail on a length that splits a record.

**M15. State** says what the bytes past the valid length are.

- **Open (1).** The session that wrote the manifest was still appending to
  the file, so whole records may follow the valid length. Whether a later
  reader uses them is the rule of the file's own format and of the store: a
  log is replayed up to its last complete group.
- **Closed (2).** The file ends at its valid length for this database. Bytes
  past it are not part of the database, whatever they hold. A checkpoint
  names the files it shares with the live database this way, and recovery
  names the files of an earlier session this way.
- **Sealed (3).** A segment only. As closed, and the valid bytes end with the
  segment's footer (2.4).

**M16. The state is about reading, never about writing.** Open means the
session that wrote the manifest had not finished the file. It does not make
the file a place to append to. A later session appends to no file a manifest
names, whatever its state, and creates its own files under ids above the last
ids (B11, STORE-07). So the format puts no limit on how many files of a kind
are open, or on which ids they have: a store may write several files of a
kind at once.

### 3.6 Order of checks

`Manifest::decode` checks, in this order:

1. the file header (H4)
2. the kind is manifest: else `ManifestError::NotAManifest`
3. the sequence is 0: else `ManifestError::NonZeroSequence`
4. the record's length and checksum: else `Truncated`, `RecordTooLong`,
   `ChecksumMismatch`
5. nothing follows the record: else `ManifestError::LengthMismatch`
6. the body, by the format version in the header:
   1. its length is 40 bytes and a whole number of entries (M5): else
      `ManifestError::LengthMismatch`
   2. the page size (M17): else `ManifestError::InvalidPageSize`
   3. the checkpoint (M10, M11)
   4. each entry, in order (3.5)
   5. the number of entries, their order, and each id, the checkpoint's
      included, against the last id of its kind (M6, M12, M8)

Every rule of this section is checked, so a manifest has exactly one encoding
(B8). A rule on an entry that is broken is `ManifestError::InvalidEntry`,
unless a more specific error is named above.

### 3.7 Examples

The manifest of a database with 32 KiB pages and no file yet is 68 bytes:

```text
4c 49 47 4e 49 4e 44 42  01 00  01 00  00 00 00 00  59 31 c8 5e   file header
28 00 00 00  a8 52 10 2d                                          record: 40 bytes
00 80 00 00                                                       page size 32,768
00 00 00 00  00 00 00 00  00 00 00 00  00 00 00 00                last file ids
00 00 00 00  00 00 00 00 00 00 00 00  ff ff ff ff ff ff ff ff     no checkpoint
```

A page size of 64 KiB is `00 00 01 00`: it does not fit in 16 bits, which is
why the field is 32.

A live file entry for `seg/00000309.seg`, sealed, 64 MiB long:

```text
03 00  03  00  35 01 00 00  00 00 00 04 00 00 00 00
```

## 4. Sizes

| Quantity | Value |
| --- | --- |
| Overhead of a raw block | 8 bytes |
| Overhead of an LZ4 block | 12 bytes, and the block is still smaller than raw (S11) |
| Longest payload of a block | 2^27 − 4 bytes |
| Footer | 8 bytes for each block, plus 28 |
| Most blocks in a segment | 2^24 − 1 |
| Manifest | 68 bytes, plus 16 for each live file |
| Longest manifest | 16,777,284 bytes (`Manifest::MAX_ENCODED_LEN`) |
| Most live files in a manifest | 2^20 |

## 5. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one before STORE-02 and FMT-08 build on it.

1. **A block carries its own length** (S4, S15). The 8 bytes of overhead are
   a checksum and a tag of length and kind. The alternative, a 5-byte header
   with the length only in the pointer, saves 3 bytes a block and makes a
   segment without a footer unreadable except through the page table. Every
   checkpoint shares one unsealed segment, and every crash leaves one, so
   those are not rare.
2. **A block is one page's base image or one run of its deltas** (S7), and
   the record kind says which. A block never mixes pages.
3. **A block does not carry its page id** (S14). There is no room for it
   within 8 bytes next to a length. A sealed segment has it in the footer. For
   an unsealed segment it has to come from the page table, or from the
   payload: a flat page image does not hold its own page id, a delta names its
   page (FMT-03), and FMT-02 decides for tree pages. If FMT-02 puts the page
   id in the tree page header, a store can list the tree pages of an unsealed
   segment by walking it.
4. **No page image has a checksum of its own** (S8). The block's checksum
   covers the compressed bytes and delta runs, so it cannot be dropped, and
   a second one inside the image would be paid on every load. FMT-04 first
   gave flat pages one; FMT-02 gave tree pages none, and the owner then
   removed it from flat pages (D-049).
5. **LZ4 block format, through the crate `lz4_flex`** (0.14.2, MIT), with only
   its safe encoder and decoder compiled in (`safe-encode`, `safe-decode`,
   `checked-decode`; no frame format, no `unsafe`). It is pure Rust, as DEC-07
   requires. The crate is not trusted for the definition: the tests check it
   against a decoder written from the LZ4 block specification, on valid and
   on damaged streams, and blocks were exchanged with liblz4 1.9.4 in both
   directions.
6. **Which LZ4 stream a writer produces is not part of the format** (S13).
   Golden files hold fixed streams and are checked by decoding them. PERF-08
   can change the compressor or its settings without a format change.
7. **An LZ4 block is valid only when it is smaller than raw** (S11), and the
   reader enforces it. This turns the acceptance test, at most 8 bytes of
   overhead, into a property of every valid segment and not only of this
   writer.
8. **Offsets are not stored in the index** (S17), and the index entry repeats
   the block's tag. An entry is 8 bytes instead of 16 or more.
9. **A segment holds at most 2^24 − 1 blocks** (S20), so that the index is one
   record. The alternative is an index split over several records.
10. **Whether a segment is sealed is in the manifest, not in the segment**
    (S3, M15). A reader that guessed from the last 20 bytes could be misled
    by a payload that ends in bytes shaped like a trailer.
11. **The manifest's sequence field is 0, and nothing counts replacements**
    (M3). Atomic replacement by rename does not need a generation number.
12. **The manifest keeps the last file id of each kind** (M7). The plan does
    not list this field. Without it, "a file id is never reused" (format.md
    3.3) cannot be kept after the highest-numbered file is deleted.
13. **A live file has a state** (M15): open, closed or sealed. The plan lists
    only a valid length. A valid length alone cannot serve both a live
    database, whose log grows past the length in its manifest and must be
    replayed to its last complete group, and a checkpoint, whose shared log
    must not be read past its length.
14. **The format does not limit open files** (M16). One open file of each
    kind would be simpler to reason about, but it would forbid the flusher
    and the cleaner each writing a segment.
15. **A manifest names a file only after its header is written** (M13). So
    the store creates a file and writes its header before it replaces the
    manifest.
16. **The manifest is not checked against the files** (M14). The store does
    that.
17. **Errors are one `FormatError` variant for each format**, `Segment` and
    `Manifest`, wrapping `segment::SegmentError` and
    `manifest::ManifestError`, as FMT-04 did. The shared errors are used
    where they apply.
18. **Decoders take the format version as a parameter**, since a record has
    none of its own (R5). The manifest takes it from its own header.
19. **A private module `wire`** holds the one helper both decoders share.
20. **The page size is a field of the manifest body** (M17, M18), added
    after FMT-02 (D-049). It is 32 bits, since 64 KiB does not fit in 16,
    and it is the first field, ahead of everything that changes from one
    manifest to the next.
21. **The manifest's decoder checks the page size's range**, and the store
    checks that it does not change. The range is a property of one manifest,
    and with it checked a `Manifest` holds a `PageSize`, not a number
    that every user would have to check again. The decoder takes the range
    from `PageSize` and has no bounds of its own, so the two cannot
    disagree.
22. **The page size type is `lignin-format::ids::PageSize`.** It is the
    page size of the store, not only of tree pages. When the field was
    added the type was `tree_page::TreePageSize`; it was renamed and moved
    with no change to a byte (D-050). The limits that are about cells stay
    in `tree_page`.

## 6. Code and tests

Run the tests with `cargo test -p lignin-format`.

| Rule | Code | Tests |
| --- | --- | --- |
| S4 to S6 | `segment::{Block, BlockTag}` | Golden bytes; every tag word either round-trips or is rejected; every kind code with a valid checksum; every single-bit flip, with the error each must give; every shorter and longer input; zero-filled bytes. |
| S7 to S10, S15 | `segment::{write_block, Block}` | Round trip of random, compressible and zero payloads of 0 to 70,000 bytes, by pointer and by walking; the longest payload and one byte more. |
| S11 | `segment::write_block`, `Block::decode` | **Acceptance test:** an incompressible payload of any length from 0 to 600 bytes, and of larger sizes, is its length plus exactly 8. No payload gives a block more than 8 bytes longer than itself. A hand-built LZ4 block one byte smaller than raw is accepted, and one of equal size is rejected. |
| S12, S13 | `Block::payload`, `Block::copy_payload_to` | A decoder written from the LZ4 specification agrees with the crate on 30,000 damaged streams; forged streams that are too short, too long or invalid are rejected. |
| S16 to S21 | `segment::{write_footer, SegmentTrailer, SegmentIndex}` | Golden bytes of a sealed segment and of an empty one; round trip with 0 to 300 blocks, checked against a walk of the blocks; every tail too short for the footer; every single-bit flip; forged footers for each check of S19; the largest index and one entry more. |
| S22 | `segment::decode_file_header` | Wrong kind, wrong id, damaged header. |
| M1 to M6 | `manifest::Manifest` | Golden bytes; round trip of random manifests in both directions; wrong kind, sequence and version; every shorter and longer input; every body length; the largest manifest and one file more. |
| M7 to M11 | `manifest::{LastFileIds, TableCheckpoint}` | Each id above its last id; each mix of set and unset checkpoint fields. |
| M17 | `Manifest::page_size`, `ManifestError::InvalidPageSize` | `the_page_size_is_one_a_store_can_have`: sizes below 8,192 and above 65,536 are rejected, before the checkpoint and any entry are looked at and after the body's length, whether the body is too short or ends in part of an entry; the sizes at both ends and between them round-trip. Both golden manifests hold one (32,768 and 65,536). Random manifests round-trip with random sizes, and forged ones with sizes in and out of range. |
| M18 | `lignin-store::segment::ManifestStore` | `the_page_size_of_the_first_manifest_is_the_stores_for_good`: a replacement with another page size is refused (`ManifestChange::PageSizeChanged`) and writes nothing. `a_store_opened_with_another_page_size_is_refused`: `check_page_size` gives `SegmentStoreError::PageSizeMismatch`. |
| M12 to M15 | `manifest::{LiveFile, FileState}` | Every kind, state and boundary length; every state code; forged entries for each rule; order and duplicates. |
| B8, B10 | all decoders | Forged records with valid checksums decode only to what encodes back to them; arbitrary and mutated bytes give an error, never a panic. Every single-bit flip of a manifest, with the error each must give. |

The golden bytes and their checksums were computed with an independent
bit-at-a-time CRC-32C and a separate encoder, not with the crate under test.
