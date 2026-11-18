# LigninDB on-disk format: flat pages

This is the specification for task FMT-04 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines the page images of the flat keyspace: the B+tree that holds ordered
data which is not part of a Merk tree (aux, meta, roots, the append-only trees
and the subtree directory, TREE-07).

It builds on [format.md](format.md) (FMT-01) and uses its rule numbers (B1 to
B11, C1 to C7, R1 to R7). The code is `lignin-format::flat_page`. Section 7
lists the choices made here that the plan left open, and section 8 maps each
rule to its tests.

Two things changed after FMT-02 (the owner's decision D-049): an image no
longer has a checksum of its own (P12), and an out-of-line value is checked
to be a value pointer (P5).

## 1. Scope

A **flat page** is one node of a B+tree, named by a page id (format.md 3.1).
There are two kinds:

- a **leaf** holds keys in ascending order, each with a value
- a **branch** holds separator keys in ascending order, between the page ids
  of its children

This document fixes the bytes of one page: its **image**. It does not say how
the tree splits, merges or picks a prefix (TREE-07), how an image is stored in
a segment (FMT-06), or what a log record carries for a flat keyspace (FMT-05).

An image is not a record of its own. It is the payload of one: a segment
block (FMT-06) or a log record (FMT-05), whose checksum covers it (R1, R4).
So an image has no checksum (P12) and no format version (R5). Its length is
one of its own fields.

## 2. Layout

All integers are little-endian and unaligned (B1, B2). Offsets count from the
first byte of the image.

### 2.1 Header

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | page kind: 1 leaf, 2 branch |
| 1 | 1 | level: 0 in a leaf, 1 to 255 in a branch |
| 2 | 2 | `len`: length of the whole image in bytes, `u16` |
| 4 | 2 | `count`: number of entries, `u16` |
| 6 | 2 | `prefix_len`: length of the prefix in bytes, `u16` |
| 8 | 4 | branch only: page id of the first child |

A leaf header is 8 bytes and a branch header is 12.

### 2.2 Body

After the header, with no gaps:

1. the **prefix**: `prefix_len` bytes
2. the **slot directory**: `count` slots of fixed width
3. the **cells**: one for each slot, in slot order, each starting where the
   one before it ends. The first starts where the slot directory ends, and the
   last ends at `len`.

A leaf slot is 5 bytes:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 2 | `end`: offset in the image at which this slot's cell ends, `u16` |
| 2 | 2 | `key_len`: length of the key suffix, `u16` |
| 4 | 1 | flags. Bit 0: the value is out of line. Bits 1 to 7 are reserved. |

A leaf cell is the key suffix (`key_len` bytes) followed by the value bytes
(the rest of the cell). For a value out of line they are the 16 bytes of a
value pointer (P5).

A branch slot is 2 bytes: `end`, as above. A branch cell is the separator's
suffix followed by a 4-byte child page id, so it is at least 4 bytes.

### 2.3 Examples

A leaf with the prefix `user/` and three entries: `user/alice` with the value
`A1`, `user/bob` with a value out of line (70,000 bytes at offset 20 of
`val/00000003.val`), and `user/carol` with an empty value. 59 bytes:

```text
01                          kind: leaf
00                          level 0
3b 00                       len 59
03 00                       count 3
05 00                       prefix_len 5
75 73 65 72 2f              prefix "user/"
23 00  05 00  00            slot 0: end 35, key_len 5, inline
36 00  03 00  01            slot 1: end 54, key_len 3, out of line
3b 00  05 00  00            slot 2: end 59, key_len 5, inline
61 6c 69 63 65  41 31       cell 0: "alice", "A1"
62 6f 62                    cell 1: "bob", then the value pointer:
  03 00 00 00                 file id 3
  14 00 00 00 00 00 00 00     offset 20
  70 11 01 00                 value length 70,000
63 61 72 6f 6c              cell 2: "carol", no value bytes
```

A branch at level 1 with the prefix `user/`, the first child 7, and two
separators: `user/b` with child 9 and `user/carol` with child 12. 35 bytes:

```text
02                          kind: branch
01                          level 1
23 00                       len 35
02 00                       count 2
05 00                       prefix_len 5
07 00 00 00                 first child: page 7
75 73 65 72 2f              prefix "user/"
1a 00                       slot 0: end 26
23 00                       slot 1: end 35
62  09 00 00 00             cell 0: "b", page 9
63 61 72 6f 6c  0c 00 00 00 cell 1: "carol", page 12
```

The empty leaf is the 8 bytes `01 00  08 00  00 00  00 00`.

## 3. Rules

**P1. A key is a byte string, and keys are ordered byte by byte.** The order
is lexicographic on unsigned bytes, and a key sorts before every longer key
that starts with it. The empty key is valid.

**P2. Keys are strictly ascending.** Slot 0 holds the smallest key. No key
appears twice.

**P3. Every key starts with the page's prefix.** A key is the prefix followed
by the suffix stored in its cell. The prefix is stored once. An empty prefix
is valid, and so is an empty suffix (in at most one entry, by P2).

**P4. The prefix is part of the page's content, not derived from it.** Any
byte string that every key starts with is a valid prefix, not only the longest
one. This lets the tree pick a prefix that stays valid when keys are added:
what the two bounds of the page's key range have in common. A prefix chosen
that way changes only when the page is split or merged. Because one set of
keys has several valid images, the tree must pick the prefix by a rule that
depends only on the tree's content, so that equal histories give equal bytes
(B9).

**P5. A leaf value is inline or out of line.** If bit 0 of the flags is 0, the
value bytes are the value, and it may be empty. If it is 1, they are a
pointer to a value stored in the value log: the 16 bytes of the value
pointer that FMT-07 defines (section 4 of `value-log-format.md`), and
nothing else. An out-of-line value of another length, or one whose 16 bytes
are not a valid pointer, is damage, and a reader rejects it. The page checks
the pointer's own rules (a file id, an offset a value can start at, a length
of at least 1). Whether the pointer leads to a value is the value log's to
say.

