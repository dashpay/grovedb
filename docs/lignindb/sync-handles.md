# Which handle syncs a file

LigninDB task IO-01, follow-up `IO-01-F2` part A ([plan](https://github.com/dashpay/grovedb/issues/1052); D-174 items 6 and 7).

This document has three things: the limit of what a sync can promise, the rule the engine keeps because of it, and a list of every place in `lignin-store` and `lignindb` that makes a file durable, with the handle that wrote and the handle that syncs. The list was read at commit `a0fac9ff`. Lines are of that commit unless a row names a branch.

## 1. The limit

The file layer's contract has it as the last rule under "Durability" (`lignin_io::vfs`):

> A write-back can fail unreported. The system writes a file's changes to storage in its own time, before any sync asks for them: that is write-back, and it can fail without a `Vfs` call reporting it. A write lost to a write-back error can cease to be visible to reads. Process exit or termination does not itself discard buffered writes; those writes can still be lost to a write-back error. And a sync on a reopened file can succeed after both the unwritten data and the error record are gone.

Four sentences of the contract hold only absent a failed write-back, and say so: a read sees every write that returned; a process that exits or is killed loses nothing it wrote; a sync that returns `Ok` on any handle covers the changes made before it; and with everything synced a crash loses nothing.

The simulator (`lignin_io::sim`) has no write-back that fails. A sync through any handle covers there what a sync through the writing handle covers. **So no crash test of the project can find a break of the rule below.** The list in section 4 was made by reading.

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
| S2 | **A segment's blocks and its footer.** Written by write-back, by the cleaner and by recovery's replay. | The writer's append handle, from S1 (`segment/store.rs:582`): a block at `segment/writer.rs:104`, the footer at `:151` | **The segment's read handle**, which is opened beside the append handle when the file is created (`segment/store.rs:584`): `Segment::sync` at `segment/store.rs:337`, the seal at `:353`. Reached from write-back (`flush/write_back.rs:1175`, `:1188`), the cleaner (`cleaner/clean.rs:692`, `:709`), replay (`recovery/replay.rs:728`, `:744`, `:861`) and `SegmentStore::sync` (`segment/store.rs:612`, from `table/checkpoint.rs:491` and `flush/checkpoint.rs:390`). | **Does not hold in its words**: see below the table. |
| S3 | **A value-log file's records.** Written by the placing of values and by the cleaner of the value log. | The writer's append handle, from S1 (`values/store.rs:661`): `values/writer.rs:170` | **The file's read handle**, opened beside the append handle (`values/store.rs:663`): `ValueFile::sync` at `values/file.rs:314`. Reached from `values/writer.rs:186` and `:195`, `flush/place.rs:122`, `flush/write_back.rs:793`, `cleaner/values.rs:390`, `values/clean.rs:433` and `ValueStore::sync` (`values/store.rs:706`). | **Does not hold in its words**: as S2. |
| S4 | Segments and value-log files of an earlier session, as recovery and the open of a checkpoint adopt them | The session that wrote them | Nobody. They are opened to be read (`segment/store.rs:115`, `values/file.rs:63`) and are closed or sealed for this session, so its syncs pass them by (`segment/store.rs:329`, `values/file.rs:301`). They are read up to the manifest's valid length, which their own session synced (S2, S3) before a manifest named it. | Holds: nothing is synced through the new handle. |
| S5 | The log, in a file this session created | One handle: created at `wal/store.rs:828`, appended to at `wal/file.rs:200` | The same (`wal/file.rs:204`, from `wal/store.rs:1397`). The log keeps it for as long as it has the file. Every sync of the log comes here: a commit (`lignindb/src/commit/committer.rs:604`), `Db::flush` (`lignindb/src/db/mod.rs:974`), the open (`lignindb/src/db/open.rs:550`), a checkpoint's step 6 (`lignindb/src/checkpoint/create.rs:453`), write-back (`flush/checkpoint.rs:79`, `:90`), the cleaner (`cleaner/values.rs:210`). | Holds. |
| S6 | The log, in a file of an earlier session whose end no manifest vouches for (recovery's adoption) | The session that wrote it | The file is not synced (`wal/store.rs:1395-1396`). Its bytes are written again: read through the adopted handle (`:1472`), appended to `wal/<id>.log.tmp` through a new handle (`:1466`), which syncs them (`:1478`); then the rename (`:1455`) and `sync_dir` of `wal/` (`:1459`). | Holds for the new file. What is read is section 6. |
| S7 | The manifest: a replacement, and a first manifest (`install`, `segment/manifest.rs:215-228`) | The handle that creates `MANIFEST.tmp` (`:242`; the bytes at `:245`) | The same (`sync_all`, `:247`), before the rename (`:223`). The name: `sync_dir` (`:226`). `ManifestStore::open` reads the manifest through a new handle and syncs the directory (`:106-108`): that sync is for the name, which a crash could still take back. The content was synced through its writer before it had the name. | Holds. |
| S8 | A table checkpoint's records | The append handle from S1 (`table/checkpoint.rs:411`; `:564`) | The same (`sync_data`, `:624`). It is dropped after the write returned (`:421`). Loading one (`:662`) syncs nothing. | Holds. |
| S9 | The name of a store's directory (`list_or_create_dir`, `segment/files.rs:112`) | - | `sync_dir` of the database directory | A name. No file's content rests on it. |

**S2 and S3.** Each segment and each value-log file has two handles from its creation: one appends, and the other reads and syncs. The append handle can be closed first: `SegmentStore::sync` and `ValueStore::sync` also sync a file whose writer was dropped. So these two are the places where a file is synced through another handle than the one that wrote.

- **What a failed write-back could lose, on Linux: nothing that was found.** The syncing handle is opened before the first block or record is appended and is open until the sync, so by item 2 of section 3 its sync returns the error, and the store then fails the file as after any failed sync (`segment/store.rs:338`, `values/file.rs:321-325`). The header, which was written before the read handle was opened, is synced through the append handle (S1).
- **On macOS it is not known**, as nothing of section 3 is.
- **Whose unit:** STORE-02 for the segments, STORE-09 for the value log. Two ways to end the difference: the store syncs through the append handle, or one handle appends, reads and syncs, as the log has it (S5). Or the rule is worded for what Linux gives: "through a handle that has been open since before the change". Which of the three it is to be is not decided here.

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
| T1 | The crash harness's `Session` (`lignin-testkit/src/crash/store.rs:367`) | It holds the stores of `lignin-store` and calls the file layer for no sync of its own: the sync of a commit (`:1290`) and `sync_log` (`:1970`) are the log's (S5), and a table checkpoint is write-back's (`:1965`). Its files are those of S1 to S8. Its file layer (`Observer`, `lignin-testkit/src/crash/observe.rs:262`) passes each sync to the handle it wraps (`:357-362`) and each directory sync to the simulator (`:302-306`), but for the syncs that a planted defect of the campaign leaves out (`:303-305`, `:329-331`), and has the set as G1. | As the stores, S2 and S3 with them. It runs on the simulator, which cannot show the difference (section 1). |

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

Sentences of the engine's documents that say more than that, and that hold absent a failed write-back:

| Where | The sentence | Whose |
| --- | --- | --- |
| `disk-full-and-backup.md:53-55` (DF1) | "After a close, also one that answered an error, and with no power cut since, it is the latest version that was published." | OPS-08 |
| `disk-full-and-backup.md:162-163` | "the next open recovers the database at the latest version that was published, or, after a power cut, at one that DF1 allows." | OPS-08 |
| `disk-full-and-backup.md:491-494` | "A sync of the log or of a segment was refused while the device was full, and that stays failed for the session. Nothing that was published is lost by it. Without a power cut the next open has every commit". A sync that the device refused is a write-back that failed, and what the system keeps in memory of the refused bytes is not promised. | OPS-08 |

`lignin-store/src/wal/mod.rs:302-312` already gives the reason for S6 in these terms: a session "may have had a sync of it fail and ended with no crash".
