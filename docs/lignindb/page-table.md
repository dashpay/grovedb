# LigninDB page table and its checkpoint

This is the specification for task STORE-03 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines:

- the **page table**: what it maps a page id to, and the rules on changing
  that (section 2)
- the **checkpoint file**, `table/<n>.pt`: the bytes that hold a copy of the
  table (section 3)
- what it means for a checkpoint to **cover** a commit version, how a
  checkpoint is written and named, and what a crash leaves (section 4)

It builds on [format.md](format.md) (FMT-01),
[segment-manifest.md](segment-manifest.md) (FMT-06) and
[transactions.md](transactions.md) (TXN-01), and uses their rule numbers. The
code is `lignin-format::table` for the bytes and `lignin-store::table` for
everything else. Section 5 gives the sizes, section 6 lists the choices made
here that the plan left open, and section 7 maps each rule to its tests.

## 1. Scope

This document fixes what the page table holds and the bytes of its
checkpoint. It does not say:

- what a block holds. A base image is defined by FMT-02 and
  [flat-page.md](flat-page.md), a delta run by FMT-03.
- when a page is written back, when a chain is consolidated, or when a
  checkpoint is taken (STORE-06), and how a block is read and applied
  (STORE-05).
- which page ids are in use (STORE-10), or which version a reader may still
  ask for (TXN-02). The table is told.
- how the log is replayed (STORE-07). Section 4 says what replay can rely on.

The table knows no keys, no trees and no page contents, and it reads no
block.

## 2. The page table

### 2.1 Chains

**T1. The table maps a page id to a chain of links.** A chain is a list of
links, from the oldest to the newest. A page with no link has no entry.

**T2. A link has a commit version and is one of three kinds.**

| Kind | Points to | Says |
| --- | --- | --- |
| base | a block | The block holds the whole page as the link's version left it. |
| delta | a block | The block holds the run of deltas that brings the page from the link before it to the link's version. |
| free | nothing | The commit of the link's version freed the page. |

A block is a segment block, named by its segment's file id, the offset it
starts at and its length in the file, frame included
(segment-manifest.md S15).

**T3. A link's version is a born version** (transactions.md PV1, PV5). The
chain up to and including a link of version *v* gives the page as commit *v*
left it: every change of every commit up to *v* is in it. A delta run may
hold the deltas of several commits (FL3); its link has the version of the
last one.

**T4. The order of a chain.** Every change to a chain keeps these, and a
change that would break one is refused:

- The versions never go down along the chain. Two links may have the same
  version: a base image that consolidates the chain up to a link has that
  link's version, and so has a delta for a change that took no version
  (PV6).
- A delta follows a base or a delta.
- A free link is followed by a base, or by nothing. It may be the first link
  of a chain.

**T5. What a reader at version *s* is given** (PV1). Take the newest link
whose version is not above *s*. If there is none, or it is a free link, the
page has no version at *s*. Otherwise the page version is that link and the
links before it back to the nearest base: the base image, and the delta runs
to apply to it in order. It was born at that newest link's version. Among
links of one version the later one in the chain is the newer.

**T6. The table holds what was written back.** A link is added when its
block is in a segment. A change that only the log and the buffer pool hold
has no link. So T5 gives the newest page version at or before *s* that is in
a segment, and the buffer pool is what knows whether a newer one exists.

### 2.2 Changes

**T7. Push** adds a link as the newest of its chain. Its version must not be
below the version of the chain's newest link (T4).

**T8. Free** is the push of a free link, by the commit that frees the page.
A page that is already freed is not freed again.

**T9. Consolidate** adds a base image in the middle of a chain: right after
the link it was made through, with that link's version. The link is named by
its block's location, not by its version, because several links can share a
version. Links that were pushed while the image was being written stay in
front of it, and still apply to it. If no link points to that location any
more, the change is refused, and the image is dead space.