**P6. A branch with *n* separators has *n* + 1 children.** Child 0 is the
first child, in the header. Child *i* + 1 is stored with separator *i*. Every
key under child 0 is less than separator 0. Every key under child *i* + 1 is
greater than or equal to separator *i* and less than separator *i* + 1, if
there is one. So a key belongs under child *k*, where *k* is the number of
separators less than or equal to it. A separator does not have to be a key
that exists: any byte string that divides the two children is valid, and a
short one saves space.

**P7. A branch may have no separator.** It then has one child. A leaf may have
no entry. Whether the tree ever writes such a page is its own rule.

**P8. The level is the page's distance from the leaves.** A leaf has level 0.
A branch has the level of its children plus one, from 1 to 255. The format
checks only that the level agrees with the kind. That children are one level
down is for the tree and the invariant checker (VER-07).

**P9. Child page ids are never 0** (format.md 3.1). A branch always has a
first child.

**P10. An image is 8 to 65,535 bytes long.** Every offset in a page is a
`u16`, which sets the upper bound. The image does not hold the page size:
whoever builds a page passes the largest image length it may use, and the
image is exactly as long as its content. That largest length comes from the
store's page size, which tree pages and flat pages share and the manifest
records (M17 of `segment-manifest.md`): it is the page size, except that a
store with 64 KiB pages gives flat pages 65,535 of the 65,536 bytes
(`PageSize::image_capacity`).

**P11. One encoding per content** (B8). The content of a page is its kind,
level, prefix, first child and entries. The layout leaves no freedom: cells
are in slot order with no gap before, between or after them, and no bit is
unused. So an image has no free space, and adding or removing an entry
produces a new image.

**P12. An image has no checksum of its own.** `format.md` asks that every
byte lies in a record with a checksum (R1, R4). For an image that record is
the segment block or the log record that holds it. Its checksum covers the
bytes as they are stored, compressed or not, and is verified before they are
decompressed or decoded (C7; S8 of `segment-manifest.md`).

