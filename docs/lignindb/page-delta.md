# LigninDB on-disk format: page delta records

This is the specification for task FMT-03 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines how a change to a tree page is written down without writing the
page:

- the bytes of a record and of a run of records (section 2)
- the rules they follow (section 3)
- how a run is read (section 4)
- how a run and a chain are applied to a page (section 5)
- how a chain is consolidated into a base image (section 6)
- the sizes (section 7)
- two questions the plan left to this task: whether the log can name a
  carried cell instead of repeating it, and what the record looks like that
  gives a value its place in the value log (section 8)

It builds on [`format.md`](format.md) (FMT-01: byte conventions B1 to B11,
records R1 to R7), on [`tree-page.md`](tree-page.md) (FMT-02: the page, the
cell, the link, and the changes of its section 5), on
[`segment-manifest.md`](segment-manifest.md) (FMT-06: the block a run is
stored in), on [`page-table.md`](page-table.md) (STORE-03: chains) and on
[`transactions.md`](transactions.md) (TXN-01: page versions, write-back,
recovery). Rule names such as B8, TP9, S7, T3 and FL7 refer to those
documents.

It does not say when a run is written, how many commits it holds or when a
chain is consolidated (STORE-06), how a log group frames the runs of a
commit (FMT-05), or which changes a write makes (TREE-03, TREE-04).

The code is `lignin-format::delta`. Section 9 says what later tasks take
from here, section 10 lists the choices made here that the plan left open,
and section 11 maps each rule to its code and its tests.

## 1. Terms

- **Change**: one of the changes `TreePageMut` makes to a tree page
  (section 5 of `tree-page.md`): a cell arrives, is replaced or leaves, or
  one part of a cell is overwritten.
- **Record**: the bytes of one change. "Record" in `format.md` means the
  unit that carries a checksum. A record here is smaller than that: it lies
  inside such a unit (PD4).
- **Run**: the records of one page, in the order the changes were made,
  behind the page's id.
- **Chain**: a base image and the runs after it, oldest first, as the page
  table has them (T1 of `page-table.md`).
- **Container**: what a run lies in: a delta block of a segment (kinds 3
  and 4 of `segment-manifest.md`) or a log group (FMT-05).

## 2. Layout

All integers are little-endian and unaligned (B1, B2).

### 2.1 Run

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | page id: the page the records change (`PageId`, never 0) |
| 4 | | one or more records, one after another |

A run does not say how long it is. Its container does.

### 2.2 Record

A record is one byte, its kind, then the slot of the cell it changes
(`SlotId`, 2 bytes, never 0), then the fields of its kind. The create
record has no slot.

| Code | Record | After the kind | Bytes |
| --- | --- | --- | --- |
| 1 | create the page | `departures` (8): the count of departed cells the page starts at | 9 |
| 2 | insert a cell | slot, `len` (2), the cell | 5 + cell |
| 3 | replace a cell | slot, `len` (2), the cell | 5 + cell |
| 4 | remove a cell | slot | 3 |
| 5 | value hash | slot, the hash (32) | 35 |
| 6 | key-value hash | slot, the hash (32) | 35 |
| 7 | left child's hash | slot, the hash (32) | 35 |
| 8 | right child's hash | slot, the hash (32) | 35 |
| 9 | left child's address | slot, the cell address (6) | 9 |
| 10 | right child's address | slot, the cell address (6) | 9 |
| 11 | left link | slot, the link (2.3) | 45 + aggregate |
| 12 | right link | slot, the link (2.3) | 45 + aggregate |
| 13 | no left link | slot | 3 |
| 14 | no right link | slot | 3 |
| 15 | fragment root | slot | 3 |
| 16 | not a fragment root | slot | 3 |
| 17 | one hosted root | slot, position (1), a cell address or 0 (6) | 10 |
| 18 | all hosted roots | slot, count (1), a cell address or 0 for each (6) | 4 + 6 for each |
| 19 | feature | slot, `len` (1), the feature | 4 + feature |
| 20 | value | slot, `len` (2), the value | 5 + value |
| 21 | value pointer | slot, the value pointer (16): file id (4), offset (8), value length (4) | 19 |

The cell of records 2 and 3 is the cell's own bytes, as section 2.3 of
`tree-page.md` lays them out. The value pointer of record 21 is the 16
bytes of section 4 of `value-log-format.md`.

