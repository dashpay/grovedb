# LigninDB on-disk format: shared rules

This is the specification for task FMT-01 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
fixes the identifiers and rules that every LigninDB file shares:

- how bytes are laid out (section 2)
- page ids, commit versions, file ids and slot numbers (section 3)
- the record checksum (section 4)
- what a record is, and the default record frame (section 5)
- the header every file starts with (section 6)
- the format version and the upgrade rule (section 7)

It does not define any file's contents. Tree pages, page deltas, flat pages,
log records, segments, the manifest and the value log are tasks FMT-02 to
FMT-07. Section 8 says what each of them, and TREE-01, TXN-01, STORE-03 and
STORE-10, take from here. Section 9 lists the choices made here that the plan
left open.

The code is the crate `lignin-format`: modules `ids`, `checksum`, `header` and
`error`. Section 10 maps each rule to its type and its tests.

"Must" marks a requirement on every format and every reader and writer.

## 1. Terms

- **File**: one of the files in a database directory, as listed under "Files
  on disk" in the plan: `MANIFEST`, `wal/<n>.log`, `seg/<n>.seg`,
  `val/<n>.val`, `table/<n>.pt`. `LOCK` is empty and has no format.
- **Record**: the unit that is written in one piece and checked in one piece
  (section 5). The file header is a record. So are a log group, a segment
  block, a value-log entry and a manifest.
- **Header**: the fixed-width fields at the start of a file or a record.
- **Build**: one compiled version of the engine.
- **Session**: the time between one open of a database directory and its
  close.

## 2. Byte conventions

**B1. Little-endian, fixed width.** Every integer in a header is an unsigned
integer of 1, 2, 4 or 8 bytes, least significant byte first. A header uses no
variable-length integers. A signed quantity, where a format needs one, is
two's complement in the same widths.

**B2. Packed, no alignment.** A header is its fields in order with no gaps.
Nothing may assume that a header or a record starts at an aligned address or
an aligned file offset. Readers copy fields out of the byte string; they never
reinterpret memory.

**B3. Fixed-width headers.** A header's length is a constant for its file
kind, record kind and format version. Everything of variable size comes after
the header, and the header (or the pointer that led to the record) says how
long it is.

**B4. Lengths and offsets.** A length inside a record is a `u32` or smaller. A
file offset is a `u64`. Bit 0 of a flags field is its least significant bit.

**B5. Byte strings.** Hashes, keys, values and the magic are raw bytes in
their own order. They are never treated as integers.

**B6. Zero is never a valid code or id.** In every enumeration (file kind,
record kind, format version) the code 0 is invalid, and page ids and file ids
start at 1. The one number for which zero is valid is the commit version: 0 is
the empty database (3.2). Zero-filled bytes are kept from decoding as a record
by the checksum rule (C4), not by this one.

**B7. Reserved means zero, and is checked.** A reserved field or bit is
written as zero. A reader rejects a non-zero value: the format version says
exactly what every byte means, so a non-zero reserved byte is damage
(`NonZeroReserved`). Giving a reserved field a meaning is a format change
(U6).

**B8. One encoding per value.** Every value has exactly one valid byte string,
and a decoder rejects every other one. So decoding what was encoded gives the
value back, and encoding what was decoded gives the same bytes back. The
round-trip tests check both directions.

**B9. Encoders are pure.** The bytes depend only on the logical input. No
clock, random number, host name or path goes into a file. Equal histories give
equal files, which is what the crash simulator and the golden files rely on.

**B10. Decoders are total.** A decoder never panics, overflows or reads out of
bounds on any input. It returns a typed error (`FormatError`). It checks every
length against the format's maximum and against the bytes available before it
reads or allocates that much, and it uses checked arithmetic on every value it
has not verified. FMT-08 fuzzes this.

**B11. Append only, write once.** A file grows by appending whole records. No
byte is rewritten in place, and a file from an earlier session is never
appended to or truncated (STORE-07). So a file header is written once, when
the file is created, and never updated. Anything that changes later lives in a
later record or in the manifest, which is replaced as a whole file (STORE-02).

## 3. Identifiers