So what detects a damaged image is the checksum of the block that holds it.
`FlatPage::decode` checks structure, not integrity. It rejects bytes that
are not a well-formed page, and that is all it can do: an image in which a
key or a value was changed into other bytes is as well formed as the
original. A reader must give `decode` bytes from a record it has verified.

The image had a checksum at first. It cost 4 bytes a page and a pass over
the page on every load, both paid a second time by the block, and tree pages
carry none (TP3 of `tree-page.md`).

**P13. A slot number is a position, not a name.** Slot *i* is the entry with
the *i*-th smallest key in this image. Adding or removing a smaller key moves
it. A reference to an entry that must outlive the image is the page id and the
key.

## 4. Reading a page

`FlatPage::decode` reads the image at the start of a byte string and ignores
what follows, so it can be given a whole frame. It checks, in this order:

1. at least 8 bytes: else `Truncated`
2. `len` is at least 8: else `FlatPage(ImageTooShort)`
3. at least `len` bytes: else `Truncated`
4. the page kind: else `UnknownRecordKind`
5. the level is 0 in a leaf and not 0 in a branch: else
   `FlatPage(InvalidLevel)`
6. in a branch, `len` is at least 12: else `FlatPage(ImageTooShort)`
7. in a branch, the first child is not 0: else `MissingId`
8. the prefix and the slot directory lie inside the image: else
   `FlatPage(DirectoryOutOfBounds)`
9. each slot, in order:
    1. its cell starts where the previous one ended, does not end before it
       starts or after `len`, and is long enough for `key_len` (leaf) or a
       child id (branch): else `FlatPage(InvalidSlot)`
    2. leaf: the reserved flag bits are 0: else `NonZeroReserved`
    3. leaf, value out of line: the value bytes are 16 bytes long: else
       `FlatPage(PointerLength)`; and they are a valid value pointer: else
       the error `ValuePointer::decode` gives, which is `MissingId`,
       `ValueLog(InvalidOffset)` or `ValueLog(EmptyValue)`
    4. branch: the child is not 0: else `MissingId`
    5. its key is greater than the previous slot's: else
       `FlatPage(KeysOutOfOrder)`
10. the last cell ends at `len`: else `FlatPage(UnusedBytes)`

No step verifies a checksum, because the image has none (P12).

A run of 8 or more zero bytes fails at step 2, and a shorter one at step 1.

`len` is limited by the bytes given and by its own 16 bits. The page size is
the store's to enforce: it gives the reader a frame of one page, or compares
the image length with its page size.

Step 9 costs one key comparison per entry. A page that sits in a buffer-pool
frame was checked when it was loaded, so `FlatPage::from_validated` borrows it
again in constant time. It runs steps 1 to 8 only. Its accessors
never read out of bounds and never panic, whatever the bytes are, but on bytes
that `decode` would reject their answers mean nothing.

### 4.1 Search

A search for a key in a page first compares the key with the prefix:

- the key is shorter than the prefix: it is less than every key of the page,
  unless its bytes are greater than the prefix's, in which case it is greater
  than every key
- the key's first `prefix_len` bytes are less than the prefix: it is less than
  every key
- they are greater: it is greater than every key
- they are equal: the rest of the key is searched for among the suffixes

The last case is a binary search over the slot directory. Each probe reads two
slots and compares the probed cell's suffix with the rest of the key, in
place. Nothing is copied and nothing is allocated.

`FlatLeaf::search` returns the slot of the key, or the slot at which it would
be inserted. `FlatBranch::child_index` returns the child of rule P6.

## 5. Writing a page

`FlatLeafBuilder` and `FlatBranchBuilder` build an image of the current format
version. The caller gives the prefix and the capacity (the largest image
length allowed, at most 65,535), then adds entries in ascending key order.

- A key that does not start with the prefix is refused (`KeyOutsidePrefix`),
  and so is one that is not greater than the last (`KeysOutOfOrder`).
- An out-of-line value that is not a value pointer is refused, with the
  errors of step 9.3 of section 4, so a builder writes no image that a
  reader rejects.