**T10. Relocate** points one link at another block, which holds a copy of
the same bytes. The link is named by its block's location. If no link points
there any more, the change is refused and the copy is dead space: a block
that was superseded and trimmed is never brought back (CL4). A block that
was superseded and is still in its chain is still live (CL1), and is
relocated like any other.

**T11. Trim** removes from one chain the links that no reader at a version
*h* or later needs (PV3, the simple test), and reports the removed links
that point to a block. Find the newest link at or below *h* that is a base
or a free link:

- If it is a base, every link older than it goes.
- If it is a free link, it goes with every link older than it. A page that
  is left with no link has no entry, and its id can be used again (ID2).
- If there is none, nothing goes.

Trim never changes what a reader at *h* or later is given. Whoever calls it
knows *h*, the oldest version a reader can ask for, and retires the blocks
it reports (PN6).

**T12. Discard** removes every link with a version above *r*, from every
chain. It is for a recovery that lands on *r* (K4).

**T13. The table trusts its callers on contents.** It cannot check that a
base image holds what its chain gives, that a delta run holds every change
since the link before it, or that a copy is a copy.

### 2.3 Lookup

**T14. Lookup is one indexed read.** The newest link of page *p* is slot
*p* − 1 of an array. No key is hashed or compared, and the cost does not
depend on the number of pages. The array is in chunks of 4096 slots, each
allocated when a page in it first gets a link, so the table grows without
moving a link.

**T15. Older links are in an arena.** Each link names the next older link
of its chain. Reading a page version follows that list from the newest link
to the base, one read for each link. The arena reuses the places of removed
links.

**T16. Every operation is one step under the table's own lock** (LT7, level
5). A read never sees half of a change. The lock is held for no file
operation and no call into other code.

## 3. The checkpoint file

### 3.1 File layout

```text
file header     20 bytes: kind 5 (page table), sequence = the file id
links record    zero or more
end record      one, the last 33 bytes
```

**P1. A checkpoint is its header, then links records, then one end
record.** It is written once, whole, and never appended to (B11).

**P2. A checkpoint does not say whether it is in force.** The manifest
does: it names one checkpoint by file id, with the file's valid length and
the commit version it covers (segment-manifest.md M9). The valid length is
the whole file as it was written.

**P3. A reader checks the header against the name**: it must decode (H4),
its kind must be page table (`TableError::NotAPageTable`) and its sequence
must be the id in the file's name (`TableError::WrongFileId`).

### 3.2 Records

Every record is in the default frame (format.md 5.1), and its payload starts
with one byte, the record kind.

| Kind | Record | Payload after the kind byte |
| --- | --- | --- |
| 1 | links | 28 bytes for each link |
| 2 | end | 24 bytes |

**P4. Kind 0 and every other code is an error** (`UnknownRecordKind`, R6).
A kind that stands where it is not allowed is
`TableError::UnexpectedRecordKind`.

**P5. A links record holds 1 to 4096 links** (`LINKS_PER_RECORD`). So its
payload is at most 114,689 bytes, and a reader never needs more than one
record in memory.

**P6. Every links record except the last holds exactly 4096 links.** A
links record with fewer is followed by the end record. So a list of links
has one way of being split into records (B8), and a missing record does not
go unnoticed.

### 3.3 Links

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | page id (never 0) |
| 4 | 4 | a `u32`: bits 0 to 27 are the block's length in bytes, bits 28 and 29 the link kind, bits 30 and 31 reserved, 0 (B7) |
| 8 | 4 | file id of the block's segment, or 0 in a free link |
| 12 | 8 | file offset of the block, `u64`, or 0 in a free link |
| 20 | 8 | commit version (never "no version") |

**P7. Link kinds.** 1 base, 2 delta, 3 free. 0 is invalid
(`TableError::UnknownLinkKind`), so zero-filled bytes are not a link.

**P8. A base or a delta names a block that can exist**
(`TableError::InvalidLink`): its length is from 8, a frame with an empty
body, to 2^27 + 4, the longest block; it starts at or after offset 20, where
the first block of a segment starts; and it ends at or before 2^64 − 1.
Whether the block is really there is not for this format to say (K6).

