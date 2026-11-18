# LigninDB on-disk format: the log

This is the specification for task FMT-05 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines the files `wal/<n>.log`, the write-ahead log:

- the file, and how its files form one log (sections 2 and 3)
- the bytes of every record (section 4)
- the group of a commit (section 5)
- the values a group carries, and what the 8 bytes of a pending value are
  (section 6)
- the records of background changes (section 7)
- table-checkpoint marks (section 8)
- how a log is read, and where a damaged one ends (section 9)
- the sizes (section 10)

It builds on [`format.md`](format.md) (FMT-01: byte conventions B1 to B11,
the checksum C1 to C7, records R1 to R7, the default frame), on
[`page-delta.md`](page-delta.md) (FMT-03: the run of a tree page), on
[`flat-page.md`](flat-page.md) (FMT-04: the flat page), on
[`value-log-format.md`](value-log-format.md) (FMT-07: the value pointer, the
move rule O5), on [`page-table.md`](page-table.md) (STORE-03: what replay can
rely on, K1 to K15), on [`page-alloc.md`](page-alloc.md) (STORE-10: what
replay reports, R1 to R7) and on [`transactions.md`](transactions.md)
(TXN-01: commit, background changes, write-back, recovery). Rule names such
as C4, K3 and FL7 refer to those documents. The crash model is the file
layer's (`lignin_io::vfs`, IO-01).

It does not say when a log file is started, synced or deleted, or how an
append is made (STORE-01), how a group is put together from a transaction
(TXN-05), when values are placed (STORE-06) or how the log is replayed
(STORE-07). Section 11 says what each of them takes from here.

The code is `lignin-format::wal`. Section 12 lists the choices made here
that the plan left open, and section 13 maps each rule to its code and its
tests.

## 1. Terms

- **Log**: the log files of one database directory, in the order of their
  file ids, each as far as it is valid.
- **Record**: the unit with a checksum (`format.md`, section 5). Every record
  of a log file is in the default frame.
- **Item**: what a reader takes or leaves as a whole: the **group** of one
  commit, the record of one **background change**, or one **checkpoint
  mark**. A group is one record or several. The other two are one record
  each.
- **Part**: a record that holds entries of a group.
- **Entry**: one thing a group says the commit did: the changes to one page,
  a freed page, a value pointer that a cell stopped or started to hold.
- **Run**: the changes to one page. A tree page's run is FMT-03's. A flat
  page's run is defined here (4.7).
- **Place**: a log file and an offset in it, between two items (`LogPlace`).
- **Address**: a position in the log as one number (`LogAddress`).
- **Pending value**: a value that is committed and not yet in the value log
  (FL7). Its cell holds 16 bytes that name no file, 8 of which are this
  document's to define (D-055).

## 2. The file

**LG1. Header.** A log file starts with the 20-byte file header, with file
kind 2 (`Wal`) and the file's id in the sequence field. The id is a
`FileId`: from 1 to 2^32 − 2, never the reserved id (F6 of
`value-log-format.md`; `InvalidFileSequence`).

**LG2. The start record comes next, and only there.** It says where the
file begins in the log (LG7) and which commit it comes after (LG8). It is
25 bytes, so the first item of a file is at offset 45
(`FIRST_ITEM_OFFSET`). A file that does not hold its header and its start
record, whole, holds nothing.

**LG3. Items, back to back.** After the start record come items, one after
another, with nothing between them and no padding. Every byte belongs to
one record (R1).

**LG4. Every record is in the default frame, and its payload starts with
its kind.**

| Code | Record | Payload after the kind byte | Bytes |
| --- | --- | --- | --- |
| 1 | start | base (8), after (8) | 25 |
| 2 | value | owner (4), part (2), value length (4), data | 19 + data |
| 3 | carried value | owner (4), source owner (4), source (16) | 33 |
| 4 | group part | flags (1), version (8), part (4), length of the entries (4), entries | 26 + entries |
| 5 | change | what (1), then the fields of that change | 4.5 |
| 6 | checkpoint mark | covers (8) | 17 |

Kind 0 and every other code is an error (`UnknownRecordKind`, R6). A payload
is at most 2^27 − 4 bytes (`MAX_RECORD_PAYLOAD_LEN`), which is the most the
default frame carries (C5).

**The payload of a record says how long it is**, apart from what the frame
says:

- a start, a carried value, a value change and a mark have one size
- a value record has the bytes that its part of a value of its length has
  (LG18)
- a part states the length of its entries
- a reclaiming step states how many pointers it lists, and the length of
  its run

A record whose frame gives another length is refused, whatever its
checksum says (`WalError::RecordLength`, `WrongValuePartLength`). The
checksum covers the length (C3), but it guards a length less well than it
guards the bytes: it finds every single flipped bit in a record of a known
extent (C5), and a flipped bit in the length changes the extent, so that
only chance, one in 2^32, is left. A log is the one file whose reader takes
a failed record for the end and goes on with what came before, so here a
part that a changed length cut short would be a commit that silently lost
changes. With the length in the payload, no single flipped bit anywhere in
a record goes unseen (section 9.3).

**LG5. No end marker.** A log file that the manifest names as closed ends at
its valid length. One that it names as open ends where a reader finds the
first item that is not whole (M15 of `segment-manifest.md`, DU2). Section 9
says what "whole" is.

## 3. Places and addresses

**LG6. A place is a log file and an offset** (`LogPlace`): the offset at
which an item begins, or the end of the valid part of the file. The offset
is from 45 to 2^63 − 1 (`MAX_LOG_OFFSET`, the largest a file API takes).
Places are in the order of the log: by file id, then by offset. A place is
never inside a group. This is the type a page-table checkpoint stores for
its count sample (LG29), and the one `lignin-store` uses to say how far the
log is durable.

**LG7. An address is the base of its file plus an offset in it**
(`LogAddress`), a `u64` that is never 0. The base of a file is in its start
record: the address of the file's first byte.

- The first log file of a database has base 0 (`LogStart::FIRST`).
- Every later file has the base of the file before it plus that file's
  valid length. So addresses go up along the log, and no two bytes of it
  have the same one.
- The base is at most 2^63 − 1.

`LogStart::address` turns an offset of its file into an address, and
`LogStart::offset_of` an address into an offset. A reader that holds an
address finds its file among the log files in use: it is the one with the
largest base at or below the address. That is a search among a handful of
files by a number, not a lookup by key.

An address names the same byte for as long as its file is kept: a log file
is never appended to by a later session and never rewritten (B11), and a
checkpoint shares the file as a hard link, with the same header and start
record.

**LG8. A file says which commit it comes after.** The start record's
`after` is the version of the last commit group before the file, or 0 if
there is none. The first group of the file has the next version (LG11).

**LG9. A file continues the log only where the one before it stops being
valid.** Let a file be valid up to the length *n*, with *v* the version of
the last group of the log up to there. The next file's start record must
be exactly base + *n* and *v* (`LogStart::next`, `LogStart::check_next`,
`WalError::NotTheNextFile`). If it is not, the log ends with the first of
the two files.

This is torn-tail detection across files. A session can start a new log
file while the end of the old one is not synced. A crash can then keep the
new file and lose the old one's end. The new file was written for the whole
of the old one, and says so: its base is past the end that was lost. A
reader that took its items would skip commits, or apply the record of a
background change whose commit is gone.

Which files a session reads is STORE-01's and STORE-07's: the manifest
names the live files. The format gives them this check.