### 2.3 Link in a record

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | page id of the child's cell (never 0) |
| 4 | 2 | slot of the child's cell (never 0) |
| 6 | 32 | the child's hash |
| 38 | 1 | height of the child's left subtree |
| 39 | 1 | height of the child's right subtree |
| 40 | 1 | length of the child's key |
| 41 | 1 | `aggregate_len`: length of the aggregate |
| 42 | `aggregate_len` | the child's aggregate data |

These are the fields of a link in a cell, in the same order.

### 2.4 Examples

**One changed hash.** The key-value hash of the cell in slot 3 of page 7
becomes `55` (32 times). The run is 39 bytes:

```text
07 00 00 00               page 7
06                        kind: key-value hash
03 00                     slot 3
55 (32 times)             the hash
```

Stored raw in a segment it is the 47 bytes

```text
27 00 00 18               block tag: kind 3, a raw delta run; 39 bytes
9c 0b 7a c7               the block's checksum
07 00 00 00  06  03 00    the run
55 (32 times)
```

**A run with several records.** Page 7 is the page of the example in
`tree-page.md`. The node in slot 1 gets a right child, in slot 5 of page 9.
A new leaf with the key `b` and the value `w` is put into slot 2. The cell
of slot 3 leaves the page. 132 bytes:

```text
07 00 00 00               page 7

0c                        kind: right link
01 00                     slot 1
09 00 00 00  05 00        the child: page 9, slot 5
99 (32 times)             its hash
02 01                     its heights: 2, 1
04                        its key is 4 bytes long
01  2a                    aggregate: 1 byte, 2a

02                        kind: insert a cell
02 00                     slot 2
4a 00                     the cell is 74 bytes
00 01 00 00               the cell. no flags. key 1, no feature, no hosted root
01 00 00 00               value length 1
cc (32 times)             value hash
dd (32 times)             key-value hash
62                        key "b"
77                        value "w"

04                        kind: remove a cell
03 00                     slot 3
```

**A run that creates its page.** Page 9 starts with 5 departures and gets a
leaf with the key `k` and the value `v` in slot 1. The first 18 bytes of
its 92:

```text
09 00 00 00               page 9
01                        kind: create the page
05 00 00 00 00 00 00 00   5 departures
02  01 00  4a 00          insert a cell into slot 1, 74 bytes
...                       the cell
```

**The record that gives a value its place.** The value of the cell in
slot 1, 300 bytes long, is at offset 64 of `val/00000013.val`. 19 bytes:

```text
15                        kind: value pointer
01 00                     slot 1
0d 00 00 00               file 13
40 00 00 00 00 00 00 00   offset 64
2c 01 00 00               value length 300
```

## 3. Rules

### 3.1 Records

**PD1. One change, one record, and every change has one.** Each change
`TreePageMut` can make to the cells of a page has a record, and a record is
exactly one such change:

| Change | Call | Record |
| --- | --- | --- |
| a new page | `init` | create |
| a new cell | `insert`, `insert_encoded` | insert |
| the whole cell, for the same key | `replace`, `replace_encoded` | replace |
| a cell leaves | `remove` | remove |
| a hash | `set_hash` | the four hash records |
| a child's address | `set_child` | the two address records |
| a link | `set_link` | the two link records, and the two that take a link away |
| the fragment-root mark | `set_fragment_root` | the two mark records |
| one hosted-root pointer | `set_hosted_root` | one hosted root |
| all hosted-root pointers | `set_hosted_roots` | all hosted roots |
| the feature | `set_feature` | feature |
| the value | `set_value` | value, value pointer |

`compact` changes no content and has no record. The plan's list for this
task is the insert, replace ("update") and remove of a cell, the link, the
hash and the value. The others are here because the tree layer makes those
changes too, and without a record of their own each would have to be
written as a replaced cell, value included: the mark changes on a rotation
(R1 of `tree-layout.md`), a hosted-root pointer whenever the root of a
hosted Merk is carried or another node becomes the root (S3), and the
feature whenever a node of a tree with counts or sums gets another count or
sum.

**PD2. A record names a cell by its slot, and a part of the cell by the
record's kind.** It never holds an offset into the page: offsets are not
stable (TP5, TP17), slot numbers are (S1). Which hash, which side and which
of the two marks is part of the kind code, so those records have no field
for it.