**P9. A free link names no block**: its length, file id and offset are 0
(`TableError::InvalidLink`).

**P10. Links are in ascending order of page id**, and the links of one page
are in the order of its chain, oldest first, one after another
(`TableError::LinksOutOfOrder`). The order runs across record boundaries.

**P11. The links of one page form a chain** as T4 has it: versions do not
go down (`TableError::LinksOutOfOrder`), a chain does not start with a
delta, and after a free link comes a base or the next page
(`TableError::InvalidChain`).

A page with no link is not in the file.

### 3.4 The end record

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | **covers**: the commit version the checkpoint covers (K1), never "no version" |
| 8 | 8 | **reaches**: the newest version of any link in the file, or "no version" if it has no link |
| 16 | 8 | the number of links in the file, `u64` |

**P12. The end record agrees with the links.** The count is the number of
links in the links records (`TableError::LinkCountMismatch`), and "reaches"
is their newest version (`TableError::ReachMismatch`).

**P13. Nothing follows the end record** (`TableError::LengthMismatch`).

### 3.5 Order of checks

A reader checks a record in this order:

1. 8 bytes are there: else `Truncated`
2. the payload is at most 114,689 bytes: else `RecordTooLong`
3. the payload is there: else `Truncated`
4. the checksum: else `ChecksumMismatch`
5. the kind, and that it may stand here
6. the length of the body, then each link in order: its fields, then its
   place after the link before it
7. for the end record, its totals

The kind comes after the checksum so that a flipped bit in it is reported as
damage (C7). Every rule of this section is checked, so a checkpoint has
exactly one encoding (B8).

### 3.6 Examples

A checkpoint with no link that covers version 0, `table/00000001.pt`, is 53
bytes:

```text
4c 49 47 4e 49 4e 44 42  05 00  01 00  01 00 00 00  09 a9 71 90   file header
19 00 00 00  93 b5 43 9d                                          record: 25 bytes
02                                                                end
00 00 00 00 00 00 00 00                                           covers 0
ff ff ff ff ff ff ff ff                                           reaches nothing
00 00 00 00 00 00 00 00                                           0 links
```

`table/00000122.pt` covers version 41. Page 7 has a base image of version 40,
4104 bytes at offset 20 of segment 310, and a delta run of version 41, 56
bytes at offset 4124. Page 9 was freed at version 38. It is 146 bytes:

```text
4c 49 47 4e 49 4e 44 42  05 00  01 00  7a 00 00 00  0b a8 75 6f   file header
55 00 00 00  45 94 b5 67                                          record: 85 bytes
01                                                                links
07 00 00 00  08 10 00 10  36 01 00 00  14 00 00 00 00 00 00 00  28 00 00 00 00 00 00 00
07 00 00 00  38 00 00 20  36 01 00 00  1c 10 00 00 00 00 00 00  29 00 00 00 00 00 00 00
09 00 00 00  00 00 00 30  00 00 00 00  00 00 00 00 00 00 00 00  26 00 00 00 00 00 00 00
19 00 00 00  cc d3 2e ea                                          record: 25 bytes
02  29 00 00 00 00 00 00 00  29 00 00 00 00 00 00 00  03 00 00 00 00 00 00 00
```

## 4. Covering, and the life of a checkpoint

### 4.1 What a checkpoint covers

**K1. A checkpoint that covers version *C* is enough, with the log after
*C*, to rebuild every page.** Every change that a commit up to *C* made to a
page is in a block that a link of the checkpoint points to. So:

- recovery loads the checkpoint and replays the log from the group of
  *C* + 1 (STORE-07);
- no log group at or before *C* is needed again, which is what FL6 asks
  before a log file is deleted;
- the page ids in use at *C* are the pages whose newest link at or below
  *C* is not a free link.

The table cannot check this. Whoever takes the checkpoint passes *C*, and
vouches that write-back has reached it.