| Identifier | Rust type | On disk | Valid values | Reserved pattern |
| --- | --- | --- | --- | --- |
| Page id | `PageId` | `u32`, 4 bytes | 1 to 2^32 − 1 | 0: no page |
| Commit version | `CommitVersion` | `u64`, 8 bytes | 0 to 2^64 − 2 | 2^64 − 1: no version |
| File id | `FileId` | `u32`, 4 bytes | 1 to 2^32 − 1 | 0: no file |
| Slot number | `SlotId` | `u16`, 2 bytes | 1 to 2^16 − 1 | 0: no slot |

A field that must hold an identifier rejects the reserved pattern
(`MissingId`). A field that may be empty stores the reserved pattern for
"none". Each type has
`encode`, `encode_optional` and `decode` for this; `decode` returns `None` for
the reserved pattern and the format decides whether `None` is allowed there.

### 3.1 Page id

A page id names one **logical page**: one entry in the store's single page
table (STORE-03).

- **It is not a location.** Where the page's bytes are (which segment, which
  offset, how many deltas) is the page table's business. Flushing,
  consolidating or relocating a page does not change its id, so a pointer
  stored in another page stays valid. This is what "Pointers, not lookups"
  needs.
- **One id space.** Tree pages, flat pages and any later page kind share it.
  The id says nothing about what the page holds.
- **Dense.** Ids are small numbers handed out from 1 upward, so the table is a
  directly indexed array with no hashing. Allocation, freeing and deferred
  reuse are STORE-10's.
- **Reusable.** A freed id may be handed out again once no snapshot can reach
  the old page. An id therefore identifies a page only together with a commit
  version at which that page is reachable (TXN-01).
- **Zero is "no page".** An absent child, an empty root and an unset pointer
  are all encoded as 0.

**Why 32 bits.** 2^32 − 1 pages is 16 TiB of live pages at 4 KiB a page, and
more at larger pages. Well before that, the page table itself (28 bytes a
page, STORE-03) would be 112 GiB, so the id is not the first limit to bind.
Against that, the id is stored in every cross-page link, every delta that
names a page, every directory entry and every back pointer. The plan's write
target of 2.4 KB logged per transition is about 40 bytes for each changed
hash, 32 of which are the hash; and a hash-only delta has to fit in 48 bytes
(FMT-03). Four bytes of id instead of eight is what leaves room for a slot, a
kind and a checksum.

If the id ever has to grow, that is a new format version (section 7). The Rust
type hides its width so that only encoders and decoders would change.

### 3.2 Commit version

A commit version counts the commits of one database.

- **Version 0 is the empty database**, before any commit
  (`CommitVersion::EMPTY`). It is a valid version to read at, and no commit
  ever has it.
- **Each committed write transaction takes the previous version plus one.**
  The first commit produces version 1. There are no gaps, and along the
  history that survives a crash a version is never reused and never goes
  down. A commit lost in a crash gives its number back to the next commit:
  commits are not fsynced one by one (DEC-04), and recovery leaves nothing
  that names a lost one (rule DU3 of [`transactions.md`](transactions.md)).
  A transaction that rolls back takes no version. A reader can therefore
  check a run of commits for a missing one.
- **It belongs to one database directory.** A checkpoint that is opened as a
  database continues from the version it was taken at, on its own line. The
  same number in two directories can mean different states, so versions are
  never compared across directories.
- **It is not a block height, a GroveVersion or a protocol version.** The
  engine does not know what a commit means to its caller.
- **2^64 − 1 is "no version".** No commit ever has it. A format may use it
  for an empty field, for example an upper bound that is still open.

What a version makes visible, what a snapshot pins and how the flusher and
cleaner respect pins are TXN-01's. This section fixes only the number.

Sixty-four bits is enough for 584 million years at 1,000 commits a second.

### 3.3 File id

A file id is the number in a numbered file's name: `seg/00000310.seg` is file
310 of kind segment.

- **Per kind.** Each kind has its own numbering. A file is identified by its
  kind and its id together.
- **From 1, increasing, never reused.** Within one database directory a number
  is used for at most one file of a kind, ever. A file that is deleted leaves
  its number unused.
- **In the name and in the header.** The name is the number in decimal, padded
  with leading zeros to eight digits. A number above 99,999,999 is written in
  full, with no padding. No other spelling names the file: `000000310.seg` is
  not file 310. The file header repeats the number (section 6), so a renamed
  or misplaced file is detected.
- **The manifest has none.** `MANIFEST` is the one unnumbered file.

