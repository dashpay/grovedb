# LigninDB on-disk format: the value log

This is the specification for task FMT-07 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines the files `val/<n>.val`, which hold the values that are too long to
sit inside their cell:

- the file (section 1)
- the record (section 2)
- how a value becomes one record or several (section 3)
- the value pointer a cell stores in place of the value (section 4)
- the back pointer from a value to its owner: what it guarantees, what a
  cleaner may conclude from it, and the rule the layers above keep when a
  cell moves (section 5)

It builds on the shared rules in [`format.md`](format.md): byte conventions
(B1 to B11), identifiers, the checksum (C1 to C7), records (R1 to R7), the
file header and the format version. Rule names such as B8 and C5 refer to
that document. Flat pages are in [`flat-page.md`](flat-page.md).

It does not decide which values go to the value log (the inline threshold,
TREE-08), when a value is written, when a file is closed and a new one
started, or when a file is cleaned (STORE-09). It does not define the tree
page cell that holds a value pointer (FMT-02).

The code is `lignin-format::value_log`. Section 9 maps each rule to its type
and its tests.

## 1. The file

**F1. Header.** A value-log file starts with the 20-byte file header, with
file kind 4 (`ValueLog`) and the file's id in the sequence field.

**F2. Values, back to back.** After the header come values, one after
another, each as its records (section 3). There is nothing else in the file:
no footer, no index, no padding. The first record is at offset 20.

**F3. One kind of record.** Every record in the file is a value record
(section 2). So a record carries no kind code.

**F4. No end marker.** The file's valid length is in the manifest (FMT-06).
A reader that walks a file stops at the first value that fails a check and
reports the offset it reached (R7). What a failed tail means is the store's
to say.

**F5. Append only.** A record is never changed after it is written, and file
space is never used twice (B11). So a file id and an offset name one value
and no other, for ever. Section 5 depends on this.

## 2. The record

A record is a default frame (5.1 of `format.md`) around 10 bytes of fields
and a part of the value. Its header is 18 bytes (`RECORD_HEADER_LEN`).

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | payload length, `u32`: 10 + *n* |
| 4 | 4 | checksum of bytes 0 to 3 followed by bytes 8 to 17 + *n* |
| 8 | 4 | owner: the page id (`PageId`) of the page whose cell points at the value |
| 12 | 2 | part, `u16`: the record's place among the value's records, from 0 |
| 14 | 4 | value length, `u32`: the length of the whole value in bytes |
| 18 | *n* | data: the value's bytes for this part |

**V1. The owner is always there.** The page id is never 0 (`MissingId`). It
is the back pointer (section 5).

**V2. The value length is at least 1.** An empty value has no record.

**V3. The part exists.** `part` is less than the number of parts a value of
that length has (V6).

**V4. The data is exactly that part's bytes.** *n* is the length V6 gives
for that part of a value of that length, never more and never less. So a
record always carries at least one byte of the value.

**V5. Order of checks.** `ValueRecord::decode` checks, in this order:

1. at least 8 bytes: else `Truncated`
2. the payload length is at most 2^27 − 4: else `RecordTooLong`
3. the whole record is there: else `Truncated`
4. the checksum: else `ChecksumMismatch`
5. the payload holds the 10 bytes of fields: else `RecordTooShort`
6. the owner (V1): else `MissingId`
7. the part (V2, V3): else `NoSuchPart`
8. the data length (V4): else `WrongPartLength`

Steps 1 to 4 are the default frame's own. Nothing in the record is used
before its checksum is verified, except the length that says where the
record ends (C7).

Example: the value `lignin value` (12 bytes), owned by a cell of page 7, is
the 30 bytes

```text
16 00 00 00                            payload length 22
ca d6 d7 75                            checksum
07 00 00 00                            owner: page 7
00 00                                  part 0
0c 00 00 00                            value length 12
6c 69 67 6e 69 6e 20 76 61 6c 75 65    "lignin value"
```

## 3. Values and parts