**PD3. A record does not name its page. The run does.** Every record of a
run changes the page whose id stands at the run's start. So a page id costs
4 bytes for each page a commit changes, not 4 for each change.

With the id in the run:

- a reader that asked for the chain of page 7 and was handed a run of
  page 9, by a damaged page table or a bug, can tell, as it can for a base
  image (TP2). `apply_chain` and `consolidate` take the page the chain is
  of and compare every run with it (`DeltaError::WrongPage`, A2)
- a store can list the runs of a segment that has no footer by walking its
  blocks (S14 and item 3 of section 5 of `segment-manifest.md`)

**PD4. A run has no checksum of its own.** `format.md` asks that every byte
lies in a record with a checksum (R1, R4). For a run that record is its
container: the segment block, whose checksum covers the stored bytes and
is verified before they are used (S8), or the log group. So
`DeltaRun::decode` checks structure, not integrity, and must be given bytes
from a container that was verified, as `TreePage::decode` must (TP3). A
flipped bit in a hash would read as another hash.

**PD5. A run has no commit version of its own.** The version a run brings
its page to is in the link of the page table that points to the run's block
(T2, T3), and for a run in the log it is the version of the group. This is
how PV5 of `transactions.md` holds for a run, as it holds for a base image,
which has no version field either. A run may hold the changes of several
commits (FL3), and nothing inside it says where one commit ends.

**PD6. One encoding** (B8). A change has exactly one record, and a decoder
rejects every other byte string: an unknown kind, a slot or an address that
is 0 where one must stand, a length that is not the length of what follows,
and bytes behind the last record. A list of changes has exactly one run.

**PD7. A cell travels as its own bytes** (TP8), behind its length. The
bytes are checked as `TreeCell::decode` checks them, by the reader and by
the writer. The length is there so that a run can be walked without reading
the cells, and it must be the cell's own length. A cell in a record is at
most 32,754 bytes (`MAX_CELL_LEN`): the longest cell the largest page
takes (TP14). A longer one could go into no page, and has no record.

**PD8. A changed hash is 35 bytes, and its record holds nothing of the cell
but the hash.** No key, no value, no length of either. So a write that
changes only hashes logs no value byte and writes none back, which is the
design rule "a hash update never rewrites a value" for the log and for the
segments.

**PD9. A link is updated as a whole, or by its address alone.**

- A link record holds the link as its cell does: 43, 51, 59 or 67 bytes in
  a tree of today's Merk (TP12), so the record is 46 to 70 bytes. Every
  link of one Merk has the same length, so within one tree a link record
  has a fixed size, as C4 of `tree-layout.md` asks, and it replaces a link
  where it is (TP10).
- When only the child's hash changed, the hash record is shorter (35).
- When only the child's cell moved, the address record is shorter (9).
  This is the record a carry leaves in the page of the carried cell's
  parent (S3).
- A link that appears, or goes (records 13 and 14), changes the length of
  its cell. That is the page's business (W2 of `tree-page.md`).

**PD10. A value is replaced by a value, or by a pointer.** Record 20 holds
the value's bytes and puts them into the cell. Record 21 holds a value
pointer and makes the cell hold it. Neither touches a hash or a link. A
value in record 20 is at most 32,682 bytes (`MAX_INLINE_VALUE_LEN`): the
longest value the longest cell holds (PD7). A longer one could go into no
cell, and has no record. Whether a value fits the cell and the page it is
meant for is for the page to say when the record is applied.

**PD11. A page is created by the first record of a run, and only there.**
The create record starts an empty page under the run's page id, with the
count of departed cells it gives, as `TreePageMut::init` does. The records
behind it fill the page. A run that creates its page needs nothing to apply
to, and if it is given a page it replaces it: a page id is used again after
its page was freed (STORE-10).