A pointer to a place in a file is a file id plus a `u64` offset, with a length
where the reader needs one. The formats that store such pointers (FMT-02,
FMT-06, FMT-07 and the page table) fix their exact layout.

### 3.4 Slot number and cell address

A slot number names one cell inside one tree page: an entry of that page's
slot directory ([`tree-page.md`](tree-page.md)).

- **It is a name, not a position.** A cell keeps its slot number from the
  commit that puts it into the page to the commit that takes it out, whatever
  happens to the page in between (S1 of [`tree-layout.md`](tree-layout.md)).
  A freed number is used again, the lowest first.
- **From 1.** Slot 1 is the first entry of the directory, and 0 is "no slot"
  (B6).
- **Tree pages only.** A flat page has positions, which move when a smaller
  key is added (P13 of [`flat-page.md`](flat-page.md)), and no slot numbers.
- **Sixteen bits** is more than a page can use: a page is at most 65,535
  bytes and a slot costs two of them.

A **cell address** is a page id followed by a slot number, six bytes
(`CellAddress`). It is what a link to a child stores, and what the subtree
directory stores for the root of a Merk (TREE-05). All zero is "no cell". A
page id with no slot, or a slot with no page id, is not a valid address
(`MissingId`).

## 4. Checksums

**C1. The function is CRC-32C.** Castagnoli polynomial `0x1EDC6F41`
(reflected: `0x82F63B78`), initial value and final XOR `0xFFFFFFFF`, input and
output reflected. This is the CRC of iSCSI (RFC 3720), ext4, and the `crc32`
instruction of SSE 4.2 and ARMv8. The check value for the nine bytes
`123456789` is `0xE3069283`.

**C2. It is stored masked.** The four bytes on disk are

```text
stored = rotate_right(crc, 15) + 0xA282EAD8   (mod 2^32)
```

as a little-endian `u32`. This is the masking LevelDB, RocksDB and the Snappy
framing format use.

The reason is nesting. The CRC of a byte string followed by its own CRC is a
constant. So if a larger record is made of smaller records that each end in a
plain CRC, the larger record's CRC depends only on their lengths, not on their
contents, and protects nothing. Masking removes that. A test shows both
halves: with plain CRCs two different contents give the same outer CRC, and
with masked CRCs they do not.

**C3. It covers the whole record except itself.** The covered bytes are every
byte of the record other than the four checksum bytes, in file order: the
bytes before the checksum field, then the bytes after it. That includes the
length and kind fields. A checksum that left the length out would accept a
zero length followed by a zero checksum.

**C4. Zero-filled bytes never verify.** The masked checksum of *n* zero bytes
is not zero for any *n* from 0 to 2^27, which is every length a checksum may
cover (C5). The test checks each of them. The shortest run of zeros whose
masked checksum is zero is 770,515,611 bytes long. So a region that was
preallocated or lost and reads back as zeros is never taken for a record.

**C5. One checksum covers at most 2^27 bytes (128 MiB)**
(`MAX_CHECKSUMMED_LEN`). CRC-32C detects every error of one, two or three bits
only while the covered bytes and the checksum together are at most 2^31 − 1
bits, which is just under 256 MiB. Two flipped bits exactly 2^31 − 1 bits
apart cancel, because *x*^(2^31 − 1) is 1 modulo the polynomial; a test
checks that, and that no smaller power is. The limit is set at half of that.
Anything larger is split into several records. This is a limit on formats,
not on values: a value larger than 128 MiB is stored as several records.

**C6. It is for accidents, not attacks.** The checksum detects torn writes,
bit rot and misdirected reads. It is not a keyed hash, and someone who can
write the files can forge it. Authenticating state is the job of Merk's
hashes.

**C7. Verify before use.** A reader verifies a record's checksum before it
uses any field of the record, except the fields it needs to find where the
record ends, which it bound-checks first (B10).

### Implementation

`lignin-format` uses the crate [`crc32c`](https://crates.io/crates/crc32c)
(0.6.9, MIT or Apache-2.0) for C1. It is the crate's only dependency. At build
time `crc32c` itself uses `rustc_version` and, through it, `semver`.

- It is pure Rust, as DEC-07 requires: no C code and no bindings. Its build
  script generates lookup tables and reads the compiler version, to decide
  whether the aarch64 path can be compiled (it can from Rust 1.80, and this
  workspace needs a newer compiler than that).
- It uses the CPU's CRC instruction where there is one (SSE 4.2 on x86_64, the
  CRC extension on aarch64), detected at run time, and a table-driven fallback
  elsewhere. That covers the three targets of DEC-06. On the development
  machine (Zen 5) it checks an 8 KiB page in about 0.25 µs.