**V6. A value is cut into parts of 134,217,714 bytes.** That number,
`MAX_PART_LEN`, is 2^27 − 14. A value of *L* bytes has

```text
parts(L) = ceil(L / MAX_PART_LEN)
```

records. Part *i* carries the bytes of the value from *i* × `MAX_PART_LEN`
up to (*i* + 1) × `MAX_PART_LEN`, or up to *L* for the last part. Every part
except the last is full. A value of at most `MAX_PART_LEN` bytes is one
record, which is the usual case.

`MAX_PART_LEN` is the most that C5 allows: the checksum of a full part
covers 4 bytes of length, 10 bytes of fields and `MAX_PART_LEN` bytes of
data, which is exactly 2^27 bytes. A smaller part would add records without
adding protection.

**V7. A value's records are back to back in one file.** They are written in
order of part, part 0 first, with nothing between them, and all of them in
the same file. So a value takes

```text
extent(L) = L + 18 × parts(L)
```

bytes of its file, and a reader fetches all of it with one positional read
(STORE-09), split or not. A writer must not let records of another value in
between.

| Value length | Records | Bytes in the file |
| --- | --- | --- |
| 1 | 1 | 19 |
| 134,217,714 | 1 | 134,217,732 |
| 134,217,715 | 2 | 134,217,751 |
| 4,294,967,295 | 33 | 4,294,967,889 |

**V8. Every part names the value.** Each record of a value repeats the
owner and the value length. A reader that puts a value together checks that
the parts come in order (`UnexpectedPart`) and that each names the same
owner and value length as part 0 (`PartOfAnotherValue`). Each record can
also be checked on its own (V3, V4).

**V9. A value is at most 2^32 − 1 bytes** (`MAX_VALUE_LEN`), which is at
most 33 parts. A longer value cannot be stored (`ValueTooLong`).

**V10. One encoding.** A value and its owner have exactly one byte string
(B8): the split is fixed by V6, and no field has a second spelling.

Example: a value of 134,217,715 bytes whose byte *i* is *i* mod 251, owned
by a cell of page 9, is two records. Their headers are

```text
fc ff ff 07  a5 18 84 93  09 00 00 00  00 00  f3 ff ff 07
0b 00 00 00  3b 79 ef 1a  09 00 00 00  01 00  f3 ff ff 07
```

The first is followed by 134,217,714 bytes of the value and the second by
its last byte.

## 4. The value pointer

A cell whose value is in the value log stores a value pointer in its place
(`ValuePointer`). It is 16 bytes.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | file id (`FileId`) of the value-log file |
| 4 | 8 | offset of the value's first record in that file, `u64` |
| 12 | 4 | value length, `u32` |

**P1. It names a whole value.** The offset is that of part 0. The value
length is the whole value's.

**P2. What is valid.** The file id is not 0 (`MissingId`). The offset is at
least 20, since the file header comes first, and at most 2^63 − 1, the
largest offset a file API takes (`InvalidOffset`). The value length is at
least 1 (`EmptyValue`). `ValuePointer::decode` checks them in that order.

**P3. It is enough for one positional read.** The value length gives the
extent (V7), so a reader asks for `extent(L)` bytes at the offset and has
every record of the value. No second read is needed to learn a length.

**P4. Reading through a pointer.** `ValuePointer::read` takes the bytes at
the offset and the id of the page whose cell holds the pointer. It verifies
the first record (section 2) and requires that:

- it has the value length the pointer states: else `WrongValueLength`
- its owner is the page whose cell holds the pointer: else `WrongOwner`

Then it verifies the other records and puts the parts together (V8), which
also requires the first record to be part 0.

The owner check matters because file space is never reused (F5). A pointer
that is stale, or that was copied to a cell of another page, leads to an
old record that is intact and has a good checksum. The owner is what tells
the reader that something is wrong.

