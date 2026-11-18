# LigninDB on-disk format: tree pages

This is the specification for task FMT-02 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines the page that holds the nodes of Merk trees:

- the bytes of a page, a cell and a link (section 2)
- the rules they follow (section 3)
- how a page is read (section 4)
- how a page is changed where it is (section 5)
- the sizes (section 6)

It builds on [`format.md`](format.md) (FMT-01: byte conventions B1 to B11,
identifiers, records), on [`tree-layout.md`](tree-layout.md) (TREE-01: what a
cell and a link hold, C1 to C7, and what a slot number is, S1 to S6), on
[`value-log-format.md`](value-log-format.md) (FMT-07: the value pointer) and
on section 9 of [`cost-model.md`](cost-model.md) (BASE-06: what an engine has
to keep for costs). Rule names such as B8, C7 and S1 refer to those documents.

It does not say which page a node goes into (TREE-01, TREE-03), what a delta
record carries (FMT-03), or how an image is stored in a segment (FMT-06). The
page does not interpret Merk's feature types and aggregates: it stores them
as bytes with a length. TP12 says what those bytes are, and MERK-01 writes
them.

The code is `lignin-format::tree_page`, with `SlotId` and `CellAddress` in
`lignin-format::ids`. Section 7 says what later tasks take from here,
section 8 lists the choices made here that the plan left open, and section 9
maps each rule to its code and its tests.

## 1. Terms

- **Node**, **cell**, **fragment**, **carry**: as in `tree-layout.md`. A cell
  is the bytes of one Merk node in a page.
- **Slot**: an entry of a page's slot directory. A slot holds one cell or is
  free. Its number is a `SlotId`.
- **Cell address**: a page id and a slot number, six bytes (`CellAddress`).
- **Image**: the bytes of a page as a segment stores them (FMT-06). An image
  has no unused byte and exactly one form for its content.
- **Page in a buffer**: the same layout in a buffer of the store's page size,
  where the page is changed. It may have unused bytes among its cells.
- **Page size**: the size of a store's tree pages, 32,768 bytes unless the
  store is told otherwise (`PageSize`).
- **Capacity**: the bytes of a page that cells and their slots can use: the
  page size, at most 65,535, less the 22 bytes of the page header.

## 2. Layout

All integers are little-endian and unaligned (B1, B2). Offsets count from the
first byte of the page.

### 2.1 Page header

22 bytes.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | page kind: 3 |
| 1 | 1 | reserved, 0 |
| 2 | 2 | `len`: length of the page in bytes, `u16` |
| 4 | 4 | page id: the page's own id (`PageId`, never 0) |
| 8 | 2 | `slot_count`: number of slots in the directory, `u16` |
| 10 | 2 | `cell_count`: number of slots that hold a cell, `u16` |
| 12 | 2 | `unused`: bytes among the cells that belong to no cell, `u16`. 0 in an image |
| 14 | 8 | `departures`: how many cells have left the page, `u64` |

### 2.2 Slot directory

After the header, `slot_count` entries of 2 bytes. Entry *i*, counted from 0,
belongs to slot number *i* + 1. It holds the offset in the page at which the
slot's cell starts, or 0 if the slot is free.

### 2.3 Cell

The cells follow the directory. A cell says how long it is by its own fields.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | flags. Bit 0: there is a left link. Bit 1: there is a right link. Bit 2: the value is out of line. Bit 3: the cell is a fragment root. Bits 4 to 7 are reserved |
| 1 | 1 | `key_len`: length of the key |
| 2 | 1 | `feature_len`: length of the feature |
| 3 | 1 | `hosted_count`: number of hosted-root pointers |
| 4 | 4 | `value_len`: length of the node's value, `u32`, wherever the value is |
| 8 | 32 | value hash |
| 40 | 32 | key-value hash |
| 72 | | the left link, if bit 0 is set (2.4) |
| | | the right link, if bit 1 is set |
| | 6 each | the hosted-root pointers: a cell address, or 0 for none |
| | `feature_len` | the feature: the node's feature type with its aggregate data |
| | `key_len` | the key |
| | | the value: `value_len` bytes, or for a value out of line 12 bytes, the file id (4) and the offset (8) of its value pointer |

### 2.4 Link

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

### 2.5 Example

Page 7, from which two cells have left, with three slots. Slot 1 holds a node
with the key `ab`, a two-byte feature, a left child in slot 3 of the same
page, two hosted-root pointers of which the second is empty, and a 300-byte
value at offset 20 of `val/00000012.val`. It is a fragment root. Slot 2 is
free. Slot 3 holds a leaf with the key `abc` and the value `v`. 247 bytes:

```text
03 00                     kind: tree page; reserved
f7 00                     len 247
07 00 00 00               page 7
03 00                     3 slots
02 00                     2 cells
00 00                     no unused bytes
02 00 00 00 00 00 00 00   2 departures
1c 00                     slot 1: the cell at 28
00 00                     slot 2: free
ab 00                     slot 3: the cell at 171

0d 02 02 02               slot 1. flags: left link, out of line, fragment
                          root. key 2, feature 2, 2 hosted roots
2c 01 00 00               value length 300
11 (32 times)             value hash
22 (32 times)             key-value hash
07 00 00 00  03 00        left child: page 7, slot 3
33 (32 times)             its hash
01 00                     its heights: 1, 0
03                        its key is 3 bytes long
01  05                    aggregate: 1 byte, 05
09 00 00 00  01 00        hosted root 0: page 9, slot 1
00 00 00 00  00 00        hosted root 1: none
03 2a                     feature
61 62                     key "ab"
0c 00 00 00               value in file 12
14 00 00 00 00 00 00 00   at offset 20

00 03 00 00               slot 3. no flags. key 3, no feature, no hosted root
01 00 00 00               value length 1
44 (32 times)             value hash
55 (32 times)             key-value hash
61 62 63                  key "abc"
76                        value "v"
```