- The tests run whichever path the test machine selects. On the x86_64
  development machine that is the SSE 4.2 path. The aarch64 path and the
  fallback are checked only when the same tests run on such a machine.
- `lignin-format` forbids `unsafe`, and the CPU instruction can only be
  reached through `unsafe`. So a fast checksum has to come from a dependency
  or be given up. The checksum is on the path of every page read and every
  commit, which is why it is not given up.
- The crate is not trusted for the definition. The tests check it against a
  bit-at-a-time reference written from C1 and against the RFC 3720 vectors.
  Replacing the implementation later changes no byte on disk.

## 5. Records

A record is the unit that is written in one piece and verified in one piece.

**R1. Every byte belongs to one record.** From the first byte of a file to its
valid length, each byte is part of exactly one record. No byte is outside a
checksum. Padding, where a format wants it, sits inside a record and is zero.

**R2. A record starts with a fixed-width header** (B3).

**R3. Its extent is known before its content is trusted.** The length is
fixed, or it is a field of the header, or the pointer that led to the record
supplies it. The reader checks it against the format's maximum and the bytes
available (B10), then verifies the checksum (C7).

**R4. It carries one checksum** as section 4 defines, and no checksum covers
more than C5 allows.

**R5. It lies in one file.** A record never spans files, and every record in a
file has that file's format version. Records carry no version of their own.

**R6. A record kind code is never 0, and an unknown code is an error**
(`UnknownRecordKind`). A reader never skips what it does not understand.

**R7. A reader reports; it does not repair.** A record that fails a check
gives a typed error. Whether that ends a log scan, rejects a page or refuses
an open is for the file's own format and the store to say.

Records do not put their file id or offset under the checksum. They do not
need to: file space is never reused (B11), so an old record cannot turn up
where a new one is expected. A format that ever reuses file space must add
them.

### 5.1 The default frame

`RecordHeader` frames a payload with its length and checksum. It is 8 bytes.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `len`: payload length in bytes, `u32` |
| 4 | 4 | checksum (section 4) of bytes 0 to 3 followed by the payload |
| 8 | `len` | payload |

- `len` is at most 2^27 − 4 (`RecordHeader::MAX_PAYLOAD_LEN`), which keeps the
  covered bytes within C5. Each format passes its own, smaller maximum to the
  decoder.
- A payload may be empty.
- A file with several kinds of record puts the kind code in the first byte of
  the payload, where the checksum covers it.
- Reading is two steps, so a reader can fetch the payload after it has seen
  the header: `RecordHeader::decode` checks the length, and
  `RecordHeader::verified_payload` checks the payload. `read_record` does both
  on a byte string and returns what follows the record; `write_record` appends
  a framed record.
- When too few bytes are available, `Truncated` says how many bytes the
  decoder needed and how many it had, both counted from the first byte it was
  given. For `read_record` that is the start of the record, so `needed` is
  the whole record's length.

Use the default frame unless a record has a tighter byte budget. Two already
do: FMT-03's hash-only delta (48 bytes in total) and FMT-06's block (8 bytes
of overhead on an incompressible block, which the default frame already uses
up before the block says how it is compressed). Such a record defines its own
header under R1 to R7. A block whose length comes from its pointer (R3), for
example, needs no length field at all. In the end the two are one: a delta
is stored in a block, and the block's frame is its only one
([`page-delta.md`](page-delta.md), PD4).

Example: the payload `lignin` (6 bytes) is the 14 bytes

```text
06 00 00 00  2d 50 d4 41  6c 69 67 6e 69 6e
```

and the empty payload is `00 00 00 00  a6 7b 11 3a`.

## 6. File header

Every file except `LOCK` starts with the same 20 bytes (`FileHeader`).

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | magic: the bytes `LIGNINDB` (`4c 49 47 4e 49 4e 44 42`) |
| 8 | 2 | file kind, `u16` |
| 10 | 2 | format version, `u16` (section 7) |
| 12 | 4 | sequence, `u32` |
| 16 | 4 | checksum (section 4) of bytes 0 to 15 |

