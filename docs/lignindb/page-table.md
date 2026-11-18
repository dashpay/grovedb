# LigninDB page table and its checkpoint

This is the specification for task STORE-03 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines:

- the **page table**: what it maps a page id to, and the rules on changing
  that (section 2)
- the **checkpoint file**, `table/<n>.pt`: the bytes that hold a copy of the
  table, and with it a sample of the value log's counts of live values
  (section 3)
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

The table remembers the newest free link it removes this way (T17).

Trim never changes what a reader at *h* or later is given. Whoever calls it
knows *h*, the oldest version a reader can ask for, and retires the blocks
it reports (PN6).

**T12. Discard** removes every link with a version above *r*, from every
chain. It is for a recovery that lands on *r* (K4).

**T13. The table trusts its callers on contents.** It cannot check that a
base image holds what its chain gives, that a delta run holds every change
since the link before it, or that a copy is a copy.

**T17. The table remembers the newest free link that a trim removed.**
`PageTable::trimmed_free` is the newest version of a free link that T11 has
removed since the table was made or loaded, or nothing if it has removed
none. It never goes down, and discard (T12) does not change it. Once the
free link of a page is gone, and the links before it, nothing in the table
says that the page was ever in use. K10 is what this version is for.

**T18. Once a page has its free link, nothing older of it is written
back.** The commit that frees a page pushes the free link at once (T8),
whatever of the page is still to be written back. A change of an earlier
commit that only the log and the buffer pool hold can then no longer get a
link: a link below the free link is refused (T7, `LinkError::OutOfOrder`).
It does not need one:

- A reader below the freeing version is given those page versions by the
  buffer pool (T6). The buffer pool keeps them, and cannot evict them,
  until no reader can ask for a version below the freeing one, which is the
  test T11 uses for the chain. Then it drops them.
- No recovery needs them. A checkpoint that holds the free link, or that
  was written after the free link was trimmed away, is named only once the
  log is durable through the freeing version (K4, K10), so a recovery from
  it lands where the page is free. A checkpoint that was begun before the
  free link was pushed covers less than those changes (K1), and replay
  takes them from the log.

So whoever writes back (STORE-06) takes such changes out of its queue when
the free link is pushed, counts them as written back when it says which
version a checkpoint covers (K1), and holds no log file back for them
(FL6): what a version pin still needs of them is in the buffer pool, and no
pin survives a crash (DU3).