## 4. Records

All integers are little-endian and unaligned (B1, B2). Each table starts at
the first byte of the payload. The 8 bytes of the frame come before it.

### 4.1 Start

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | kind: 1 |
| 1 | 8 | **base**: the log address of the file's first byte, `u64`, at most 2^63 − 1 |
| 9 | 8 | **after**: the version of the last commit group before the file, never "no version" |

### 4.2 Value

One record of a value that a commit wrote. The layout is the value log's
(section 2 of `value-log-format.md`) behind the kind byte.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | kind: 2 |
| 1 | 4 | owner: the page id of the page whose cell the value belongs to, never 0 |
| 5 | 2 | part, `u16`: the record's place among the value's records, from 0 |
| 7 | 4 | value length, `u32`: the length of the whole value, at least 1 |
| 11 | *n* | data: the value's bytes for this part |

### 4.3 Carried value

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | kind: 3 |
| 1 | 4 | owner: the page the cell was carried to, never 0 |
| 5 | 4 | source owner: the page that the value's record in the value log names, never 0 |
| 9 | 16 | source: the value pointer the cell held before the commit |

### 4.4 Group part and entries

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | kind: 4 |
| 1 | 1 | flags. Bit 0: this is the last part of its group. Bits 1 to 7 are reserved, 0 (B7) |
| 2 | 8 | version: the commit version of the group, never 0 and never "no version" |
| 10 | 4 | part, `u32`: the part's place among the parts of its group, from 0 |
| 14 | 4 | length of the entries in bytes, `u32`: the rest of the payload |
| 18 | | entries, one after another |

An entry is one byte, its kind, and then:

| Code | Entry | After the kind | Bytes |
| --- | --- | --- | --- |
| 1 | tree page | `len` (4), a run of page deltas (`page-delta.md` 2.1) | 5 + run |
| 2 | flat page | `len` (4), a flat run (4.7) | 5 + run |
| 3 | freed page | page id (4) | 5 |
| 4 | released value pointer | the pointer (16) | 17 |
| 5 | acquired value pointer | the pointer (16) | 17 |

### 4.5 Change

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | kind: 5 |
| 1 | 1 | what: 1, 2 or 3 |

**What 1, the value of a tree cell** (48 bytes as a record):

| Offset | Size | Field |
| --- | --- | --- |
| 2 | 4 | page id |
| 6 | 2 | slot of the cell |
| 8 | 16 | **from**: the place the cell held: a value pointer, or a pending value |
| 24 | 16 | **to**: the value pointer the cell holds from the step on |

**What 2, the value of a flat leaf's entry** (46 bytes): the same without
the slot. The entry is the one that holds `from`.

**What 3, reclaim**:

| Offset | Size | Field |
| --- | --- | --- |
| 2 | 1 | flags. Bit 0: the page is freed. Bit 1: a tree run follows. Bit 2: a flat run follows. Bits 3 to 7 are reserved, 0 |
| 3 | 4 | page id |
| 7 | 8 | only if the page is freed: the commit version it is freed with |
| | 2 | `count`: the number of released value pointers |
| | 16 each | the released pointers |
| | 4 | only with bit 1 or 2: `len`, the length of the run, `u32` |
| | `len` | only with bit 1 or 2: the run, which ends the payload |

### 4.6 Checkpoint mark

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | kind: 6 |
| 1 | 8 | covers: the commit version the checkpoint covers, never "no version" |

### 4.7 Flat run

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | page id: the page the records change, never 0 |
| 4 | | one or more records, one after another |

A record is one byte, its kind, and then:

| Code | Record | After the kind | Bytes |
| --- | --- | --- | --- |
| 1 | image | `len` (2), an image of the page | 3 + image |
| 2 | insert an entry | `key_len` (2), key, flags (1), `len` (2), value | 6 + key + value |
| 3 | replace a value | as insert | 6 + key + value |
| 4 | remove | `key_len` (2), key | 3 + key |
| 5 | remove a range | `key_len` (2), key, `count` (2) | 5 + key |
| 6 | insert a child | `key_len` (2), separator, child page id (4) | 7 + separator |
| 7 | set the prefix | `len` (2), prefix | 3 + prefix |

An image is a flat page image (`flat-page.md`) of the page the run names:
the page id it holds at offset 4 is the run's. A key is the whole key, with
the page's prefix. Bit 0 of an entry's flags says that the value is out of
line: its bytes are then the 16 bytes of the value's place, a value pointer
or a pending value. Bits 1 to 7 are reserved, 0.

### 4.8 Examples

**A new log file.** `wal/00000045.log`, the first of its database:

```text
4c 49 47 4e 49 4e 44 42  02 00  01 00  2d 00 00 00  dd e8 5e 0d   file header
11 00 00 00  fe 82 69 e2                                          record: 17 bytes
01                                                                start
00 00 00 00 00 00 00 00                                           base 0
00 00 00 00 00 00 00 00                                           after version 0
```

**A group.** Commit 1 gives the cell in slot 1 of page 7 the new value
`a value too long for its cell` (29 bytes), in place of one of 300 bytes at
offset 20 of `val/00000012.val`. The cell's value hash becomes `55` (32
times). 154 bytes, at offset 45:

```text
28 00 00 00  f4 a9 9e b0                 record: 40 bytes
02                                       value
07 00 00 00                              owner: page 7
00 00                                    part 0
1d 00 00 00                              value length 29
61 20 76 61 6c 75 65 ...                 "a value too long for its cell"

62 00 00 00  99 5b b2 81                 record: 98 bytes
04                                       group part
01                                       the last part
01 00 00 00 00 00 00 00                  version 1
00 00 00 00                              part 0
50 00 00 00                              80 bytes of entries
01  3a 00 00 00                          entry: tree page, a run of 58 bytes
07 00 00 00                              page 7
05  01 00  55 (32 times)                 the value hash of slot 1
15  01 00                                record 21, slot 1: the value's place,
  ff ff ff ff  2d 00 00 00 00 00 00 00  1d 00 00 00    pending, address 45
04                                       entry: a released value pointer,
  0c 00 00 00  14 00 00 00 00 00 00 00  2c 01 00 00    file 12, offset 20
```

The value's first record is at offset 45 of a file with base 0, so its
address is 45. The run puts it into the cell, as the 8 bytes of the pending
value, and until the value is placed the cell holds

```text
ff ff ff ff  2d 00 00 00 00 00 00 00  1d 00 00 00
```

**The record that places it.** Write-back has appended the value at offset
364 of `val/00000013.val`, and gives the cell in slot 1 of page 7 the
pointer. A replay that has applied the group finds the cell holding `from`,
so the step is due (LG28). 48 bytes:

```text
28 00 00 00  1b 29 94 77                 record: 40 bytes
05  01                                   change: the value of a tree cell
07 00 00 00  01 00                       page 7, slot 1
ff ff ff ff  2d 00 00 00 00 00 00 00  1d 00 00 00    from: pending, address 45
0d 00 00 00  6c 01 00 00 00 00 00 00  1d 00 00 00    to: file 13, offset 364
```

**A carried value** for a cell that a commit carried from page 7 to page 9,
whose 300 bytes are at offset 64 of `val/00000013.val`. 33 bytes:

```text
19 00 00 00  05 e0 dc de                 record: 25 bytes
03                                       carried value
09 00 00 00                              owner: page 9
07 00 00 00                              source owner: page 7
0d 00 00 00  40 00 00 00 00 00 00 00  2c 01 00 00    source
```