A chain in the page table never starts with a run (T4, P11): the first
block of a new page is a base image. So the create record is for the log,
where a commit that makes a page has to say so (FL4: "a change that makes a
page needs no image").

**PD12. A run holds at least one record.** A page that did not change has
no run.

**PD13. Records are for tree pages.** A flat page image is immutable
(P13 of `flat-page.md`) and has no deltas.

### 3.2 Chains

**PD14. A chain is applied in order, and each run once.** Records are not
idempotent: an insert into a slot that holds a cell and a remove of a free
slot are refused, and a remove that was applied twice with an insert
between would take the wrong cell. What keeps a run from being applied
twice is the version of the page's newest link (K3 of `page-table.md`), not
anything in the run.

**PD15. The same changes give the same image.** A page's content after a
run depends only on its content before and on the records, not on where
the cells lay in the writer's buffer, on how the changes were cut into
runs, or on whether the chain was consolidated on the way. A page takes or
refuses a change by its free bytes alone, which are a function of its
content (W4), and an image has one form (TP16). So the image that a base
image and a chain give is, byte for byte, the image of the frame the
changes were first made in. This is the acceptance test of the task.

## 4. Reading a run

`DeltaRun::decode(bytes, version)` reads the run that is exactly `bytes`.
It takes the format version of the file the run is in, since a run carries
none (R5). It checks, in this order:

1. the 4 bytes of the page id are there: else `Truncated`
2. the page id is not 0: else `MissingId`
3. a record follows: else `Delta(EmptyRun)`
4. each record in turn (see below). A create record that is not the first
   record of the run is `Delta(MisplacedCreate)`, once it has been read as
   a record

Bytes behind the last record are read as the next record, so a run with
anything else at its end fails in step 4.

A record is checked in this order:

1. the kind is one of the table: else `UnknownRecordKind`
2. the slot is there, else `Truncated`, and is not 0, else `MissingId`
3. each field in the order it stands: its bytes are there, else
   `Truncated`; then its value:
   - a child's address is a whole address: else `MissingId`
   - a hosted-root pointer is a whole address or all zero: else `MissingId`
   - a value pointer is one `ValuePointer::decode` accepts: else
     `MissingId`, `ValueLog(InvalidOffset)` or `ValueLog(EmptyValue)`
   - a cell's `len` is at most 32,754: else `TreePage(CellTooLarge)`,
     before its bytes are asked for
   - a value's `len` is at most 32,682: else `TreePage(FieldTooLong)`,
     before its bytes are asked for
   - the `len` bytes are exactly a cell: else the error of
     `TreeCell::decode`. A cell that is longer or shorter than `len` is
     `TreePage(CellLengthMismatch)`

`Truncated` counts both of its numbers from the first byte of the run.

Decoding makes one pass and allocates nothing. `DeltaRun::deltas` then
gives the changes one at a time, borrowed from the run's bytes
(`Delta<'_>`), and `DeltaRun::created` says whether the run creates its
page.

## 5. Applying

`DeltaRun::apply(frame, size, version)` applies a run to the page at the
start of `frame`, a buffer of the store's page size.
`apply_chain(page, frame, size, version, chain)` does it for the runs of
the chain of `page`, oldest first. `Delta::apply` makes one change to a
`TreePageMut`.

**A1. Applying a record is making its call** (PD1), with the cell, link or
bytes of the record. Nothing else is written. So what `tree-page.md` says
of each change holds for its record: a hash, an address, a mark, one
hosted-root pointer, and a link, feature or value of the same size are
written where they are, with no allocation; a cell that grows moves inside
the page; a slot keeps its number; only a remove counts a departure.

**A2. A run is applied to the page it names.** A run that does not create
its page is refused for a frame that holds another page
(`Delta(WrongPage)`), before any change. A run that creates its page has
no page to be compared with: whoever applies it alone chose the frame by
the page the run names. In a chain the page is known, so `apply_chain` and
`consolidate` are given it and refuse a run of another page, also one that
creates its page, before that run changes anything. `consolidate` refuses
a base image of another page in the same way.

**A3. A record that is refused changes nothing, and ends the run.** The
errors are those of the call: `SlotFree`, `SlotOccupied`, `PageFull`,
`CellTooLarge`, `KeyChanged`, `NoSuchLink`, `NoSuchHostedRoot`. The records
before it stay applied, and none after it is. The frame then holds a valid
page that is no version of the page, and its caller drops it: on a chain
that was written by a correct writer, for the page it is applied to, no
record is ever refused, so a refusal means a damaged table, a run applied
out of its place, or a bug.

**A4. The frame holds a valid page** (W5 of `tree-page.md`): an image that
`TreePage::decode` accepted and that was copied to the start of the frame,
or a page that runs were applied to before. On such a page no record that
was read makes `apply` panic or leave the page invalid. A run that creates
its page needs only the buffer.

**A5. The page size is the store's.** A chain written under one page size
may not fit under a smaller one.

`version` is the format version of the page in the frame. A page that a
run creates is in the current version. With one format version there is
nothing to convert. The task that adds a version says how a page and a run
of different versions meet (U9 of `format.md`).

## 6. Consolidating

`consolidate(page, base, chain, version, size, out)` appends to `out` the
base image that stands for the chain of `page`: it reads `base` as an
image (`TreePage::decode`), applies the runs in a buffer of its own, and
writes the page with `TreePage::write_image`.

- `base` is `None` for a chain whose first run creates its page. A chain
  with no base whose first run does not is `Delta(MissingBase)`.
- A base image that is given is read and checked, and must be of `page`,
  also when a run of the chain creates the page again.
- On any error nothing is written to `out`.
- The result is an image (TP16), so it reads back with `TreePage::decode`,
  and by PD15 it is the image a writer's frame would give.

A store that has the page in a frame already does not need this function:
`apply_chain` on the frame and `write_image` of it are the same thing
(STORE-06).

## 7. Sizes

| | Bytes |
| --- | --- |
| run header: the page id | 4 |
| record header: kind and slot | 3 |
| a changed hash | 35 |
| a child's new address | 9 |
| a link, in a tree with no count or sum | 46 |
| a link, in the other trees of today's Merk | 54, 62 or 70 |
| a mark, a link that goes, a cell that leaves | 3 |
| one hosted-root pointer | 10 |
| a new or replaced cell | 5 + the cell |
| a value in the cell | 5 + the value |
| a value's place in the value log | 19 |
| creating a page | 9 |

**A hash-only delta.** The acceptance test asks for at most 48 bytes. The
record is 35. With the page id it is 39, and with the 8 bytes of a segment
block (length, kind and checksum) it is 47. Each further hash of the same
page is 35 bytes more. In the log, the 4 bytes of the page id and whatever
FMT-05 puts around a run are paid once for each page a commit changes.

**The cells of the reference workload** (section 6 of `tree-page.md`): a
document node with two children is 203 bytes without its slot, so its
insert record is 208, and a document leaf 122.

## 8. Two questions left to this task

### 8.1 Can the log name a carried cell instead of repeating it?

A carried cell is a remove in one page and an insert in another (S2), and
the insert repeats the cell's bytes, which the old page's chain already
holds. On the reference workload 1.63 cells are carried per transition
(section 6 of `tree-page.md`), 304 bytes of cells by the owner's figure for
this task. As inserts they cost those bytes and 5 more for each, about 312
bytes per transition. A record that named the cell where it was, by its
old address, would be 9 bytes, 15 per transition. That is about 297 bytes
per transition less in the log, 12% of the plan's target of 2.4 KB. On a
tree that does not grow the saving is nearly nothing: 0.024 cells are
carried per transition there (14.4 of `tree-layout.md`).

**A segment cannot name it.** A page version is built from its own chain
and from nothing else: that is what makes a block live or dead (CL1), lets
a chain be consolidated and trimmed by itself (T9, T11), and lets a page
be loaded with one walk of one chain (T5). A run that said "the cell that
page 7 had in slot 3" would need an older version of page 7 for as long as
the run is in a chain.

**The log cannot name it under the rules as they stand**, for the same
reason seen from recovery:

- A table checkpoint is a copy of every chain, not a cut at one version
  (K2). After a crash each page is at its own version, and replay applies
  to a page only the changes above its newest link (K3). When the old page
  was written back after the carry and the new page was not, replay has to
  apply the carry to the new page, and the old page it would copy from no
  longer holds the cell. Its earlier version may be consolidated away
  (T9) and trimmed (T11).
- A merge carries the cells out of a page and frees it in the same commit
  (M2, M3 of `tree-layout.md`). A freed page's chain is trimmed as soon as
  no reader needs it, and replay then finds changes to a page it has no
  image of, and drops them (K11, FL4). The carry out of that page would be
  dropped with them.

So a named carry would make the pages of one commit depend on each other
in recovery. It would need: the old page's version from before the carry
kept readable until the new page's block is written, across trimming,
consolidation and cleaning; an order between the write-backs of the two
pages; and a replay that reads one page to rebuild another. That is new
machinery in STORE-03, STORE-06 and STORE-07, where a mistake loses a
cell, for a tenth of the log on a growing tree.

**So this format has no such record**, and a carried cell is an insert.
The record could be added while format version 1 is open (U10), with the
rules above. PERF-04 measures the log on real traces and is the place to
decide whether it is worth them.

FMT-05 has the same question for the carried cell's value (FL7, O5 of
`value-log-format.md`). It is easier there: a value is named by a pointer
into a file that stays until its count is zero and no snapshot can reach
it, not by a page version.