**K2. A checkpoint is a copy of every chain, not a cut at *C*.** It holds
the chains as the table had them, which can include links of versions above
*C*: blocks that were written back before the checkpoint was taken. The end
record's "reaches" is the newest of them.

**K3. Replay skips what a page already has.** A link of version *v* says its
chain holds every change up to *v* (T3). So replay applies to a page only
the changes of versions above the page's newest link. Without this rule, a
change that is both in a block and in the log would be applied twice.

**K4. Nothing newer than the log survives** (FL4, DU3). Recovery can land on
a version *r* below "reaches" only if the log was not durable through
"reaches" when the manifest named the checkpoint. The plan leaves the choice
to STORE-06 and STORE-07, and the table serves both:

- *The log goes first.* The log is made durable through "reaches" after the
  checkpoint is written and before the manifest names it. Then *r* is never
  below "reaches".
- *Recovery discards.* The links above *r* are removed (T12). That gives the
  table of *r* only if the links they replaced are still in their chains, so
  trim (T11) must not have run past the version the log was durable through.

**K5. A checkpoint is taken while the table changes.** Each chain is copied
in one step (T16), and different chains at different moments. This is sound
because of what the changes of section 2.2 do to one chain: after any of
them the chain still holds every change up to its newest version, and that
version has not gone down. So each copied chain holds every change up to *C*,
whichever moment it was copied at.

**K6. A checkpoint points only to blocks its manifest vouches for.** A
manifest that names a checkpoint also names, for every block a link of the
checkpoint points to, the block's segment, with a valid length at or past
the block's end. This is the segment store's rule for a location stored in
another file, and it is checked twice: before a manifest that names the
checkpoint is installed, against that manifest, and when the checkpoint is
loaded (`PageTableError::NotCovered`). A segment that a checkpoint in force
points into is therefore not deleted, and not named shorter, until another
checkpoint is in force.

**K7. "Covers" never goes down.** A checkpoint is not installed if it
covers an earlier version than the one in force
(`PageTableError::CoversWentBack`): log files at or before the version in
force may already be gone.

### 4.2 Writing and naming

| Step | What happens |
| --- | --- |
| 1 | The file `table/<id>.pt` is created under an id above every id used before, with its header, and the file and its name are made durable. |
| 2 | The links are appended, a record at a time, and then the end record. |
| 3 | The file is synced. |
| 4 | Every segment is synced. |
| 5 | The manifest is replaced by one that names the checkpoint, and the segments with the lengths that are now durable (K6). |
| 6 | The file of the checkpoint that was named before is deleted. |

Steps 1 to 3 are `TableCheckpoints::write`, and steps 4 to 6 are
`TableCheckpoints::install`. Between them the caller can make the log
durable (K4).

**K8. A checkpoint id is never given to two files that a manifest named.**
The manifest records the last id it named (M7). An id whose file was written
and never named can be used again by a later session, which first removes
that file.

**K9. A session that opens the directory removes every file in `table/`
with a checkpoint's name that the manifest does not name.** Such a file was
written and not named, or replaced and not yet deleted. Other entries are
left alone.

### 4.3 After a crash

Given the file layer's contract and the segment store's, a crash at any
point leaves:

| Crash | Checkpoint in force afterwards |
| --- | --- |
| before the rename in step 5 | The old one, with its file and its blocks. The new file may exist, with any content. |
| from that rename until step 5 returns | The old one or the new one. The new file was synced in step 3 and the blocks in step 4, so the new one is whole. |
| after step 5 | The new one. The old file may still exist. |

In every case the checkpoint the manifest names loads, and every block it
points to is in a segment the manifest names, below its valid length, with
the bytes that were appended.

### 4.4 When the file layer fails

| Failed | Left behind |
| --- | --- |
| Steps 1 to 3 | No new checkpoint: the file is removed. Its id is not used again in this session. |
| Step 4, or step 5 before its rename | The old manifest and its checkpoint, in force. The new checkpoint can be installed again or discarded. |
| Step 5, in its rename or the sync of the directory | The old manifest or the new one. Neither checkpoint file is deleted until the manifest has been opened again. |
| Step 6 | The new manifest, in force, and the old file, which the next call or the next session deletes. |