**P5. The file's version decides the extent.** A database may hold value-log
files of several format versions (U4). The extent and the record layout are
those of the version in the header of the file the pointer names, not of
the file that holds the pointer. `extent_len`, `ValuePointer::read` and the
decoders take that version.

**P6. In a flat leaf the pointer is the entry's value.** An entry whose
out-of-line flag is set (P5 of `flat-page.md`) holds these 16 bytes as its
value bytes, and nothing else. FMT-02 decides how a tree page cell stores
the pointer. A cell that already stores the value length can store the file
id and the offset alone and build the pointer from the three.

Example: the pointer to the record in section 2, when it is the first
record of `val/00000012.val`, is

```text
0c 00 00 00  14 00 00 00 00 00 00 00  0c 00 00 00
```

## 5. The back pointer

The owner field of a record is its back pointer: the id of the page whose
cell held the pointer to the value when the record was written. It is there
so that a cleaner reading a value-log file can find the cell that points at
each value by going to one page, with no keyed lookup and no walk of a
tree. The store, which knows no keys, could not do a keyed lookup anyway.

### 5.1 What it names

**O1. It names a page. The pointer names the cell.** Inside the page, the
owning cell is the one that holds the value pointer to this value: file,
offset and length. A cleaner has that pointer, because it knows which file
it is reading and at which offset the value starts (`scan` gives it).

- In a **tree page** (FMT-02) the owner is the node cell whose value is
  that pointer.
- In a **flat leaf** (FMT-04) the owner is the entry whose value is out of
  line and is that pointer (P6). `ValuePointer::find_in_flat_leaf` returns
  its position.

The back pointer holds no slot. A slot in a flat leaf is a position in key
order, which moves whenever a smaller key is added or removed (P13 of
`flat-page.md`), and a record cannot be updated to follow it. The pointer
is a name that never changes: by F5 it names one value for ever, and by O2
one cell holds it. Finding the cell costs a look through the cells of one
page, with no key comparison.

**O2. One owner.** At every commit version, at most one cell holds a
pointer to a given value. A value is appended for one cell, and a cell that
is copied gets a value of its own.

### 5.2 What it guarantees

**O3. It is exact for as long as the cell stays in its page.** The back
pointer is right when it is written. It stays right through everything
that happens to the cell inside that page: a new slot, a new page image, a
delta, a consolidation, a changed hash, other cells coming and going. None
of these needs anything written to the value log.

**It goes stale in one case: the cell is carried to another page.** Flat
pages split and merge (TREE-07), and Merk nodes move between pages under
fragment maintenance (TREE-03). The record was written once and cannot
follow.

**O4. What a cleaner may conclude.** For a value at offset *o* of file *F*,
with length *L* and back pointer *P*:

- **Hit.** Page *P* exists and holds a cell whose pointer is (*F*, *o*, *L*).
  Then the value is live and that cell is its owner. This is always true.
  It rests only on F5 and O2: page ids are reused, but a cell of a reused
  page points somewhere else.
- **Miss.** Page *P* does not exist, or holds no such cell. Then the owner
  is not in page *P*. Either the value is dead (its cell was deleted, was
  given another value, or went with a dropped page), or its cell was
  carried to another page. **The record cannot tell which.** A miss alone is
  not proof that a value is dead.

So the back pointer is authoritative when it hits and only a hint when it
misses. Sections 5.3 and 5.4 are what turns a miss into a safe answer.

A value is **live at a commit version** if some cell holds its pointer in
the state of that version. A hit and a miss are about the version the
cleaner looks at, which is the latest. A value that is dead there may still
be live for an open snapshot or a checkpoint (O8).

### 5.3 The move rule

**O5. A cell that is carried to another page takes its value with it.**
Whoever moves a cell that holds a value pointer to another page appends
the value again, with the new page as its owner, and stores the new
pointer in the moved cell, in the same commit. The old records are then
dead.

Under O5 no back pointer is stale at any commit version, and a miss means
dead. O5 applies to a move between pages only. Rewriting a cell where it
is needs nothing (O3).