The empty page 1 is the 22 bytes
`03 00 16 00  01 00 00 00  00 00  00 00  00 00  00 00 00 00 00 00 00 00`.

## 3. Rules

### 3.1 The page

**TP1. The first byte is the page kind, and a tree page is kind 3.** Flat
pages took 1 (leaf) and 2 (branch) in `flat-page.md`. So any page is
identified by its first byte, the length of any page image is the `u16`
at offset 2, and its own page id is the `u32` at offset 4 (TP2; P14 of
`flat-page.md`). Kind 0 is invalid (B6), so zero-filled bytes are not a page.

**TP2. A page holds its own id.** A segment block does not name its page
(S14 of `segment-manifest.md`). With the id in the header:

- a store can list the tree pages of a segment that has no footer by walking
  its blocks, as it can for deltas
- a reader that asked for page 7 and was handed the image of page 9, by a
  damaged page table or a bug, can tell, at the cost of one comparison
- the cells of a page can tell which of their links stay in the page (TP11),
  with no second form of link for that case

**TP3. An image has no checksum of its own.** `format.md` asks that every
byte lies in a record with a checksum (R1, R4). For an image that record is
the segment block: its checksum covers the stored bytes, compressed or not,
and is verified before they are used (S8 of `segment-manifest.md`). A second
checksum inside the image would cost 4 bytes and one more pass over 32 KiB
on every load, and a page that is changed in a buffer could not keep it
true. So `TreePage::decode` checks structure, not integrity, and must be
given bytes from a record that was verified.

**TP4. The page size is a parameter, one of 8, 16, 32 and 64 KiB**
(`PageSize`, 3.5 of `format.md`). An image is as long as its content and at most the page
size, and at most 65,535 bytes, because offsets in a page are 16 bits: a
page of 64 KiB leaves its last byte unused, as a flat page does (P10 of
`flat-page.md`). The image does not record the page size. A reader passes
the size of its store's pages, which the manifest records (M17 of
`segment-manifest.md`). An image written under a smaller page size
reads under a larger one. The lower bound is what TP14 needs.

### 3.2 Slots

**TP5. A slot number is a name** (S1 to S4 of `tree-layout.md`). It is a
16-bit number from 1 (`SlotId`); 0 is "no slot", as for every identifier
(B6). The slot directory maps the number to where the cell is now, so a cell
can move inside its page, and the page can be written as a new image, with
no change to its number. Nothing in this format renumbers a cell.

**TP6. A slot is freed only when its cell leaves the page**, and then its
number can be given to another cell. `TreePage::lowest_free_slot` is the
number S4 says a new cell takes. The directory is as long as the highest
slot in use: its last entry is never free.

**TP7. The page counts the cells that leave it** (`departures`). The count
goes up by one each time a cell is removed from the page, because its node
was deleted or because it was carried to another page. Nothing else changes
it: not a change to a cell, not a cell that moves inside the page, not a new
image. And nothing else puts another node into a slot: a cell is replaced
only by a cell with the same key (W1). So a slot number that was read when
the count was *n* names the same node for as long as the count is *n*. This
is the check S6 of
`tree-layout.md` proposes for an address kept in memory. The count is 64
bits and does not wrap in practice. A page that is created starts at 0, or
at a count its creator gives (section 8, item 9).

### 3.3 Cells

**TP8. One node, one cell, and a cell is its own bytes.** A cell holds what
C1 of `tree-layout.md` lists and nothing that depends on where it is: no
offset into its page and no shortened address. So a cell that is carried to
another page is copied byte for byte (`TreePageMut::insert_encoded`), and a
delta can carry a cell as it is.

**TP9. Every hash Merk stores is stored, at a place the cell's header and
links fix.** The value hash is at offset 8 of the cell and the key-value
hash at 40. The hash of the left child is at 78. The hash of the right child
is at 78 if there is no left link, and otherwise 42 bytes plus the left
link's aggregate further on. None of these places depends on the key, the
feature or the value, which all come after the links. So a hash is read, and
overwritten where it is, without reading or moving a byte of the value
(`TreeCell::hash`, `TreePageMut::set_hash`). As today, a node's own hash is
not in its cell: it is in the link of its parent.