**A checkpoint mark** for a checkpoint that covers version 2 is the 17 bytes
`09 00 00 00  7d 48 0c 37  06  02 00 00 00 00 00 00 00`.

## 5. The group of a commit

**LG10. One commit, one group, and the group carries the version** (CV1).
A run carries none (PD5): the version a group's runs bring their pages to
is the group's.

**LG11. The versions of the groups go up by one**, along the log and across
its files (`format.md` 3.2, CV6): the first group of a file has the
version after the start record's `after`, and each further one the version
after the group before it. Anything else ends the log
(`WalError::VersionGap`). A background change and a mark take no version.

**LG12. A group is its values, then its parts, with nothing between.** The
values are value records and carried value records (section 6), in the
order the writer gave them. The parts are numbered from 0, each names the
group's version, and the last one says that it is the last. Most groups
have no value and one part.

**LG13. A group is taken whole or not at all.** Nothing of a group counts
until its last part has been read and verified. A log that ends before
that ends before the group's first record (section 9). This is what keeps a
crash from leaving part of a commit (CM5).

**LG14. The entries are in one order.**

1. The pages, in ascending order of page id, each at most once. A page has
   a tree run, a flat run, or is freed.
2. The released value pointers, in ascending order of file id and offset,
   each once.
3. The acquired value pointers, in the same order.

The order runs across the parts. An entry out of its order, or there twice,
is `WalError::EntriesOutOfOrder`. So a set of changes has one list of
entries (B8), and a page that a group both changes and frees, which would
be two things to say of one page, cannot be written.

**LG15. What a group says of pages** is what recovery reports to the page
id allocator (ID4; R3 of `page-alloc.md`):

- A page with a run was written by the commit. A tree run that starts with
  the create record (PD11), and a flat run that starts with an image
  (LG19), say that the page needs no image to apply to.
- A page with a freed entry was freed by the commit. The commit lists every
  page it frees, also one whose changes of earlier commits were never
  written back.

**LG16. What a group says of the value log's counts** is the commit's
change to the counts of live values (DU5, O7): every pointer a cell stopped
holding, and every pointer a cell started to hold. A commit that writes new
values acquires nothing: its values have no pointer yet (FL7). The acquired
entries are for a commit that stores pointers to values which are already
in the value log, under a manifest that covers them, as a bulk load can.

**LG17. A group may have no entry.** A commit that takes a version and
changes no page (ST5) is one part with no entry, 26 bytes. A part with no
entry is the only part of its group (`WalError::EmptyPart`).

**LG18. What is too long for one checksum is cut in one way** (C5).

- *A value* longer than 2^27 − 15 bytes (`MAX_VALUE_PART_LEN`) is several
  value records, each full but for the last, back to back and in order of
  part, as in the value log (V6 to V8). A value of at most 2^32 − 1 bytes
  is at most 33 records.
- *The entries* of a group that do not fit one record are several parts. A
  part is closed when the next entry does not fit into it, and not before
  (`WalError::PartNotFull`). An entry is never cut in two.
- *A run* is at most 2^27 − 27 bytes (`MAX_RUN_LEN`), so that its entry
  fits a part. A longer one is refused (`WalError::RunTooLong`). A run is
  what a commit did to one page. What it leaves of a page is at most a few
  page sizes, and a writer that changed a cell many times in one commit
  writes what the changes come to.
- *A group* has no limit of its own. With 2^32 parts it can hold 2^59
  bytes of entries.

**LG19. A flat page's changes are records of that page, by key.** A flat
run is to a flat page what a run of page deltas is to a tree page: the
changes of one page, applied to the page as it was. It is not a page image
for every change, and it is not a list of puts and deletes of the keyspace.

- *Not an image for every change.* A flat page is rewritten whole in a
  segment (P11 of `flat-page.md`). In the log that would be a page of up to
  64 KiB for a directory entry of 40 bytes, on every commit that moves the
  root of a Merk.