### 8.2 The record that gives a value its place

A value that is too long for its cell reaches the value log at write-back,
not at commit (FL7, D-042). The flusher appends the pending values of a
page, has a manifest cover them, and gives each cell its pointer in a page
step (PV7).

**That change is record 21, the value pointer**: the slot and the 16-byte
pointer, 19 bytes. Applying it is `set_value` with the pointer. The cell's
hashes, links, key and feature keep their bytes. When the cell already
holds the 12 bytes of a value's place, the record overwrites them and the
value length, and nothing else in the page moves. A value the cleaner moves
to another file is the same record with the new pointer (O9 of
`value-log-format.md`).

It is used in two places:

- **In a segment**, as a record of the page's run, in its place among the
  page's other changes.
- **In the log**, as the change to the page that the record of the page
  step carries (PV6, DU5). FMT-05 puts around it what recovery needs to
  recognise the change against the page: the pointer the cell held before,
  and the change to the counts. The record itself is not conditional. A
  reader that must apply it only if the cell still holds another pointer
  looks at the cell first (`TreeCell::value_pointer`).

**What the cell holds before that is not settled, and not by this
format.** A record carries a cell as `tree-page.md` defines it, and that
has two forms for a value: the bytes in the cell, or the place of the
bytes in the value log. A pending value is neither. Three ways are open,
and the records here serve each of them unchanged:

