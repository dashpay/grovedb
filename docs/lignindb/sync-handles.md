# Which handle syncs a file

LigninDB task IO-01, follow-up `IO-01-F2` part A ([plan](https://github.com/dashpay/grovedb/issues/1052); D-174 items 6 and 7).

This document has three things: the limit of what a sync can promise, the rule the engine keeps because of it, and a list of every place in `lignin-store` and `lignindb` that makes a file durable, with the handle that wrote and the handle that syncs. The list was read at commit `a0fac9ff`. Lines are of that commit unless a row names a branch. Rows S2 and S3 were read again at `0232a406`, with the change that the follow-up `STORE-02-F2` of task STORE-02 made to the two stores (D-196 item 1): the lines they give are of `0232a406`, in files that change does not touch, and the files it touches are named by function. Row T1 names the functions of the harness's file layer for the same reason.

Section 7 is of task IO-03: what the file layer that opens files with `O_DIRECT` (`lignin_io::linux`) changes in the above, which is nothing, and why.

## 1. The limit

The file layer's contract has it as the last rule under "Durability" (`lignin_io::vfs`):

> A write-back can fail unreported. The system writes a file's changes to storage in its own time, before any sync asks for them: that is write-back, and it can fail without a `Vfs` call reporting it. A write lost to a write-back error can cease to be visible to reads. Process exit or termination does not itself discard buffered writes; those writes can still be lost to a write-back error. And a sync on a reopened file can succeed after both the unwritten data and the error record are gone.

Four sentences of the contract hold only absent a failed write-back, and say so: a read sees every write that returned; a process that exits or is killed loses nothing it wrote; a sync that returns `Ok` on any handle covers the changes made before it; and with everything synced a crash loses nothing.

The simulator (`lignin_io::sim`) can fail a write-back unreported ("A write-back that fails"). With `Told::Writers` it tells of the failure the handles that made the changes the write-back was to write, and no other, which is the least the contract promises: a sync through any other handle is then told nothing, and can return `Ok` over bytes that storage does not have. **So a test on the simulator can find a break of the rule below, where it fails a write-back of the file and tells the writers only.** The tests that do are in section 4, below the table of the store. The list in section 4 was made by reading.

## 2. The rule

**A file's changes are synced through the handle that wrote them, kept open until that sync returned.**

This is the rule the log has had since STORE-01 ("only the log syncs its files", D-075 item 3), made general. What it is for: the one sync that is known to report a failed write-back is a sync through a handle that was open when the write-back failed (section 3). A handle that was opened later can sync with `Ok`.

What follows from it for code:

- A file is not closed with changes that no sync through its writing handle has covered, if anything is to rely on them later.
- A file of an earlier session is never made durable by syncing it. Either a manifest vouches for its bytes, because the session that wrote them synced them before it named their length, or the bytes are written into a new file that is synced through its own handle.
- A file that is given to a set (`Vfs::sync_together`) is given as the handle that wrote it.

What it does not cover:

- **A directory.** A change of names is made by a path, and a directory is synced by its path: the file layer opens it for the call. There is no writing handle to keep. The contract's rule for names assumes a file system that journals its directories.
- **A read.** The rule is about what a sync vouches for. What a read through a new handle returns of bytes that no sync covered is section 6.

## 3. What Linux does

Read in the source of Linux v6.12.

1. **A failed write-back leaves a record on the file in memory, and the data is not tried again.** ext4 clears a buffer's dirty mark when it submits it (`fs/ext4/page-io.c:493`). When the write then fails, it records the error on the file's mapping (`mapping_set_error`, `fs/ext4/page-io.c:118-121`) and ends the write-back. The page is clean and can be reclaimed, and a read after that returns what the disk has.
2. **Every descriptor has a mark of its own in that record, taken when it was opened** (`fs/open.c:917`). `fsync` and `fdatasync` compare the record with the descriptor's mark after they have written the file's data out, return the error if the record has moved, and move the mark (`file_check_and_advance_wb_err`, `mm/filemap.c:735-761`). ext4 makes that comparison at `fs/ext4/fsync.c:176`, and btrfs at `fs/btrfs/file.c:1869`. `file_write_and_wait_range` makes it too (`mm/filemap.c:794`): XFS calls that at `fs/xfs/xfs_file.c:138`, and ext4 at `fs/ext4/fsync.c:158`. So a descriptor that was open when a write-back failed is told of it by its next sync, whatever other descriptors were told.
3. **A descriptor that is opened after the failure is told only if no other was told before it** (`errseq_sample`, `lib/errseq.c:123-131`).
4. **The record is in the file's inode in memory.** An open descriptor keeps the inode there. With none open the kernel may evict it (`fs/inode.c:941-947`, `fs/inode.c:956-964`), and an inode that is read in again starts with an empty record (`fs/inode.c:209`).

Items 1 and 4 are the sequence the contract gives: write-back fails with no sync in flight, the last handle is dropped and the inode evicted, the file is opened again and synced. Item 3 is a second way to the same end, with the inode in memory throughout: another descriptor is told first, which a sync of the file from another process is enough for, and a descriptor that is opened after that syncs with `Ok`. Item 2 is why the rule works, and it gives a little more than the rule asks: a sync through any handle that has been open since before the change reports the failure, also one that did not write.

**macOS is not established.** APFS is not published as source. Apple's manual page for `fsync` lists `EIO` ("An I/O error occurred while reading from or writing to the file system") and does not say whether the error of an earlier write-out is kept for a later call, or for which descriptor.

## 4. The list

"Holds" means the rule of section 2 in its words. Paths without a crate are in `lignin-store/src`.

### The store

| # | What is made durable | Which handle wrote | Which handle syncs | The rule |
| --- | --- | --- | --- | --- |
| S1 | The header of a new numbered file: a segment, a value-log file, a log file, a table checkpoint (`create_file`, `segment/files.rs:128-163`) | The append handle that created the file (`:136`; the header at `:158`) | The same (`sync_data`, `:160`). The name: `sync_dir` of the kind's directory (`:162`). | Holds. |
| S2 | **A segment's blocks and its footer.** Written by write-back, by the cleaner and by recovery's replay. | The append handle from S1, which the segment and its writer share from `SegmentStore::create` on: a block in `SegmentWriter::append`, the footer in `SegmentWriter::seal` | **The same handle**, in `Segment::sync` and `Segment::seal_with` (`segment/store.rs`). Every sync of a segment after the one of its header (S1) comes to one of the two: `SegmentWriter::sync` (write-back, `flush/write_back.rs:1236`; the cleaner, `cleaner/clean.rs:756`), `SegmentWriter::seal` (`flush/write_back.rs:1223`, `cleaner/clean.rs:729`, replay at `recovery/replay.rs:728`, `:744`, `:861`) and `SegmentStore::sync` (from `table/checkpoint.rs:492` and `flush/checkpoint.rs:574`). | Holds: see below the table. |
| S3 | **A value-log file's records.** Written by the placing of values, by the cleaner of the value log and by a bulk load (`lignindb/src/txn/load.rs`). | The append handle from S1, which the file and its writer share from `ValueStore::create` on: `ValueWriter::append`, and `ValueWriter::append_records` for the copies of a cleaning (`values/clean.rs:295`) | **The same handle**, in `ValueFile::sync` (`values/file.rs`). Every sync of a value-log file after the one of its header (S1) comes to it: `ValueWriter::sync` (`flush/place.rs:149`, `flush/write_back.rs:805`, `cleaner/values.rs:396`, `values/clean.rs:433`; a bulk load, `lignindb/src/txn/load.rs:265`, `:296`), `ValueWriter::finish` (`flush/write_back.rs:854`, `cleaner/clean.rs:871`, `cleaner/values.rs:304`, `:354`; a bulk load, `lignindb/src/txn/load.rs:246`, `:286`), `ValueWriter::give_up` (`flush/write_back.rs:844`, `cleaner/values.rs:376`) and `ValueStore::sync`, which nothing in `lignin-store` or `lignindb` calls. | Holds: as S2. |
| S4 | Segments and value-log files of an earlier session, as recovery and the open of a checkpoint adopt them | The session that wrote them | Nobody. They are opened to be read (`segment/store.rs:115`, `values/file.rs:63`) and are closed or sealed for this session, so its syncs pass them by (`segment/store.rs:329`, `values/file.rs:301`). They are read up to the manifest's valid length, which their own session synced (S2, S3) before a manifest named it. | Holds: nothing is synced through the new handle. |
| S5 | The log, in a file this session created | One handle: created at `wal/store.rs:828`, appended to at `wal/file.rs:200` | The same (`wal/file.rs:204`, from `wal/store.rs:1397`). The log keeps it for as long as it has the file. Every sync of the log comes here: a commit (`lignindb/src/commit/committer.rs:604`), `Db::flush` (`lignindb/src/db/mod.rs:974`), the open (`lignindb/src/db/open.rs:550`), a checkpoint's step 6 (`lignindb/src/checkpoint/create.rs:453`), write-back (`flush/checkpoint.rs:79`, `:90`), the cleaner (`cleaner/values.rs:210`). | Holds. |
| S6 | The log, in a file of an earlier session whose end no manifest vouches for (recovery's adoption) | The session that wrote it | The file is not synced (`wal/store.rs:1395-1396`). Its bytes are written again: read through the adopted handle (`:1472`), appended to `wal/<id>.log.tmp` through a new handle (`:1466`), which syncs them (`:1478`); then the rename (`:1455`) and `sync_dir` of `wal/` (`:1459`). | Holds for the new file. What is read is section 6. |
| S7 | The manifest: a replacement, and a first manifest (`install`, `segment/manifest.rs:215-228`) | The handle that creates `MANIFEST.tmp` (`:242`; the bytes at `:245`) | The same (`sync_all`, `:247`), before the rename (`:223`). The name: `sync_dir` (`:226`). `ManifestStore::open` reads the manifest through a new handle and syncs the directory (`:106-108`): that sync is for the name, which a crash could still take back. The content was synced through its writer before it had the name. | Holds. |
| S8 | A table checkpoint's records | The append handle from S1 (`table/checkpoint.rs:411`; `:564`) | The same (`sync_data`, `:624`). It is dropped after the write returned (`:421`). Loading one (`:662`) syncs nothing. | Holds. |
| S9 | The name of a store's directory (`list_or_create_dir`, `segment/files.rs:112`) | - | `sync_dir` of the database directory | A name. No file's content rests on it. |

**S2 and S3.** Each segment and each value-log file has two handles from its creation: one appends and syncs, and the store reads through the other, which syncs nothing. The file and its writer share the first. `SegmentStore::sync` and `ValueStore::sync` also sync a file whose writer was dropped, so the file keeps that handle for itself, for as long as the store has the file: a segment while it has its writer, and after that until a sync through it has covered every block that was appended, or a sync of the segment has failed; a value-log file until it is closed. Neither `SegmentStore::drop_segment` nor the end of a session syncs a file that leaves the store by it: nothing vouches for bytes of such a file that no sync had covered. A handle that the file still has then is closed no later than the last owner of the file's record is dropped, which can be a walk, a `SyncRecord` or a `ValueHold`. A handle that a sync in flight was given is open until that sync has returned. The two handles are kept apart, and not made one as the log has it (S5), because the file layer of section 7 opens the reading handle with `O_DIRECT` and no other.

- **The sync that is to vouch for a block or a record is the one that the contract has checked for a failed write-back.** It is made through the handle that wrote the bytes, which the contract's last rule under "Durability" names. Where that sync fails, the store fails the file as after any failed sync, and no manifest gives the file a length past what an earlier sync covered. On Linux, item 2 of section 3 has that sync report the failure.
- **On macOS nothing of section 3 is established.** The stores make there the sync that the contract names.
- **How it is tested.** `segment::tests::write_back` and `values::tests::write_back` of `lignin-store` fail a write-back of a file with `Told::Writers`, and require the next sync of the file to fail, whichever way it is asked for. The store's crash campaign (`lignin_testkit::crash`) fails write-backs with `Told::Writers`, by default in the runs of its odd seeds. Its own acceptance test plants the syncs of what is appended to a segment, and to a value-log file, through another handle, and requires each to be found (`lignin-crash planted`, `segment-handle` and `value-handle`). And in the sessions of every lineage of a run, with or without such a failure, a sync that is asked through a handle of a file to which another handle has written since its last sync is a violation; the recoveries that the campaign makes of each crash state run on the simulator with nothing between, and are not held to that. The rows of the engine (L1 to L7) are not run with that fault by this campaign.

### The engine

| # | What is made durable | Which handle wrote | Which handle syncs | The rule |
| --- | --- | --- | --- | --- |
| L1 | A checkpoint's hard links (`lignindb/src/checkpoint/files.rs:244`) | The live database's stores | Nobody in the creation, and no directory sync is relied on for their content. Segments, value-log files and the table checkpoint are taken at the lengths of the sampled manifest (`lignindb/src/checkpoint/create.rs:413-439`), which the stores synced (S2, S3, S8). The log's files are linked after the log has synced itself (`create.rs:453`: S5, S6). The names: `files.rs:311`, then the target's own (`:176`). | Holds, and rests on S2 and S3. |
| L2 | A checkpoint's copies, where the file system makes no link | The handle that creates the copy (`lignindb/src/checkpoint/files.rs:284`; `:295`). The source is read through a new handle (`:253`). | The same (`sync_all`, `:299`) | Holds. |
| L3 | A checkpoint's manifest (`lignindb/src/checkpoint/files.rs:320`) | As S7 | As S7 | Holds. |
| L4 | A file the engine has a checkpoint or a backup carry (`Checkpointer::carrying`, `lignindb/src/checkpoint/create.rs:197`, and `Target::carry`, `lignindb/src/checkpoint/files.rs:210`; `Backup::carrying`) | Its owner | A linked one: nobody. "The engine vouches that such a file is durable and is never written again" (`lignindb/src/checkpoint/create.rs:193-194`). A copied one: L2, L5. | The caller's to keep. Nothing outside tests carries a file at `a0fac9ff`. Whoever first does has to have synced it through the handle that wrote it. |
| L5 | `Backup::copy` (`lignindb/src/backup/mod.rs`) | The handle that creates each copy (`:623`; `:634`). The source is read through new handles (`:476`, `:537`, `:613`). | The same (`sync_all`, `:638`), before the next file is begun. The names: `:644`, `:647`. Then the copy's log is read through new handles (`:651`) and the manifest is written as S7 (`:654`). | Holds. |
| L6 | The reserve (`Reserve::fill`, `lignindb/src/space.rs`) | The handle that creates `RESERVE.new` (`:215`; `:270`) | The same (`sync_all`, `:273`), before the rename (`:217`). The name: `:220`. Where a reserve of the right length is found, only the directory is synced (`:204-207`): the file was given its name after its own sync returned, in whichever call made it, so its content does not rest on that directory sync. | Holds. |
| L7 | The name of a new database directory (`lignindb/src/db/open.rs:161`) and of its lock file (`:367`); a deletion (`lignindb/src/checkpoint/files.rs:474`, `:555`, and its rename of the manifest to another name, `:574`); what a failed creation or copy takes away (`files.rs:145`, `:353`, `:367`; `lignindb/src/backup/mod.rs:571`, `:678`, `:692`) | - | `sync_dir` | Names. No file's content rests on them. |

### The file layer and the crash harness

| # | What is made durable | Which handle syncs | The rule |
| --- | --- | --- | --- |
| G1 | A set (`Vfs::sync_together`) as the trait provides it (`lignin-io/src/vfs.rs:373`): the single calls, one after another. `SimVfs` and the harness's file layer have that method, and `StdVfs` makes the same calls on Linux and for what is not on APFS. | Each file through the handle its item holds, each directory by its path, one call for each | The caller's to keep: the item is the handle that wrote. An item cannot be a file's path. |
| G2 | A set on an APFS volume (`sync_sharing`, `lignin-io/src/std_fs.rs:707-729`) | `fsync` through each item's own handle (`:718`), a directory through a descriptor the file layer opened for the call (`:666`, `:719`). Then `F_FULLFSYNC` once, through the last item of the volume (`:722`). | Each file's changes are handed over through the handle the caller gave. The flush is the device's and is asked through one of them; if it fails, every handle of the set is stopped (`:722-728`). What macOS reports to which descriptor is not established (section 3). |
| T1 | The crash harness's `Session` (`lignin-testkit/src/crash/store.rs:367`) | It holds the stores of `lignin-store` and calls the file layer for no sync of its own: the sync of a commit (`:1290`) and `sync_log` (`:1970`) are the log's (S5), and a table checkpoint is write-back's (`:1965`). Its files are those of S1 to S8. Its file layer (`Observer`, `lignin-testkit/src/crash/observe.rs`) passes each sync to the handle it wraps (`ObservedFile::sync`) and each directory sync to the simulator, but for the syncs that a planted defect of the campaign leaves out or hands to another handle (`Sabotage`), and has the set as G1. | As the stores. Where its runs fail a write-back, the simulator tells the handles that wrote and no other (section 1). |

Nothing in `lignin-store` or `lignindb` calls `sync_together` at `a0fac9ff`. The two callers that are coming are in section 5.

## 5. The two units that are changing such places

Both are branches that are not merged. The rows are what they will be if the branches merge as they are.

### `STORE-07-F2` part 2, `lignin/STORE-07-recovery-syncs` at `f66d3827`

| # | What is made durable | Which handle wrote | Which handle syncs | The rule |
| --- | --- | --- | --- | --- |
| P1 | A new log file and the manifest that names it, as one set (`LogStore::commit`, `wal/store.rs:1885-1892`, through `name_new_file`, `:1904-1920`) | The log file: its one handle, created with nothing synced (`create_unsynced_file`, `segment/files.rs:178-186`; `wal/store.rs:856`). The manifest's temporary file: the handle that creates it (`segment/manifest.rs:323`). | The set is `Data` of the log file's handle (`wal/file.rs:202`: "the handle the file is appended to and synced through"), `Dir` of `wal/`, and `All` of the temporary file through the handle that wrote it, which the manifest store hands to the caller's closure (`segment/manifest.rs:328`). Then the rename (`:301`) and `sync_dir` (`:304`). If the set fails, the log file is given up and removed. | Holds. |
| P2 | The header of a segment, a value-log file and a table checkpoint (`create_file`, `segment/files.rs:141-153`) | As S1 | As S1 (`:150`, `:153`) | Holds. |
| P3 | The name of a store's directory (`list_or_create_dir`, `segment/files.rs:102-123`) | - | `sync_dir` of the database directory, now only where this session made a directory that no sync has covered | A name. |
| P4 | A recovery of a closed store: five syncs in three calls (`recovery/mod.rs:283`) | | The database directory, for the manifest's name (`segment/manifest.rs:143`); the set of P1; the database directory again. No file is synced through a handle that was opened again. S6 is as it was (`wal/store.rs:1484`, `:1537`). | Holds. |

S2 and S3 are not touched by the branch.

### `TXN-06-F1`, `lignin/TXN-06-rename-into-place` at `b0220478`

The rows were read at `b0220478`. At `ea070d91`, where the branch is as this is written, `checkpoint/files.rs` is the same, and nothing since adds or moves a sync, a writing open, a link or a rename.

| # | What is made durable | Which handle wrote | Which handle syncs | The rule |
| --- | --- | --- | --- | --- |
| Q1 | A checkpoint's copies and its manifest, which is now written under its own name in the directory the checkpoint is built in | Each by the handle that creates it (a copy: `lignindb/src/checkpoint/files.rs:492`; the manifest: `:522`). Each handle is kept open in `Target::written`. | One set (`Target::settle`, `files.rs:541-549`): `All` of each written file through the handle that wrote it, then `Dir` of each directory made, of the building directory and of its parent. | Holds: the field's own comment is the rule ("each open through the handle that wrote it until it is made durable through that handle"). |
| Q2 | The checkpoint's name (`Target::place`, `files.rs:567-581`) | - | The rename of the directory, then `sync_dir` of the parent | A name. |
| Q3 | The hard links | As L1 | As L1 | As L1. |

On an APFS volume the flush of Q1's set is asked through its last item, which is the parent directory (G2).

## 6. What rests on the narrowed sentences, and is not a sync

**Recovery reads bytes that no sync covered.** The end of the log behind the manifest's valid length is read through new handles (`wal/reader.rs:496`), and S6 copies it as it reads it (`wal/store.rs:1472`). Absent a failed write-back, and with no crash since, that is what the last session appended. After a write-back that failed, it can be what the disk had before. The reader then ends the log at the first item that is not whole (LG30, LG31 of `log-format.md`), and the open lands on an earlier commit: at or after the last one that was durable, as DU2 has it, but before commits that the last session published and that no power cut took.

**A backup copies such bytes too.** Of a file that the source's manifest names open, `Backup::copy` takes all the file has (`lignindb/src/backup/mod.rs:488-489`), measured and read through new handles (`:476`, `:613`). The copy makes them durable (L5), and the version of the copy is where its log ends (`:651`). So after a write-back that failed in the source, a copy can end at an earlier commit than the source's session published, with no power cut.

Three sentences of `disk-full-and-backup.md` about the open that follows such a failure say it with this limit (lines at `7cc55fbc`):

| Where | The sentence |
| --- | --- |
| `disk-full-and-backup.md:53-61` (DF1) | "With no power cut since, it is the latest version that was published after a close that returned `Ok`, and also after a close that answered an error, unless the operating system failed to write to the device something that the session wrote to its files. A sync that the device refused is such a failure, and what the system keeps in memory of the bytes it could not write is not promised (`sync-handles.md`, section 6): the open can then land on an earlier version with no power cut, and never on one before the newest that was made durable." |
| `disk-full-and-backup.md:168-171` | "the next open recovers the database at a version that DF1 allows. A refused sync is a failure of the kind that DF1 names: also with no power cut, the open can land before the latest version that was published." |
| `disk-full-and-backup.md:500-506` | "Nothing that was made durable is lost by it. A commit that was published after the newest durable version is in the next open if the system still has the log whole up to it, which it does not promise after a sync that was refused (DF1): the open can land on an earlier commit than the last one with no power cut, and `Db::version` says where it landed." |

Two sentences of the same document say more than that. They hold absent a failed write-back, and the second with no power cut:

| Where | The sentence | Whose |
| --- | --- | --- |
| `disk-full-and-backup.md:68-70` (DF2) | "A commit that was answered `CommitError::NotDurable` is published: the next open has it unless a power cut loses it." That answer is given where the sync of the log at the commit was refused (DF4), which is a failure of the kind that DF1 names. | OPS-08 |
| `disk-full-and-backup.md:183-185` (DF8) | "A commit that was waiting for the lock when a failed sync of a segment was recorded is still taken: it is in the log like any other, and the next open has it." | OPS-08 |

`lignin-store/src/wal/mod.rs:302-312` already gives the reason for S6 in these terms: a session "may have had a sync of it fail and ended with no crash".

## 7. The file layer that reads with `O_DIRECT` (IO-03)

`lignin_io::linux::DirectVfs` is built on Linux with the feature `direct_io`. It is `StdVfs` but for the files it opens to read (`OpenMode::Read`), which it opens with `O_DIRECT`. Kernel sources below are Linux v6.12.

### 7.1 Which sentences of the contract hold more strongly: none

Where a write with `O_DIRECT` goes past the page cache, which a file system can decline for a given write, it does not wait there, and a write that storage refuses is reported by the write itself. This layer opens no file with the flag to write, so it gains nothing of it. A handle that is opened to write or to append is `StdVfs`'s own handle, and every row of section 4 is the same on both file layers: the limit of section 1 holds, and the rule of section 2 is needed, as before.

Why its writes are not direct:

- **A direct write has to be whole blocks on file systems a store runs on.** The direct path that ext4 uses refuses a write whose offset or length is no multiple of the device's logical block (`fs/iomap/direct-io.c:295-308`), and btrfs makes one that is not aligned through the cache. The log appends a group of any length with one `write`, unless the system takes fewer bytes than it was given or a signal interrupts the call (`lignin-store/tests/wal_syscalls.rs` counts the calls and the bytes, on this layer too where the feature is built), a segment and a value-log file take blocks and records of any length, and every file's length is exact: a manifest names a valid length to the byte. With `O_DIRECT` each of them would have to be written in padded blocks and cut back to its length, which is a change of the stores and of the log's rule, not of the file layer.
- **A direct write would not end the need for the sync either.** Where it goes past the cache, it reports the failure of the data it wrote. The file's length, the blocks a file system allocates for it, and the name are written by the file system in its own time, and the device's cache is flushed by nobody: `fdatasync` through the writing handle would still be what makes an append durable.

So if a later unit makes the writes of a file direct, the sentences that would hold more strongly for that file's bytes are the four that section 1 lists as holding only absent a failed write-back, and only for writes of whole blocks inside the file's length that the file system makes past the cache. The rules for a length, for a name and for a directory would not change.

### 7.2 What a read with `O_DIRECT` does where the page cache holds a write

Segments and value-log files are appended through one handle and read through another (S2, S3). On this layer the first is `StdVfs`'s and writes into the page cache, and the second reads past it where the file system reads directly. Three things were read in the source for that.

1. **A read that waits writes the range out first.** Before a read with `O_DIRECT` goes to storage, the system writes out the dirty pages of the range it reads and waits for them: `kiocb_write_and_wait` (`mm/filemap.c:2719-2732`), which the generic read calls (`generic_file_read_iter`, `mm/filemap.c:2805`) and so does the path of ext4, XFS and btrfs (`iomap_dio_rw`, `fs/iomap/direct-io.c:611`; ext4 comes to it from `fs/ext4/file.c:94`). A read that may not wait, which is how a ring first tries a read (`io_uring/rw.c:848-856`), is refused there if the range holds a page that is to be written out or is being written (`mm/filemap.c:2725-2729`), and the ring then makes it as one that waits. btrfs writes the range out a second time where a compressed write is under way (`fs/btrfs/direct-io.c:394-424`), and where it cannot read directly it reads through the page cache: `btrfs_direct_read` returns with what it read directly, which can be nothing or a first part, and `filemap_read` goes on with the rest (`fs/btrfs/file.c:3779-3785`), for a read that is not aligned (`check_direct_read`, `fs/btrfs/direct-io.c:1010`), for a range that a buffered write still holds (`fs/btrfs/direct-io.c:89-108`, with `fs/iomap/direct-io.c:702-706`), and for an extent that is compressed or held in the tree (`fs/btrfs/direct-io.c:477`). On every one of these ways a read sees a write that returned before it began, as the contract's rule for threads asks, with the contract's own exception: a write that is lost to a failed write-back (item 2). The conformance suite runs on this layer, and `a_direct_read_sees_what_was_appended_through_another_handle` appends pieces of any length through one handle and reads each back through the other with no sync between.
2. **A read can fail for a write-back that failed, of its own range or of another part of the file. No read is sure to.** Where a read writes its range out and waits, an error from `filemap_write_and_wait_range` ends the read with that error: it is an error that the writing out itself returned, and else what `filemap_check_errors` finds when it looks afterwards. Where the helper returns none, the read goes on and returns its own result. A write-back that fails after it was started shows only in those marks, and where a sync or another read clears them first, this read finds nothing and goes on (`mm/filemap.c:696` and `:2806-2807`, `fs/iomap/direct-io.c:612-613`). The check is of the marks of the whole file, not of the range, and it is made also where the range had nothing to write (`mm/filemap.c:684-696`: it stands behind the condition, not under it). So a read that fails can be of another block than the one whose write-back failed. The check clears the marks it finds (`mm/filemap.c:358-363`), so the next read of the file does not fail for them. And there are reads that find nothing: one that may not wait and finds no page to write out in its range returns from `kiocb_write_and_wait` before the check (`mm/filemap.c:2725-2731`), and a ring first tries a read so; one that begins at or after the end of the file makes no check on the path of `iomap` (`fs/iomap/direct-io.c:605-611`); and a read that comes after a sync or another read cleared the marks finds none (`mm/filemap.c:752-759` for a sync). **So nothing may be built on a read's failing after a failed write-back: what reports one is the sync of section 3, as on `StdVfs`.** What a store on this layer must be ready for is the other side: an I/O error from a read of a block that was written well, where a read through `StdVfs` would have been answered from the cache if the cache still held the pages. After a failed write-back the pages are clean, and on either layer a later read can return what storage held before the write: the contract has that as "a write lost to a write-back error can cease to be visible to reads". What a batch of reads (`DirectVfs::read_batch`) does with an error that the system answers a read of a ring with, and with a read whose answer a ring lost, is in the module documentation of `lignin_io::linux` under "Reads together". It is made so that an error of a write-back that a ring's answer carries is not asked for a second time, when the mark could be gone.
3. **A failed read does not take the failure away from a sync.** A failed write-back is recorded twice on the file in memory: in the record that descriptors compare their marks with, and as a flag (`mapping_set_error`, `include/linux/pagemap.h:243-254`). `filemap_check_errors` reads and clears the flags only (`mm/filemap.c:354-365`). A descriptor's mark moves in `file_check_and_advance_wb_err` alone, which the sync paths call (`mm/filemap.c:607` and `:794`, `fs/btrfs/file.c:1869`). So item 2 of section 3 is as it was: a descriptor that was open when the write-back failed is told by its next sync, whatever a direct read was told before.

What follows for S2 and S3 on this layer: the handle that syncs a segment or a value-log file is the handle that appended to it, which is `StdVfs`'s own on this layer, so the syncs of the two stores are the same calls on both layers. The handle the store reads through, here a descriptor that was opened with `O_RDONLY | O_DIRECT`, syncs nothing. The difference is the one of item 2: a store on this layer can meet the failure of a write-back first in a read of some block of the file, as an I/O error of that read, and then in the sync as well.

A cost that follows from item 1, for whoever tunes the cold cache (PERF-06): where the file system reads directly, a block that is read back while the page cache holds its write and has not written it out is written out for that read, and the read waits for it.

### 7.3 Failure tracking for each file

The log's rule that only the log syncs its files (D-075 item 3) would be lifted by a file layer that knows, for each file, whether a write-back of it failed since a change was made, whichever handle asks. **This layer cannot give that, and `O_DIRECT` is not the way to it.** Its writes are `StdVfs`'s, and a direct write would report the data only (section 7.1).

What could give it is in the layer that writes, and is not built: by item 2 of section 3, a descriptor that the file layer itself keeps open for a file from the first open of it to the last close has a mark from before every change the process makes, so a sync through that descriptor reports every failed write-back of those changes, whichever handle made them. Kept in a table by device and inode number, that would make "a sync through any handle of this process" as good as "a sync through the writing handle" on Linux, at the price of one more descriptor for each open file and a table that every open and close goes through. On macOS nothing of it is established (section 3). Whether to build it is IO-01's and the owner's to decide, not this section's.

The segments and the value log do not need it: each syncs its files through the handle that appended (S2, S3). For them it would rest the rule on the record that one kernel keeps, which D-196 item 1 ruled against, and it would cost one more descriptor for every file the two stores have open. Nor could a test prove it at the simulator's strictest setting: with `Told::Writers`, a descriptor that made none of the changes is told of no failure.

### 7.4 What tests cover it

The conformance suite of `lignin-io` runs on `DirectVfs` on the files of the system (`linux/tests.rs`). The simulator has no file that is read past a cache, so no crash test runs on this layer, and no test makes a write-back fail under a direct read: section 7.2 is from the kernel's source. What a crash may leave behind is what the contract allows after the same calls on `StdVfs`, because every write, sync and change of names is `StdVfs`'s call. What it does leave can differ: a direct read can have the system write a range out sooner than it would have.