- *Not the keyspace's puts and deletes.* Recovery rebuilds each page by
  itself: a page that a table checkpoint holds at a newer version takes no
  change of an older commit, and the others do (K2, K3). A put that names
  a key and no page would have to be routed through branch pages that can
  be older or newer than the leaves under them. And the store, which
  applies the log, knows no trees (the plan's crate layering).
- *So: the keyspace's put, delete and range delete, as each page they
  touch sees them.* A put is an insert or a replace in one leaf. A delete
  is a remove. A range delete is a remove of a range in each leaf it cuts
  into, and a freed entry for each page that lies inside it. A split is an
  image, or inserts, for the new page, a remove of a range and perhaps a
  new prefix in the old one, and an insert of a child in the parent.
- *An image where it is shorter, or where there is no page.* The first
  record of a run may be an image: the page becomes it, whatever it was.
  It is how a new page is made, with the empty leaf if nothing else, and
  how a page is written whole. The image has no checksum of its own (P12):
  the part's covers it (D-050). It holds its page's id (P14 of
  `flat-page.md`), and that is the page of the run (`WalError::WrongPage`).

`FlatRun::apply` gives the image of the page a run leaves. The page it is
given is the page the run names (`WalError::WrongPage`).

- *Each record is checked against the entries as the records before it
  left them:* an insert of a key that is there, a replace or a remove of
  one that is not, a range past the last entry and a record for the other
  kind of page are refused where they stand.
- *The prefix and the room are checked for the image the run leaves,* by
  the page's own builder: every key starts with the prefix, and the image
  is no longer than the store's page size. Between two records of a run
  there is no page, only its entries. So a run may set the prefix before
  it removes the keys that do not have it, and insert before it removes.

On a run written by a correct writer and applied once, in its place,
nothing is refused (A3 of `page-delta.md`).

## 6. Values

### 6.1 The bytes of a new value

**LG20. A value that a commit writes out of line is in the commit's group,
whole** (FL7, D-042), as value records before the group's parts. Its owner
is the page of the cell it belongs to, as in the value log (O1). Write-back
later appends it to the value log under the same owner.

### 6.2 What the 8 bytes of a pending value are

**LG21. They are the log address of the value's first record**: of a value
record, or of a carried value record (LG23). `LogAddress::of` reads the
address from a pending value, `LogAddress::pending` makes the pending value
of an address, and `GroupWriter::value` and `GroupWriter::carried` return
it for what they wrote.

This is what D-055 asks of them:

- **They lead to the value's bytes with no lookup by key.** The address
  gives the file and the offset (LG7), and the value's length, which the
  cell holds beside it, gives how much to read: `logged_read_len` bytes are
  enough for either record that can stand there. One positional read, as
  for a value in the value log (P3). `read_pending_value` verifies what it
  finds.
- **They are never all zero.** An address is the base of its file plus an
  offset, and the offset of a record is at least 45: the header and the
  start record stand before the first item of every file. So the smallest
  address a value can have is 45. `LogAddress` cannot hold 0, as
  `PendingValue` cannot hold an id of all zeros (P7).
- **They stay resolvable until the value is placed.** The log file that
  holds a pending value is kept until then (FL6, D-046).
- **Recovery reproduces them.** The cell is in the log as the bytes of its
  insert or replace record, with the address in it, and the group is where
  it was: in the same file, behind the same start record. The frame that
  replay builds is the frame the commit installed.

**A reader checks what it finds** (`read_pending_value`): the record at the
address must be of the pending value's length (`WrongValueLength`) and must
name the page of the cell that holds it as its owner (`WrongOwner`), as a
read through a value pointer does (P4). That is what it can check, and no
more: an address that leads to another value of the same page and the same
length is read as that value. The checks catch a cell that was carried
without its value and an address that is off; they do not stand in for the
writer giving each cell the address of its own value, which is the writer's
rule here as O2 is for a value pointer. The reader is handed the bytes that
stand at the address; that they are those bytes is the caller's to see to.

**LG22. A group is written for one place.** The address of a value depends
on where its group lands in the log, and the cells that hold it are in the
same group. So whoever writes a group knows the address of its first byte
before it writes the cells (`GroupWriter::new`). The log is appended to by
commits and by background changes, so the place is known only where
appends are put in order. STORE-01 gives a commit its place and keeps
other appends behind it until the group is in. A group that was written
for one place is not appended at another.

Only a group is written for its place. A background change and a mark hold
no address of their own, so a page step appends its record under the page
step's lock with nothing but the record's bytes (D-059). Two things make
the rule cheap to keep for a group:

- *The length of a group does not depend on its place.* An address is 8
  bytes whatever it is. So the log can set the bytes of a group aside when
  the commit asks for its place, and take the group when it is written,
  while other appends go behind it.
- *Every append that is not a commit's is made under the counts lock*
  (LT7, level 4a, as `transactions.md` stands): the three background
  changes and the mark. A commit that holds that lock from before it asks
  for its place until its group is in, as a commit whose cells stop holding
  pointers already does (CM3), is not overtaken.

Which of the two STORE-01 and TXN-05 use is theirs to choose. Either way:

- *The file is chosen before the place is given.* A group lies in one file
  (R5). The first item of the next file is 45 addresses further on than
  the end of this one, so a group that was written for the end of a file
  is written again if the log turns to a new file first.
- *A place that was given and not used is given back before anything else
  is appended.* Bytes that were set aside and never written would be a
  hole, and nothing behind a hole is part of the log (LG30).

Before its commit a new value is in no log. What its cell holds in the
transaction's private frame is the transaction's own (WT4): any 8 bytes
that are not zero. The commit puts the addresses in.

### 6.3 A carried value: repeated or named

A cell that a commit carries to another page stops holding its value
pointer, and its value is pending again in the new page (O5, FL7). On the
reference workload 0.8 values are carried for each one written
(`tree-layout.md` X4), 368 bytes of values per transition.

**LG23. The log can name a carried value instead of repeating it.** The
carried value record holds the pointer the cell held before the commit, and
the page that the value's record in the value log names as its owner, which
a read through that pointer is checked against (P4). The new cell holds the
address of the carried value record, as it would hold the address of a
value record. A reader goes from the cell to the record, and from the
record to the value log. 33 bytes in place of 19 and the value.

A group can always repeat the value instead, as a value record with the new
owner. That needs no rule beyond those for a new value.

**The source is a value pointer, never a pending value.** A cell that is
carried while a new value of it is still pending gets the bytes again: they
are in memory with the frame (FL7), so no read is needed, and a record that
named another record of the log would make one log file depend on another.
A cell whose pending value is a named one, and that is carried a second
time before the value is placed, is named again by the same pointer.

**A commit releases a pointer only for a cell that held one.** The commit
that carries a cell with a placed value releases the cell's pointer (O5,
O7, FL7), whether it repeats the value or names it. A cell that was pending
held no pointer, so the commit that carries it again releases none. The
pointer that a named value names was released by the first carry, and a
second release would take from its file's count a value that the count
does not hold.

**LG24. The file a carried value names is kept for as long as the pending
value can be read, by anyone and after a crash.** This is the condition
under which naming is safe, and it asks more than FL7's "until the value
has been placed again":

- The commit that carries the cell releases the old pointer, and its group
  says so (LG16). The count of the old file can reach zero at that commit.
- The pending value ends when it is placed (a page step, section 7), or
  when a later commit replaces or deletes its cell.
- Until that end is durable, a crash can land where the cell is pending.
  Replay then rebuilds the cell with the address of the carried value
  record, and the value is where that record says. If the old file was
  deleted meanwhile, the value is lost.

With FL7's wording alone this sequence loses a value: the carry is
committed and made durable; the value is placed, in memory; the old file,
whose count is zero and whose release is durable, is dropped from the
manifest and deleted; a crash loses the record of the placing, which was
never synced; replay finds the cell pending and the file gone.

So whoever keeps value-log files **holds** the file a carried value names:

- from the commit that names it
- until the record that ends the pending value (the placing, or the group
  of the commit that removes the cell) is durable, or a table checkpoint
  that covers the carrying commit is in force
- and for as long as a version pin can read a page version in which the
  cell is pending

**The end of a hold is a retirement, not a deletion** (PN6). While the hold
lasts, an operation that begins now can still reach the file through the
cell. So the file is retired when its last hold ends, and deleted only when
the operations that were running at that moment have ended: one of them
may have read the name and not yet the value. That the file's count was
zero earlier retires nothing, and a wait that began then does not count.

**After a crash no hold is left (FL1), and recovery puts the holds back at
the end of replay, not record by record.** A file is held for each pending
value that a cell still holds once the log is replayed and that leads to a
carried value record. A carried value record whose cell was placed,
replaced or removed later in the log names a file that may rightly be gone
by then: replay neither opens it nor holds it. Until replay has ended and
the holds are back, no value-log file is deleted.

A file that several pending values name, of one cell that was carried twice
or of several cells, is held until the last of them has ended.

A read of a named pending value is not answered from the frame, as FL7
says of a new value: the frame has the name, and the bytes are read from
the value log, with the owner the record gives.

**What is not taken:** keeping the old pointer counted until the placing,
which would then be a move of the value (section 7). The counts would keep
the file with no hold, across a crash too. But the value's record would
name a page that no longer holds its cell, for as long as the value is
pending, and the cleaner reads a count above the owners it can find as a
cell that was carried without its value (O6).

**This document defines the record and the condition. It adds nothing to
`lignin-store`.** Until STORE-06 and STORE-07 keep LG24, a writer repeats
(section 11).

## 7. Background changes

**LG25. A background change is one record, of one page step.** It is in the
same log as the groups, between them (PV6, CM9), and takes no version. The
step writes it, so a step that is refused leaves no record, and the records
that change one page are in the log in the order the steps were made
(PV7). Its bytes say nothing of where it stands in the log, so the step
makes them before it knows that, and appends them as they are (D-059).

Three changes alter the bytes of a page as a handle reads them (PV7), and
each has its record:

| Change | Record | What it does to the counts |
| --- | --- | --- |
| a pending value is placed | value, with `from` a pending value | acquires `to` |
| a value is moved: by the cleaner, or regrouped | value, with `from` a value pointer | releases `from`, acquires `to` |
| what a dropped subtree left is removed | reclaim | releases the pointers it lists |

**LG26. The value record says what the cell held before** (`ValueMove`).
`from` is a place, a value pointer or a pending value, and `to` is a value
pointer: a step never leaves a value pending (`ReservedFileId`). They name
a value of one length and are not the same place
(`WalError::InvalidChange`), when the record is written and when it is
read. Record 21 by itself does not check the length (PD10, PD16 of
`page-delta.md`); the record around it does, and a step is due only while
the cell holds exactly `from`, so a step never changes how long a cell
says its value is. Its change to a tree page is record 21 of
`page-delta.md` with `to` (`ValueMove::delta`). In a flat leaf, which is
the page the record names (`WalError::WrongPage`), the entry that holds
`from` gets `to` (`ValueMove::apply_to_flat_leaf`). No two cells hold one
place (O2), so `from` names the entry, and no key is needed.