1. *A place that names no file yet.* The cell has its final shape, with 12
   bytes for the value's place, and those bytes hold a pattern that
   `value-log-format.md` reserves for "not in the value log yet", for
   example the last file id with a number of the holder's choice as the
   offset. The value's bytes are in the log group and beside the frame.
   Placing the value overwrites 12 bytes. This needs one rule in
   `value-log-format.md` and no change to a page or to a record, and it is
   the one this document recommends.
2. *The value in the cell until it is placed.* Needs no new rule at all:
   the insert record carries the value inside the cell, and record 21
   later shrinks the cell. It works only for a value that fits a cell,
   16,370 bytes in a page of 32 KiB, and until write-back a page holds its
   new values at full length, so the layout would be planned for cells
   that are longer than they stay.
3. *A third form of the cell*, marked by one of its reserved flags. That
   is a change to `tree-page.md`.

FMT-05 and TREE-08 need the answer: the first to tie a value in a log
group to its cell, the second to read a pending value.

## 9. What the tasks that build on this take from it

| Task | Takes from here | Still its own to define |
| --- | --- | --- |
| FMT-05 Log record format | A run is the unit for "the structure deltas and hash records of one page": the page id once, then the records in order. `DeltaRunWriter` writes it into a group's buffer, `DeltaRun::decode` reads it. A run has no length, checksum or version: the group gives all three. The create record says that a commit makes a page (PD11). Record 21 is the page change of a placed or moved value (8.2). | How runs are framed in a group. The value records, and how one is tied to its cell (8.2). The record of a background change. Whether a carried value is repeated or named. |
| STORE-05 Page materialisation | The base image is copied to a frame, then `apply_chain` with the page's id and the runs of the chain. Each run is checked against that id (`WrongPage`). Any error rejects the page (A3). Decoding a run allocates nothing, and neither does applying a hash, an address, a mark, one hosted-root pointer, or a link, feature, value or value pointer that takes the room of the one before it. | The load path, and the checksums of the blocks. |
| STORE-06 Delta write-back | A run for the changes of one page since its last link, of one commit or of several folded together: pushing the changes of several runs into one writer is the fold. `consolidate`, or `apply_chain` and `write_image` on a frame, for a new base image. A hash-only change is 35 bytes for each hash and no value byte. Record 21 after the value is covered (8.2). | Thresholds. Where the changes of a page are kept until they are written. |
| STORE-07 Recovery | A run applies once and in order (PD14): K3 decides whether a page already has it. A run that creates its page needs no image. | Replay. |
| STORE-09 Value log | Moving a value is record 21 with the new pointer, 19 bytes in the page's run. | The page step. |
| TREE-03 Fragment maintenance | A record for each thing it does to a page: insert, remove, the address in the parent's link (9 bytes), the mark (3), a hosted-root pointer (10). A carried cell is a remove and an insert with the cell's bytes (8.1). | Which changes a write makes. |
| TREE-04 Lazy hashing | One 35-byte record for each hash that changed, whatever else the cell holds. A link whose hash, heights and aggregate changed together is one link record. | When hashes are evaluated. |
| TREE-08 Out-of-line values | Record 20 puts a value into its cell, record 21 its place. The pending form (8.2). | The threshold, and the read of a pending value. |
| TREE-09 Subtree drop | Removing a left-over cell is the remove record, 3 bytes. | The walk. |
| FMT-08 Decoder fuzzing | The decoder: `DeltaRun::decode`, with `deltas` and `created` on what it accepts. `DeltaRun::apply`, `apply_chain` and `consolidate` on valid pages only (A4). The golden bytes of section 2.4. | The targets. |
| PERF-04 Write-back tuning | The sizes of section 7, and the price of a carried cell in the log (8.1). | Tuning. |