**H1. These 20 bytes are the same in every format version, forever.** That
covers the layout, the magic, and the header's checksum: its function, its
masking and the bytes it covers (sections 4 and 6 as written here). A later
version changes what follows the header, never the header. So any build can
tell what any file is, and can tell a damaged header from a file that is
merely newer than it understands.

**H2. File kinds.**

| Code | Kind | File | Defined by |
| --- | --- | --- | --- |
| 1 | `Manifest` | `MANIFEST` | FMT-06 |
| 2 | `Wal` | `wal/<n>.log` | FMT-05 |
| 3 | `Segment` | `seg/<n>.seg` | FMT-06 |
| 4 | `ValueLog` | `val/<n>.val` | FMT-07 |
| 5 | `PageTable` | `table/<n>.pt` | STORE-03, [`page-table.md`](page-table.md): the page table's links and a sample of the value log's counts |

The codes are registered here so that tasks working in parallel do not choose
the same one. A new kind is a format change (U6).

**H3. Sequence.** For a numbered kind it is the file's id (3.3) and is never
0. A reader that finds a file under a name that does not match its header's
kind and sequence must refuse it. For `Manifest` FMT-06 says what the field
holds; this specification accepts any value there.

**H4. Order of checks.** `FileHeader::decode` checks, in this order:

1. at least 20 bytes: else `Truncated`
2. the magic: else `BadMagic`
3. the checksum: else `ChecksumMismatch`
4. the format version is one this build reads: else
   `UnsupportedFormatVersion`
5. the kind code is defined in that version: else `UnknownFileKind`
6. the sequence is not 0 for a numbered kind: else `InvalidFileSequence`

The checksum comes before the version so that a flipped bit in the version
field is reported as damage, not as "written by a newer build". The version
comes before the kind because a newer version may define kinds this build has
never heard of.

**H5. Written once.** The header is the first thing written to a new file and
is never changed (B11). A file shorter than 20 bytes was never completely
created.

Example: `seg/00000310.seg` in format version 1 starts with

```text
4c 49 47 4e 49 4e 44 42   magic "LIGNINDB"
03 00                     kind 3, segment
01 00                     format version 1
36 01 00 00               sequence 310
d2 c7 2f a6               checksum
```

## 7. Format version and upgrade rule

### 7.1 The number

**V1. One number for the whole format.** The format version is a `u16`,
starting at 1. Zero is never a version. There is one number for all file
kinds together, not one per kind: a file's kind and its format version fix the
layout of every byte in it.

**V2. Every file carries it**, in its header (section 6). Every record in the
file has that version (R5).

**V3. The manifest's version is the database's version.** The format version
in the header of `MANIFEST` is the **database format version**. This is the
"format version" the plan lists in the manifest; FMT-06 does not add a second
one.

**V4. No file is newer than its manifest.** Every live file has a format
version less than or equal to the database format version. A file that breaks
this makes the database invalid.

**V5. It is local to the node.** The format version never affects a root hash,
a proof or an `OperationCost`. It has nothing to do with GroveVersion or the
protocol version, and two nodes on different format versions agree on
everything a peer can observe.

In code, `FormatVersion` is an enum with one variant for each version the
build can read. A decoder matches on it, so adding a version does not compile
until every decoder says what it does for that version, and removing a
variant removes read support everywhere at once.

### 7.2 The upgrade rule

**U1. A build reads a range and writes one version.** It reads every version
from `FormatVersion::OLDEST_READABLE` to `FormatVersion::CURRENT`, and every
file it creates is written at `CURRENT`.

**U2. Open refuses what it cannot read.** If the database format version, or
the version of any file the build needs, is outside the readable range, open
fails with `UnsupportedFormatVersion` and writes nothing. A build never
guesses at a newer format.

**U3. Upgrading is opening for writing with a newer build.** There is no
separate upgrade step. On such an open the build first replaces the manifest
with one at `CURRENT`, and only then creates other files at `CURRENT`. That
order is what keeps V4 true if the open is interrupted.

**U4. Nothing is rewritten for an upgrade.** Files from older versions stay as
they are and stay live. They go away in the normal course of consolidation and
cleaning. A database may therefore hold files of several versions at once, and
each file is decoded by its own header's version. This is what immutable files
and hard-linked checkpoints need: an upgrade cannot touch a file that a
checkpoint shares.