**LG27. A reclaiming step removes or frees, and nothing else** (`Reclaim`,
CL6).

- Its run only takes out: remove records in a tree page, removes in a flat
  page. A run that does anything else is `WalError::InvalidChange`.
- It can free its page. The version is the drop's or a later one
  (`page-alloc.md` A4), and is one that a commit before the record has
  (`WalError::FreedAhead`). A step that frees a page whose cells are all
  left-overs need not list them.
- It lists the value pointers that the cells it removed held, in ascending
  order, also where it frees the page and does not list the cells.
- It removes something or frees its page.

**LG28. How replay tells a page that has a background change from one that
lacks it.** A background change has no version (DU5). Let *g* be the
version of the last group before the record in the log.

1. A page whose newest link is of a version above *g* has the change: a
   commit that came after the step is in its chain. The change is skipped.
2. Otherwise the change is made exactly when the page still has what the
   record says it had: the cell holds `from` (`ValueMove::is_due_in_tree`,
   `is_due_in_flat_leaf`), the cell or entry that a reclaiming step takes
   out is there, or the page that it frees with no run is not freed.

The first step is needed. A slot that a removal frees is taken again by a
later commit (S1), so "the cell is there" alone would remove the new cell
from a page that already has both. With the first step the test is asked
only of a page as background work left it since its last commit, and
background work adds no cell and gives no place a second time.

What the record does to the counts is applied by its place alone, whatever
became of its change to the page (DU2, DU5).

**Which background changes replay looks at: every one from the place of
the checkpoint's sample on, for its page and for the counts, and none
before it.** A background change has no version, so where the group of
*C* + 1 stands says nothing about it: a change that stands between the
sample's place and that group is looked at like any other.

- *A change before the place is in the checkpoint.* The sample is taken
  before the checkpoint copies its first chain (LG29), and a page step
  holds the counts lock from its record to its count. So a step whose
  record is before the place was finished before the sample, and every
  chain was copied after it. The sample counts it too.
- *A change after the place is in the chain that was copied, or is not.*
  The test above tells which, and its counts are applied by its place.

Were a sample taken after a chain had been copied, the checkpoint could
hold a page without a change that its sample counts, with the record before
the place, where no replay looks: the page would hold a pointer that the
counts have given up. With no checkpoint in force, replay looks at every
background change of the log.

## 8. Table-checkpoint marks

**LG29. A mark is written when the sample of the counts is taken for a
page-table checkpoint, and the place of the sample is the end of the
mark.** The sample is read under the counts lock, together with its place
in the log (DU5, K12). Whoever takes it appends a mark while it holds that
lock, with the version the checkpoint will cover, and takes the end of the
mark as the place. A mark changes nothing.

It is there so that the place can be checked. The counts recovered from a
sample include the changes after its place and no others, so a place that
is off by one record gives wrong counts with nothing to show it. With the
mark:

- **The checkpoint's decoder** accepts a place only at or after offset 62,
  the end of a mark that is the first item of its file
  (`table::FIRST_SAMPLE_OFFSET`, P18 of `page-table.md`).
- **Recovery** reads the log file of the place from its start, and the item
  that ends at the place must be a mark that covers what the checkpoint
  covers. If it is not, the checkpoint or the log is not the one that was
  written.

A mark covers no version that a commit before it does not have
(`WalError::CheckpointAhead`): the log goes first (K4). A checkpoint that
was written and never named leaves a mark that nothing points to.

Three things about a sample follow from what the log holds. They are for
whoever writes and installs a checkpoint (STORE-06) and whoever keeps the
log's files (STORE-01). `page-table.md` states them as K14, with what
`TableCheckpoints` checks of each:

- **The sample is taken, and its mark written, before the checkpoint
  copies its first chain.** LG28 rests on it: a background change before
  the place is then in every chain.
- **The checkpoint is named only when the log is durable through every
  record whose change one of its chains holds:** it is synced after the
  last chain is copied. A page step can place or move a value after the
  sample and before its page is copied. Its record is then after the
  place, and its change is in the checkpoint. If a crash loses the record,
  the checkpoint holds a pointer that no count includes, and the file can
  be deleted with a live value in it. Durable through the sample's place
  (K14) is not enough, and a background change has no version for K4 to
  speak of.
- **The log file that the place names is kept while the checkpoint is in
  force**, also when the mark is the last item in it and every commit in
  it is covered. Recovery reads it to check the mark, and begins there
  with the background changes (LG28).

## 9. Reading a log, and where it ends

### 9.1 A record

`Record::decode` reads one record by itself and checks, in this order:

1. 8 bytes are there: else `Truncated`
2. the payload is at most 2^27 − 4 bytes: else `RecordTooLong`
3. the whole record is there: else `Truncated`
4. the checksum: else `ChecksumMismatch`
5. the kind: an empty payload is `Wal(RecordLength)`, an unknown code
   `UnknownRecordKind`
6. the fields of its kind, in the order they stand

The kind comes after the checksum, so that a flipped bit in it is damage
(C7). `record_len` gives the length of a record from its first 8 bytes, for
a reader that fetches a file piece by piece.

The fields: a record of one size has that size (`Wal(RecordLength)`); a
page id, a slot and a file id are not 0 (`MissingId`); a version is one
(`MissingId`), and a commit's is not 0 (`Wal(NotACommit)`); reserved bits
are 0 (`NonZeroReserved`); a value pointer is one (`MissingId`,
`ValueLog(InvalidOffset)`, `ValueLog(EmptyValue)`) and is not a pending
value (`ReservedFileId`), which stands only where a cell's place does: in
a cell or an entry of a run, and in the `from` of a value change; a value
record is a
part of a value of its length, with that part's bytes
(`Wal(NoSuchValuePart)`, `Wal(WrongValuePartLength)`); a run is a run
(`DeltaRun::decode`, `FlatRun::decode`); the entries of a part are in
order (LG14), and so on for each rule of sections 5 to 8.

### 9.2 What may follow what

`LogCursor` reads the records of one file in order.

| The cursor is | It takes | Anything else |
| --- | --- | --- |
| between items | a value or carried value record, which begins a group; part 0 of the next version; a change; a mark | `MisplacedStart`, `UnexpectedValuePart`, `UnexpectedPart`, `VersionGap` |
| inside a value | the value's next part, with the same owner and length | `UnfinishedValue`, `UnexpectedValuePart`, `PartOfAnotherValue` |
| among the values of a group | another value or carried value; part 0 of the next version | `GroupInterrupted`, `MisplacedStart`, `UnexpectedValuePart`, `UnexpectedPart`, `VersionGap` |
| among the parts of a group | the next part of the same version | `GroupInterrupted`, `MisplacedStart`, `UnexpectedPart`, `PartOfAnotherGroup`, `PartNotFull`, `EntriesOutOfOrder` |

A value begins with its part 0 (`UnexpectedValuePart`). A part that
follows another is as full as LG18 has it (`PartNotFull`), and its entries
go on in the order of LG14 (`EntriesOutOfOrder`). A part with no entry that
is not the only part of its group is refused when it is read by itself
(`EmptyPart`, 9.1).

### 9.3 The tail

