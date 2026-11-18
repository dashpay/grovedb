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

## 1. Scope

A **flat page** is one node of a B+tree, named by a page id (format.md 3.1).
There are two kinds:

- a **leaf** holds keys in ascending order, each with a value
- a **branch** holds separator keys in ascending order, between the page ids
  of its children

This document fixes the bytes of one page: its **image**. It does not say how
the tree splits, merges or picks a prefix (TREE-07), how an image is stored in
a segment (FMT-06), or what a log record carries for a flat keyspace (FMT-05).

An image is a record in the sense of format.md section 5. It has one checksum,
no format version of its own (R5), and its length is one of its own fields
(R3).

## 2. Layout

All integers are little-endian and unaligned (B1, B2). Offsets count from the
first byte of the image.

### 2.1 Header

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | page kind: 1 leaf, 2 branch |
| 1 | 1 | level: 0 in a leaf, 1 to 255 in a branch |
| 2 | 2 | `len`: length of the whole image in bytes, `u16` |
| 4 | 4 | checksum (format.md section 4) of bytes 0 to 3 followed by bytes 8 to `len` − 1 |
| 8 | 2 | `count`: number of entries, `u16` |
| 10 | 2 | `prefix_len`: length of the prefix in bytes, `u16` |
| 12 | 4 | branch only: page id of the first child |

A leaf header is 12 bytes and a branch header is 16.

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
(the rest of the cell).

A branch slot is 2 bytes: `end`, as above. A branch cell is the separator's
suffix followed by a 4-byte child page id, so it is at least 4 bytes.

### 2.3 Examples

A leaf with the prefix `user/` and three entries: `user/alice` with the value
`A1`, `user/bob` with a 4-byte out-of-line pointer, and `user/carol` with an
empty value. 51 bytes:

```text
01                          kind: leaf
00                          level 0
33 00                       len 51
4d f1 d0 f8                 checksum
03 00                       count 3
05 00                       prefix_len 5
75 73 65 72 2f              prefix "user/"
27 00  05 00  00            slot 0: end 39, key_len 5, inline
2e 00  03 00  01            slot 1: end 46, key_len 3, out of line
33 00  05 00  00            slot 2: end 51, key_len 5, inline
61 6c 69 63 65  41 31       cell 0: "alice", "A1"
62 6f 62  01 02 03 04       cell 1: "bob", the pointer
63 61 72 6f 6c              cell 2: "carol", no value bytes
```

A branch at level 1 with the prefix `user/`, the first child 7, and two
separators: `user/b` with child 9 and `user/carol` with child 12. 39 bytes:

```text
02                          kind: branch
01                          level 1
27 00                       len 39
9d d0 b9 92                 checksum
02 00                       count 2
05 00                       prefix_len 5
07 00 00 00                 first child: page 7
75 73 65 72 2f              prefix "user/"
1e 00                       slot 0: end 30
27 00                       slot 1: end 39
62  09 00 00 00             cell 0: "b", page 9
63 61 72 6f 6c  0c 00 00 00 cell 1: "carol", page 12
```

The empty leaf is the 12 bytes `01 00 0c 00  a7 1a 0d 64  00 00  00 00`.

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
value bytes are the value. If it is 1, they are a pointer to a value stored
outside the page. This format does not define that pointer or check it: the
layout is the value pointer's, which FMT-07 defines (section 4 of
`value-log-format.md`), and the page stores whatever bytes it is given. A
value may be empty.

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

**P10. An image is 12 to 65,535 bytes long.** Every offset in a page is a
`u16`, which sets the upper bound. The page size is not part of this format:
whoever builds a page passes the largest image length it may use, and the
image is exactly as long as its content. A store with 64 KiB pages gives flat
pages 65,535 of the 65,536 bytes.

**P11. One encoding per content** (B8). The content of a page is its kind,
level, prefix, first child and entries. The layout leaves no freedom: cells
are in slot order with no gap before, between or after them, and no bit is
unused. So an image has no free space, and adding or removing an entry
produces a new image.

**P12. The checksum covers every byte of the image except itself** (C3). It is
verified before any field other than `len` is used (C7).

**P13. A slot number is a position, not a name.** Slot *i* is the entry with
the *i*-th smallest key in this image. Adding or removing a smaller key moves
it. A reference to an entry that must outlive the image is the page id and the
key.

## 4. Reading a page

`FlatPage::decode` reads the image at the start of a byte string and ignores
what follows, so it can be given a whole frame. It checks, in this order:

1. at least 8 bytes: else `Truncated`
2. `len` is at least 12: else `FlatPage(ImageTooShort)`
3. at least `len` bytes: else `Truncated`
4. the checksum: else `ChecksumMismatch`
5. the page kind: else `UnknownRecordKind`
6. the level is 0 in a leaf and not 0 in a branch: else
   `FlatPage(InvalidLevel)`
7. in a branch, `len` is at least 16: else `FlatPage(ImageTooShort)`
8. in a branch, the first child is not 0: else `MissingId`
9. the prefix and the slot directory lie inside the image: else
   `FlatPage(DirectoryOutOfBounds)`