O5 is the smallest rule that makes the back pointer enough to find the
owning cell. Its cost is that a page split or merge writes the values of
the cells it moves once more. Section 8, item 6 lists what the alternatives
would take.

### 5.4 The cleaner's rules

**O6. A file is never deleted on misses alone.** The store keeps, for each
value-log file, its own count of live values: one more when a cell is made
to hold a pointer into the file, one fewer when a cell stops holding one
(the cell is deleted, its value is replaced or appended again, or its page
is dropped). The count comes from what the layers above report, not from
back pointers.

A cleaner treats a file as empty only when it has found the owner of as
many values as that count says are live, and has moved each of them. If it
finds fewer, a cell was carried without its value, against O5. The file
stays, its bytes stay, and the store reports the break.

With O6, breaking O5 costs space and raises an error. Without O6, O5 would
be the only thing between a moved cell and a lost value.

**O7. The count belongs to a commit version.** It is the count of the
latest committed version, and it changes in the same commit as the cell
change that causes it, never earlier. A count that ran ahead of the pages
could reach zero while a committed cell still points into the file. After
a crash the count is that of the version recovery lands on: it is
recovered with the pages, from the same log, or counted again from them.

**O8. A file outlives every version that can reach it.** The count speaks
for the latest version only. A snapshot pins an older one, in which a cell
may hold a pointer that the latest version no longer has: the cell was
deleted, given another value or moved since, or the cleaner moved its
value. So a file that becomes empty at commit version *v* is deleted only
when no snapshot pinned at a version before *v* is left. A checkpoint
holds its own hard link to the file (TXN-06) and keeps the file for
itself.

**O9. Cleaning a file** is then:

1. Walk the file (`scan`). Each value comes with the pointer its owner
   must hold.
2. For each value, look in the page its back pointer names, in the latest
   version, for the cell that holds that pointer (O1). No such cell: go on
   to the next value.
3. Otherwise append the value to another file with the same back pointer,
   and put the new pointer in the cell: through a page delta for a tree
   page, through a new image for a flat leaf (STORE-09). The old value
   counts as one fewer in its file from the commit that changes the cell.
4. The file is empty when its count is zero (O6, O7), and is deleted when
   O8 allows.

**O10. The reader checks the owner too** (P4). A reader always knows which
page it is reading for. So a cell that was carried without its value is
caught the first time its value is read (`WrongOwner`), and VER-07 can
check O2 and O5 for every cell. Appending the value again for the cell, as
O5 asked, repairs it.

## 6. Errors

Every decoder returns `FormatError` and never panics (B10). The checks that
are specific to the value log are `FormatError::ValueLog(ValueLogError)`:

| Error | Meaning |
| --- | --- |
| `WrongFileKind` | The file header is valid but is not a value log's. |
| `RecordTooShort` | The payload is shorter than the 10 bytes of fields. |
| `NoSuchPart` | A value of that length has no part with that number (V2, V3). |
| `WrongPartLength` | The record does not carry that part's number of bytes (V4). |
| `UnexpectedPart` | A value does not start with part 0, or its parts are not in order (V7). |
| `PartOfAnotherValue` | A later part names another owner or value length than part 0 (V8). |
| `WrongValueLength` | The value at a pointer is not of the pointer's length (P4). |
| `WrongOwner` | The value at a pointer is owned by another page (P4, O10). |
| `EmptyValue` | An empty value, or a pointer to one (V2, P2). |
| `ValueTooLong` | A value above 2^32 − 1 bytes (V9). |
| `InvalidOffset` | A pointer into the file header, or past 2^63 − 1 (P2). |

`Truncated`, `RecordTooLong`, `ChecksumMismatch`, `MissingId` and the file
header's errors keep their meaning from `format.md`. When a value of several
records is cut short, `Truncated` counts from the first byte of the value.

## 7. What the tasks that build on this take from it