**LG30. A log file is valid up to the end of its last whole item.** `scan`
reads a file and gives its items in order. It stops at the first record
that fails a check of 9.1 or 9.2, and at the end of the bytes. What it has
given by then is the longest prefix of the file that holds whole items
only, and `LogTail` says where that prefix ends (`valid_len`), which
version its last group has, and why the bytes after it, if there are any,
are not part of the log.

- A group whose last part is missing, or any of whose records fails, is not
  given, and the prefix ends before its first record (LG13).
- Nothing after a failed record is read. A later record that is whole does
  not count: the log is its items in order, with no version left out
  (LG11).
- A file that does not begin with the header of a log and a start record
  is refused as a whole: it holds nothing.

This is the acceptance test of the task: a truncated or bit-flipped log
decodes to the longest valid prefix of whole groups.

**LG31. Why that prefix is the right one under the file layer's crash
model.** The model (`lignin_io::vfs`): storage writes a sector whole or not
at all and damages no other byte; of the bytes that were not synced a
crash keeps any part, each where it was written; and bytes under a length
that survived without them read as zeros.

- *A byte of a log file is what was appended there, or zero.* File space
  is never used twice and a file is never appended to by a later session
  (B11). So no old record can stand where a new one is expected, and a
  record needs neither its file nor its offset under its checksum
  (`format.md`, section 5).
- *Zeros are not a record.* The masked checksum of zeros is not zero (C4).
- *A record that lost a sector fails its checksum*, which covers its
  length too (C3), as surely as a checksum of 32 bits can tell (below).
- *A group that lost a record is not whole* (LG13), and a file that lost
  its end does not connect to the next (LG9).
- *What was synced is there.* Appending behind synced bytes does not
  damage them, so the log is not padded to a sector or a block after a
  sync.

So a reader takes exactly the items that are in the file as they were
written, up to the first that is not. Every item that was synced is among
them, which is DU2: recovery lands between the last durable commit and the
last one whose group is in the log.

**What is certain, and what the checksum makes unlikely.** Three kinds of
damage are always found:

- *A file that is cut short,* at any length. The record the cut is in is
  not whole (`Truncated`), and a cut between two records of a group leaves
  the group unfinished.
- *One flipped bit, anywhere in a record.* In the payload or in the
  checksum, the checksum finds it: CRC-32C finds every single flipped bit
  in a record of a known extent (C5). In the length, the payload finds it:
  it says how long it is (LG4).
- *Zeros from the first byte of a record on* (C4).

Everything else is found as surely as a checksum of 32 bits finds it, and
that includes the crash model's own case of a sector that is lost from the
middle of a record. Two byte strings of one length that were not made for
each other have the same checksum once in 2^32 times. `format.md` takes
that for every file: the checksum is for accidents (C6). In the log it
weighs more than elsewhere, because what passes here is the content of a
commit, and what fails is the end of the log and not an error. Two things
follow, and neither is changed here, since the checksum is the plan's
(FMT-01):

- The guarantee is one of chance, not a proof. A value whose bytes were
  chosen for it can be made to have, with a sector of it read as zeros,
  the checksum it had whole. That takes knowing to the byte where the
  value will lie in its log file and where the sectors begin, and the
  loss itself cannot be brought about by choosing bytes.
- A value of zeros that loses a sector reads as it was written, and is.

**What it does not claim.** A record that is whole and stands at another
place than it was written to is taken as a record. The crash model has no
such case. A group that does is still refused by its version, and a
checkpoint mark by its place (LG29).

**LG32. A reader reports, and the store decides** (R7). `scan` knows
nothing of the manifest. The manifest gives every log file a valid length
(M13), and the store reads the tail against it:

- *Below the valid length, a failure is damage, in an open file as in a
  closed one.* The manifest has vouched for those bytes: they held whole
  records when it was written. A tail whose `valid_len` is below the
  manifest's length is never taken for the end of the log, and the open
  fails. To cut the file there would take back commits that were durable.
- *In a closed file the log ends at the valid length.* The reader is given
  those bytes and no more, and they are whole items to their end.
- *In an open file the tail begins at the valid length.* Whole items that
  follow it are part of the log (M15), the first failure behind it is
  where the log ends, and recovery names the file closed at `valid_len`.

When an append fails, the engine cuts the file at the end of its last
whole item (CM9). `LogStart::next` gives the start record of the file that
follows.

## 10. Sizes

| | Bytes |
| --- | --- |
| file header and start record | 45 |
| a group: its one part, before the entries | 26 |
| each further part | 26 |
| a page's run, around the run | 5 |
| a freed page | 5 |
| a released or acquired value pointer | 17 |
| a value, around its bytes, for each 128 MiB | 19 |
| a carried value | 33 |
| the placing or the move of a value: tree cell, flat entry | 48, 46 |
| a reclaiming step, around its run | 17, and 8 if it frees, 16 for each pointer, and 4 if it has a run |
| a checkpoint mark | 17 |

A run of a tree page is the 4 bytes of its page id and its records
(`page-delta.md`, section 7). So a commit that changes one hash logs 26 +
5 + 39 = 70 bytes, and each further hash 35 bytes, or 44 in another page.

On the reference workload a transition dirties 7.29 pages
(`tree-page.md`, section 6), writes one value out of line and carries 0.8:

| Per transition | Bytes |
| --- | --- |
| around the runs: 7.29 × 5 | 36 |
| around the new value | 19 |
| the placing of it and of the carried values: 1.8 × 48 | 86 |
| the pointers the carried cells stop holding: 0.8 × 17 | 14 |
| the carried values, repeated: 0.8 × 19 and their bytes | 15 + 368 |
| or named: 0.8 × 33 | 26 |

A block of 20 transitions is one group, so the 26 bytes of the part are
about 1 byte per transition, and a page that several transitions of the
block change is in the group once. PERF-04 measures the whole.

## 11. What the tasks that build on this take from it