This is a rule for changes that have no link. What is already written back
of a freed page can still be consolidated (T9) and relocated (T10) while its
links are in the chain: a base image that is added below the free link holds
what its chain gave, and the table takes it. The table does not stop a
caller from consolidating a chain that no reader will ask for again; that is
wasted work, not an error.

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
counts record   zero or more
end record      one, the last 53 bytes
```

**P1. A checkpoint is its header, then links records, then counts records,
then one end record.** It is written once, whole, and never appended to
(B11).

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
| 2 | end | 44 bytes |
| 3 | counts | 24 bytes for each value-log file |

**P4. Kind 0 and every other code is an error** (`UnknownRecordKind`, R6).
A kind that stands where it is not allowed is
`TableError::UnexpectedRecordKind`.

**P5. A links record holds 1 to 4096 links** (`LINKS_PER_RECORD`). So its
payload is at most 114,689 bytes, and a reader never needs more than one
record in memory. The limit holds for a payload that is handed to the
decoder without its frame as well (`RecordTooLong`).

**P6. Every links record except the last holds exactly 4096 links.** A
links record with fewer is followed by a counts record or the end record.
So a list of links has one way of being split into records (B8), and a
missing record does not go unnoticed.

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

### 3.4 Counts

The counts records hold a sample of the value log's count of live values:
one count for each value-log file that has one (K12). What a count means is
the value log's business (`lignin-store::values`, `value-log-format.md` O6
and O7). This format carries it.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | file id of the value-log file (never 0) |
| 4 | 4 | a `u32`: bit 0 says that the count is known to be wrong, bits 1 to 31 are reserved, 0 (B7) |
| 8 | 8 | the number of live values in the file, `u64` |
| 16 | 8 | the bytes those values take in the file, `u64` |

**P14. Counts records come after the links and before the end record.** A
links record never follows a counts record
(`TableError::UnexpectedRecordKind`). A checkpoint of a database with no
value-log file has no counts record.

**P15. A counts record holds 1 to 4096 counts**
(`VALUE_COUNTS_PER_RECORD`), and every counts record except the last holds
exactly 4096. A counts record with fewer is followed by the end record. So
its payload is at most 98,305 bytes, shorter than the longest links record,
and a list of counts has one way of being split into records (B8). A record
with more is `TableError::LengthMismatch`.

**P16. A count names its file, and its reserved bits are 0.** A file id of
0 is `MissingId`, and a reserved bit that is set is `NonZeroReserved`. The
two numbers can be anything: whether a count is one a file can have is for
the value log to say when it takes the sample back.

**P17. Counts are in ascending order of file id, one for a file**
(`TableError::CountsOutOfOrder`). The order runs across record boundaries.
A file with no count is not in the checkpoint.

### 3.5 The end record

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | **covers**: the commit version the checkpoint covers (K1), never "no version" |
| 8 | 8 | **reaches**: the newest version of any link in the file, or "no version" if it has no link |
| 16 | 8 | the number of links in the file, `u64` |
| 24 | 4 | **place**: the file id of the log file the sample of the counts was taken in, or 0 |
| 28 | 8 | the offset in that log file, `u64`, or 0 |
| 36 | 8 | the number of counts in the file, `u64` |

**P12. The end record agrees with the links and the counts.** The count of
links is the number of links in the links records
(`TableError::LinkCountMismatch`), "reaches" is their newest version
(`TableError::ReachMismatch`), and the count of counts is the number of
counts in the counts records (`TableError::ValueCountMismatch`).

**P13. Nothing follows the end record** (`TableError::LengthMismatch`).

**P18. The place is a place in the log, or none**
(`TableError::InvalidLogPlace`). A place is a log file and an offset in it
at or after 20, where the first record of a log file starts: the boundary
between two records, or the end of the file. "None" is file id 0 with
offset 0, and stands for the place before every record of the log. Places
are in the order of the log: by file id, then by offset.

### 3.6 Order of checks

A reader checks a record in this order:

1. 8 bytes are there: else `Truncated`
2. the payload is at most 114,689 bytes: else `RecordTooLong`
3. the payload is there: else `Truncated`
4. the checksum: else `ChecksumMismatch`
5. the kind, and that it may stand here
6. the length of the body, then each link or count in order: its fields,
   then its place after the one before it
7. for the end record, its place in the log, then its totals: the links,
   "reaches", the counts

The kind comes after the checksum so that a flipped bit in it is reported as
damage (C7). Every rule of this section is checked, so a checkpoint has
exactly one encoding (B8).

### 3.7 Examples

A checkpoint with no link and no count that covers version 0,
`table/00000001.pt`, is 73 bytes:

```text
4c 49 47 4e 49 4e 44 42  05 00  01 00  01 00 00 00  09 a9 71 90   file header
2d 00 00 00  9c 72 89 fb                                          record: 45 bytes
02                                                                end
00 00 00 00 00 00 00 00                                           covers 0
ff ff ff ff ff ff ff ff                                           reaches nothing
00 00 00 00 00 00 00 00                                           0 links
00 00 00 00  00 00 00 00 00 00 00 00                              no place in the log
00 00 00 00 00 00 00 00                                           0 counts
```

`table/00000122.pt` covers version 41. Page 7 has a base image of version 40,
4104 bytes at offset 20 of segment 310, and a delta run of version 41, 56
bytes at offset 4124. Page 9 was freed at version 38. The sample of the
counts was taken when the log ended at offset 81,940 of `wal/00000017.log`:
value-log file 5 holds 3 live values that take 4096 bytes, and the count of
file 8 is known to be wrong. It is 223 bytes:

```text
4c 49 47 4e 49 4e 44 42  05 00  01 00  7a 00 00 00  0b a8 75 6f   file header
55 00 00 00  45 94 b5 67                                          record: 85 bytes
01                                                                links
07 00 00 00  08 10 00 10  36 01 00 00  14 00 00 00 00 00 00 00  28 00 00 00 00 00 00 00
07 00 00 00  38 00 00 20  36 01 00 00  1c 10 00 00 00 00 00 00  29 00 00 00 00 00 00 00
09 00 00 00  00 00 00 30  00 00 00 00  00 00 00 00 00 00 00 00  26 00 00 00 00 00 00 00
31 00 00 00  8d bb b4 28                                          record: 49 bytes
03                                                                counts
05 00 00 00  00 00 00 00  03 00 00 00 00 00 00 00  00 10 00 00 00 00 00 00
08 00 00 00  01 00 00 00  00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00
2d 00 00 00  60 0a e0 e4                                          record: 45 bytes
02  29 00 00 00 00 00 00 00  29 00 00 00 00 00 00 00  03 00 00 00 00 00 00 00
    11 00 00 00  14 40 01 00 00 00 00 00  02 00 00 00 00 00 00 00