| Task | Takes from here | Still its own to define |
| --- | --- | --- |
| STORE-09 Value log | `encode_value` or `write_value` to append, `ValuePointer` and its extent for the one positional read, `scan` and O4 to O9 for cleaning, `record_len` to read a file piece by piece. A value's records go into one file with nothing between (V7). The count of live values for each file, committed and recovered with the pages (O6, O7), and the rule for deleting an empty file (O8). | File rotation, when a value is durable, what a failed tail means, how the count is stored, when a file is cleaned. |
| TREE-08 Out-of-line values | `ValuePointer::read` with the cell's page for the read path. `MAX_VALUE_LEN`. An empty value is never out of line (V2). Regrouping a page's values at consolidation appends them again with the same owner, which needs nothing more. | The inline threshold. Telling the store when a cell stops holding a pointer (O6). |
| TREE-03 Fragment maintenance, TREE-07 Flat keyspace | The move rule O5 for every cell with a value pointer that goes to another page in a split, a merge or a fragment move. | Which cells move. |
| TREE-09 Subtree drop | A dropped cell that holds a value pointer counts as one fewer live value in its file (O6). | How it learns of those cells without reading every page, or whether it reads them. |
| FMT-02 Tree page codec | The pointer's three fields and their valid ranges (section 4). An accessor that finds the cell holding a given pointer (O1). No slot id has to stay stable for the value log. | How the cell stores the pointer. Slot ids: this task adds none. |
| FMT-04 Flat page codec | The out-of-line value of a flat leaf entry is the 16-byte pointer (P6). | Nothing changes in its bytes. |
| FMT-08 Decoder fuzzing | The decoders to fuzz: `ValueRecord::decode`, `read_value`, `scan`, `record_len`, `ValuePointer::decode`, `ValuePointer::read`. Golden bytes are in the tests. | The fuzz targets and the committed golden files. |
| VER-07 Physical invariant checker | O2 and O5 as invariants: every value pointer leads to a value whose owner is the page of the cell that holds it, and no two cells hold the same pointer. | The checker. |

## 8. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one while version 1 is still open (U10).

1. **A value is at most 2^32 − 1 bytes** (V9), so the value length is a
   `u32` in the record and in the pointer, as B4 has it. Merk's cost
   functions already take value lengths as `u32`.
2. **The part size is the largest C5 allows**, and the split is fixed (V6).
   A value is split only when one checksum cannot cover it.
3. **A value's records are back to back in one file** (V7). This keeps the
   read to one positional read for a split value too, and needs no pointer
   from one record to the next. The cost is that a writer appends a value's
   records without interleaving.
4. **The value pointer is defined here**, is 16 bytes, and is what a flat
   leaf stores as an out-of-line value (section 4). `format.md` and
   `flat-page.md` first left it to FMT-02 and now point here. FMT-02 still
   owns how a tree cell stores it.
5. **The back pointer is a page id, with no slot** (O1). The owner is found
   in the page by the pointer it holds. This serves tree cells and flat
   entries alike and is unaffected by anything that happens inside a page.
   No `SlotId` is added by this task; FMT-02 defines slots if it needs them.
6. **The move rule O5.** The register asks for a back pointer that "is
   enough to find the owning cell". With records that are written once and
   cells that move between pages, no back pointer can be that on its own:
   it is enough while the cell stays in its page, and O5 is the smallest
   rule that makes it enough always. The alternatives each need more:
   - *Leave the value where it is and let the back pointer go stale.* The
     store must then know which values are dead without the back pointer
     (a set of dead values for each file, not only a count), a file with a
     stale live value cannot be deleted until that value is appended again
     at some later point, and a reader can no longer check the owner (P4).
   - *Put the owner's key in every record* and fall back to a keyed lookup
     on a miss. It costs the key and the subtree's identity in every
     record, and the cleaner has to call up into the tree layer.
   - *Give each value a stable id and a table from id to location*, as
     pages have. Cells then never hold a location, and neither cleaning nor
     moving a cell touches the other side. It adds a table lookup to every
     read and a table to keep on disk.