- An entry that would take the image past the capacity is refused with
  `PageFull`, and the builder is unchanged. This is how a caller finds where a
  page ends and the next begins.
- `push` takes a key as one byte string. `push_key` takes a key borrowed from
  another page, in its two parts, and gives the same image. A page is rebuilt
  from an old one this way, under the same prefix or another, without joining
  each key first.

Two helpers serve the tree's choice of keys. `common_prefix_len` is the number
of leading bytes two keys share; the longest prefix a page can use is what its
smallest and largest key share. `shortest_separator` gives, for two
neighbouring keys, the shortest byte string that divides them (P6).

## 6. Sizes

| | Leaf | Branch |
| --- | --- | --- |
| Header | 8 | 12 |
| Each entry, on top of its key suffix and value | 5 | 6 |

An image is its header, the prefix, and for each entry the overhead above plus
the key suffix and, in a leaf, the value bytes (16 for a value out of
line). `FlatLeaf::entry_len` and
`FlatBranch::entry_len` give the size of one entry.

An entry must fit in one page with its key. A key longer than a page has room
for is not representable; splitting or indirecting long keys would be the
tree's job (see section 7, item 9).

## 7. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one before TREE-07 and FMT-05 build on it.

1. **Prefix compression is one prefix for the whole page** (P3, P4), not a
   delta against the previous key with restart points. Every key can be
   compared in place, which is what makes the binary search free of
   allocation, and an insert never re-encodes its neighbours. A page whose
   keys share nothing stores an empty prefix and loses 0 bytes.
2. **An image has no free space** (P11). It is exactly as long as its content,
   which matches segments storing images at their actual length (FMT-06) and
   gives one encoding per content (B8). The cost is that every change to a
   page builds a new image. In-place insertion into a frame, if TREE-07 needs
   it for speed, would be a different in-memory form, not a change to these
   bytes.
3. **No sibling links and no fence keys.** A cursor moves between leaves
   through its parent stack, as TREE-06 does for Merkle trees. A link would
   make a split, merge or drop touch a neighbour page that is otherwise
   untouched.
4. **A level byte** (P8), so the invariant checker and `lignin inspect` can
   tell a branch's height without walking down.
5. **A flag for out-of-line values** (P5). Flat values can be larger than a
   page (bulk-append chunks, for example), so the leaf needs to say which
   kind of bytes it holds. The pointer's layout is FMT-07's (section 4 of
   `value-log-format.md`), so that tree and flat cells use the same one.
   The page left the pointer opaque until FMT-07 had defined it. Now a
   reader checks it, and so does a builder. `FlatValue::OutOfLine` still
   holds the 16 bytes as they are in the page, so the accessors of a page
   borrowed with `from_validated` stay total; a caller that wants the fields
   passes them to `ValuePointer::decode`.
6. **Offsets are 16 bits, so an image is at most 65,535 bytes** (P10). The
   page size itself is not fixed here: the builders take it as a parameter,
   and any value up to 65,535 that has room for the header and the prefix
   works. FMT-02 set the page size for tree pages (8, 16, 32 or 64 KiB, 32 KiB
   unless the store is told otherwise; 3.5 of `format.md`), flat pages share
   it, and the manifest records it (P10).
7. **Slot numbers in a flat page are positions** (P13), so no `SlotId` type
   was needed and none was added to `lignin-format::ids`. The positions fit in
   16 bits, as the owner decided for slots. A back pointer from the value log
   to a flat leaf (FMT-07, STORE-09) cannot be a page id and a slot: it has to
   be a page id and a key, or the cleaner has to look for the pointer in the
   page. FMT-07 chose the second (section 5 of `value-log-format.md`).
8. **The first byte of an image is a page kind code**, with 1 and 2 taken
   here. A tree page is kind 3 (TP1 of `tree-page.md`), so any page can be
   told apart from its first byte, and the length of any page image is the
   `u16` at offset 2.
9. **A key must fit in a page.** There are no overflow keys. RocksDB puts no
   limit on an aux or meta key, so TREE-07 has to set one or handle long keys
   above this format.