## 5. Sizes

| Quantity | Value |
| --- | --- |
| A link in memory | 28 bytes |
| A link in a checkpoint | 28 bytes |
| A chunk of the array, and of the arena | 4096 links, 114,688 bytes |
| A checkpoint | 53 bytes, plus 28 for each link, plus 9 for each links record |
| Longest record of a checkpoint | 114,697 bytes |
| Table of one million pages, one link each, in memory | 28,102,760 bytes |
| Its checkpoint | 28,002,258 bytes |
| Most links in the arena | 2^32 − 1 |

The plan's acceptance test is that the table for one million pages fits in
32 MB. Both the table in memory and its checkpoint file do, with one link
for each page, which is a page that has been consolidated. Every further
link, a delta run or an older link kept for a reader, is 28 bytes more in
each. One million pages with 1.1 million links are 30,970,472 bytes in
memory, and 32,000,000 bytes hold one million pages with 139,187 further
links.

## 6. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one before STORE-05, STORE-06, STORE-07, STORE-08 and
STORE-10 build on it.

1. **A link is 28 bytes, not 32, and a chain longer than one link uses
   more** (section 5). format.md 3.1 had assumed 32 bytes a page. The
   acceptance test holds for one link a page, and with room for 14% more
   links. It does not hold for a table in which every page has a delta: that
   is 56 MB. A layout that keeps short chains inside a fixed entry would
   make every page pay for the room.
2. **The table holds locations in segments only** (T6). A page version that
   is only in the log has no link, and the buffer pool keeps it. The
   alternative, links that point into the log, would let such a page be
   evicted and read back from the log, and would tie the table to the log's
   format.
3. **A freed page is a link** (T2, T8). The store has to keep the old chain
   for the readers below the freeing version, and a checkpoint has to say
   that the page is free, or a crash would bring it back as a page in use.
   STORE-10 keeps the list of ids that are free; the table only records the
   commit.
4. **Links of one version are allowed** (T4), so a consolidation is named by
   the location of the last link it includes (T9), and a relocation by the
   location it moves (T10).
5. **Trim uses the simple test of PV3** (T11): one oldest version, not the
   set of pinned versions. A link between two pins that no pin needs stays
   until the older pin goes.
6. **The table has one lock, and its changes take `&self`** (T16). It is
   read on a buffer pool miss and written once for each block that is
   written back, so one lock is not on the path of a cached read. Sharding
   it can come later without a change to its interface.
7. **What "covers" means** (K1), and that a checkpoint can reach past it
   (K2), with every link carrying its version (K3). The alternative, a
   checkpoint cut at *C*, would need the table to stop changing while it is
   written, or a second copy of it.
8. **A checkpoint is copied chain by chain while the table changes** (K5),
   and written a record at a time. Its totals are therefore in an end
   record, not in a header.
9. **Whether the log goes first or recovery discards is left to STORE-06
   and STORE-07** (K4), as the plan has it. The checkpoint's "reaches" and
   `PageTable::discard_after` are there for either.
10. **The manifest must vouch for every block of the checkpoint it names**
    (K6), checked at installation and at load. The segment store states the
    rule; here it is enforced.
11. **"Covers" never goes down** (K7).
12. **The record size is fixed at 4096 links** (P6), so that a checkpoint
    has one encoding.
13. **Links are fixed-width, 28 bytes** (3.3). Variable-length integers
    would make a checkpoint about a third smaller and are ruled out for
    headers by B1; they were not used in the body either, to keep the
    decoder trivial.
14. **A checkpoint does not list the page ids that are free.** A page with
    no link is not in the file, and the highest page id in it is the highest
    with a link. STORE-10 derives its free list from that and from replay.