7. **The cleaner's rules O6 to O8.** They ask STORE-09 for a count of live
   values for each file that commits and recovers with the pages, ask the
   tree layers to report every pointer a cell stops holding, including
   under a subtree drop (TREE-09), and keep an empty file until no snapshot
   from before it became empty is left. They are here because without them
   a broken O5, a count that runs ahead of the pages, or an old snapshot
   loses a value silently.
8. **A reader checks the owner** (P4, O10). It costs one comparison and is
   what catches a cell that was carried without its value.
9. **No record kind, no footer, no record for an empty value** (F2, F3, V2).
10. **An offset is at most 2^63 − 1** (P2).
11. **Decoders take the format version of the value-log file** (P5), as
    7.1 of `format.md` asks, so that a second version does not compile
    until each decoder says what it does for it.
12. **The value log's own errors are one variant of `FormatError`** that
    wraps `ValueLogError`, as the flat page's are.

## 9. Code and tests

All of this is in `lignin-format::value_log`. Run the tests with
`cargo test -p lignin-format value_log`.

| Rule | Code | Tests |
| --- | --- | --- |
| The file (section 1) | `scan`, `Values` | Golden bytes of a file; every value comes back with the pointer to it; every other file kind, a short header and a damaged header are rejected; a torn, damaged or zero-filled tail ends the walk at the last whole value. |
| The record (section 2) | `ValueRecord`, `record_len`, `RECORD_HEADER_LEN` | Golden bytes; the record is the default frame around its fields and data; round trip; every shorter input; every single-bit flip, with the error each must give; each invalid field with a valid checksum; the order of checks; random records with a valid checksum decode only when valid and encode back to the same bytes. |
| Values and parts (section 3) | `encode_value`, `write_value`, `read_value`, `Value`, `extent_len`, `checked_value_len`, `MAX_PART_LEN`, `MAX_VALUE_LEN` | A value one byte above `MAX_PART_LEN` is two records with golden headers, and one of exactly `MAX_PART_LEN` is one. The split at every length from 1 to 40 for part sizes 1 to 7, with the same code and a smaller part size. Parts out of order, missing, repeated or from another value. The length limits, without allocating a value. |
| The value pointer (section 4) | `ValuePointer` | Golden bytes; round trip; random pointers decode only when valid, with the error each must give; a read through a pointer for the wrong page, with the wrong length, with too few bytes, or into the middle of a value is rejected; a pointer is found in a flat leaf only where it is an out-of-line value. |
| O1 to O5: the back pointer finds the owner | `ValueRecord::owner`, `scan`, `ValuePointer::find_in_flat_leaf` | `the_back_pointer_is_enough_to_find_the_owning_cell`: 6,000 random inserts, overwrites, deletes and moves between pages, over tree pages (a model whose slots change on every write) and flat leaves (real FMT-04 images, rebuilt on every write), with page ids reused. A cleaner that sees only a file's bytes, and looks only in the page each back pointer names, finds every cell that points into the file, moves its value, and leaves every logical value unchanged. |
| O4, O6, O10: the stale case | the same, and `ValuePointer::read` | `a_cell_carried_to_another_page_without_its_value_is_never_dropped`: a cell is moved between pages of each kind without its value. The back pointer misses, the reader gets `WrongOwner`, the cleaner finds fewer owners than the count and keeps the file, and the value's bytes are still there. Appending the value again repairs it, and the file can then be deleted. |
| O8: snapshots | the same | `a_cleaned_file_outlives_the_snapshots_that_can_reach_it`: a snapshot is taken, then one cell is moved to another page, one overwritten and one deleted, and the cleaner empties the file. The file stays while the snapshot is open and every value reads through it; a snapshot taken after the file became empty does not keep it. |
| B10 | `ValueLogError` | Arbitrary bytes and damaged files are rejected or decode to values that encode back to the same bytes; every error's message. |

The golden bytes were computed with an independent bit-at-a-time CRC-32C,
not with the crate under test.