## 10. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one while version 1 is still open (U10).

1. **Every change of `TreePageMut` has a record** (PD1), which is more than
   the plan's list: the fragment-root mark, the hosted-root pointers, the
   feature, the address of a child alone, and the creation of a page.
2. **"A hash-only delta is at most 48 bytes" is read as the whole block a
   segment stores**: 47 bytes for one hash. `format.md` (3.1, 5.1 and
   section 8) expected a delta with a compact header and a checksum of its
   own. FMT-06 has since put a run into a block that has both, so the run
   has neither.
3. **Which hash, which side and which mark is in the kind code** (PD2):
   one byte less in each of those records.
4. **The page id is in the run, once** (PD3). The owner's note for this
   task has the page id as the block's. A block does not hold it (S14), so
   it is the run's first field: 4 bytes a run, which pays for the check of
   A2 and for walking a segment without a footer.
5. **No checksum in a run** (PD4), as for a tree page image.
6. **No commit version in a run** (PD5). PV5 of `transactions.md` names
   this task among those that lay out the born version. The page table's
   link has it, and so does the log group. A version field would make a
   hash-only block 55 bytes.
7. **A page is created by a record** (PD11), and a run that creates its
   page replaces whatever its frame held.
8. **A cell in a record has a 2-byte length**, and is at most the longest
   cell of the largest page (PD7). A value that record 20 puts into a cell
   has a 2-byte length too, and is at most the longest value such a cell
   holds (PD10), although a cell's own field for the length of its value
   is 4 bytes: that field also serves a value out of line.
9. **A link record holds the link at its own length**, 42 bytes and the
   aggregate, not one size for all trees.
10. **One record for each changed hash.** A node whose value changed has
    two (the value hash and the key-value hash), 70 bytes. A record for
    the pair would be 67. Not taken: it is a second way to write two
    changes.
11. **A value pointer record holds the whole 16-byte pointer**, value
    length included, although the cell has that length already when a
    value is placed or moved. One record then also serves a value that is
    replaced by a longer or shorter one out of line. A record without the
    length would be 4 bytes shorter and could only move a value.
12. **No record names a carried cell** (8.1).
13. **No record for a pending value, and no pending form** (8.2). This is
    an open point for the owner: `tree-page.md` has no form for a value
    that is neither in its cell nor in the value log, and FMT-05 and
    TREE-08 need one.
14. **An empty run is not a run** (PD12).
15. **Applying a run is not all or nothing** (A3). Making it so would cost
    a copy of the page for each run. The caller drops the frame.
16. **`DeltaRun::decode` checks every record before any is applied**, and
    `deltas` reads them a second time. The second read does no I/O and no
    allocation.
17. **A change is the same type for the writer and the reader**
    (`Delta<'_>`), which borrows a cell, a link, a hash and a value from
    whoever has them. The hosted-root pointers are a list that is either a
    slice of addresses or the bytes of a record (`RootList`).
18. **The errors are one variant of `FormatError`**, `Delta(DeltaError)`,
    for what is the run's own: `EmptyRun`, `MisplacedCreate`, `WrongPage`,
    `MissingBase`. A part that is too long, a cell that is too large and
    everything a page refuses keep the tree page's errors.