| Task | Takes from here | Still its own to define |
| --- | --- | --- |
| STORE-01 Write-ahead log | The file: header, start record, items (section 2). `write_file_start` for a new file, with `LogStart::next` of the file before it. One item is one append. A commit is told the address its group will begin at, and nothing else is appended before the group (LG22, with the two ways to keep it). A background change and a mark are appended with nothing but their bytes (LG25, D-059). `LogCursor` with `record_len` is the recovery iterator: it stops at the first incomplete group, and `valid_len` is where an open file is cut, never below the manifest's valid length (LG30, LG32). The file that the place of a checkpoint's sample names is kept while the checkpoint is in force (LG29). `LogStart::check_next` across files (LG9). | Rotation, sync policies, the locks, which files it reads, the cut after a failed append. |
| STORE-06 Delta write-back | A commit's values come out of its group as `Group::values`, each with its address. The placing of a value is `ValueMove` with `from` the pending value, written in the page step (LG25, LG26). The mark when it takes a sample, before it copies the first chain, and a sync of the log after it copied the last, before the checkpoint is named (LG29). **To name carried values it keeps LG24:** a hold on the file a carried value names, until the end of the pending value is durable. Until it does, groups repeat. | Thresholds, the page step. The sample is taken, and the mark written, before `TableCheckpoints::write` copies the chains: see the open point below the table. |
| STORE-07 Recovery | `scan` or `LogCursor` for each file, in order, with LG9 between them. For a group: runs by page, with K3; `Entry::Freed` and the pages of the runs for `alloc::Recovery` (LG15); `Entry::Released` and `Entry::Acquired` for the counts, by the place (DU5). For a change: every one from the sample's place on, wherever the group of *C* + 1 stands; LG28 for the page, and its counts by the place. A tail below a file's valid length in the manifest is damage (LG32). The item that ends at the sample's place is a mark that covers what the checkpoint covers (LG29). With named values, the holds of LG24, put back at the end of replay. | Replay. |
| STORE-03 Page table | `LogPlace` is now `lignin_format::wal::LogPlace`, re-exported by `table`. Its offset is from 45, and a sample's place from 62 (P18). The checkpoint's bytes are as they were. | |
| STORE-09 Value log, STORE-08 | The record of a moved value is `ValueMove` with `from` the old pointer. The count changes of a value record are `released` and `acquired`. A file that a carried value names is held, and the end of the hold is a retirement (LG24). | |
| TXN-05 Commit | `GroupWriter`: the values first, each returning what its cell holds, then the pages in ascending order, then the pointers. A writer that is dropped before `finish` takes its bytes back, so a commit that fails before its append leaves nothing. The cells of the group hold addresses of the group's own place (LG22). | How a group is put together from the write set. |
| TREE-03, TREE-07, TREE-08 | A new value out of line is `GroupWriter::value` with its page. A carried cell's value is `value` with the bytes, or `carried` with the old pointer and the page its record names (LG23), and the pointer the cell held is released either way. A cell carried while pending held no pointer and releases none; it is given its bytes, or, if its pending value is a named one, named again (LG23). Flat pages are logged as flat runs: an insert, a replace, a remove or a range for each page a put, a delete or a range delete touches, an image for a new page (LG19). | The inline threshold, what a private frame holds before commit. |
| TREE-09 Subtree drop | A reclaiming step is `write_reclaim`: the removes of one page, the version it frees the page with, the pointers the cells held (LG27). | The walk. |
| FMT-08 Decoder fuzzing | `scan`, `Record::decode`, `record_len`, `read_file_start`, `read_logged_value`, `read_pending_value`, `FlatRun::decode` with `FlatRun::apply` on valid pages, `LogCursor::step`. The golden bytes of 4.8. | The targets. |
| VER-03 Store crash tests | `tests/wal_crash.rs` is the format's part: every state the simulator lets a crash leave of a log file, and of two. | The store's. |
| PERF-04 Write-back tuning | The sizes of section 10. Whether carried values are named (LG23). Whether flat runs should hold keys without the page's prefix (section 12, item 12). | |

**What `page-table.md` has to take up, found here and changed here only
where it names the log.** K13 has a recovery read the log "from two points,
the group of *C* + 1 for the pages and the place for the counts", and K14
names a checkpoint once the log is durable up to the place of its sample.
For background changes neither is enough, and LG28 and LG29 say what is:
the pages take the background changes from the sample's place on; the
sample comes before the first chain is copied; the log is synced after the
last chain is copied and before the checkpoint is named; and the file the
place names is kept. K13 now points to LG28 for background changes, and
K14 has since been changed to state the other three (the owner's decision
D-063, item 4): `TableCheckpoints::write` refuses a sample whose mark is
not in the log before it copies its first chain, and
`TableCheckpoints::install` refuses a log that is durable less far than
its end when the last chain was copied.

## 12. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one while version 1 is still open (U10).

1. **The 8 bytes of a pending value are a log address** (LG21): one number
   for the whole log, with the base of each file in a start record. A file
   id and an offset of 32 bits each would need no start record, and would
   limit a log file, and so a commit, to 4 GiB, which a value of the
   longest length does not fit.