10. **Flat-page errors are one `FormatError` variant**, `FlatPage`, wrapping
    `flat_page::FlatPageError`. The shared errors (`Truncated`,
    `UnknownRecordKind`, `MissingId`, `NonZeroReserved`) are used where they
    apply, and a value pointer that is not valid keeps the value log's
    errors, as in a tree page.
11. **The reader takes the format version as a parameter**, since a page has
    none of its own (R5): `FlatPage::decode(bytes, version)`. The builders
    write the current version.
12. **The allocation test implements `GlobalAlloc`, which needs `unsafe`.** It
    is in the integration test `lignin-format/tests/flat_page_alloc.rs`, not
    in the crate, which still forbids unsafe code.
13. **No checksum in the image** (P12). What a checksum inside the image
    added was a check after decompression, of the decompressor. A wrong
    decompression is found by its length (S12 of `segment-manifest.md`) and
    by the structural checks of section 4, not by a checksum.
14. **A flat page does not hold its own page id.** A tree page does (TP2).
    A reader that was handed the image of another flat page cannot tell
    from the image. Nothing here adds the field: it would be 4 bytes a page
    and a change for TREE-07 to ask for.

## 8. Code and tests

All of this is in `lignin-format::flat_page`. Run the tests with
`cargo test -p lignin-format`.

| Rule | Code | Tests |
| --- | --- | --- |
| Layout (section 2) | `FlatLeafBuilder`, `FlatBranchBuilder`, `FlatPage::decode` | Golden bytes for a leaf with a value out of line, a branch, the empty leaf and a branch with one child; the kind codes, header lengths and entry overheads. |
| Round trip (P11, B8) | the builders, `FlatLeaf`, `FlatBranch` | 2,000 random pages: content to bytes to content, and bytes to content to bytes. 80,000 images with a bit flipped or bytes changed decode only to content that encodes back to the same bytes. |
| P5 | `FlatPage::decode`, `FlatLeafBuilder` | `an_out_of_line_value_is_a_value_pointer`: 15, 17, 1 and 0 bytes; no file id; an offset below 20 and one above 2^63 − 1; a length of 0. Each is rejected with its error, before the key order is looked at and after the reserved bits, and the builder refuses the same values and is unchanged. The same bytes are accepted as an inline value. |
| P12 | `segment::Block`, `FlatPage::decode` | `the_block_that_holds_an_image_detects_every_changed_bit`: every single-bit flip of a raw and of an LZ4 block that holds a flat page image is rejected by the block. Of the same flips made in the image alone, more than a thousand decode as a page, which is why the block's checksum is what detects damage. |
| P1 to P4, section 4.1 | `FlatLeaf::search`, `FlatLeaf::get`, `FlatKey` | Search agrees with a sorted model for keys in the page, next to them, shorter than the prefix and outside it. Keys compare by their bytes wherever the prefix ends. |
| P6 | `FlatBranch::child_index`, `FlatBranch::child_for` | Routing agrees with a model for the same probes. |
| No allocation | `FlatPage::decode`, `FlatPage::from_validated`, every accessor | `tests/flat_page_alloc.rs` runs under a counting allocator: decoding, 806 searches over a 400-entry leaf, iteration in both directions and routing through a branch allocate nothing. |
| Section 4 | `FlatPage::decode`, `FlatPageError` | Each step is rejected with its error, in order; every shorter input; zero-filled bytes. |
| B10 | `FlatPage::decode`, `FlatPage::from_validated` | 200,000 arbitrary byte strings, most of them given a plausible header, and every damaged image above, go through every accessor without a panic. Iteration ends at the first slot that describes no cell. |
| P10, sections 5 and 6 | the builders | Capacity and prefix limits; keys outside the prefix or out of order; a full page refuses an entry and is unchanged; the size formula; an image of exactly 65,535 bytes; pages rebuilt from borrowed keys under four different prefixes equal the pages built from whole keys. |

The golden bytes were written by a separate encoder, not by the crate under
test.