19. **`apply_chain` and `consolidate` are told which page the chain is of**
    (A2). A store knows it, and a run that creates its page could
    otherwise put another page into the frame.
20. **Two things changed in `tree_page`.** Its test model and test helpers
    are now visible to the other tests of the crate, which the tests here
    use. And a `TreeCell` can be compared with another: two cells are equal
    when their bytes are. Nothing else changed there.

## 11. Code and tests

All of this is in `lignin-format::delta`. Run the tests with
`cargo test -p lignin-format delta`.

| Rule | Code | Tests |
| --- | --- | --- |
| Layout (section 2) | `Delta`, `DeltaRun`, `DeltaRunWriter` | `golden_bytes`: a run with a record of every kind that changes a cell, written out by hand, is what the writer writes and what the reader reads, and applied to the example page of `tree-page.md` it gives the page a model expects. `a_run_that_creates_its_page_starts_with_the_create_record`, `the_documented_run_is_132_bytes`: the examples of 2.4. `sizes_are_the_documented_ones`. |
| PD15, sections 5 and 6 | `DeltaRun::apply`, `apply_chain`, `consolidate` | **Acceptance test**, `a_base_image_plus_any_delta_chain_is_the_consolidated_image`: 42,000 random changes on five pages of four sizes, made by a writer through `TreePageMut` and written as records. At random points the base image and the runs so far are consolidated, applied in a frame, consolidated in two steps and folded into one run, and each must give, byte for byte, the image of the writer's frame and the image a model lays out by hand. Each record is also read back and applied to a second frame, which must stay equal to the writer's frame in every byte; a change the writer's page refuses must be refused as a record with the same error. The pages fill up, are consolidated and are created again on the way. `the_same_changes_give_the_same_image_however_they_are_cut`. |
| PD8, the 48 bytes | `HASH_RECORD_LEN`, `Delta::Hash` | **Acceptance test**, `a_hash_only_delta_is_at_most_48_bytes`: the record of every hash of every cell of full pages of three sizes is 35 bytes, its run 39, its block 47 raw and never more than 48 compressed; the documented block byte for byte, with a checksum computed independently; applying it changes the 32 bytes of that hash and no other byte of the page. |
| PD1, A1 | `Delta::apply` | In the acceptance test the second frame takes records where the writer makes calls. `hosted_roots_are_the_same_list_given_or_read`. |
| PD6, B8 | `Delta::encode_into`, `DeltaRun::decode` | `what_was_written_reads_back_and_what_was_read_is_written_the_same`: 3,000 runs. `every_single_bit_flip_is_rejected_or_reads_as_what_it_says`. `a_change_that_would_not_read_back_is_not_written`. `changes_are_compared_by_what_they_change`. |
| Section 4 | `DeltaRun::decode` | Each check has its error, in order: `each_check_of_a_run_has_its_error`, `every_kind_code_is_a_record_or_an_error`, `each_check_of_a_record_has_its_error`, `each_check_of_a_cell_in_a_record_has_its_error`. `every_prefix_of_a_run_is_a_shorter_run_or_an_error`. |
| B10, A4 | every reader, `DeltaRun::apply` | `arbitrary_bytes_are_an_error_or_a_run_and_never_a_panic`: 60,000 byte strings, a third arbitrary, a third records of every kind with arbitrary fields, a third valid runs with bytes changed. What decodes is written back to the same bytes and applied to a full page, which must be valid afterwards. |
| A2, A3, PD11, section 6 | `DeltaRun::apply`, `consolidate`, `DeltaError` | `a_run_is_applied_to_the_page_it_names`, `a_run_that_creates_its_page_needs_no_page`, `a_refused_change_keeps_the_changes_before_it_and_makes_none_after_it`, `a_chain_with_no_base_must_create_its_page`, `consolidation_takes_an_image_and_nothing_else`, `a_full_page_refuses_a_record_that_needs_room`, `a_change_too_long_for_a_record_is_refused_by_the_page_too`, `every_error_has_its_message`. |
| No allocation | `DeltaRun::decode`, `DeltaRun::apply` | `tests/delta_alloc.rs` runs under a counting allocator. |

The checksum of the block in 2.4 was computed with an independent
bit-at-a-time CRC-32C, not with the crate under test.