10. each slot, in order:
    1. its cell starts where the previous one ended, does not end before it
       starts or after `len`, and is long enough for `key_len` (leaf) or a
       child id (branch): else `FlatPage(InvalidSlot)`
    2. leaf: the reserved flag bits are 0: else `NonZeroReserved`
    3. branch: the child is not 0: else `MissingId`
    4. its key is greater than the previous slot's: else
       `FlatPage(KeysOutOfOrder)`
11. the last cell ends at `len`: else `FlatPage(UnusedBytes)`

A run of 8 or more zero bytes fails at step 2, and a shorter one at step 1.

`len` is limited by the bytes given and by its own 16 bits. The page size is
the store's to enforce: it gives the reader a frame of one page, or compares
the image length with its page size.

Step 10 costs one key comparison per entry. A page that sits in a buffer-pool
frame was checked when it was loaded, so `FlatPage::from_validated` borrows it
again in constant time. It runs steps 1 to 3 and 5 to 9 only. Its accessors
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
| Header | 12 | 16 |
| Each entry, on top of its key suffix and value | 5 | 6 |

An image is its header, the prefix, and for each entry the overhead above plus
the key suffix and, in a leaf, the value bytes. `FlatLeaf::entry_len` and
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
5. **A flag for out-of-line values, with the pointer left opaque** (P5). Flat
   values can be larger than a page (bulk-append chunks, for example), so the
   leaf needs to say which kind of bytes it holds. The pointer's layout is
   FMT-07's (section 4 of `value-log-format.md`), so that tree and flat
   cells use the same one.
6. **Offsets are 16 bits, so an image is at most 65,535 bytes** (P10). The
   page size itself is not fixed here: the builders take it as a parameter,
   and any value up to 65,535 that has room for the header and the prefix
   works. FMT-02 sets the page size for tree
   pages; if flat pages share it, nothing here changes.
7. **Slot numbers in a flat page are positions** (P13), so no `SlotId` type
   was needed and none was added to `lignin-format::ids`. The positions fit in
   16 bits, as the owner decided for slots. A back pointer from the value log
   to a flat leaf (FMT-07, STORE-09) cannot be a page id and a slot: it has to
   be a page id and a key, or the cleaner has to look for the pointer in the
   page. FMT-07 chose the second (section 5 of `value-log-format.md`).
8. **The first byte of an image is a page kind code**, with 1 and 2 taken
   here. If FMT-02 puts its own kind code in the first byte of a tree page,
   with a different number, any page can be told apart from its first byte.
9. **A key must fit in a page.** There are no overflow keys. RocksDB puts no
   limit on an aux or meta key, so TREE-07 has to set one or handle long keys
   above this format.
10. **Flat-page errors are one `FormatError` variant**, `FlatPage`, wrapping
    `flat_page::FlatPageError`. The shared errors (`Truncated`,
    `ChecksumMismatch`, `UnknownRecordKind`, `MissingId`, `NonZeroReserved`)
    are used where they apply.
11. **The reader takes the format version as a parameter**, since a page has
    none of its own (R5): `FlatPage::decode(bytes, version)`. The builders
    write the current version.
12. **The allocation test implements `GlobalAlloc`, which needs `unsafe`.** It
    is in the integration test `lignin-format/tests/flat_page_alloc.rs`, not
    in the crate, which still forbids unsafe code.

## 8. Code and tests

All of this is in `lignin-format::flat_page`. Run the tests with
`cargo test -p lignin-format`.

| Rule | Code | Tests |
| --- | --- | --- |
| Layout (section 2) | `FlatLeafBuilder`, `FlatBranchBuilder`, `FlatPage::decode` | Golden bytes for a leaf, a branch, the empty leaf and a branch with one child; the kind codes, header lengths and entry overheads. |
| Round trip (P11, B8) | the builders, `FlatLeaf`, `FlatBranch` | 2,000 random pages: content to bytes to content, and bytes to content to bytes. Images whose bytes were changed and whose checksum was then recomputed decode only to content that encodes back to the same bytes. |
| P1 to P4, section 4.1 | `FlatLeaf::search`, `FlatLeaf::get`, `FlatKey` | Search agrees with a sorted model for keys in the page, next to them, shorter than the prefix and outside it. Keys compare by their bytes wherever the prefix ends. |
| P6 | `FlatBranch::child_index`, `FlatBranch::child_for` | Routing agrees with a model for the same probes. |
| No allocation | `FlatPage::decode`, `FlatPage::from_validated`, every accessor | `tests/flat_page_alloc.rs` runs under a counting allocator: decoding, 806 searches over a 400-entry leaf, iteration in both directions and routing through a branch allocate nothing. |
| Section 4 | `FlatPage::decode`, `FlatPageError` | Each step is rejected with its error, in order; every shorter input; every single-bit flip; zero-filled bytes. |
| B10 | `FlatPage::decode`, `FlatPage::from_validated` | 200,000 arbitrary byte strings, most of them given a plausible header and a valid checksum, and every damaged image above, go through every accessor without a panic. Iteration ends at the first slot that describes no cell. |
| P10, sections 5 and 6 | the builders | Capacity and prefix limits; keys outside the prefix or out of order; a full page refuses an entry and is unchanged; the size formula; an image of exactly 65,535 bytes; pages rebuilt from borrowed keys under four different prefixes equal the pages built from whole keys. |

The golden bytes were computed with an independent bit-at-a-time CRC-32C, not
with the crate under test.