2. **A group is written for one place** (LG22). It follows from item 1, and
   it is what STORE-01 and TXN-05 have to build: the commit knows where its
   group lands before it writes it. Page steps append to the same log
   under their own lock (D-059), so the place is either set aside for the
   group or kept by the counts lock; LG22 names both. An id that does not
   depend on the place (the commit's version and a number, say) would need
   a map from versions to groups on every read of a pending value.
3. **A log file has a start record** (LG2), with the file's base and the
   version it comes after, and **a file continues the log only where the
   one before it stops being valid** (LG9).
4. **A carried value can be named, under LG24** (LG23), and LG24 asks more
   than FL7's wording. The record is defined. Using it waits for the hold.
   The other way, keeping the old pointer counted until the placing, is
   not taken. The owner decides between "repeat", "name with the hold" and
   that.
5. **The source of a carried value is a value pointer only.** A cell
   carried while a new value of it is pending is given its bytes again,
   and one whose pending value is a named one is named again. A cell that
   was pending releases no pointer (LG23).
6. **A value in the log has the value log's record layout** behind a kind
   byte, and is its own record with its own checksum, before the parts. A
   value inside a part would be read through a checksum over up to
   128 MiB.
7. **A value record carries no version.** It belongs to the group whose
   part follows it. 8 bytes less for each value.
8. **A group is its values and then one part or several** (LG12, LG18),
   with the version and a number in every part, and the split of the
   entries fixed (B8).
9. **The entries are in a fixed order** (LG14): pages ascending and once.
   The writer sorts the pages it changed.
10. **A group can acquire pointers** (LG16), though no commit does today.
11. **Flat pages are logged as runs of their own records, by page** (LG19).
    The owner's note asked for logical puts, deletes and range deletes and
    not page images. They are logical within a page, and they name the
    page, because recovery rebuilds page by page. An image is one of the
    records: for a new page, and where it is shorter.
12. **A flat record holds the whole key**, not the key without the page's
    prefix. Self-contained, and about 32 bytes longer for a key under a
    subtree prefix. PERF-04 can ask for the other while version 1 is open.
13. **Flat records are strict**: insert and replace are two records, and a
    range is a first key and a count, not two bounds. A run that is
    applied to the wrong page is refused instead of doing something else.
    The prefix and the room are checked for the image a run leaves, not
    between its records (LG19).
14. **The record of a background change does not list its count change
    where the fields give it** (LG25): a placing acquires `to`, a move
    releases `from` as well.
15. **A reclaiming step's record is of one page** and can free it (LG27).
16. **The replay rule for a background change** (LG28), and that replay
    looks at every background change from the sample's place on. It is
    STORE-07's to keep, and is stated here because the records are
    recognisable only under it.
17. **A checkpoint mark holds the version the checkpoint covers, and the
    place of a sample is the end of its mark** (LG29). It does not hold the
    checkpoint's file id, which is given out when the file is created,
    after the sample.
18. **`LogPlace` moved to `lignin-format::wal`**, and `table` re-exports
    it. Its offset is from 45 to 2^63 − 1, and the place of a sample from
    62. The bytes of a checkpoint did not change. Three tests of
    `lignin-store::table` use other offsets for it.
19. **A run is at most 2^27 − 27 bytes** (LG18), and is not cut.
20. **Records do not carry their place** (LG31), as `format.md` has it for
    every file.
21. **The errors are one variant of `FormatError`**, `Wal(WalError)`. What
    a run, a page, a value pointer or the frame refuses keeps its own
    error.
22. **`lignin-format` has `lignin-io` as a dev-dependency**, for the test
    of the crash model. The library does not depend on it.
23. **The payload of a record says how long it is** (LG4): a part states
    the length of its entries and a reclaiming step that of its run, 4
    bytes each. A flipped bit in a frame's length is then found with
    certainty and not by a chance of 2^32 to 1. The default frame of
    `format.md` has the same gap for every record whose payload does not
    say its length; only the log is changed here, because only a log
    takes a failed record for its end.
24. **What a checksum of 32 bits does not prove is stated, and not
    changed** (LG31): a sector lost from the middle of a record is found
    with the chance the checksum gives. A stronger guarantee would take a
    longer checksum or a log in blocks, which is the plan's to decide
    (FMT-01).
25. **What a sample needs** (LG29): it is taken before the checkpoint
    copies its first chain, the log is synced after the last chain is
    copied, and the file its place names is kept. `page-table.md` has the
    first as the way `TableCheckpoints::write` is called and does not
    state the other two.
26. **A tail below the manifest's valid length is damage, in an open file
    too** (LG32).
27. **A group writer that is dropped takes its bytes back.** A commit
    that fails and returns cannot leave a value for the next group.

## 13. Code and tests

All of this is in `lignin-format::wal`. Run the tests with
`cargo test -p lignin-format wal`, and the two integration tests with
`cargo test -p lignin-format --test wal_crash --test wal_alloc`.

| Rule | Code | Tests |
| --- | --- | --- |
| Layout (section 4) | `write_file_start`, `GroupWriter`, `CarriedValue`, `ValueMove`, `write_reclaim`, `CheckpointMark`, `FlatRunWriter` | `tests/golden.rs`: every record of 4.8 and a group with a carried value, a flat run, a freed page and pointers, byte for byte against bytes from a separate encoder, and read back. The group of 4.8 is replayed on its page: the cell then holds the pending value, and the record that places it is due and places it. `flat::golden_bytes`. |
| LG30, LG31: the tail | `scan`, `LogScan`, `LogTail` | **Acceptance test**, `a_truncated_or_bit_flipped_log_decodes_to_the_longest_valid_prefix_of_whole_groups`: six logs of random items, half of them written with records of at most 200 bytes so that values and groups take several records. Cut at every length, with every single bit flipped, and with ranges read as zeros, each gives exactly the items before the damage, each written again to the bytes it was read from, and ends where the last of them ends, at the version of its last group, with the failure at the record that holds the damage. `damage_never_gives_an_item_that_was_not_written`. The logs are drawn, not chosen against the checksum: what the test shows for a range read as zeros is what LG31 says of it. |
| LG31: the crash model | the same | **`tests/wal_crash.rs`**, on `lignin_io::sim`: a log is appended to a simulated file, one append for each item, with syncs. After every append, each state a crash can leave (what a device that writes in order leaves, with every tear; each single lost write; every state while there are few; drawn ones), for sectors of 1, 64, 512 and 4096 bytes: the scan gives the items of the longest prefix that is as written, each with the bytes and the version it was written with, and every item that was synced. |
| LG9: two files | `LogStart::{next, check_next}` | `a_crash_that_loses_the_end_of_one_log_file_ends_the_log_there`, on the simulator: the second file follows exactly when all of the first is there. `the_log_goes_on_in_the_next_file_where_this_one_stops_being_valid`. |
| Section 9.1 | `Record::decode`, `record_len` | `tests/records.rs`: each check of each record with its error, in order; every shorter input; every single bit flip of a record; every kind code; every error's message. |
| LG1, LG2 | `write_file_start`, `read_file_start`, `scan` | `a_log_file_begins_with_the_header_of_a_log_and_a_start_record`: a file of every length below 45; another kind of file; a first record that is not a start; a file id of 0 and the reserved id, and the last id there is. |
| Section 9.2, LG11 to LG14 | `LogCursor` | `tests/cursor.rs`: each rule of the table with its error; versions across a file's start; a file read record by record is the file read whole, and a refused record leaves the cursor where it was. |
| LG6, LG7 | `LogPlace`, `LogAddress`, `LogStart` | `a_place_is_a_file_and_an_offset_behind_the_start_record`, `an_address_is_the_base_of_its_file_and_an_offset`. |
| LG18 | `GroupWriter`, `Limits` | `a_value_is_cut_into_records_of_one_size`: every length from 1 to 40 in parts of 1 to 7 bytes, with the same code and a shorter record. `a_value_one_byte_above_the_longest_record_is_two_records`, at the real limit. `a_group_too_long_for_one_record_is_cut_between_entries_in_one_way`, `an_entry_too_long_for_a_record_is_refused`. |
| LG12 to LG17 | `GroupWriter`, `Group`, `Entry` | `a_group_is_written_in_one_order_and_a_refused_call_leaves_it_as_it_was`, `a_group_that_is_given_up_leaves_nothing`, `a_group_whose_writer_is_dropped_leaves_no_value_for_the_next_group`. |
| LG21, LG23 | `LogAddress::{of, pending}`, `read_pending_value`, `logged_read_len` | `a_pending_value_leads_to_its_bytes_or_to_the_value_it_was_carried_from`, `the_parts_of_a_value_come_whole_in_order_and_from_one_value`, `the_bytes_to_read_at_an_address_cover_either_record`. |
| LG19 | `FlatRun`, `FlatChange`, `FlatRunWriter` | `a_page_and_a_run_give_the_page_a_model_gives`: 600 runs of drawn changes on one leaf against a model, with images and prefixes. `a_branch_takes_separators_and_gives_them_up`, `a_change_that_does_not_fit_its_page_is_refused`, `each_check_of_a_run_has_its_error`, `a_change_that_would_not_read_back_is_not_written`. An image of another page than the run's is refused by the reader and by the writer, and a run is not applied to another page's image. A run whose later records mend what an earlier one did to the prefix and the room applies, and each shorter run of it is refused. |
| LG4: a record says how long it is | `GroupPart`, `Reclaim`, `LoggedBytes`, the records of one size | `a_changed_length_is_refused_whatever_the_checksum_says`: every record of the six logs of the acceptance test, with each of the 32 bits of its length flipped and the checksum that the new length and the bytes it then covers have. Each is refused where it stands, none by its checksum. In `tests/records.rs`, a part and a reclaiming step that state another length than they have. |
| LG26 to LG28 | `ValueMove`, `Reclaim`, `write_reclaim` | `tests/changes.rs`: a placing and a move are due exactly while the cell holds the old place, in a tree page and in a flat leaf, and change the place and nothing else; a leaf of another page that holds the place is not the step's; a reclaiming step's rules. In `tests/records.rs`, a pending value where a value pointer must stand is `ReservedFileId`: the `to` of a value change, the source of a carried value, a released or acquired pointer of a group and of a reclaiming step. |
| LG29 | `CheckpointMark`, `table::FIRST_SAMPLE_OFFSET` | `the_versions_of_the_groups_go_up_by_one_and_nothing_is_ahead_of_them`; in `table`, `the_end_record_must_agree_with_the_counts_and_name_a_place_in_the_log`. |
| B8 | every reader and writer | In the acceptance test and in the tests of arbitrary bytes, every item that is read is written again and gives the bytes it was read from. |
| B10 | every reader | `arbitrary_bytes_are_an_error_or_a_log_and_never_a_panic`: 45,000 files, a third arbitrary, a third records with valid checksums and arbitrary fields, a third written logs with a record changed under a valid checksum. `an_arbitrary_flat_run_is_an_error_or_a_run_that_applies_or_is_refused`. |
| No allocation | `scan`, `LogCursor`, `Group::{values, entries}` | `tests/wal_alloc.rs` runs under a counting allocator. |

The golden bytes and their checksums were computed with an independent
bit-at-a-time CRC-32C and a separate encoder, not with the crate under
test.