```

## 4. Covering, and the life of a checkpoint

### 4.1 What a checkpoint covers

**K1. A checkpoint that covers version *C* is enough, with the log after
*C*, to rebuild every page that is in use where recovery lands.** Every
change that a commit up to *C* made to a page is in a block that a link of
the checkpoint points to, unless a later commit freed the page (T18, K10).
With K4 and K10, which keep the log ahead of the checkpoint:

- recovery loads the checkpoint and replays the log from the group of
  *C* + 1 (STORE-07);
- no log group at or before *C* is needed again, which is what FL6 asks
  before a log file is deleted;
- a page is in use if it has a link and its newest link is not a free
  link, and replay brings that forward: it allocates and frees the pages
  that the commits after *C* did, but for K3 (STORE-10).

The table cannot check this. Whoever takes the checkpoint passes *C*, and
vouches that write-back has reached it: when the checkpoint is begun, no
change of a commit up to *C* is still to be written back, except to a page
whose free link has been pushed since (T18). A commit that failed (CM9) made
no page version, and has no link.

**K2. A checkpoint is a copy of every chain, not a cut at *C*.** It holds
the chains as the table had them, which can include links of versions above
*C*: blocks that were written back before the checkpoint was taken. The end
record's "reaches" is the newest of them.

**K3. Replay skips what a page already has.** A link of version *v* says its
chain holds every change up to *v* (T3). So replay applies to a page only
the changes of versions above the page's newest link. Without this rule, a
change that is both in a block and in the log would be applied twice.

**K4. The log goes first** (FL4, DU3). A manifest names a checkpoint only
once the log is durable through the newest version the checkpoint covers or
reaches, and through the newest free link it may have lost (K10). That
version is `CheckpointInfo::log_needed`. Whoever installs the checkpoint
says which version the log is durable through, and a checkpoint that needs
more is refused (`PageTableError::LogBehind`). So a recovery never lands on
a version below a link of the checkpoint it loads, and never below the
version the checkpoint covers. The version stated is one that recovery would
reach: the groups up to it are synced, in the part of the log that recovery
reads (CM9).

K1 depends on this. A checkpoint that reaches past *C* may hold, for a
page, only a base image newer than *C*: the links that gave the page at *C*
can have been trimmed (T11), before the checkpoint was written or while it
was. Such a chain still holds every change up to *C*, but it cannot give the
page as a commit between *C* and that image left it. With the log durable
through "reaches", recovery never asks for that.

FL4 first offered a second way, in which recovery removes the page versions
born after the version it lands on, and has dropped it for the reason given
here. The table has the operation (T12), and it is not enough by itself: it
gives the table of that version only if the links that the removed ones
replaced are still in their chains, which nothing here makes sure of. A
recovery that finds a link above the version it lands on has found this rule
broken.

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

A checkpoint that was being written while a segment it points into was
dropped is refused in the same way, and is written again.

**K7. "Covers" never goes down.** A checkpoint is not installed if it
covers an earlier version than the one in force
(`PageTableError::CoversWentBack`): log files at or before the version in
force may already be gone.

**K10. A checkpoint never forgets a page that a crash can bring back.** A
page that was freed at version *w* and then trimmed (T11) has no link left.
A checkpoint that is written after that has no trace of the page, and its
"reaches" does not include *w*. If it were named with the log durable only
below *w*, a crash could lose the commit of *w*, and recovery would land
where the page is in use, with a table that has no chain for it. A commit is
published before it is durable (DU1), and a trim asks only whether a reader
is left, so this needs nothing unusual: a base image at version 1, a free
link at 2, a trim at 2, and a checkpoint that covers 1.

So a checkpoint notes the newest free link that the table has trimmed away
(T17), and K4 asks for the log to be durable through it as well. Then a
recovery from the checkpoint lands at or above the version that freed every
page the checkpoint has lost, and each of them is free there, or has been
made again by a later commit that replay takes.

- The version is noted **after the last chain is copied**, not when the
  write begins. A trim can remove the free link of a page while the
  checkpoint is being written (K5), before that page's chain is copied. A
  version noted first could be older than a free link the copy lacks. Noted
  afterwards it can be newer than the copy needs, and never older.
- **The checkpoint file does not hold it**, and the bytes of section 3 are
  as they were. Once the checkpoint is named, the log is durable through
  that version and stays so, and nothing asks for it again. So a table that
  is loaded starts with no such version (T17), although it lacks the pages
  that earlier sessions freed and trimmed: no crash can bring those back.
  `CheckpointInfo::trimmed_free` is therefore set for a checkpoint that was
  written and empty for one that was loaded.
- It asks for no more of the log where the page still has a link: a free
  link that went from under a newer base image (T11) is older than that
  image, which "reaches" covers.

**K11. Replay can meet a change to a page that the table has no link of.**
Let a checkpoint cover *C* and lack a page that was freed at *w* (K10), with
*C* below *w*. If commits between *C* and *w* changed the page, replay
(K3) finds their changes in the log and no image to apply them to. Such a
change is dropped: the page version it belongs to is not rebuilt, and none
is needed, because the log is durable through *w* and replay goes on to the
commit that frees the page. Recovery can check it: when replay ends, every
page that had a change dropped has been freed, or made again, by a later
commit that was replayed. If one has not, the checkpoint or the log is
damaged. The change that makes a page is not such a change: it needs no
image.

In the two other cases K3 already makes replay skip the old page: when its
free link is still in the checkpoint, and when a link of a new page under
the same id is.

### 4.2 The sample of the value counts

**K12. A checkpoint carries a sample of the value log's counts** (the
owner's decision D-042). The value log keeps, for each of its files, the
count of live values, and that count "is recovered with the pages"
(`value-log-format.md` O7): recovery starts from a sample of the counts and
applies the changes that the log holds after the sample's place. The sample
is stored here so that recovery has one thing to name: the checkpoint the
manifest names is also the sample it uses, and a sample whose checkpoint was
written and never named is never used.

The table does not read the sample, and `lignin-store::table` does not call
the value log. Whoever takes a checkpoint (STORE-06) takes the sample from
the value log, with the place the log had reached, and passes it to
`TableCheckpoints::write`. Whoever recovers (STORE-07) gets it back from
`PageTable::load`, in `CheckpointInfo::sample`, and hands it to the value
log. A database with no value-log file has an empty sample. A manifest that
names no checkpoint has no sample: the counts start empty, and every change
in the log applies.

**K13. The place of the sample is its own.** A checkpoint copies each chain
at another moment (K5), and none of those moments has to be the sample's. A
page that the checkpoint copied may already hold a change whose record is
after the sample's place. Replay skips that change for the page (K3) and
still applies it to the counts: what the counts include is decided by the
place alone. So a recovery reads the log from two points, the group of
*C* + 1 for the pages and the place for the counts, and needs it from the
earlier one on.

**K14. A checkpoint is named only once the log is durable up to the place
of its sample.** Whoever installs the checkpoint says how far the log is
durable, as a commit version (K4) and as a place, and a checkpoint whose
sample was taken after that place is refused
(`PageTableError::LogBehindSample`). Otherwise a crash could leave a log
that ends before the sample was taken, and counts that include changes
recovery does not have. The checkpoint file, and the sample in it, is
durable before it is named (step 3 below). A sample with no place asks
nothing of the log.

**K15. What the table does not check about a sample.**

- That the places of successive checkpoints do not go back. The log has to
  be kept from the place of the checkpoint in force, and a later checkpoint
  with an earlier place would ask for log that may be gone. The manifest
  does not hold the place, so the table could check this only by reading
  the checkpoint in force, and whoever deletes log files is who takes the
  samples. It takes each sample after the one before it.
- What the counts say. The format checks their order and their reserved
  bits (P16, P17), and the value log judges the numbers when it takes the
  sample back.

### 4.3 Writing and naming

| Step | What happens |
| --- | --- |
| 1 | The file `table/<id>.pt` is created under an id above every id used before, with its header, and the file and its name are made durable. |
| 2 | The links are appended, a record at a time, then the counts of the sample, and then the end record. |
| 3 | The file is synced. |
| 4 | The checkpoint is refused unless the log is durable through what it covers, reaches and may have lost (K4, K10), and up to the place of its sample (K14). Every segment is synced. |
| 5 | The manifest is replaced by one that names the checkpoint, and the segments with the lengths that are now durable (K6). |
| 6 | The file of the checkpoint that was named before is deleted. |

Steps 1 to 3 are `TableCheckpoints::write`, and steps 4 to 6 are
`TableCheckpoints::install`. Between them the caller makes the log durable
through `CheckpointInfo::log_needed` (K4). At the end of step 2, when the
last chain has been copied, the newest free link the table has trimmed away
is noted (K10).

**K8. A checkpoint id is never given to two files that a manifest named.**
The manifest records the last id it named (M7). An id whose file was written
and never named can be used again by a later session, which first removes
that file.

**K9. A session that opens the directory removes every file in `table/`
with a checkpoint's name that the manifest does not name.** Such a file was
written and not named, or replaced and not yet deleted. Other entries are
left alone.

### 4.4 After a crash

Given the file layer's contract and the segment store's, a crash at any
point leaves:

| Crash | Checkpoint in force afterwards |
| --- | --- |
| before the rename in step 5 | The old one, with its file and its blocks. The new file may exist, with any content. |
| from that rename until step 5 returns | The old one or the new one. The new file was synced in step 3 and the blocks in step 4, so the new one is whole. |
| after step 5 | The new one. The old file may still exist. |

In every case the checkpoint the manifest names loads, with the sample of
the value counts that was written with it, and every block it points to is
in a segment the manifest names, below its valid length, with the bytes
that were appended. Whichever version recovery lands on, from the
one the log was durable through to the last one in the log (DU2), the
checkpoint and the log after the version it covers give every page that is
in use at that version, and no other (K1, K4, K10).

### 4.5 When the file layer fails

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
| A count in a checkpoint | 24 bytes |
| A checkpoint | 73 bytes, plus 28 for each link and 9 for each links record, plus 24 for each count and 9 for each counts record |
| Longest record of a checkpoint | 114,697 bytes |
| Table of one million pages, one link each, in memory | 28,102,776 bytes |
| Its checkpoint, with no count | 28,002,278 bytes |
| Most links in the arena | 2^32 − 1 |

The plan's acceptance test is that the table for one million pages fits in
32 MB. Both the table in memory and its checkpoint file do, with one link
for each page, which is a page that has been consolidated. Every further
link, a delta run or an older link kept for a reader, is 28 bytes more in
each. One million pages with 1.1 million links are 30,970,488 bytes in
memory, and 32,000,000 bytes hold one million pages with 139,186 further
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
9. **The log goes first, and installing a checkpoint enforces it** (K4).
   FL4 left the choice between this and a recovery that discards newer page
   versions to STORE-06 and STORE-07. The second way is sound only if
   trimming never passes the version the log is durable through, at any
   time, which would hold back reclaiming until the next sync of the log.
   The cost of the first is one sync of the log for each table checkpoint.
   `PageTable::discard_after` stays, for a recovery to find the rule broken.
   No version pin is held while a checkpoint is written: a chain that is
   trimmed, consolidated or freed during the copy is still a chain that K4
   and K10 make safe to recover from.
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
17. **The table remembers the newest free link it trimmed away, and a
    checkpoint is named only once the log is durable through it** (T17,
    K10; the owner's decision D-039). STORE-10 found that a checkpoint could
    otherwise forget a page whose freeing commit a crash can still lose
    (page-alloc.md, choice 9). The ways not taken:
    - A rule on whoever trims (STORE-06, STORE-08): pass the smaller of the
      oldest pin and the version the log is durable through. Every caller
      would have to keep it, and reclaiming would wait for the log.
    - Keeping the free link until the log is durable through it. The table
      would have to be told of every sync of the log.
    - Asking that the checkpoint *cover* the free link, not only that the
      log be durable through it. Replay would then never meet the changes
      of K11, but a checkpoint could not be named while write-back is
      behind the newest trimmed free link, which under a steady load of
      freed pages can be always.
    - Writing the version into the checkpoint. Nothing reads it after the
      checkpoint is named (K10), and the file would change.
    The cost is at most one further sync of the log before a checkpoint is
    named, which K4 asks for anyway when the checkpoint reaches past what
    is durable.
18. **Nothing older is written back once a page has its free link** (T18).
    T8 has the commit push the free link, so that the table says a page is
    free from the commit on (STORE-10 builds on that: page-alloc.md A11,
    R2). The other way, in which write-back pushes the free link in the
    order of versions after everything older, would keep those versions
    evictable, and would leave a freed page looking in use until write-back
    reaches its free. With T18 the buffer pool holds what is not written
    back of a freed page until no reader can ask for it, and FL6 is read
    with the buffer pool's copy counting as a copy.
19. **The sample of the value counts is in counts records before the end
    record, and its place in the log is in the end record** (3.4, 3.5, K12;
    the owner's decision D-042 put the sample in this file). The ways not
    taken:
    - In the end record. The number of value-log files has no small bound,
      and the end record is the last 53 bytes of the file, of one size.
    - Before the links. After them, the records of the links are byte for
      byte what they were, and a sample that is taken late, after the
      chains were copied, can still be written without a change to the
      file.
    - One record of any length. Records of 4096 counts keep a reader to one
      record in memory, as for the links (P5), and the totals are in the
      end record, as theirs are.
    A count is fixed-width, 24 bytes, with 31 reserved bits beside the one
    it uses. The entry is defined in `lignin-format::table`, which knows
    nothing of `lignin-store`: `ValueCount` has the fields of the value
    log's `FileCount`, and whoever passes a sample converts.
20. **A place in the log is a log file id and an offset** (P18). No task
    has defined one yet: FMT-05 owns the log's records and STORE-01 the
    log. The pair is what the registered file kind `wal/<n>.log` gives, and
    its order is the log's as long as log file ids go up, which the
    manifest's rule on ids makes sure of (M7). If FMT-05 defines a place of
    its own, this field takes it. Until it does, the format can check only
    that an offset is not inside the header of its file. That it is the
    boundary of a record is for FMT-05 to define, and for whoever takes the
    sample to keep.
21. **Installing a checkpoint checks the place of its sample against the
    log** (K14), so `TableCheckpoints::install` is told how far the log is
    durable as a version and as a place (`LogDurable`). The value log asks
    for this of "whoever stores a sample"; here it is enforced, as K4 is.
22. **That the place never goes back is left to whoever takes the samples**
    (K15).
23. **The bytes of the checkpoint file changed with the sample**: the end
    record is 53 bytes, not 33, and there is a third record kind. Format
    version 1 is not frozen, and no checkpoint of the earlier layout
    exists outside tests.

## 7. Code and tests

Run the tests with `cargo test -p lignin-format table` and
`cargo test -p lignin-store table`.

| Rule | Code | Tests |
| --- | --- | --- |
| T1 to T8 | `table::PageTable::{push, read, newest, chain}` | What a reader is given at every version; each refused link, by its error; a freed page and its reuse. A drawn workload of 36,000 steps, with refused ones, against a model that keeps each chain as a list. |
| T9, T10 | `PageTable::{consolidate, relocate}` | An image through the newest link and through an older one; a later delta of the same version is kept; a location that is gone. |
| T11, T12 | `PageTable::{trim, discard_after}` | Every oldest version of a chain with two base images; a freed page before and after its freeing version; the arena reuses what a trim frees. |
| T17 | `PageTable::trimmed_free` | Free links trimmed in any order of their versions, with and without a newer link of the page; a trim that removes none; a discard. The drawn workload compares it with the model's. |
| T18 | none: a rule for STORE-06 | The model of 4.4 has commits whose changes stay in the buffer pool, is told that a checkpoint covers them once their page is freed, and never writes them back: every landing version still has every page. |
| T14, T15 | `table::chains` | **Acceptance test:** the newest link is one node read, and a page version one read for each of its links, in tables of 1 to 1,000,000 pages and for any page id. |
| T16 | `PageTable` | Readers on three threads see whole chains while a writer pushes, relocates and trims. |
| Section 5 | `PageTable::memory_usage`, `TableCheckpoints::write` | **Acceptance test** (`tests/page_table.rs`): one million pages are at most 32,000,000 bytes in memory and exactly 28,002,258 in the checkpoint file, which loads back; so are one million pages with 1.1 million links, in memory. |
| P1 to P6 | `lignin_format::table::{Encoder, Decoder, encode, decode}` | Golden bytes; round trip in both directions; records of 4095, 4096, 4097 and 8192 links; misplaced, empty and oversized records; every shorter and longer input; every single-bit flip; zero-filled bytes. |
| P7 to P11 | `lignin_format::table::{Link, LinkKind, BlockPointer}` | A forged link for each rule, with a valid checksum; each order and chain that is refused, also across records; every chain that is allowed. |
| P12, P13 | `lignin_format::table::End` | Each total wrong; bytes after the end record. |
| P14, P15 | `lignin_format::table::{Encoder, Decoder, ValueCounts}` | Counts records of 1, 4095, 4096, 4097 and 8197 counts, with and without links before them; a links record after a counts record, a counts record after one that is not full or after the end record, an empty one, one that is not whole counts and one of 4097; the encoder refuses a link after a count. |
| P16, P17 | `lignin_format::table::ValueCount` | A forged count for each rule, with a valid checksum: file id 0, each reserved bit, counts out of order and two of one file, also across records; any two numbers are taken. |
| P18 | `lignin_format::table::LogPlace` | No place; a place at 20 and at the largest offset; an offset with no file and one inside the file header; the order of places. |
| B8, B10 | both decoders | 40,000 forged files with valid checksums, 30,056 of them with counts, decode only to what encodes back to them; arbitrary and mutated bytes give an error, never a panic. |
| K1 to K3 | `table::{TableCheckpoints, CheckpointInfo}`, `PageTable::load` | What a checkpoint reports; a table that has no checkpoint; an empty one; several records. |
| K5 | `TableCheckpoints::write` | Checkpoints written while another thread pushes: each loaded chain is a chain the page once had. Checkpoints written while another thread pushes, consolidates, trims, frees and reuses: each loaded chain gives a whole page version, a base image and every delta after it, no older than the page had when the write began. |
| K4 | `TableCheckpoints::install` | A checkpoint whose page was trimmed to an image newer than the version it covers is refused until the log is durable through that image's version; so is one that covers more than the log has. |
| K10 | `TableCheckpoints::{write, install}`, `CheckpointInfo::{trimmed_free, log_needed}` | The case STORE-10 found: base image at 1, free link at 2, trim at 2, a checkpoint that covers 1 is refused with the log durable through 1, the checkpoint in force still gives the page at 1, and it is named once the log is durable through 2. What a checkpoint needs of the log when it covers, reaches or lost the newest version, and when the id is used again. What a loaded checkpoint and table say. Checkpoints written while another thread frees pages and trims them away, from the last page down: a checkpoint that lacks a page asks for the log through the commit that freed it, also when that came after the write began. |
| K11 | none: a rule for STORE-07 | The model of 4.4 replays the log on what each crash leaves, drops a change it has no image for, and fails if a page is left that way. |
| K12, K13 | `TableCheckpoints::write`, `PageTable::load`, `CheckpointInfo::sample` | A checkpoint gives back the sample it was written with: none, a few counts, 8195 counts in three records, a place and no count. A sample whose counts are out of order is not written, and its file is removed. In the crash tests of 4.4 every checkpoint is written with a drawn sample, and the sample of the checkpoint each crash leaves in force is the one written with it. |
| K14 | `TableCheckpoints::install`, `LogDurable` | A checkpoint is refused while the log is durable to no place, to an earlier file or to an earlier offset than its sample's, and named at that place and after it; the version is asked for first; a sample with no place asks nothing. |
| K6, K7 | `TableCheckpoints::install`, `PageTable::load` | A link past a segment's end and one into no segment are not installed; a manifest with a shorter or a missing segment does not load; a checkpoint that covers less is refused. |
| K8, K9 | `TableCheckpoints::{open, discard, manifest_replaced}` | Ids across sessions; an older checkpoint does not replace a newer one; what opening removes and leaves. |
| 4.4 | the whole module | A crash after every operation that can leave something new, in a script of 41 steps: all 190 crash points, every state of each (82,723 plans, 1,469 distinct states); again with writes torn at 16 bytes; three drawn workloads; a database that goes on through 100 crashes, 51 of which lose commits. Each state is opened as a session opens it and checked against a model: the checkpoint is the old one or the new one, it loads, every block it points to reads back with the bytes appended, `table/` holds no other checkpoint, and a new checkpoint can be installed. The model keeps the log, the version it is durable through and the commits each block holds, and for every version the crash can land on it works out what the checkpoint and the replayed log give of each page: that must be the page of that version, for a page in use and for a free one. The script frees pages and trims them away while the log is behind, one with a change that was never written back and one whose id is used again, and takes checkpoints that cover no more than the log has: 288 of its recoveries are from a checkpoint that has no link of a page which was in use at the version it covers, and replay drops a change in each. |
| 4.5 | the whole module | `EIO` and `ENOSPC` at every operation of the same script, once and as a full disk, and appends that cannot be taken back: the step fails with the injected error, every crash state and the next session are checked as above, and a session that settles the failure goes on. |

The golden bytes and their checksums were computed with an independent
bit-at-a-time CRC-32C and a separate encoder, not with the crate under test.