**TP10. A link has one form** (C4). It names the child's cell by page id and
slot, whether or not the child is in the same page. It then holds what
Merk's link holds, without the child's key: the hash, the two heights, the
aggregate, and **the length of the child's key**. The length is there
because the cost of fetching the parent depends on it (N1 of
`cost-model.md`: `record_len` counts each child's key) and the parent must
be charged without reading the child. The key itself is in the child's
cell.

A link is 42 bytes plus its aggregate. Changing the address, the hash, the
heights, the key length, or the aggregate to one of the same length,
overwrites the link where it is. A link that appears or goes changes the
length of the cell. So does a link whose aggregate changes its length: the
writer takes one and lays the cell out again. Among the nodes of one Merk
that does not occur, because an aggregate written by the rule of TP12 has
the length of its variant, and so a link that exists is overwritten where
it is.

**TP11. The fragment-root table is one bit in each cell.** Bit 3 of the
flags marks the cells that are the root of a fragment (F3 of
`tree-layout.md`): the cells no cell of the same page points at.
`TreePage::fragment_roots` lists them. The mark is set by whoever places the
cell, and setting it is a change of one byte.

The table can be derived from the page alone, because a page knows its id:
a cell is a fragment root exactly when no link and no hosted-root pointer of
the page names it. `TreePage::check_fragments` checks that, and that no
address into the page names a free slot, the cell it is in, or a cell
another address names (C5), and that a link to a cell of the page states
the length that cell's key has (TP10). It is not part of `decode`: between
two changes
of one write a page is read while a link is ahead of the cell it names, and
inside a transaction of iteration two a hosted-root pointer may be behind
(T3 of `tree-layout.md`). VER-07 runs it on committed states.

**TP12. The feature and the aggregates are Merk's tag and a payload, each
with its length.** The cell has a length byte for the feature and each link
has one for its aggregate, so each is 0 to 255 bytes. The page stores those
bytes and does not interpret them: it has no list of Merk's feature types,
which is what the crate layering asks of a format.

Their encoding is fixed here, for the one writer of those bytes (MERK-01):

- one byte, the tag Merk itself gives the variant: `TreeFeatureType` and
  `AggregateData` encode with the same tags
- then the variant's numbers in the order Merk declares them, each at its
  full width: a count is 8 bytes, a sum 8 bytes, a 128-bit sum 16 bytes,
  little-endian, a signed number in two's complement (B1)

There is no varint. So the length of a feature or an aggregate depends on
its variant alone and never on its numbers, and it is the size Merk's own
cost rules give the variant (the cost class of `cost-model.md`, plus the
tag). The variants of upstream `develop` at `f1016919`:

| Tag | Feature, aggregate | Numbers | Bytes with the tag |
| --- | --- | --- | --- |
| 0 | `BasicMerkNode`, none | | 1 |
| 1 | `SummedMerkNode`, `Sum` | sum | 9 |
| 2 | `BigSummedMerkNode`, `BigSum` | 128-bit sum | 17 |
| 3 | `CountedMerkNode`, `Count` | count | 9 |
| 4 | `CountedSummedMerkNode`, `CountAndSum` | count, sum | 17 |
| 5 | `ProvableCountedMerkNode`, `ProvableCount` | count | 9 |
| 6 | `ProvableCountedSummedMerkNode`, `ProvableCountAndSum` | count, sum | 17 |
| 7 | `ProvableSummedMerkNode`, `ProvableSum` | sum | 9 |
| 8 | `ProvableCountedAndProvableSummedMerkNode`, `ProvableCountAndProvableSum` | count, sum | 17 |
| 9 | `ProvableCountedAndProvableBigSummedMerkNode`, `ProvableCountAndProvableBigSum` | count, 128-bit sum | 25 |
| 10 | `CountedBigSummedMerkNode`, `CountAndBigSum` | count, 128-bit sum | 25 |
| 11 | `ProvableBigSummedMerkNode`, `ProvableBigSum` | 128-bit sum | 17 |

The table is Merk's and is here to show the widths. It is not part of the
format, and the code of this crate does not hold it.

**A link keeps its size.** Merk gives every node of a tree the variant of
the tree's type, so every link of one Merk has an aggregate of the same
length, and a link is 43 bytes in a tree with no count or sum, and 51, 59 or
67 bytes in the others. The widest aggregate there is, a count with a
128-bit sum (tags 9 and 10), is 25 bytes and its link 67. Whatever a write
below a node changes in a link that exists, the hash, the heights, the
numbers of the aggregate or the address, it is as long as before and is
overwritten where it is (TP10). The same holds for the feature of a node.
So within one Merk a cell changes its length only when a child appears or
goes, when its value changes its length or its place, or when the number
of its hosted roots changes. The page itself does not require this:
`set_link` and `set_feature` take bytes of another length and lay the cell
out again.

**A variant that Merk adds later needs no new page format and no new format
version.** Its tag and its numbers are written by the rule above, and a page
stores them as it stores any others. What it costs: one arm in MERK-01's
encoding, and cells of such a tree that are as much longer as its numbers
are. The limit is 255 bytes for a feature or an aggregate; the widest today
is 25. The price of this is the length byte in each link and in each cell.
A table of variants inside the format would save those bytes and would need
a format version for every tree type Merk gains.

Where this ends: a variant with a field that is not a count or a sum, or
whose aggregate has another tag than its feature, needs a line here that
says how its payload is written, and still no change to the page. A feature
or an aggregate above 255 bytes does not fit the length byte and would need
a new format version.

**TP13. A value is in its cell or in the value log, and its length is
always in the cell.** `value_len` is the value's length in both cases, so
the length is read without the value and without a second read
(`TreeCell::value_len`). With bit 2 of the flags clear, the value's bytes
are the last `value_len` bytes of the cell. With it set, the cell ends with
12 bytes, the file id and the offset of the value pointer of FMT-07, and the
pointer's third field is `value_len`: `TreeCell::value` returns the
`ValuePointer` built from the three, checked as section 4 of
`value-log-format.md` asks. An empty value is never out of line (V2 there).

The value log's back pointer names a page and no slot. The cell that owns a
value is the one that holds the pointer to it, and
`TreePage::find_value_pointer` finds it (O1 of `value-log-format.md`).

**TP14. A cell with its slot is smaller than half the capacity of a page**
(C7), for every key and value Merk accepts today. This is how:

- `TreePageMut` refuses a cell that is longer than
  `PageSize::max_cell_len` (`CellTooLarge`), and `decode` refuses a page
  that holds one. For a page of 32,768 bytes the longest cell is 16,370
  bytes.
- Everything in a cell but its value has a length field of one byte: the
  key, the feature, each aggregate, the number of hosted roots. So a cell
  whose value is out of line is at most 2,718 bytes, 2,720 with its slot,
  whatever it holds (`TreeCell::MAX_OUT_OF_LINE_LEN`). That is less than
  half the capacity of the smallest page, 8 KiB, which is why the smallest
  page is 8 KiB.
- A key is 0 to 255 bytes because Merk accepts no longer one: every form of
  `Merk::apply` goes through one function that refuses a batch with a key
  above 255 bytes (`apply_unchecked_with_old_value_observer`,
  `merk/src/merk/apply.rs`), a link's encoding has one byte for the key's
  length (`merk/src/tree/link.rs`), and GroveDB checks the same before it
  reaches Merk. So no key needs an overflow, and the format has none.
- A value has any length up to 2^32 − 1 bytes, which is the value log's
  limit (V9 of `value-log-format.md`) and the width of every value length in
  Merk's cost rules. A value that does not fit in its cell goes out of line.
  `PageSize::max_inline_value_len` gives the longest value that still
  fits in a cell with given other parts, so the threshold policy of TREE-08
  can be "inline up to my threshold, and never above this".

No size that Merk accepts today is refused, and no new limit is introduced.

**TP15. The hosted-root pointers are a list by position** (C3). Entry *i* is
the address of the root cell of the *i*-th Merk the element hosts, or all
zero if that Merk has no root. The page does not know what the positions
mean. A cell has 0 to 255 of them; GroveDB's elements host at most four
Merks today (an indexed tree with three axes).

### 3.4 Images and pages in a buffer

**TP16. An image has one form** (B8). Its cells are in the order of their
slots, the first starts where the directory ends, each starts where the one
before it ends, and the last ends at `len`. `unused` is 0, the last slot of
the directory holds a cell, `cell_count` is the number of slots that hold
one, and every reserved bit is zero. `decode` rejects everything else. So
equal pages have equal images, whatever was done to them in which order
(B9), and what was read encodes back to the same bytes.

**TP17. A page in a buffer has the same layout, with room to change.** Its
cells may be in any order, with unused bytes before, between and after
them. No cell lies past `len`, and `unused` counts the bytes between the
directory and `len` that belong to no cell. The header, the directory and
the cells are read
in the same way, so the same accessors serve both, and an image that is
copied to the start of a buffer is a page in a buffer with nothing else to
do. Going back is `TreePage::write_image`, which writes the cells in slot
order. The unused bytes are never written to a file. Their layout is not
part of the format: a later build may place cells differently in a buffer
and reads the same images.

## 4. Reading a page

`TreePage::decode(bytes, version, size)` reads the image at the start of a
byte string and ignores what follows, so it can be given a whole buffer. It
takes the format version of the file the image is in, since an image
carries none (R5), and the store's page size. It checks, in this order:

1. at least 22 bytes: else `Truncated`
2. the page kind is 3: else `UnknownRecordKind`
3. the reserved byte is 0: else `NonZeroReserved`
4. `len` is at least 22: else `TreePage(PageTooShort)`
5. at least `len` bytes: else `Truncated`
6. the page id is not 0: else `MissingId`
7. the directory lies inside the page: else `TreePage(DirectoryOutOfBounds)`
8. `len` is at most what a page of that size may be: else
   `TreePage(PageTooLong)`
9. `unused` is 0: else `TreePage(NotCompact)`
10. each slot that is not free, in slot order:
    1. its cell starts where the last one ended, or where the directory
       ends: else `TreePage(CellsOutOfOrder)`
    2. the cell (see below), and that it ends inside the page: else
       `TreePage(InvalidSlot)`, or the cell's own error
    3. the cell is not too large for the page size (TP14): else
       `TreePage(CellTooLarge)`
11. the last slot is not free: else `TreePage(TrailingFreeSlot)`
12. the last cell ends at `len`: else `TreePage(UnusedBytes)`
13. `cell_count` is the number of cells found: else
    `TreePage(CellCountMismatch)`

A cell is checked in this order, in a page and by `TreeCell::decode`, which
reads a cell from its own bytes:

1. its 72 fixed bytes are there: else `Truncated`
2. the reserved flags are 0: else `NonZeroReserved`
3. for each link, left then right: its 42 fixed bytes are there, else
   `Truncated`, and neither the page id nor the slot of the child is 0, else
   `MissingId`
4. for a value out of line: the 12 bytes are there, else `Truncated`, and
   they and `value_len` make a valid value pointer: else `MissingId`,
   `ValueLog(InvalidOffset)` or `ValueLog(EmptyValue)`
5. all of the cell is there: else `Truncated`
6. each hosted-root pointer is a whole address or all zero: else
   `MissingId`
7. `TreeCell::decode` only: nothing follows the cell: else
   `TreePage(CellLengthMismatch)`

Steps 1 to 9 cost a fixed amount, and step 10 one pass over the cells with
no allocation. A page that sits in a buffer was checked when it was loaded,
so `TreePage::from_validated` borrows it again in constant time: it runs
steps 1 to 7. Its accessors never read out of bounds and never panic,
whatever the bytes are, but on bytes that were never checked their answers
mean nothing. `write_image` of such bytes writes at most 65,535 bytes,
though slots that share a cell would give more.

`TreePage::check` is the full check for a page in a buffer. It runs
everything above except steps 9 and 12 and the order of step 10.1, and
instead requires that every cell lies behind the directory, that no two
cells share a byte (`OverlappingCells`), and that `unused` is the number of
bytes the cells leave (`UnusedBytesMismatch`). It sorts the cells, so unlike
everything else here it allocates.

Reading is `TreePage::cell(slot)`, which gives a `TreeCell`: the key, the
feature, the two hashes, `link(side)` with the child's address, hash,
heights, key length and aggregate, the hosted roots, the value or the value
pointer, the value's length, and the fragment-root mark. A `TreeCell`
borrows from the page. Opening one reads the cell's header and the fixed
part of its links, and nothing of the key or the value.

## 5. Changing a page

`TreePageMut` is a page in a buffer of the store's page size, open for
changes. `TreePageMut::init` starts an empty page, and `TreePageMut::open`
opens what the buffer holds in constant time.

| Change | Call | What it writes |
| --- | --- | --- |
| a new cell | `insert`, `insert_encoded` | the cell, at the end of the page; the slot |
| a cell leaves | `remove` | the slot and the header: `departures` goes up by one |
| a hash | `set_hash` | 32 bytes, where they are |
| a child's address | `set_child` | 6 bytes, where they are |
| the fragment-root mark | `set_fragment_root` | 1 byte, where it is |
| one hosted-root pointer | `set_hosted_root` | 6 bytes, where they are |
| a link | `set_link` | the link where it is, if there was one with an aggregate of the same length. Otherwise the cell is laid out again |
| the feature | `set_feature` | the feature where it is, if its length is the same. Otherwise the cell is laid out again |
| the value | `set_value` | the value where it is, if it takes the same room: a value of the same length, a pointer in place of a pointer, or a pointer and a value of 12 bytes in place of each other. Otherwise the cell is laid out again |
| all hosted-root pointers | `set_hosted_roots` | where they are, if their number is the same. Otherwise the cell is laid out again |
| the whole cell, for the same key | `replace`, `replace_encoded` | the cell, where it is if it is not longer |

**W1. No change moves a slot number, and a slot keeps its node.** A cell
that is laid out again keeps its slot. Only `remove` counts as a departure
(TP7). `replace` takes a cell with the same key and refuses any other
(`KeyChanged`): a node never changes its key, so another key is another
node, and that one arrives by `remove` and `insert`. The page cannot tell
two nodes with equal keys apart, which cells of different Merks may have;
that `replace` is given the same node is its caller's part.

**W2. A cell that grows moves to the end of the page.** The bytes it leaves
are unused, and `unused` counts them. A cell that shrinks stays where it is.
When the end of the page has no room for a cell but the page has enough
unused bytes, the cells are gathered: they are written in slot order behind
the directory, as in an image, and the cell goes behind them.
`TreePageMut::compact` does the same on request. Cells are never placed
into a gap between two cells.

**W3. The directory grows where the first cell is.** A new highest slot
number makes the directory longer. A cell that starts where the directory
has to grow is moved to the end of the page first, and the bytes it leaves
serve the slots that follow. A page that was just loaded pays this once: its
first cell starts where its directory ends.

**W4. A write that is refused changes nothing.** `PageFull` (the page has
too few free bytes), `CellTooLarge` (TP14), `SlotOccupied`, `SlotFree`,
`KeyChanged`, `NoSuchLink`, `NoSuchHostedRoot` and `FieldTooLong` are found
before a byte is written, and so is a cell handed to `insert_encoded` or
`replace_encoded` whose hosted-root pointers do not decode (`MissingId`). `TreePage::free_bytes` is what a page still has for cells and
their slots: its capacity less the directory and the cells.

**W5. `TreePageMut` needs a valid page.** It keeps a valid page valid: one
that `decode` accepted and was copied into the buffer, or that was started
with `init`, and then changed only here. On other bytes it may refuse a
write with `InvalidSlot` after it has moved cells, or panic on an index, as
safe code does. It reads no file and holds no checksum, so damage has to be
found before the page is opened: by the block's checksum and by `decode`.

Gathering allocates a copy of the cells, and laying a cell out again
allocates that cell. A hash, an address, a mark, and a link, feature or
value of the same size are written with no allocation.

## 6. Sizes

| | Bytes |
| --- | --- |
| page header | 22 |
| slot directory entry, for each slot up to the highest in use | 2 |
| cell header: flags, three lengths, the value's length | 8 |
| value hash and key-value hash | 64 |
| link, without its aggregate | 42: address 6, hash 32, heights 2, key length 1, aggregate length 1 |
| hosted-root pointer | 6 |
| a value out of line | 12 |

A cell is

```text
72 + key + feature
   + (42 + aggregate) for each link
   + 6 for each hosted root
   + value, or 12
```

bytes, plus 2 for its slot (`TreeCell::len_of`). The smallest cell, a leaf
with an empty key and an empty value, is 72 bytes. The feature and each
aggregate are 1, 9, 17 or 25 bytes (TP12), so a link is 43, 51, 59 or 67
bytes.

Against the sizes `tree-layout.md` assumed for its simulation (C6): the page
header is 22 bytes, not 32. A cell header is 8 bytes, not 4, because it has
a length for the feature and a count of hosted roots, and a 32-bit value
length. A link is 2 bytes more, for the two lengths. A value out of line is
12 bytes, not 16, because the cell header already has its length. The
feature and the aggregates are one byte each in a tree with no count or sum,
as in Merk's record, so a document node of the reference workload with two
children is 205 bytes with its slot and a document leaf 119, against 201
and 119 as assumed. In a tree with counts or sums a cell is longer than
Merk's record would suggest: each count and sum is 8 bytes where Merk's
record has a varint, in the feature and in each link. A node of a sum tree
with two children and sums below 64 is 21 bytes longer for it.

`layout_sim::CellCosts::tree_page()` holds these sizes and is what the
simulation now uses, with every count and sum at its full width; a test in
`lignin-testkit` compares them with the constants of this crate, and one
lays out a sum tree and a count-sum tree. `CellCosts::assumed()` keeps the earlier ones. The
acceptance test of TREE-01 on the reference workload, one million
transitions at 32 KiB, gives:

| | Required | With the assumed sizes | With these sizes |
| --- | --- | --- | --- |
| nodes per page | at least 15 | 129.0 | 126.1 (2,020,007 nodes in 16,023 pages) |
| fill, at the end and at its lowest sample | at least 70% | 75.0%, 72.1% | 74.8%, 71.9% |
| pages dirtied per transition, over the load and in its last tenth | at most 8 | 7.04, 7.21 | 7.29, 7.21 |

The reference workload has no tree with a count or a sum, so the widths of
TP12 do not enter these numbers. A page holds 2% fewer nodes, because a
cell is a few bytes longer, and a transition dirties a quarter of a page
more on average. 1.634 cells are
carried per transition and 0.805 values are appended again (369 bytes),
against 1.626 and 0.803 (368 bytes).

## 7. What the tasks that build on this take from it

| Task | Takes from here | Still its own to define |
| --- | --- | --- |
| FMT-03 Page delta records | A delta names a cell by page id and slot, and a part of it by `HashField`, `Side` or a hosted-root position; offsets inside a page are not stable and are never logged. A hash update is 32 bytes and writes no other byte (TP9). A link of the same aggregate length is overwritten where it is (TP10). A cell travels as its own bytes (TP8): `TreeCell::decode`, `insert_encoded`, `replace_encoded`. Applying a chain is calling `TreePageMut`; a base image is `TreePage::write_image`, which has one form (TP16), so a base image plus a chain can be compared with the consolidated image byte for byte. `remove` is the one change that counts a departure, so a carried cell is a remove and an insert. | The records ([`page-delta.md`](page-delta.md)). Whether a carry is logged as one record: it is not (its section 8.1). |
| STORE-04 Buffer pool | A frame is `PageSize::get()` bytes. An image is copied or decompressed to the start of a frame and is then a page in a buffer (TP17). `TreePage::from_validated` borrows it for readers in constant time with no allocation. The first byte of a frame tells a tree page from a flat page (TP1). | Frames, pins, latches. |
| STORE-05 Page materialisation | `TreePage::decode` with the store's page size, on a block whose checksum was verified (TP3). The page id in the header is compared with the id that was asked for (TP2). | The load path. |
| STORE-06 Delta write-back | A base image is `write_image` of the frame, at its exact length (`used_bytes`). A hash-only change writes 32 bytes of the frame and no value byte. | Thresholds. |
| STORE-09 Value log | `TreePage::find_value_pointer` finds the owner of a value in the page its back pointer names. Moving a value is `set_value` with the new pointer, 12 bytes written where the old ones were. | The cleaner. |
| TREE-02 Node handles | `TreeCell` is the borrowed view of a node: no allocation, the value's length without the value, the child's key length without the child. `TreePageMut` has the operations of `NodeMut`. A handle is a cell address with the page's `departures` when it was read: while the count is the same, the slot names the same cell (TP7). | The handle, and what it does when the count has moved. Whether a page that is created anew for a reused page id continues a count (section 8, item 9). |
| TREE-03 Fragment maintenance | `lowest_free_slot` for S4. `free_bytes`, `used_bytes` and `TreeCell::len_of` to plan what fits. `fragment_roots`, `set_fragment_root`, and `check_fragments` in tests. `insert_encoded` to carry a cell, then `set_child` in its parent. `PageSize::max_cell_len` is C7. | Placement. |
| TREE-05 Subtree directory | `CellAddress`, six bytes, with all zero for "no cell". | The entry. |
| TREE-08 Out-of-line values | `PageSize::max_inline_value_len` bounds the threshold (TP14). A cell stores a `ValuePointer` as 12 bytes and its length. | The threshold. |
| MERK-01 LigninNodeStore | The link gives the child's key length, so `record_len` and Merk's link sizes are computed from the parent's cell alone. The key of a link is in the child's cell. The feature and the aggregates are written by the rule of TP12: Merk's tag, then each count and sum at its full width. `record_len` needs the varint lengths of those numbers, which Merk computes from the numbers. | The functions that write and read those bytes for each variant, and one more arm when Merk gains a variant. |
| MERK-05 Iteration | A cursor's value is a node record in Merk's encoding, which repeats each child's key. Those keys come from the children's cells, by the addresses in the links. | Whether records are built, or callers move to node views (PERF-05). |
| FMT-08 Decoder fuzzing | The decoders: `TreePage::decode`, `TreePage::from_validated` with every accessor, `TreePage::check`, `TreePage::check_fragments`, `TreeCell::decode`. `TreePageMut` is fuzzed from valid pages only (W5). The golden bytes of section 2.5. | The targets. |
| VER-07 Physical invariant checker | `TreePage::check_fragments` for F3 and C5 inside a page, and for the key length a link states for a child in the same page. `TreePage::check` for a page in a buffer. | Links that cross pages: the address, and the key length. The heights and the aggregate in a link, which are Merk's to check: the page does not interpret them. |
| FMT-04 Flat page codec | It followed three things after this task (section 8, items 3 and 12, and TP2): its image has no checksum, its decoder checks an out-of-line value, and it holds its own page id at the offset a tree page does. | |

## 8. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one while version 1 is still open (U10).

1. **A page holds its own id** (TP2). Four bytes a page.
2. **The page size is not in the image** (TP4). A store must record it,
   because a page in a buffer may grow to it and C7 is measured against it:
   opening a store with a smaller page size than it was written with must
   be refused. When this format was written no file had a field for it.
   The manifest body now does, as a fixed field of version 1, and the store
   refuses another size (M17 and M18 of `segment-manifest.md`).
3. **No checksum inside the image** (TP3). What a checksum inside the image
   adds is a check after decompression, of the decompressor. Flat pages had
   one when this format was written (4 bytes, and a pass over the page on
   each load) and have since dropped it for the same reason (P12 of
   `flat-page.md`).
4. **Slot numbers start at 1 and 0 is "no slot"** (TP5), as page ids and
   file ids do, so B6 holds without an exception and a zero-filled address
   is "no cell". `SlotId` and `CellAddress` are in `lignin-format::ids`.
5. **A link always has the full address** (TP10). `tree-layout.md` allowed a
   shorter link to a cell of the same page. It would save 4 bytes in most
   links, about 2% of a page, and cells would no longer be their own bytes
   (TP8).
6. **The fragment-root table is a bit in each cell** (TP11), not a list. A
   list would be a third region of the page that grows and shrinks. Listing
   the roots reads one byte of each cell.
7. **Feature and aggregates are Merk's tag and its numbers at full width,
   each with a length byte, and the page does not interpret them** (TP12).
   A link is then fixed in size for each variant, not one size for the
   whole database: 43 to 67 bytes. The alternatives: a table of Merk's
   variants in the format saves the length byte in each link and cell and
   needs a format version for every tree type Merk gains; one link size for
   all, wide enough for the widest aggregate, makes every link 67 bytes,
   24 more than a tree with no count or sum needs, and still needs a format
   version for a wider one. Full width costs up to 7 bytes for each count
   and sum against Merk's varints, and is what keeps a link's size the same
   under every hash update.
8. **The value's length is in the cell header, and a value out of line
   stores 12 bytes** (TP13), as P6 of `value-log-format.md` suggests. A flat
   leaf stores the whole 16-byte pointer.
9. **`departures` is 64 bits and the page's creator gives its start** (TP7).
   A page id is reused after its page is freed (STORE-10). If the new page
   starts at 0, a handle that outlived the old page could meet its own count
   again. Three ways out, for TREE-02: a handle also keeps something the
   store changes when a page id is reused; or a new page starts at a number
   no earlier page under that id reached, for which the field is wide
   enough to hold, say, a commit version in its high bits; or no handle
   outlives the write that could free its page.
10. **Pages are 8 KiB to 64 KiB** (TP4, TP14). Below 8 KiB a cell with the
    longest of every part would not be smaller than half a page. The
    simulation of `tree-layout.md` also ran 4 KiB; this format does not
    serve it. The owner ruled that a size is a power of two (3.5 of
    `format.md`); this format took every size in between until then.
11. **An image has one form and a page in a buffer does not** (TP16, TP17).
    The price is that a base image is written by a pass that orders the
    cells, not by copying the frame.
12. **Whether flat-page decode should check the length of an out-of-line
    value.** It should: FMT-07 fixed the pointer at 16 bytes after FMT-04
    was written, so a flat leaf entry that is marked out of line and is not
    a valid `ValuePointer` is damage, and `FlatPage::decode` can say so with
    `ValuePointer::decode`. That was a change to `flat_page`, not made by
    this task; `FlatPage::decode` checks it now (P5 of `flat-page.md`).
13. **A cell is replaced only by a cell with the same key** (W1). The
    plan's "cell update" needs no more, and it makes "a slot names one node
    between two departures" a property of the writer instead of a rule for
    its callers.
14. **`TreePageMut` does not survive damaged pages** (W5). Making every
    write total on arbitrary bytes would cost a full check on each open.
15. **Cells are not placed into gaps** (W2). A page gathers its unused bytes
    when the end has no room. A free list inside the page would save some of
    those passes and is not part of the format, so it can be added later.
16. **The tree page's errors are one variant of `FormatError`**,
    `TreePage(TreePageError)`, as for the other formats. A value pointer
    that is not valid keeps the value log's errors.
17. **The simulation's default sizes are now this format's**
    (`CellCosts::tree_page`). The tables of sections 14 and 15 of
    `tree-layout.md` were computed with the assumed sizes and are not
    recomputed here, except the three targets (section 6).

## 9. Code and tests

All of this is in `lignin-format::tree_page`. Run the tests with
`cargo test -p lignin-format tree_page`.

| Rule | Code | Tests |
| --- | --- | --- |
| Layout (section 2) | `TreePage`, `TreeCell`, `TreeLink`, `CellContent` | `golden_bytes`: the example of 2.5, written out by hand, is what the codec writes and what it reads. `the_empty_page_is_its_header`. `sizes_are_the_documented_ones`. A second layout of every page and cell, written from this document in the tests (`tests/model.rs`), is compared with the codec's bytes in every test below. |
| Round trip (TP16, B8) | `TreePage::decode`, `TreePage::write_image`, `TreeCell::decode`, `CellContent::encode` | `random_changes_keep_every_cell_under_its_slot`: 48,000 random changes on five pages of four sizes, each checked against a model through every accessor, and the page's image against the model's bytes, decoded and encoded again. `a_cell_reads_back_from_its_own_bytes`. `every_single_bit_flip_is_rejected_or_reads_as_what_it_says`: an image with one bit flipped is rejected or decodes to content that encodes back to exactly those bytes. |
| TP9: hashes | `TreeCell::hash`, `TreePageMut::set_hash` | **Acceptance test**, `any_stored_hash_can_be_read_or_overwritten_without_touching_the_value_bytes`: on full pages of three sizes, every hash of every cell is the 32 bytes at an offset before the key and the value; overwriting it changes those 32 bytes and no other byte of the page, and the value is where it was. `a_hash_is_read_from_bytes_that_are_not_the_key_or_the_value`: with every key and value in the page overwritten, every hash and link reads the same. |
| TP5 to TP7, W1: slots | `SlotId`, `TreePage::lowest_free_slot`, `TreePage::departures`, `TreePageMut::{remove, replace}` | `slot_numbers_stay_and_the_lowest_free_one_is_taken_next`, `the_count_of_departures_wraps_and_never_stops`, `a_cell_is_replaced_only_by_a_cell_of_the_same_node`, and the random test, whose model counts departures. |
| TP8: a cell is its bytes | `TreePageMut::insert_encoded`, `replace_encoded` | `a_cell_is_carried_to_another_page_as_its_bytes`, `a_cell_that_would_not_decode_is_not_taken`. |
| TP10, section 5: in place | `TreePageMut::{set_child, set_link, set_fragment_root, set_hosted_root, set_hosted_roots, set_feature, set_value}` | `a_change_of_the_same_size_is_made_where_the_cell_is`: each change writes only the bytes of its part, and no cell moves. |
| TP12: features and aggregates | `TreeLink`, `TreePageMut::{set_link, set_feature}` | `every_aggregate_fits_a_link_and_is_rewritten_where_it_is`: a feature and two aggregates of each width Merk has (1, 9, 17, 25 bytes), of a wider one and of 255 bytes are stored, and rewritten with other numbers where they are. |
| TP11: fragment roots | `TreePage::fragment_roots`, `TreePage::check_fragments` | `the_fragment_roots_are_the_cells_no_cell_of_the_page_points_at`, `the_fragment_check_agrees_with_the_model_on_random_pages` (3,000 pages). |
| TP13: values | `CellValue`, `TreeCell::value_len`, `TreePage::find_value_pointer` | `the_cell_that_holds_a_value_pointer_is_found_by_the_pointer`, and the pointer errors in `each_check_of_a_cell_has_its_error`. |
| TP4, TP14: sizes | `PageSize`, `TreeCell::MAX_OUT_OF_LINE_LEN`, `TreeCell::len_of` | `ids::tests::a_page_size_is_one_of_four_powers_of_two`. `a_cell_and_its_slot_are_smaller_than_half_of_what_cells_can_use`: the limits at each of the four sizes; what cells can use is even at three of them and odd at the largest, whose image stops at 65,535 bytes, so the rounding is checked both ways without a size that is not a power of two. `a_cell_is_smaller_than_half_a_page_whatever_its_key_and_value`: the largest cell with its value out of line fits three times in the smallest page; every key length from 0 to 255 fits; a value one byte above the longest inline value is refused and fits out of line. `a_cell_too_large_for_a_smaller_page_size_is_refused_on_reading`. A compile-time assertion ties the smallest page to the largest out-of-line cell. |
| Section 4 | `TreePage::decode`, `TreePage::check`, `TreeCell::decode`, `TreePageError` | Each step is rejected with its error, and in order (`each_check_of_the_header_has_its_error`, `each_check_of_the_slots_has_its_error`, `each_check_of_a_cell_has_its_error`, `the_checks_run_in_their_documented_order`, `each_check_of_a_page_in_a_buffer_has_its_error`); every shorter input; zero-filled bytes; a page longer than its store's pages. |
| B10 | every reader | `arbitrary_bytes_are_an_error_or_a_page_and_never_a_panic`: 60,000 byte strings, a third arbitrary, a third arbitrary behind a header that passes and with slots that share an offset, a third valid images with bytes changed, through every accessor. `a_page_whose_slots_share_a_cell_is_written_out_within_a_page`. |
| Section 5 | `TreePageMut` | `a_full_page_refuses_a_write_and_takes_it_once_there_is_room`, `the_directory_grows_past_the_cells_in_its_way`, `the_directory_grows_in_a_page_with_no_room_at_its_end`, `gathering_the_unused_bytes_gives_the_image`, `a_write_to_a_free_slot_or_a_missing_part_is_refused`, `a_page_is_opened_in_a_buffer_of_its_page_size`. In the random test every refused write is compared with what the model expects, and must leave every byte of the buffer as it was. `a_damaged_page_is_refused_where_the_damage_is_met` shows W5: a slot that leads nowhere is `InvalidSlot` to whatever touches it. |
| No allocation | every accessor, and the writes of section 5 that are made in place | `tests/tree_page_alloc.rs` runs under a counting allocator. |
| Identifiers | `ids::{SlotId, CellAddress}` | Round trip of every slot number; the reserved patterns; half an address. |