15. **Errors are one `FormatError` variant**, `Table`, wrapping
    `table::TableError`, as FMT-04, FMT-06 and FMT-07 did. In the store,
    `LinkError` is a refused change to a chain and `PageTableError`
    everything about a checkpoint.
16. **format.md 3.1 was corrected**: it said 32 bytes a page and 128 GiB for
    2^32 pages; it now says 28 bytes and 112 GiB.

## 7. Code and tests

Run the tests with `cargo test -p lignin-format table` and
`cargo test -p lignin-store table`.

| Rule | Code | Tests |
| --- | --- | --- |
| T1 to T8 | `table::PageTable::{push, read, newest, chain}` | What a reader is given at every version; each refused link, by its error; a freed page and its reuse. A drawn workload of 36,000 steps, with refused ones, against a model that keeps each chain as a list. |
| T9, T10 | `PageTable::{consolidate, relocate}` | An image through the newest link and through an older one; a later delta of the same version is kept; a location that is gone. |
| T11, T12 | `PageTable::{trim, discard_after}` | Every oldest version of a chain with two base images; a freed page before and after its freeing version; the arena reuses what a trim frees. |
| T14, T15 | `table::chains` | **Acceptance test:** the newest link is one node read, and a page version one read for each of its links, in tables of 1 to 1,000,000 pages and for any page id. |
| T16 | `PageTable` | Readers on three threads see whole chains while a writer pushes, relocates and trims. |
| Section 5 | `PageTable::memory_usage`, `TableCheckpoints::write` | **Acceptance test** (`tests/page_table.rs`): one million pages are at most 32,000,000 bytes in memory and exactly 28,002,258 in the checkpoint file, which loads back; so are one million pages with 1.1 million links, in memory. |
| P1 to P6 | `lignin_format::table::{Encoder, Decoder, encode, decode}` | Golden bytes; round trip in both directions; records of 4095, 4096, 4097 and 8192 links; misplaced, empty and oversized records; every shorter and longer input; every single-bit flip; zero-filled bytes. |
| P7 to P11 | `lignin_format::table::{Link, LinkKind, BlockPointer}` | A forged link for each rule, with a valid checksum; each order and chain that is refused, also across records; every chain that is allowed. |
| P12, P13 | `lignin_format::table::End` | Each total wrong; bytes after the end record. |
| B8, B10 | both decoders | 40,000 forged files with valid checksums decode only to what encodes back to them; arbitrary and mutated bytes give an error, never a panic. |
| K1 to K3 | `table::{TableCheckpoints, CheckpointInfo}`, `PageTable::load` | What a checkpoint reports; a table that has no checkpoint; an empty one; several records. |
| K5 | `TableCheckpoints::write` | Checkpoints written while another thread pushes: each loaded chain is a chain the page once had. |
| K6, K7 | `TableCheckpoints::install`, `PageTable::load` | A link past a segment's end and one into no segment are not installed; a manifest with a shorter or a missing segment does not load; a checkpoint that covers less is refused. |
| K8, K9 | `TableCheckpoints::{open, discard, manifest_replaced}` | Ids across sessions; an older checkpoint does not replace a newer one; what opening removes and leaves. |
| 4.3 | the whole module | A crash after every operation that can leave something new, in a script of 27 steps: all 138 crash points, every state of each (81,437 plans, 1,169 distinct states); again with writes torn at 16 bytes; three drawn workloads; a database that goes on through 100 crashes. Each state is opened as a session opens it and checked against a model: the checkpoint is the old one or the new one, it loads, every block it points to reads back with the bytes appended, `table/` holds no other checkpoint, and a new checkpoint can be installed. |
| 4.4 | the whole module | `EIO` and `ENOSPC` at every operation of the same script, once and as a full disk, and appends that cannot be taken back: the step fails with the injected error, every crash state and the next session are checked as above, and a session that settles the failure goes on. |

The golden bytes and their checksums were computed with an independent
bit-at-a-time CRC-32C and a separate encoder, not with the crate under test.