**U5. Opening read-only upgrades nothing.** It creates no file and leaves the
database format version as it was.

**U6. Any change to bytes or their meaning is a new version.** That includes a
new field, a new record kind or file kind, a new code in an enumeration, a
changed meaning of an existing field, a use for a reserved field, and a change
to a record's checksum or to what it covers. There are no compatible minor
changes, because a reader never skips what it does not know (R6, B7).

The file header is the one thing a new version cannot change (H1). A version
may change the checksum of every record after the header, but not the
header's own. Otherwise an older build would report a newer file as damaged,
when U2 requires it to report an unsupported version.

**U7. There is no downgrade.** Once a newer build has opened a database for
writing, older builds refuse it (U2). The way back is a checkpoint taken
before the upgrade: its manifest is at the old version, and the upgraded
database never changes the files they share (B11, U4).

**U8. Read support for an old version ends only with a way out.** A build may
drop a version from its readable range only if a released tool rewrites such a
database at a newer version (the `lignin` command, OPS-01). Until an operator
runs it, U2 stops the open with a typed error.

**U9. What a new version must bring.** The task that adds version *N*:

1. adds a section to this document saying what changed and for which file
   kinds
2. adds the `FormatVersion` variant, and keeps the decoders for every older
   version in the readable range
3. says, for each changed layout, how an older record is read in place or
   converted on load, or states that the version needs the offline rewrite of
   U8
4. commits golden files for version *N* and keeps the older ones decoding
   (FMT-08)

**U10. Version 1 freezes at the first release.** Until LigninDB first ships in
a GroveDB release (the cutover, OPS-07), version 1 is still being defined:
FMT-02 to FMT-07 fill it in, and its layouts may change without a new number
while the golden files are regenerated. From that release on, version 1 never
changes and U6 applies without exception.

## 8. What the tasks that build on this take from it

| Task | Takes from here | Still its own to define |
| --- | --- | --- |
| FMT-02 Tree page codec | `PageId` for cross-page pointers, with 0 for "none". A page image has no version of its own (R5). It lies in a record with a checksum (R1, R4): the segment block that holds it, so it carries none itself ([`tree-page.md`](tree-page.md), TP3). | The cell layout, the page header, and how a cell stores the value pointer. `SlotId` and `CellAddress` (3.4) were added by this task. The pointer's fields and its 16-byte encoding are FMT-07's. |
| FMT-03 Page delta records | The record rules R1 to R7 and the checksum. A run of page deltas lies in a record with a checksum (R1, R4): the segment block or the log group that holds it, so it carries none itself ([`page-delta.md`](page-delta.md), PD4). | The records and the run. A changed hash is 35 bytes, and 47 as a block. |
| FMT-04 Flat page codec | As FMT-02: a flat page image lies in the record that holds it and carries no checksum itself ([`flat-page.md`](flat-page.md), P12). | Everything inside the page. |
| FMT-05 Log record format | `CommitVersion` for a commit group; consecutive groups differ by one (3.2). The default frame. A group above 128 MiB is several records (C5). | Group layout, record kinds, and what a failed record means for the tail (R7). |
| FMT-06 Segment, manifest format | The file header and kind codes. The manifest's header version is the database format version (V3). `FileId` for naming live files. | Block headers, the footer index, the manifest body, the meaning of the manifest's sequence field (H3). The manifest body also records the store's page size, which no page image holds ([`segment-manifest.md`](segment-manifest.md), M17). |
| FMT-07 Value log format | The default frame. A value above 128 MiB is several records (C5). `PageId` for the back pointer and `FileId` for the value pointer. | The record layout, the value pointer, and the rules that make the back pointer enough to find the owning cell ([`value-log-format.md`](value-log-format.md)). |
| TREE-01 Merkle host model | A page id is logical, stable under relocation, dense and shared by all page kinds (3.1). | Everything about what goes in a page. |
| TXN-01 Version model | The commit version is a dense count from 0, with 2^64 − 1 free as a marker (3.2). | Visibility, pins, and every rule about writers. |
| STORE-03 Page table, STORE-10 Page id allocation | Page ids run from 1 and are dense, so a table index is the id minus one. An id is reused only under STORE-10's rule. | The table's own file format and the allocator. |

## 9. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one before formats are built on it.

1. **Page ids are 32 bits**, not 64 (3.1). Smaller records against a ceiling
   of 2^32 − 1 pages.
2. **Commit versions are dense**: each commit is the previous plus one (3.2).
   TXN-01 owns the version model and could ask for gaps; nothing here needs
   them.
3. **File ids are a shared identifier** (3.3). The register names page ids and
   commit versions. File ids are added because FMT-02, FMT-06 and FMT-07 each
   store one and run in parallel.
4. **The checksum is stored masked** (C2), and **one checksum covers at most
   128 MiB** (C5).
5. **The file header is 20 bytes and frozen across versions** (H1), with the
   file kind codes registered here (H2).
6. **The upgrade rule** (7.2): one format number for all files, a build writes
   only its current version, upgrade happens on the first open for writing,
   nothing is rewritten, and there is no downgrade. An opt-in upgrade (a build
   that keeps writing the old version until told otherwise) would allow
   rolling a build back. It was not chosen because it needs every encoder kept
   for every old version. The bytes on disk are the same either way, so this
   can still change when version 2 is designed.
7. **In-page slot numbers were not defined by FMT-01.** They are part of the
   page layout, and the plan fixed their width at 16 bits. FMT-02 added
   `SlotId` and the cell address (3.4): TREE-05's subtree location needs a
   cell address, which is a page id plus a slot. FMT-07's back pointer does
   not: it names a page and stores no slot (section 5 of
   `value-log-format.md`).
8. **`crc32c` is a dependency** (section 4), in place of a slower checksum
   written in safe Rust inside the crate. Its software fallback is not
   exercised by the tests on the development machine.
9. **Version 1 is not frozen until the first release** (U10).
10. **The code is in four modules**, not only `ids`, which is the one module
    the register names: `ids` (identifiers), `checksum`, `header` (file
    header, format version, record frame) and `error`.
11. **The page-table checkpoint file has a kind code but no format task.**
    `table/<n>.pt` is in the plan's file list, so it is registered in H2. No
    FMT task defines its layout; this document assumes STORE-03 does, under
    the rules here. It does, in [`page-table.md`](page-table.md), section 3:
    links records, counts records for the value log's counts of live
    values, and an end record.

## 10. Code and tests

All of this is in `lignin-format`. Run the tests with
`cargo test -p lignin-format`.

| Rule | Code | Tests |
| --- | --- | --- |
| Identifiers (section 3) | `ids::{PageId, FileId, CommitVersion, SlotId, CellAddress}` | Round trip in both directions over boundary and random values; the reserved pattern; byte order; ordering; `CommitVersion::next`. Every slot number; half a cell address. |
| C1 | `checksum::Checksum` (the CRC itself is private) | RFC 3720 vectors; agreement with a bit-at-a-time reference on random input. |
| C2, C3 | `checksum::Checksum` | Masked vectors; round trip; the same checksum however the covered bytes are split; every single-bit flip rejected; the nesting test. |
| C4 | `checksum::Checksum` | No zero checksum for any run of 0 to 2^27 zero bytes; zero-filled bytes rejected as a file header and as a record. |
| C5 | `checksum::MAX_CHECKSUMMED_LEN`, `RecordHeader::MAX_PAYLOAD_LEN` | The polynomial's period is 2^31 − 1 bits and the limit lies inside it; the length bound at, above and far above the maximum. |
| Default frame (5.1) | `header::{RecordHeader, read_record, write_record}` | Golden bytes; round trip of records one after another; random 8-byte headers decode only to what encodes back to them; every shorter input; every single-bit flip, with the error each one must give; the length checked before the payload. |
| File header (section 6) | `header::{FileHeader, FileKind}` | Golden bytes for each kind; round trip from values, and from random headers with a valid checksum; every shorter input; every single-bit flip; every unreadable version and every unknown kind with a valid checksum; each step of the order of checks (H4); sequence 0. |
| Format version (section 7) | `header::FormatVersion` | Every `u16` either round-trips or is rejected. |
| B10 | `error::FormatError` | Each rejection above is asserted by its error. `MissingId`, `NonZeroReserved` and `UnknownRecordKind` are defined here for the formats that need them and have no decoder yet. |

The golden bytes and checksum vectors in the tests were computed with an
independent bit-at-a-time CRC-32C, not with the crate under test.
