<!-- This chapter is docs/lignindb/operator/messages.md, written here by lignin-tools/tests/operator_docs.rs: edit that file, then run `LIGNIN_OPERATOR_DOCS=write cargo test -p lignin-tools --test operator_docs`. -->

# Messages at an open, a checkpoint and a recovery

This chapter lists the errors and the report lines that a person who runs
a node on GroveDB's LigninDB engine can meet when a database is opened,
when a checkpoint is made or removed, when a session ends and the
database is opened again, and when `lignin` and `lignin migrate` run. For
each it says what it means and what to do. It is part of task OPS-05 of
the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).

Each message is quoted from the code, with `…` where the code puts a
value (a path, a number, the words of an error underneath). The column
**Code** names the file the words are in; a test of the repository
(`lignin-tools/tests/operator_docs.rs`) finds every quoted message in that
file, so a message that changes in the code changes here too.

The engine writes no log of its own: everything it has to say is in the
error of the call that met it, and in its counts (`Db::stats`, which
`lignin stats` prints).

## How a message reaches you

An error is a chain: each layer says what it was doing, then `: `, then
the words of the layer below. Read it from the left: **the first part
this chapter lists says what to do**, unless its row sends you to the
row of the words after it; the parts after it say where, down
to what the system answered ("No space left on device (os error 28)",
"Permission denied (os error 13)").

Through GroveDB, an error of the LigninDB engine is a storage error with
the engine's words after `lignindb: `:

| Message | What it is | Code |
| --- | --- | --- |
| `storage_cost error: …` | GroveDB's error for a failure of its storage | `grovedb/src/error.rs` |
| `storage error: …` | the storage layer's error, with the engine's words | `storage/src/error.rs` |
| `lignindb: …` | the words of the LigninDB engine follow | `grovedb/src/engine/lignin/error.rs` |
| `not supported: …` | a limit of the engine, or a name it refuses, followed by its words | `grovedb/src/error.rs` |
| `conflict: another commit wrote a key after this transaction first wrote it, or took a value that a write of this transaction keeps` | a commit conflict: the application makes its transaction again. No action of yours. | `storage/src/error.rs` |
| `not durable: the commit happened, but it is not known to survive a crash` | a commit was published and the sync it asked for failed; the session takes no write any more ([When a session ends](#when-a-session-ends)) | `storage/src/error.rs` |
| `unsettled: the engine could not find out whether the commit happened` | the session takes no write any more; the next open settles whether the commit is there ([When a session ends](#when-a-session-ends)) | `storage/src/error.rs` |

Through the tool, an error is a line that begins `lignin: ` on the
standard error, and the exit status is 1; arguments that are no
subcommand give the same line, then the usage, and the status 2.

## At an open

An open is `Db::open`: what a node does when it starts, what an open of
a checkpoint on LigninDB does (`GroveDb::<LigninEngine>::open_checkpoint_with_engine`,
or `GroveDb::open_checkpoint` where LigninDB is the build's default
engine), and what every `lignin` subcommand but
`manifest`, `segments` and `migrate` does. Through `lignin` the words
come after `the database did not open: `, behind the tool's `lignin: `.

The first group says that the directory is not one this build opens.
Each is found before anything is written: **the directory is as it was**,
for a directory that nothing else changes while it is being opened
(`OpenError::left_untouched`).

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… holds no database` | the directory has no database, and the open was told not to make one (`lignin` and `open_checkpoint` never make one) | check the path | `lignindb/src/db/error.rs` |
| `… was written by another storage engine (…: …)` | the directory holds an entry that only another engine makes (RocksDB's `CURRENT`, `IDENTITY`, `MANIFEST-…`, `OPTIONS-…` or an `.sst` file), or a `MANIFEST` that is no LigninDB file | point the node at the LigninDB database; a RocksDB database is migrated first ([the runbook](migration.md)) | `lignindb/src/db/error.rs` |
| `… is a LigninDB database of format version …, which this build does not read` | the manifest names a format version this build does not read: a newer one, where a later GroveDB wrote the database, or one that no build writes, as 0 | where a later release wrote the database, run it; otherwise the manifest is damaged: restore from a backup, and report it | `lignindb/src/db/error.rs` |
| `… has files of a database (…) and no manifest` | the files of a database without the manifest that says what they hold: a copy that was cut short, or files removed by hand | restore from a backup; a cut-short copy is removed and made again ([backup](backup-and-full-disk.md#what-a-cut-short-copy-or-checkpoint-leaves)) | `lignindb/src/db/error.rs` |
| `… lists … as something other than a regular file (a named pipe is not opened and a symbolic link is not followed): no database is opened or made` | `LOCK`, `MANIFEST`, `RESERVE` or a file the manifest names is a link, a pipe or a directory | put the real file back; the engine follows no link | `lignindb/src/db/error.rs` |
| `… lists … as something other than a directory of its own (a symbolic link is not followed): no database is opened or made beside it` | `wal`, `seg`, `val` or `table` is a link or a file | the four directories are the database's own and cannot be links to elsewhere | `lignindb/src/db/error.rs` |
| `… was changed while it was being opened: the database that was seen there is gone, or the lock file that was taken is not the one at the path any more. No database is opened or made there` | something removed, replaced or emptied the directory during the open | find what did, and open again | `lignindb/src/db/error.rs` |
| `… holds an entry named MANIFEST.deleting, the name a removal gives the manifest of what it removes: no database is opened or made beside it` | the directory holds that entry. A deletion of a checkpoint renames the manifest to that name before it removes any file, so where no `MANIFEST` is beside it and the entry is a manifest, a deletion may have been cut short; with a `MANIFEST` beside it, no deletion left it. The names alone do not say that a deletion was meant | as [the backup chapter](backup-and-full-disk.md#what-a-cut-short-copy-or-checkpoint-leaves) says: finish the deletion only where you know that this directory was being deleted; otherwise delete and rename nothing, and restore from a backup. With a `MANIFEST` beside it, remove and rename nothing either: either entry can be the database's manifest, and the deletion goes by the manifest it finds and would remove the whole database | `lignindb/src/db/error.rs` |
| `… has a final file name with the suffix reserved for checkpoint creation (.creating, ignoring ASCII case): no database was opened or made` | the path is where a checkpoint is built before it gets its name, and is never opened | open the checkpoint's own path once its creation has returned | `lignindb/src/db/error.rs` |
| `… holds a database and no LOCK file (a copy left it out, or it was removed), and the file is made in no directory that holds a database` | a copy left `LOCK` out | with no process having the directory open, put an empty file `LOCK` there ([backup](backup-and-full-disk.md#check-a-backup-and-restore-one)) | `lignindb/src/db/error.rs` |
| `… has pages of … bytes, and … were asked for` | the options name a page size the database was not made with | leave `page_size` unset ([tuning](tuning.md)): a database keeps its page size | `lignindb/src/db/error.rs` |

The rest:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… is open in another handle` | another process (or another handle of this one) has the database open: a node that runs, a second instance, or `lignin` | stop the other process; run `lignin` on a stopped node or on a checkpoint | `lignindb/src/db/error.rs` |
| `cannot … …: …` | the file layer refused an operation: the action (the row below), the path and the system's words | fix what the system says (permissions, a device that is full or read-only), and open again | `lignindb/src/db/error.rs` |
| `lock`, `list`, `create`, `look at`, `sync the directory` | the action that follows `cannot` in `cannot … …: …`: what the open asked of the file layer | as for `cannot … …: …` | `lignindb/src/db/open.rs` |
| `manifest: …` | the manifest could not be read, written or replaced; the words of [the files](#the-files-at-an-open) follow | as those words say | `lignindb/src/db/error.rs` |
| `recovery: …` | the store could not be recovered; the words of [the recovery](#in-a-recovery) follow | as those words say | `lignindb/src/db/error.rs` |
| `the first commit of the database (…): …` | the first commit of a new database failed. The next open makes it again, unless its group is whole in the log: then the recovery of that open has it | fix what the words after it say, and open again | `lignindb/src/db/error.rs` |
| `the catalog`, `the data column`, `the aux column`, `the roots column`, `the meta column`, `the counts`, `the directory`, `the first manifest`, `the group`, `the install`, `the keyspace of unlinked pages`, `the log`, `the page ids`, `the sync of the log`, `the version` | the step that follows `the first commit of the database (`: what of a new database's first commit failed | as for `the first commit of the database (…): …` | `lignindb/src/db/open.rs` |
| `the record of the keyspaces: …`, or `the record of the keyspaces: …: …` | the record of where the flat keyspaces are could not be read, or is not what this build writes; the words after it say which | where they are the system's, fix what they say and open again; where the record is not what this build writes, open with the GroveDB release that wrote the database, and if that is this one, keep the directory, restore from a backup, and report it | `lignindb/src/db/error.rs` |
| `the first page of the store is not the catalog's`, `it has no layout`, `its layout is not one this build reads`, `a keyspace has no root page`, `the subtree directory` | the reason that follows `the record of the keyspaces: `: what of the record, or of the subtree directory it names, could not be read or is not what this build writes. After `the subtree directory` come the subtree directory's own words ([below](#the-words-of-the-layers-below)) | as for `the record of the keyspaces: …` | `lignindb/src/db/open.rs` |
| `what earlier drops left` | the reason that follows `the record of the keyspaces: ` where the record of the drops that earlier sessions had not reclaimed could not be read | as for `the record of the keyspaces: …` | `lignindb/src/db/mod.rs` |
| `cannot start the background work: …` | the system refused a thread, or the thread of write-back ended before its loop ran; the words after it say which | where the system refused it, raise the process's thread limit and open again; otherwise open again, and report it where it comes again | `lignindb/src/db/error.rs` |
| `the thread of write-back ended before its loop was seen to run` | after `cannot start the background work: `: write-back's thread ended at once | open again; report it where it comes again | `lignindb/src/db/mod.rs` |
| `… was changed while it was being opened, and a lock file may have been made in it: …` | a new database was to be made, and something wrote into the directory meanwhile | find what did; the words after it say what the directory is now | `lignindb/src/db/error.rs` |

Through `EitherEngine`, the engine chosen at the open, these come before
either engine is called, and change nothing. GroveDB's own deletion of a
checkpoint has one more:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| ``the directory … holds a database of … (its entry `…`) and … was chosen for it: it is not opened, and nothing was written to it`` | the configuration chose one engine and the directory holds the other's database | choose the engine the directory holds, or migrate it | `grovedb/src/engine/either.rs` |
| ``the directory … holds entries of both engines (`…` is …'s and `…` is …'s): it is opened as neither`` | files of both engines in one directory | find out which is the database; nothing opens it as it is | `grovedb/src/engine/either.rs` |
| `the directory … could not be listed to see which engine's database it holds: …` | the listing failed; the system's words follow | fix what they say (permissions, a path that is no directory) | `grovedb/src/engine/either.rs` |
| ``the path … ends in `…`; it cannot be used for a RocksDB open for the caller or for checkpoint creation`` | a RocksDB open or a checkpoint creation was given a path whose last name ends in `.creating` | give another path: that name is where LigninDB builds a checkpoint | `grovedb/src/engine/either.rs` |
| `the directory … has no entry of a database of either engine, so it is no checkpoint: no engine was called for it` | a checkpoint was to be opened or removed at a directory that holds neither engine's database | check the path | `grovedb/src/engine/either.rs` |
| ``the directory … holds a database of … (its entry `…`) and was to be removed as a checkpoint of …: no engine was called for it`` | the deletion of a checkpoint was given the other engine's database | check the path and the engine; nothing was removed | `grovedb/src/engine/either.rs` |
| `refusing to delete checkpoint: path too short (safety check)` | after `data corruption error: `, though nothing of a database was read: GroveDB's deletion of a checkpoint refuses a path of fewer than two components before it asks the engine | give the checkpoint's full path | `grovedb/src/checkpoints.rs` |

### The files at an open

These follow `manifest: `, `recovery could not go on: ` or
`the log was not recovered: `. They name a file of the database.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… has no MANIFEST` | the manifest is gone | restore from a backup | `lignin-store/src/segment/error.rs` |
| `a replacement of the manifest of … did not finish; open it again` | a replacement of the manifest was cut off, and which of the two is in force is found out by an open | open again | `lignin-store/src/segment/error.rs` |
| `… is … bytes long, shorter than its valid length of …` | a file is shorter than the manifest says it was made durable at: it was cut, or copied in part | restore from a backup | `lignin-store/src/segment/error.rs`, `lignin-store/src/wal/error.rs` |
| `… has format version …, newer than the manifest's …` | a file of a newer build sits in an older database | restore from a backup | `lignin-store/src/segment/error.rs`, `lignin-store/src/wal/error.rs` |
| `… is … bytes long, but the manifest does not name it and records no id that high` | a file of data is in one of the database's directories, under an id above every id that the manifest records, and the manifest does not name it: the open does not know whose it is. A copy that put an older `MANIFEST` beside newer files leaves this as well | do not move or remove the file to make the open pass, and do not open the directory again until you know where the file came from. A directory that mixes the files of two states is no database: restore it from a backup | `lignin-store/src/segment/error.rs`, `lignin-store/src/wal/error.rs` |
| `the manifest no longer names …, but it could not be deleted: …` | a file the database let go of could not be removed | fix what the system says (permissions), and open again | `lignin-store/src/segment/error.rs`, `lignin-store/src/wal/error.rs` |
| `… at offset …: …` | a file does not decode where it should: damage | restore from a backup, and report it | `lignin-store/src/segment/error.rs`, `lignin-store/src/wal/error.rs` |
| `cannot … …: …` | the file layer refused an operation on the manifest, a segment or a log file: the action, the path and the system's words | fix what the system says (permissions, a device that is full or read-only), and open again | `lignin-store/src/segment/error.rs`, `lignin-store/src/wal/error.rs` |
| `… is damaged at offset …, below its valid length of …: …` | a log file is damaged inside what was made durable | restore from a backup, and report it | `lignin-store/src/wal/error.rs` |
| `the valid length … of … lies inside what begins at offset …` | the manifest's length of a log file ends inside a record: damage | restore from a backup, and report it | `lignin-store/src/wal/error.rs` |
| `the manifest vouches for … bytes of …, and the log ends before that file` | a log file that the manifest needs is missing | restore from a backup | `lignin-store/src/wal/error.rs` |
| `the header of … says it is log file …` | a log file under another file's name | restore from a backup | `lignin-store/src/wal/error.rs` |
| `is locked`, `has another writer` | after the path of a file: the default file layer found the file's lock held by another handle, in this process or another. For `LOCK`, a process has the database open | find the process that has the database open, and open it from one at a time | `lignin-io/src/std_fs.rs` |
| `is a directory`, `is not a directory` | after a path: the default file layer found a directory where a file belongs, or the other way round | find out what put it there before anything else; where it is a database's file, restore from a backup | `lignin-io/src/std_fs.rs` |
| `an earlier sync of this file failed (…), so this handle can no longer make it durable` | a sync of the file failed earlier, and the file layer refuses to say later ones succeeded: the system's words of the first failure are in the brackets | as for a failed sync: open again once the cause is gone ([when a session ends](#when-a-session-ends)) | `lignin-io/src/std_fs.rs`, `lignin-io/src/linux.rs` |
| `an earlier append to this file failed (…) and could not be undone (…), so this handle no longer knows where the file ends`, `the file is … bytes long where … were expected, so something else changed it` | an append failed and its bytes could not be taken back, or the file's length changed under the handle | open again once the system's words in the brackets are dealt with; where something else changed the file, find out what before anything else | `lignin-io/src/std_fs.rs` |
| `append needs a file opened with OpenMode::Append`, `write_at needs a file opened with OpenMode::Write`, `set_len needs a file opened with OpenMode::Write or OpenMode::Append` | the engine asked the default file layer for an operation its handle was not opened for | a defect: report it with the whole message | `lignin-io/src/std_fs.rs` |
| `… cannot be opened for direct reads: …` | the application gave the store the file layer with direct reads (`lignin_io::linux::DirectVfs`, Linux), and the file system does not take them for that file, or reports an alignment for them that the file layer cannot use (`the system reports the alignment …`); the words after it say which | open with the default file layer, or put the database on a file system that takes direct reads | `lignin-io/src/linux.rs` |
| `the system reports the alignment …` | the reason that follows `… cannot be opened for direct reads: …` where the system reports an alignment of direct reads that the file layer cannot use | as for `… cannot be opened for direct reads: …` | `lignin-io/src/linux.rs` |
| `… is a directory`, `… needs a file that was opened to write` | the file layer with direct reads was asked to open a directory as a file, or to write through a file it opened to read | a defect: report it with the whole message | `lignin-io/src/linux.rs` |
| `the io_uring instance of a batch of reads was given up, and it is not known whether the system made this read or what it brought`, `an earlier operation on this file panicked` | a read of the file layer with direct reads cannot be known to have been made, or an operation on the file failed in the middle | open again; report it where it comes again | `lignin-io/src/linux.rs`, `lignin-io/src/std_fs.rs` |
| `memory for direct reads cannot be kept out of a fork: …`, `a mapping at address 0` | the file layer with direct reads could not set up the memory a read goes through; the system's words follow the first | open again with the default file layer, and report it with the whole message | `lignin-io/src/linux/ring.rs` |
| `direct reads at offset … twice ended at offset …, before the end of the file` | two direct reads of a file in a row brought no new bytes before its end, so the file layer gave up on it | open again; where it comes again, open with the default file layer and report it | `lignin-io/src/linux/aligned.rs` |
| `every log file id has been used` | the log has used the last id a log file can have (2³² − 2) | a database that is built again (a state sync, or a migration from a backup) begins its ids again | `lignin-store/src/wal/error.rs` |

### Other words of the log and the segments

These follow the same prefixes, or `the commit was not made: ` and
`the log: `, or `write-back: `. Most say that a rule of the engine did not
hold, which no operator's action causes: **report them with the whole
message**. The ones that say otherwise are marked.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `an append to … failed, and the item is not in the log: …` | the file layer refused an append; the commit or record is not in the log, and no crash brings it back. The words after it say why, as "No space left on device" on a full device | [make room](backup-and-full-disk.md#what-to-do-in-this-order) where the device is full; otherwise as the system's words say | `lignin-store/src/wal/error.rs` |
| `an append to … failed and left the file as long as it was, and a crash can still bring the item back: …` | the same, where a crash before the next sync can still bring the item back | the same | `lignin-store/src/wal/error.rs` |
| `an append to … failed, and it is not known whether the item is in the log: …` | the log cannot tell whether the item is in it, and takes nothing more in this session; the next open settles it | open again once the cause is gone | `lignin-store/src/wal/error.rs` |
| `an append to … failed, and no manifest names the file closed yet` | the log waits for a manifest that names the file closed before it goes on in its next file; write-back's next turn or the next commit makes it, which needs a little room | make room where the device is full | `lignin-store/src/wal/error.rs` |
| `the log has no file that takes items since an append to … failed: …` | the same wait, with the system's answer to the append | the same | `lignin-store/src/wal/error.rs` |
| `the log has no file that takes items` | the same wait, before the log has turned to a file | the same | `lignin-store/src/wal/error.rs` |
| `a sync of … failed, so the log cannot be made durable in this session: …` | a sync of the log failed: nothing appended after the last sync that succeeded can be made durable in this session, and the session takes no write | open again once the cause is gone ([When a session ends](#when-a-session-ends)) | `lignin-store/src/wal/error.rs` |
| `a sync of segment … failed` | a sync of a segment failed in this session | the same | `lignin-store/src/segment/error.rs` |
| `the reader of the log has failed before` | a read of the log met an error before, which the first message said | as that one says | `lignin-store/src/wal/error.rs` |
| `manifest of … passed to the log of …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `the end of the log that was passed and the manifest disagree on log file …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `the log already has file … to turn to` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `the log has no file to turn to` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `no manifest names log file … yet` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `a group written for offset … of log file …, ` then `where the log now ends at offset … of file …` or `where the log now has no file that takes items` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `the record at offset … of what was offered may not stand at the end of the log: …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `… bytes were offered as one item, and the first item in them is … bytes long` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `what was offered is not …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `the log is to be made durable through offset … of file …, and ` then `ends at offset … of file …` or `holds nothing` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `the log is to be kept from offset … of file …, and is durable ` then `up to offset … of file …` or `nowhere` | the log is not durable up to the place it was to be kept from, so no file before it was dropped; this follows a sync that failed | as for the failed sync | `lignin-store/src/wal/error.rs` |
| `a place of another log, or of an earlier session` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `no log file …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `no log file in use holds the address …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/wal/error.rs` |
| `… bytes of … from offset … on do not fit in memory here` | an item of the log is longer than this machine can hold in memory | open on a machine with more memory; report it | `lignin-store/src/wal/error.rs` |
| `a value whose records take … bytes does not fit in memory here` | a value is longer than this machine can hold in memory | the same | `lignin-store/src/wal/error.rs` |
| `… already has a MANIFEST` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `invalid manifest replacement: …` | a new manifest would undo what the one in force says; one of the six reasons below follows | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `the page size is …, and the store was created with …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `the last … file id is lower than it was` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `… file … is new, but its id is not above the last id of its kind` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `… file … has a lower valid length than it had` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `… file … was closed or sealed and has another state or length` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `page-table checkpoint … has another length or commit version than it had` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `… has pages of … bytes and was opened with pages of …` | the options name another page size than the store was made with | leave `page_size` unset ([tuning](tuning.md)) | `lignin-store/src/segment/error.rs` |
| `manifest of … passed to the segment store of …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `no segment …` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `… bytes at offset … of segment … are not inside its blocks, which end at …` | a link of the page table points behind the blocks of a segment | damage: restore from a backup, and report it | `lignin-store/src/segment/error.rs` |
| `segment … is not sealed` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `no manifest names segment … yet` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `segment … takes no more blocks` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `segment … is full` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `segment … still has a writer` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |
| `every segment id has been used` | the segment store has used the last id a segment can have | a database that is built again (a state sync) begins its ids again | `lignin-store/src/segment/error.rs` |
| `the segment store is read-only` | a rule of the engine does not hold | report it with the whole message | `lignin-store/src/segment/error.rs` |

### The words of the layers below

These come last in a message, after one of the messages above that says
which part of the store met them. The message before them says what to
do where it says so; otherwise the row here does.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `unsupported format version …` | the header of a file names a format version this build does not read: a newer one, where a later GroveDB wrote the file, or one that no build writes, as 0 | where a later release wrote the database, run it; otherwise the file is damaged: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/error.rs` |
| `not a LigninDB file: bad magic` | a file in the database's directories is no LigninDB file | do not open the directory again until you know where the file came from; restore from a backup | `lignin-format/src/error.rs` |
| `truncated: need … bytes, have …`, `checksum mismatch: stored …, computed …`, `unknown file kind code …`, `… file with a sequence number that is no file id`, `record payload of … bytes exceeds the maximum of …`, `page delta: …`, `flat page: …`, `segment: …`, `manifest: …`, `page-table checkpoint: …`, `tree page: …`, `value log: …`, `log: …`, `page size … is not 8192, 16384, 32768 or 65536`, `identifier field holds no identifier`, `file id field holds the reserved id`, `reserved field is not zero`, `unknown record kind code …` | the bytes of a file are not what the format allows: a checksum that does not match, a length that does not fit, a field that holds what no writer writes. The words before them name the file, the place and the kind of record | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/error.rs` |
| `a … file, not a manifest`, `file header sequence is …, expected 0`, `length is …, expected …`, `page size … is not 8192, 16384, 32768 or 65536`, `unknown file state code …`, `entry breaks a rule for its file kind or state`, `live files are not in ascending order of kind and id` | why a manifest does not decode: these follow `manifest: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/manifest.rs` |
| `a … file, not a log`, `a log file that does not begin with a start record`, `a start record that is not the first record`, `a log file that starts at address …`, `a log file that starts at address … after version …, where the log before it ends at … after version …`, `offset … is no place in a log file`, `a log address past the end of the log`, `a record of kind … with a payload of … bytes`, `part … of a value of … bytes`, `part … of a value where part … belongs`, `a part of a value with … bytes, where the part has …`, `a part of another value inside a value`, `a value whose last part is missing`, `a record of kind … where a value must stand`, `a value of … bytes where the pending value states …`, `a value owned by page …, read for a cell of page …`, `version 0 where the version of a commit must stand`, `a commit group of version … after the group of version …`, `part … of a group where part … belongs`, `a part of another commit inside a group`, `a record of kind … inside a commit group`, `the log ends inside a commit group`, `a part of a group with no entry`, `a part of a group that the next entry fits into`, `a group of more parts than can be numbered`, `entries out of order`, `a value behind the first entry of its group`, `a run of … bytes exceeds the maximum of …`, `a background change that no page step makes`, `a page freed with version …, where the latest commit is …`, `a checkpoint mark that covers version …, where the latest commit is …`, `a flat run with no record`, `an image that is not the first record of its flat run`, `an image of … bytes that states a length of …`, `a field of … bytes in a record of a flat run`, `the place of a value out of line in … bytes`, `a range of fewer than two entries`, `a flat run that starts from no image, and no page`, `a run or an image of page … where one of page … belongs`, `a change for the other kind of flat page`, `a key that must be in the page is not`, `a key that must not be in the page is`, `a range that runs past the last entry`, `no entry holds the place the value is moved from` | why a log file or one of its records does not decode: these follow `log: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/wal.rs` |
| `buffer of … bytes for a page of …`, `page length … is below the …-byte header`, `page length … exceeds the capacity of …`, `slot directory ends at …, past the page length …`, `the last slot of the directory is free`, `slot … does not lead to a cell`, `the cell of slot … does not start where the cell before it ends`, `cells end at …, before the image length …`, `… unused bytes among the cells of an image`, `page states … unused bytes among its cells, and has …`, `page states … cells, and has …`, `the cell of slot … starts inside another cell`, `cell of … bytes exceeds the maximum of …`, `… of … bytes exceeds the maximum of …`, `cell of … bytes given as … bytes`, `page full: need … bytes, … are free`, `slot … already holds a cell`, `slot … holds no cell`, `the cell of slot … has no … link`, `the cell of slot … has no hosted-root pointer …`, `the cell of slot … points at no cell of its own page`, `a link of the cell of slot … states a wrong length for its child's key`, `the cell of slot … cannot be replaced by another key's cell`, `two cells of the page point at the cell of slot …`, `the fragment-root mark of slot … disagrees with the page's links` | why a tree page does not decode: these follow `tree page: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/tree_page.rs` |
| `a … file, not a page-table checkpoint`, `file header carries id …, expected …`, `record kind code … is not allowed here`, `length is …, expected …`, `unknown link kind code …`, `link breaks a rule for its kind`, `links are not in ascending order of page id and version`, `the kinds of a page's links do not form a chain`, `end record counts … links, the records hold …`, `end record's newest version is not the newest version of the links`, `counts are not in ascending order of file id`, `end record counts … counts, the records hold …`, `the place in the log is not one a checkpoint mark ends at` | why a page-table checkpoint does not decode: these follow `page-table checkpoint: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/table.rs` |
| `a … file is not a value log`, `record payload of … bytes is shorter than its … bytes of fields`, `a value of … bytes has no part …`, `part carries … bytes, expected …`, `found part … where part … belongs`, `part names another owner or value length than part 0`, `pointer states a value of … bytes, the record one of …`, `value is owned by page …, not by page …`, `an empty value has no record`, `value of … bytes is longer than the value log stores`, `no value starts at offset …` | why a value-log file or one of its values does not decode: these follow `value log: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/value_log.rs` |
| `image length … is below the …-byte header`, `… page with level …`, `prefix and slot directory end at …, past the image length …`, `slot … does not describe a cell`, `out-of-line value … is … bytes long, not the … of a value pointer`, `key … is not greater than the key before it`, `cells end at …, before the image length …`, `capacity … exceeds the maximum of …`, `key does not start with the page prefix`, `page full: need … bytes, capacity is …` | why a flat page does not decode: these follow `flat page: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/flat_page.rs` |
| `a … file, not a segment`, `file header carries id …, expected …`, `record kind code … is not allowed here`, `length or offset is …, expected …`, `LZ4 block of … bytes is not smaller than its … raw bytes`, `compressed bytes do not decompress to the stated length` | why a segment or one of its blocks does not decode: these follow `segment: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/segment.rs` |
| `a run with no record`, `a create record that is not the first of its run`, `a run or a base image of page … applied to page …`, `a chain that does not create its page, and no base image` | why a run of page deltas does not decode: these follow `page delta: ` | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-format/src/delta.rs` |
| `cannot … …: …`, `the manifest no longer names …, but it could not be deleted: …` | the file layer refused an operation on a page-table checkpoint; the system's words follow | fix what they say, and open again | `lignin-store/src/table/error.rs` |
| `every checkpoint id has been used` | every page-table checkpoint id has been used | a database that is built again (a state sync) begins its ids again | `lignin-store/src/table/error.rs` |
| `… at offset …: …`, `page …: …`, `… is … bytes long, shorter than its valid length of …`, `… has format version …, newer than the manifest's …`, `… covers version …, but the manifest says …`, `the checkpoint points to a block that ends at offset … of segment …, whose valid length is …`, `the checkpoint points into segment …, which the manifest does not name` | a page-table checkpoint, or what it points to, is not what the manifest and the log say | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-store/src/table/error.rs` |
| `the checkpoint needs the log durable through version …, and it is durable through …`, `the log ended at offset … of log file … when the checkpoint's last chain had been copied, and it is durable up to offset … of log file …`, `the log ended at offset … of log file … when the checkpoint's last chain had been copied, and none of it is durable`, `a checkpoint that covers version … cannot replace one that covers …`, `the sample of the value counts was taken at offset … of log file …, and the log ended at offset … of log file … before the first chain was copied`, `the sample of the value counts was taken at offset … of log file …, and the log had no record before the first chain was copied` | a table checkpoint was not installed: the log is not durable as far as it needs, or it would replace a newer one. Write-back's next turn tries again, and the one in force stands | nothing, unless it comes again with a sync that failed: then as that message says | `lignin-store/src/table/error.rs` |
| `… bytes at offset … of segment … cannot be a block`, `a link of version … cannot follow the chain's newest, of version …`, `a delta needs a base image beneath it`, `the page is already freed`, `no link of the chain points to offset … of segment …`, `the page table holds as many links as it can` | the page table refused a link to a block | report it with the whole message; open again, and restore from a backup where it comes again | `lignin-store/src/table/error.rs` |
| `there is no such value-log file`, `it ends past the end of its file`, `its file's count of live values is already zero`, `the bytes its file counts as live do not include it`, `no durable manifest covers it`, `its rewrite was counted and not reported as installed` | the reason, after `the item at … makes a cell hold a value at offset … of value-log file …: `, why that value is not where the cell says | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-store/src/values/error.rs` |
| `cannot … …: …`, `the manifest no longer names …, but it could not be deleted: …` | the file layer refused an operation on a value-log file; the system's words follow | fix what they say, and open again | `lignin-store/src/values/error.rs` |
| `a sync of value-log file … failed` | a sync of a value-log file failed: the file is closed where it was durable, and the values behind that are appended again elsewhere; the session goes on | where the device is full, [make room](backup-and-full-disk.md#what-to-do-in-this-order) | `lignin-store/src/values/error.rs` |
| `a value whose records take … bytes does not fit in memory here` | a value is longer than this machine can hold in memory | open on a machine with more memory; report it | `lignin-store/src/values/error.rs` |
| `every value-log file id has been used` | every value-log file id has been used | a database that is built again (a state sync) begins its ids again | `lignin-store/src/values/error.rs` |
| `… at offset …: …`, `manifest of … passed to the value log of …`, `… is … bytes long, shorter than its valid length of …`, `… has format version …, newer than the manifest's …`, `the header of … says it is value-log file …`, `… is … bytes long, but the manifest does not name it and records no id that high`, `value-log file … has … live values, but the manifest does not name it`, `the count of value-log file … was too low, and the manifest does not name the file`, `no value-log file …`, `a value of … bytes at offset … of value-log file … is not inside its values, which end at …`, `no durable manifest covers the value at offset … of value-log file …`, `no manifest names value-log file … yet`, `value-log file … takes no more values`, `value-log file … is still being written`, `the value log is read-only`, `a writer, copies, a mark or a retirement of another value log, or of an earlier session`, `the value at offset … of value-log file … cannot be counted: …`, `value-log file … has … live values whose owners were not found in the pages their records name`, `the owner of a value could not be asked: …` | a value-log file, or what the manifest or the pages say of it, is not what it must be | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-store/src/values/error.rs` |
| `… could not be read for page …: …` | a block of a page could not be read; the words after it say why | fix what the system says, and open again; damage where they say the bytes are not what the format allows | `lignin-store/src/load/error.rs` |
| `the block at offset … of segment …`, `the store's pages are … bytes and the pool's frames …`, `page … has no version at …: the commit of version … freed it`, `no segment holds a version of page … at …`, `… holds a … where the chain of page … has a …`, `… does not give page …: …`, `… holds … bytes for page …, whose image is … long`, `… is a run of deltas for page …, which is a flat page`, `… is a run that creates page …, in a segment`, `… is a run of … bytes for page …, and a run of this store has at most …`, `… leaves page … with a value that is not in the value log`, `the buffer pool refused the page: …` | a page's blocks do not give the page that the page table and the log say | damage: restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-store/src/load/error.rs` |
| `no page id is left` | every page id has been used | a database that is built again (a state sync) begins its ids again | `lignin-store/src/alloc/error.rs` |
| `page id … is …, not held by an open transaction`, `page id … is …, not a committed page`, `page id … cannot be freed at version 0`, `the page table holds page … at version …, and recovery landed on version …`, `replay reported page … for version …, and recovery landed on version …`, `replay reported page … for version 0` | the page ids of the store are not what the page table and the log say | report it with the whole message; open again, and restore from a backup where it comes again | `lignin-store/src/alloc/error.rs` |
| `version …`, `no version`, `page … was loaded as born at version … for a reader at version …`, `page … was loaded as born at version … for a reader at version …, and it was superseded at version …`, `the handle belongs to another buffer pool`, `an operation still holds the latch of the private copy of page …`, `… already holds a private copy of page …`, `the frame of page … is not a private copy any more`, `a private copy of page … made from … was installed over …`, `a commit of version … cannot follow … of page …`, `page … as born at version … is not waiting to be written back`, `page … as born at version … still has attached values`, `the frame of page … as born at version … is no page version of the pool any more: a page step replaced it, or its page was freed and the pool dropped it`, `page … as born at version … is the current version of its page`, `the frame is not the current version of its page`, `an open write transaction holds a private copy of the page`, `the new frame belongs to another buffer pool` | the buffer pool refused what it was handed: a rule of the engine does not hold | report it with the whole message; open again, and restore from a backup where it comes again | `lignin-store/src/cache/error.rs` |
| `page … cannot be written back: …` | write-back cannot write a page; it writes every other page and takes no table checkpoint, so the log grows ([backup and full disk](backup-and-full-disk.md#the-log-grows-and-does-not-shrink)) | open again; where the same page fails again, restore the database from a backup, keep the damaged directory, and report it with the whole message | `lignin-store/src/flush/error.rs` |
| `the store's pages are … bytes and the pool's frames …`, `the page table refused the block of page … at offset … of segment …: …`, `a page step on page … was refused after its record was written: …`, `a pending value of page … as born at version … has no bytes of its length`, `a sample of the value counts was taken at offset … of log file …, before the sample at offset … of log file …`, `the values of page … were not regrouped: …` | write-back met what a rule of the engine does not allow | report it with the whole message; open again, and restore from a backup where it comes again | `lignin-store/src/flush/error.rs` |
| `the cleaner's segments: …`, `the cleaner's values: …`, `write-back, for the cleaner: …`, `the block of page … at offset … of segment … could not be read: …`, `the block of page … at offset … of segment … is not what its tag says: …`, `the link of page … to offset … of segment … was not pointed at offset … of segment …: …` | the cleaner could not read or move a block, or a value; the words after it say why | where they are the system's, fix what they say; otherwise report it with the whole message; open again, and restore from a backup where it comes again | `lignin-store/src/cleaner/error.rs` |
| `flat page: …`, `page load: …`, `buffer pool: …`, `value log: …`, `page ids: …` | a flat keyspace (one that holds keys in order and no Merk; the subtree directory is kept in one) met an error of the layer these name: the format of a flat page, the loader, the buffer pool, the value log or the page ids; that layer's words follow | as their row says | `lignin-tree/src/flat/error.rs` |
| `flat page …: …` | a page of a flat keyspace is not what the keyspace's tree needs: the page and the reason follow | do not open the directory again; restore the database from a backup, and report it with the message | `lignin-tree/src/flat/error.rs` |
| `a value of … bytes is longer than the value log holds`, `an inline limit of … bytes is not between … and …`, `page … holds a pending value whose bytes are not with it`, `page … was freed by this writer` | a flat keyspace was asked for what its rules refuse: a value longer than a value can be, an inline limit out of its bounds, or a page that its own writer had freed or whose pending value it does not hold | report it with the message | `lignin-tree/src/flat/error.rs` |
| `a leaf without the entry its slots name` | the reason that follows `flat page …: …`: what of the page is wrong | as for `flat page …: …` | `lignin-tree/src/flat/cursor.rs` |
| `a bucket of long keys that names no page` | the reason that follows `flat page …: …`: what of the page is wrong | as for `flat page …: …` | `lignin-tree/src/flat/long_key.rs` |
| `a branch where a leaf is expected`, `a leaf where a branch is expected`, `a branch without the child its slots name`, `a branch without the separator its slots name` | the reason that follows `flat page …: …`: what of the page is wrong | as for `flat page …: …` | `lignin-tree/src/flat/node.rs` |
| `two entries that hold one pending value`, `an image longer than a frame` | the reason that follows `flat page …: …`: what of the page is wrong | as for `flat page …: …` | `lignin-tree/src/flat/store.rs` |
| `the image of another page`, `a page that is not one level below the branch that names it`, `a tree of more than 255 levels`, `a leaf and a branch below one branch`, `a branch that is not as it was read` | the reason that follows `flat page …: …`: what of the page is wrong | as for `flat page …: …` | `lignin-tree/src/flat/tree.rs` |
| `subtree directory: …` | the flat keyspace the subtree directory is kept in failed; its words follow (`flat page …: …` and the rows above) | as their row says | `lignin-tree/src/directory/error.rs` |
| `subtree directory record ` | a record of the subtree directory is not what its key says, or does not agree with another: the record's key in hexadecimal and the reason follow | do not open the directory again; restore the database from a backup, and report it with the message | `lignin-tree/src/directory/error.rs` |
| `the directory has no header` | the reason that follows `subtree directory record ` and the record's key: what of the record is wrong | as for `subtree directory record ` | `lignin-tree/src/directory/read.rs` |
| `a header that is not an id`, `a child record that is not a prefix and an id`, `a child record under no position and key`, `not a prefix`, `a dropped Merk's record that is not an address`, `not an id`, `an entry that ends early`, `an entry with no id`, `a root address that is not one`, `a root key with no root`, `an ancestor with no id`, `bytes after the entry`, `a host with no Merk above it, or Merks above with no host` | the reason that follows `subtree directory record ` and the record's key: what of the record is wrong | as for `subtree directory record ` | `lignin-tree/src/directory/record.rs` |
| `an id that was not given out`, `the handle has other dropped Merks`, `an id that another Merk has`, `a root in the cell of a dropped Merk`, `a root in the cell of another Merk`, `a root that has no record`, `a root record of no entry`, `a record of no kind`, `a child record that no Merk leads to`, `an entry that no Merk leads to`, `a Merk that two hosts have`, `a Merk with no node hosts a Merk`, `an entry under the record of another host`, `an entry that names another parent`, `an entry with other Merks above it`, `a left-over entry that a lookup returns`, `a Merk in use hosts a Merk with no entry`, `a left-over record of a Merk that is entered` | the reason that follows `subtree directory record ` and the record's key (the check of `lignin verify`): what of the record is wrong | as for `subtree directory record ` | `lignin-tree/src/directory/check.rs` |
| `a Merk in use hosts a Merk that has no entry`, `a Merk is reached twice walking the Merks a move covers` | the reason that follows `subtree directory record ` and the record's key: what of the record is wrong | as for `subtree directory record ` | `lignin-tree/src/directory/rekey.rs` |
| `a hosted Merk whose entry names no host`, `the Merk has its root elsewhere` | the reason that follows `subtree directory record ` and the record's key: what of the record is wrong | as for `subtree directory record ` | `lignin-tree/src/directory/write.rs` |
| `every Merk id was given out` | every Merk id has been used | a database that is built again (a state sync) begins its ids again | `lignin-tree/src/directory/error.rs` |
| `a Merk is entered under the prefix …`, `no Merk is entered under the prefix …`, `the host's Merk, under the prefix …, is not entered`, `the host in the Merk under the prefix … has a Merk at position …`, `the Merk under the prefix … would move below itself`, `the Merk under the prefix … has no host, and is moved to no other`, `the Merk under the prefix … is below a moved one, and the move names no prefix for it`, `the Merk under the prefix … has no root and Merks below it`, `no Merk has its root at …`, `two Merks would have their root at …`, `the Merk under the prefix … is named twice in one change`, `… of … bytes is longer than 255 bytes`, `a Merk is more than 65535 Merks deep`, `the Merk under the prefix … is in use, and no left-over`, `records of Merks below the dropped Merk … are still there`, `no dropped Merk has the id …` | a change asked of the subtree directory breaks one of its rules (a prefix is given by its first four bytes) | report it with the message | `lignin-tree/src/directory/error.rs` |
| `subtree drop: …` | the reclaiming of a dropped subtree, or the open's reading of the drops that earlier sessions left, met an error of a layer below; that layer's words follow. A step of the reclaiming that failed and changed nothing is made again later | as the row of those words says | `lignin-tree/src/drop/error.rs` |
| `the step on page … is in the log and was not made: …`, `reclaiming was given up after a step that broke` | a step of the reclaiming that the log holds could not be made: the reclaiming of that subtree is given up for the session, and what the subtree left stays where it is, holding its space | open again; where it comes again, keep the directory as it is and report it with the message | `lignin-tree/src/drop/error.rs` |
| `what a dropped subtree left in page …: …` | a page that a dropped subtree left is not what a drop leaves: the page and the reason follow | do not open the directory again; restore the database from a backup, and report it with the message | `lignin-tree/src/drop/error.rs` |
| `record of unlinked pages: …` | a record of the pages that a range delete left does not decode: the reason follows | do not open the directory again; restore the database from a backup, and report it with the message | `lignin-tree/src/drop/error.rs` |
| `the Merk under the prefix ………….. is dropped with a Merk it is below`, `every id of a record of unlinked pages was given out` | a drop named a Merk together with one it is below, or every id of a record of unlinked pages was used | report it with the message | `lignin-tree/src/drop/error.rs` |
| `no cell where a pointer of the subtree leads`, `a cell that another page names is not marked as the root of its fragment`, `a cell that two pointers name`, `a pointer that names a free slot of its page`, `a cell that its page names is marked as the root of a fragment`, `one value pointer that two hold`, `a branch that names no child`, `no page where a cell of the subtree was`, `no tree page where a cell of the subtree was`, `the image of another page`, `a cell that stays names a cell of the subtree`, `a flat tree that names no page`, `no entry where one was read`, `an image longer than a frame`, `no flat page where a flat tree names one`, `a page that is not one level below the branch that names it`, `a branch where a leaf must be`, `a value out of line that is no place`, `a bucket of long keys that names no page` | the reason that follows `what a dropped subtree left in page …: …`: what of the page is wrong | as for `what a dropped subtree left in page …: …` | `lignin-tree/src/drop/step.rs` |
| `a branch that names no child`, `a page that the pages a range delete left name twice` | the reason that follows `what a dropped subtree left in page …: …`: what of the page is wrong | as for `what a dropped subtree left in page …: …` | `lignin-tree/src/drop/flat.rs` |
| `a flat tree that names no page`, `a branch that names no child`, `a page that the pages of a flat tree name twice`, `a bucket of long keys whose tree begins at a page of the tree it is in` | the reason that follows `what a dropped subtree left in page …: …`: what of the page is wrong | as for `what a dropped subtree left in page …: …` | `lignin-tree/src/drop/column.rs` |
| `a cell that two cells of the subtree name` | the reason that follows `what a dropped subtree left in page …: …`: what of the page is wrong | as for `what a dropped subtree left in page …: …` | `lignin-tree/src/drop/merk.rs` |
| `a key that is kept in a bucket` | the reason that follows `record of unlinked pages: …`: what of the record is wrong | as for `record of unlinked pages: …` | `lignin-tree/src/drop/step.rs` |
| `a key that is no number`, `a record with no version`, `a version that is none`, `pages that are not whole`, `a page that is none`, `a level that is none`, `a keyspace with no header` | the reason that follows `record of unlinked pages: …`: what of the record is wrong | as for `record of unlinked pages: …` | `lignin-tree/src/drop/flat.rs` |
| `a lead that leaves a prefix no room` | the reason that follows `record of unlinked pages: …`: what of the record is wrong | as for `record of unlinked pages: …` | `lignin-tree/src/drop/column.rs` |

## In a recovery

Every open is a recovery: it loads the table checkpoint the manifest
names, replays the log on it up to the last commit whose group the log
holds whole, and begins a log file of its own. It lands on one committed
version. These words follow `recovery: `.

Two kinds are told apart (`RecoveryError::is_damage`). **The first
group below** says which part of the store could not be read, and the
words after it say why: what the system answered to a file operation
(fix what it says, and open again), the buffer pool's refusal of its
size (below, with what to do), or the words of [the files](#the-files-at-an-open),
whose own rows say what to do. A message of that group that comes again
at the next open, with nothing for the system to fix and no option of
the open to change (the buffer pool's size among them), is damage as
well, but for ids that are used up, whose rows say to build the database
again.
**Damage** (the second group) is a store whose files do not hold what
they must: restore the database from a backup, keep the damaged
directory, and report it with what `lignin verify` says of it (where
that opens it at all).

| Message | What it means | Code |
| --- | --- | --- |
| `recovery could not go on: …` | the manifest or a segment could not be read; [the files](#the-files-at-an-open) say which | `lignin-store/src/recovery/error.rs` |
| `the table checkpoint did not load: …` | the page-table checkpoint could not be read | `lignin-store/src/recovery/error.rs` |
| `the value log was not recovered: …` | a value-log file could not be read | `lignin-store/src/recovery/error.rs` |
| `the log was not recovered: …` | a log file could not be read; [the files](#the-files-at-an-open) say which | `lignin-store/src/recovery/error.rs` |
| `the buffer pool refused a rebuilt page: …` | the buffer pool could not be made, or refused a page that replay rebuilt; the words after it say which, below | `lignin-store/src/recovery/error.rs` |
| `a buffer pool of … bytes does not hold one frame of … bytes` | after the line above: `cache_size` is below one frame, which is a page and what the pool keeps with it; the second number is the least size. Give a larger one ([tuning](tuning.md#memory)) | `lignin-store/src/cache/error.rs` |
| `the buffer pool is too large to allocate` | after the line above: `cache_size` is more than one allocation can be. Give a smaller one | `lignin-store/src/cache/error.rs` |
| `the buffer pool refused the rebuilt frame of page …: …` | the pool refused a page that replay rebuilt: a defect, to report with the whole message | `lignin-store/src/recovery/error.rs` |
| `the page ids were not recovered: …` | the record of page ids could not be read | `lignin-store/src/recovery/error.rs` |
| `the store's pages cannot be read: …` | the loader of pages could not be made | `lignin-store/src/recovery/error.rs` |
| `page …, which the item at … changes, could not be read: …` | a page that the log changes could not be read | `lignin-store/src/recovery/error.rs` |
| `the page table refused a link of page … that replay made: …` | the page table refused what the log replays | `lignin-store/src/recovery/error.rs` |

Damage:

| Message | What it means | Code |
| --- | --- | --- |
| `the item at … does not apply to page …: …` | a logged change does not fit its page | `lignin-store/src/recovery/error.rs` |
| `page … is no page of its format as the log leaves it: …` | a page that replay made does not decode | `lignin-store/src/recovery/error.rs` |
| `the log ends at commit …, and the table checkpoint covers commit …` | the log is behind the checkpoint: log files are missing | `lignin-store/src/recovery/error.rs` |
| `the log goes on from commit …, and the table checkpoint covers commit …: the commits between them are missing` | commits between the checkpoint and the log are missing | `lignin-store/src/recovery/error.rs` |
| `the table checkpoint holds … links of versions after …, the last commit of the log` | the checkpoint is ahead of the log | `lignin-store/src/recovery/error.rs` |
| `no checkpoint mark ends at …, the place of the table checkpoint's sample` | the log does not hold the mark the checkpoint names | `lignin-store/src/recovery/error.rs` |
| `the mark that ends at … is of a checkpoint that covers commit …, and the table checkpoint covers commit …` | the log's mark is of another checkpoint | `lignin-store/src/recovery/error.rs` |
| `the log changes … pages that the table checkpoint has no image of and that no later commit freed or made again, page … among them` | the log changes pages that the store does not have | `lignin-store/src/recovery/error.rs` |
| `the item at … makes a cell hold a value at offset … of value-log file …: …` | a cell names a value that the value log does not hold | `lignin-store/src/recovery/error.rs` |
| `the reclaiming step at … frees page …, which is in use and does not hold what the step took out of it` | a logged reclaiming step does not fit its page | `lignin-store/src/recovery/error.rs` |
| `a pending value of page … is named by the log and not carried in it` | the log names a value it does not hold | `lignin-store/src/recovery/error.rs` |

A place in the log is written `offset … of log file …`.

## When a session ends

A session ends when the store meets something it cannot make good
without being opened again: a refused sync of the log or of a segment,
a commit whose fate is unknown, or a log with no file id left. From then
on every commit is refused with the same reason and every read is
answered. **What to do is the same for each but one: close the database
and open it again** (stop the node and start it), once the cause is gone
(for a full device, [make room](backup-and-full-disk.md#what-to-do-in-this-order)).
The one is a log with no file id left (`every log file id has been used, and the log can be given no further file`):
an open needs a new log file, and where the manifest has recorded the
last id as used, the next open fails too. Keep the handle that reads
until the database is built again, as its row says.
Nothing that was made durable is lost; the open lands on a committed
version, which can be earlier than the last one published after a
refused sync.

| Message | What it means | Code |
| --- | --- | --- |
| `the session takes no write, and the database has to be opened again: ` | the start of every such refusal; the reason follows | `lignindb/src/db/error.rs` |
| `a sync of the log failed with version … published (…)` | the log makes nothing durable any more in this session | `lignindb/src/db/error.rs`, `lignindb/src/commit/error.rs` |
| `a sync of a segment failed, and no table checkpoint is installed until then` | write-back could not make a segment durable | `lignindb/src/db/error.rs` |
| `every log file id has been used, and the log can be given no further file` | the log's ids are used up; where the manifest has recorded the last id as used, the next open fails for the same reason ([the files](#the-files-at-an-open)), so keep the handle that reads | `lignindb/src/db/error.rs` |
| `whether commit … is in the log is not known (…)` | a commit's group may or may not be in the log; the next open settles it | `lignindb/src/commit/error.rs` |
| `commit … is in the log and could not be installed (…)` | the commit is in the log, and the next open recovers it where its bytes reached the device: it can have been refused before the log was synced, so a power cut can lose it. GroveDB answers that commit as `unsettled: the engine could not find out whether the commit happened`; a write after it is refused before it begins, with the reason that stopped the session, as a storage error. The same words come with a later commit that found such a group in the log (`the log would give the commit version …, and the latest published version is …`): that later commit failed and did not happen, and the words are of the earlier one | `lignindb/src/commit/error.rs` |

A commit that meets the end itself says so in words of its own:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `the session takes no commit: …` | the commit was refused because the session had ended | open again | `lignindb/src/commit/error.rs` |
| `commit … is published and not known to be durable: …` | published; its sync failed | open again | `lignindb/src/commit/error.rs` |
| `whether commit … is in the log is not known, and the session takes no further commit: …` | as above | open again | `lignindb/src/commit/error.rs` |
| `commit … is in the log and could not be installed, and the session takes no further commit: …` | as above | open again | `lignindb/src/commit/error.rs` |
| `the commit was not made: …` | the commit did not happen and never will; the session goes on, but where the words after it are `the log would give the commit version …, and the latest published version is …` (below), after which it takes no commit | as the row of the words after it says: for the version gap, open again (its row below); for a full device, [make room](backup-and-full-disk.md#what-to-do-in-this-order); where those words are a defect's, report it. Where they have no row, nothing | `lignindb/src/commit/error.rs` |
| `the session takes no commit yet: …` | the session waits for the log to be cut after a failed append, or for a manifest in doubt to be settled; the transaction is whole | make room where the words say the device is full; where they name a directory that was removed or cannot be written, stop the node | `lignindb/src/commit/error.rs` |
| `the log would give the commit version …, and the latest published version is …` | the log holds a group the session never published: the session has stopped | open again | `lignindb/src/commit/error.rs` |
| `the log: …` | after `the commit was not made: `: the log refused the commit's group; where its words say the device is full, the session waits for room | where the device is full, [make room](backup-and-full-disk.md#what-to-do-in-this-order); otherwise as the row of the log's words, or the system's, says | `lignindb/src/commit/error.rs` |
| `the page table: …` | after `the commit was not made: `: the page table refused what the commit was to install | open again; report it if it comes again | `lignindb/src/commit/error.rs` |
| `a stale hash could not be made: …` | after `the commit was not made: `: a hash that was stale could not be made at the commit | report it with the whole message | `lignindb/src/commit/error.rs` |
| `the place of the group in the log was taken again and again while the commit held the lock on the value counts` | after `the commit was not made: `: the log gave the group a place and gave it away again, more often than a commit tries; something appends to the log without the lock the commit holds | a defect: report it with the whole message | `lignindb/src/commit/error.rs` |
| `a commit outside the transaction wrote a key after the transaction first wrote it` | a conflict: the application makes its reads and writes again (GroveDB answers it as its conflict, `Error::Commit(CommitError::Conflict)`) | nothing | `lignindb/src/commit/error.rs` |
| `the commit was refused: …` | a write of the transaction was refused at its commit; the words say why, and the transaction is open | where the words are the application's, nothing for the database; where they are the system's (an error of a file operation), fix what they say | `lignindb/src/commit/error.rs` |
| `a scope of the transaction is open` | the application committed a transaction whose write scope it had not ended | nothing for the database; the application's | `lignindb/src/commit/error.rs` |
| `the transaction has a pending mark left` | the application committed a transaction that a mark of it still waits in | nothing for the database; the application's | `lignindb/src/commit/error.rs` |

## At a close

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `write-back: …` | write-back failed at the close; the next open replays what the close did not cover | open again; if the words say the device is full, [make room](backup-and-full-disk.md#what-to-do-in-this-order) first | `lignindb/src/db/error.rs`, `lignin-store/src/flush/error.rs` |
| `a sync of a segment failed in this session, and no table checkpoint is installed until the store is opened again` | after `write-back: `: a segment could not be made durable in the session | open again | `lignin-store/src/flush/error.rs` |
| `the cleaner: …` | the cleaner could not close the files it was moving into; what it moved stands | open again | `lignindb/src/db/error.rs` |
| `segment …, which takes the moved blocks, could not be made durable: …` | after `the cleaner: `: the cleaner's segment could not be made durable | open again | `lignin-store/src/cleaner/error.rs` |
| `the … thread panicked` | a background thread of the engine panicked: what it held is as a crash leaves it | open again, and report it | `lignindb/src/db/error.rs` |
| `… was not found to hold this database any more: it was removed or replaced while the database was open, and the close finished nothing in it` | the directory was removed or replaced under the open handle | nothing for the database: it is gone from that path. A database whose directory was removed while it was open, as a checkpoint's can be, ends so | `lignindb/src/db/error.rs` |

## At a checkpoint

A checkpoint is made by `GroveDb::create_checkpoint(path)` and removed by
the deletion of the engine the node runs
([backup](backup-and-full-disk.md#back-up-a-running-node), step 4); on
LigninDB these are `Db::checkpoint` and `lignindb::checkpoint::delete`.
A creation that fails can leave nothing at `path`, or the whole
checkpoint there where its last steps could not be made durable nor
undone, and a part of one in `path.creating`: look at both before you
make it again, since a creation refuses a `path` that is there. Through
GroveDB the words follow `lignindb: `; a refusal for the name is
`not supported: `.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… is there already, and a checkpoint is made in a new directory` | the path is taken: a checkpoint, a file or an empty directory | give a new path. Remove nothing that is there unless you know what it is: the refusal says only that something has that name | `lignindb/src/checkpoint/error.rs` |
| `… has no directory above it to make the checkpoint in, or no name there` | the path has no directory above it, as `/`, or no name of its own, as one that ends in `..`; a directory above that is missing is answered by `could not … …: …` instead | give the path of a new directory, with its name, in a directory that is there | `lignindb/src/checkpoint/error.rs` |
| `… has a name that checkpoints are built under, and none is made there` | the path ends in `.creating` | give another name | `lignindb/src/checkpoint/error.rs` |
| `… is locked, as a directory that a checkpoint is being built in` | another creation at the same path is running | wait for it | `lignindb/src/checkpoint/error.rs` |
| `… was not found to be the directory whose lock is held, with what was put into it, and nothing was given the name of the target` | the build directory `….creating` is not what the creation locked and filled: it was replaced, holds an entry the creation did not make, or lacks one it made | find out what changed it before anything else; make the checkpoint again only where you can account for what that directory holds ([backup](backup-and-full-disk.md#what-a-cut-short-copy-or-checkpoint-leaves)), since a creation takes over what it finds there | `lignindb/src/checkpoint/error.rs` |
| `… is not listed as a file, and none was made there: the lock of its directory was not taken` | the entry `LOCK` of the build directory `….creating` is no file: something other than a creation made that directory, or put that entry there | remove nothing, and do not make the checkpoint at that path again until you can account for what the directory holds ([backup](backup-and-full-disk.md#what-a-cut-short-copy-or-checkpoint-leaves)): a creation takes over what it finds there | `lignindb/src/checkpoint/error.rs` |
| `the session takes no commit, and no checkpoint is made of it: …` | the session has ended ([When a session ends](#when-a-session-ends)); through GroveDB it ends `…: no checkpoint is made before the database is opened again` | open again | `lignindb/src/checkpoint/error.rs` |
| `…: no checkpoint is made before the database is opened again` | GroveDB's end of the two refusals that need an open | open again | `grovedb/src/engine/lignin/checkpoint.rs` |
| `the log could not be made durable: …` | the creation's sync of the log failed: the session has ended | open again | `lignindb/src/checkpoint/error.rs` |
| `the manifest was in doubt, and settling it answered an error: …` | a replacement of the manifest had not finished, and settling it failed | make the checkpoint again; make room first if the device is full | `lignindb/src/checkpoint/error.rs` |
| `the manifest could not be read: …` | the manifest was in doubt again when the creation read it | make the checkpoint again | `lignindb/src/checkpoint/error.rs` |
| `the log holds commit …, and the latest published version is …` | the log holds a commit the session never published, as after a commit that panicked between its append and its publication | open again | `lignindb/src/checkpoint/error.rs` |
| `the checkpoint's log ends at …, and the log is durable …`, its last words `up to …` or `nowhere` | the log's sync returned without covering where the checkpoint's log ends | make the checkpoint again; report it if it comes again | `lignindb/src/checkpoint/error.rs` |
| `log file … …` | a log file that the checkpoint's log needs, up to the file that holds its end, is not there or not closed and durable as the checkpoint needs it; the reason follows the file's id | make the checkpoint again; report it if it comes again | `lignindb/src/checkpoint/error.rs` |
| `lies before the end of the checkpoint's log and is not closed`, `is not durable through the end of the checkpoint's log`, `holds the end of the checkpoint's log and is not among the log's files` | the reasons that follow `log file … ` | as for `log file … …` | `lignindb/src/checkpoint/create.rs` |
| `the log: …` | the log refused to be held for the checkpoint, or to say what its files are; the log's words follow | as those words say; then make the checkpoint again | `lignindb/src/checkpoint/error.rs` |
| `… is not the plain name of a file in the database directory` | a file that the application asked a checkpoint to carry beside the store's is named otherwise than by one plain name | the application's | `lignindb/src/checkpoint/error.rs` |
| `the manifest of the checkpoint could not be written: …` | the checkpoint's manifest could not be encoded | report it | `lignindb/src/checkpoint/error.rs` |
| `could not … …: …` | the file layer refused an operation of the creation or the removal: the action (the row below), the path and the system's words | fix what the system says. Where a creation met it, make the checkpoint again. Where a deletion met it, delete again once the system's words are dealt with, and make no checkpoint for it: a deletion finishes what one cut short left, a `….creating` directory beside the path among it, and where nothing is left at the path it answers `… is not a directory of its own, and nothing was removed` | `lignindb/src/checkpoint/error.rs` |
| `append to`, `create`, `create the directory`, `link`, `list`, `lock`, `look at`, `look for`, `look for the lock file in`, `make durable what is in`, `measure`, `open`, `read`, `remove`, `remove the directory`, `rename`, `sync the directory`, `write` | the action that follows `could not` in `could not … …: …`: what the creation or the removal asked of the file layer | as for `could not … …: …` | `lignindb/src/checkpoint/files.rs` |

The deletion of a checkpoint refuses, and removes nothing, with:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… is not a directory of its own, and nothing was removed` | the directory above does not list the path's name as a directory: the path is a link, a file or nothing, or, on a file system that takes several spellings for one name, a spelling other than the one the directory lists | give the name as the directory above lists it (`ls`), the spelling the checkpoint was made with. Remove nothing by hand: what is there is not known to be a checkpoint | `lignindb/src/checkpoint/error.rs` |
| `… is no checkpoint, and nothing of it was removed: …` | the directory holds files and no manifest, or what is no database of this engine | look at it before anything removes it | `lignindb/src/checkpoint/error.rs` |
| `… is open, and nothing of it was removed` | a handle has the checkpoint open | close it first | `lignindb/src/checkpoint/error.rs` |

## lignin

The tool's own words, besides the engine's above.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `the database did not open: …` | the open failed: the words of [At an open](#at-an-open) follow | as they say | `lignin-tools/src/error.rs` |
| `… is no database: …` | the directory is no database that the tool reads; the reason follows | as the reason says | `lignin-tools/src/error.rs` |
| `its final name ends in .creating: it is a checkpoint that is being made, or the part of one that a creation cut short left, and it is never opened. Run lignin on the checkpoint's target once its creation has returned. The next creation of a checkpoint at that target takes over what a cut creation left, and lignindb::checkpoint::delete removes it` | the reason, for a path that ends in `.creating` | run `lignin` on the checkpoint's own path once its creation has returned | `lignin-tools/src/open.rs` |
| `it holds MANIFEST.deleting: a removal of it is under way, or one was cut short and some of its files may be gone. lignindb::checkpoint::delete finishes the removal. Where no removal made that entry and the database is to be kept, remove the entry and nothing else` | the reason, for a directory that holds that entry | as for the open's refusal of the same entry ([At an open](#at-an-open)): finish a deletion only where you know one was under way, and with a `MANIFEST` beside the entry remove nothing. The deletion the words name, `lignindb::checkpoint::delete`, is LigninDB's of [the backup chapter](backup-and-full-disk.md#delete-the-checkpoint-of-a-backup) | `lignin-tools/src/open.rs` |
| `this build of lignin has no …: build lignin-tools with the Cargo feature …` | `migrate` was asked of a build without the feature `migrate` | build as [the runbook](migration.md#2-set-the-variables) does | `lignin-tools/src/error.rs` |
| `the background work failed: …` | `lignin compact`: a turn of write-back or the cleaner failed; its report is written | as the words say | `lignin-tools/src/error.rs` |
| `the migration: …` | the refusals and failures of `lignin migrate`, [below](#lignin-migrate) | as they say | `lignin-tools/src/error.rs` |
| `… at byte …: …` | `lignin segments` or `pages`: a file the manifest names is not what the manifest says at that byte | damage: restore from a backup, and report it | `lignin-tools/src/error.rs` |
| `a page: …` | a page could not be read; the store's words follow | as they say; damage where they say a page does not decode | `lignin-tools/src/error.rs` |
| `a read: …` | a record, a node or an entry could not be read | as the words after it say | `lignin-tools/src/error.rs` |
| `a write: …` | `lignin load`: the write transaction refused a write of the dump | as the words after it say | `lignin-tools/src/error.rs` |
| `the commit: …` | `lignin load`: the commit failed; the words of [When a session ends](#when-a-session-ends) follow where it ended the session | as they say | `lignin-tools/src/error.rs` |
| `the dump: …` | `lignin load`: the file is no dump this build reads, or does not hold what `load` needs | give a dump of this build, into a database that has none of its subtrees | `lignin-tools/src/error.rs` |

`lignin verify` prints the checker's report and exits with 0, 3 or 4:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `checked …, …` | the first line, but for an `INCOMPLETE RESTORE: the root holds the mark` before it: where the check ran (`the still store, after the close`, or `beside the background work`) and whether the hashes were made again by Merk's rules (all but those the rules take as stored) | | `lignin-tools/src/verify.rs` |
| `sound: every rule holds` | every rule that the check holds holds. The exit status is 0, or 4 where the report also says `INCOMPLETE RESTORE: the root holds the mark`, which then decides | nothing where the status is 0; otherwise as the row of the incomplete restore says | `lignindb/src/verify/report.rs` |
| `INCOMPLETE RESTORE: the root holds the mark of a state-sync restore that did not finish. Its elements commit to Merks that were not restored, so the database is unsound by its own statement: discard it. No hash was made again.` | exit status 4: a state sync into this database did not finish, so it is unsound by its own statement | discard the database; sync again or restore from a backup | `lignin-tools/src/verify.rs` |
| `… value hashes of trees are the ones grove versions 1 to 3 left after a delete below them (GroveDB #686), not the ones the rules make` | counted after the report: value hashes of the kind those versions left, which the check takes as no damage. A cell does not say which version wrote it, so the check cannot tell such a hash from another that is the same | nothing where grove versions 1 to 3 wrote the database; where later versions alone wrote it, report it | `lignin-tools/src/verify.rs` |

A report with a rule that does not hold (exit status 3) says what does
not hold, and where. Keep the database as it is, restore from a backup,
and report it with the whole report.

## lignin migrate

The exit statuses are in [the runbook](migration.md#6-migrate). The run
writes these lines on its way; lines that begin `migrate:` are for
scripts and hold words of the form name=value.

| Message | What it means | Code |
| --- | --- | --- |
| `the source …: RocksDB identity …, last write …; …` | the first line: the source and its lock | `lignin-tools/src/migrate/mod.rs` |
| `no process has it open for writing, and none can open it while this runs` | the source's lock is held for the run | `lignin-tools/src/migrate/mod.rs` |
| `it has no LOCK file, so nothing keeps a process from opening it for writing while this runs: a file that such a process makes or changes before the target was compared is found then, and what it does later is not seen` | a source with no `LOCK` file: make sure nothing opens it | `lignin-tools/src/migrate/mod.rs` |
| `migrate: survey …` | the counts of the survey | `lignin-tools/src/migrate/mod.rs` |
| `the migration projects a peak of about … MiB of memory: … MiB for a commit and the buffer pool, and about … bytes with its key and its value for each node of the largest Merk it hands over, which it holds whole (…). …` | the projected peak, and the limit held | `lignin-tools/src/migrate/mod.rs` |
| `the source can be migrated` | the end of a survey that found nothing to refuse | `lignin-tools/src/migrate/mod.rs` |
| `the load needs about … MiB on the device of … while it runs; …` | the room the run needs, and what is free | `lignin-tools/src/migrate/mod.rs` |
| `going on with the load in …: … records were carried` | a run that goes on from where a stopped one stood | `lignin-tools/src/migrate/mod.rs` |
| `every record of the source that this migration carries reads the same from the target: … records. The walk of the grove came to … subtrees…. The root hash is …` | the comparison found no difference, and the target's root hash is the source's | `lignin-tools/src/migrate/mod.rs` |
| `not in the target: … records under the prefixes of pending drops, which … marks stand for` | records a pending drop was still to remove: left behind by rule | `lignin-tools/src/migrate/mod.rs` |
| `not in the target: the records under … prefixes that nothing leads to, which are left behind` | as `--unreachable leave` says | `lignin-tools/src/migrate/mod.rs` |
| `the target, checked after its close with every hash made again by Merk's rules:` | the physical check of the target follows | `lignin-tools/src/migrate/mod.rs` |
| `migrated … into …: loaded, compared and verified` | the last line of a run that succeeded | `lignin-tools/src/migrate/mod.rs` |
| `THE TARGET IS NOT ITS SOURCE: … differences. It is left in … and is not to be used` | exit status 3: keep the report and report it as a defect | `lignin-tools/src/migrate/mod.rs` |
| `THE TARGET IS NOT SOUND. It is left in … and is not to be used` | exit status 3: the same | `lignin-tools/src/migrate/mod.rs` |
| `THE DEVICE OF … IS FULL: the migration stopped (…). Make room and run the same command again: it goes on from where this run stood` | exit status 4 | `lignin-tools/src/migrate/mod.rs` |

Its refusals (exit status 1) follow `the migration: `, after the tool's
`lignin: `. Nothing
was written to the target unless the words say so.

| Message | What to do | Code |
| --- | --- | --- |
| `it holds no RocksDB database (no file CURRENT)` | give the RocksDB directory of the node | `lignin-tools/src/migrate/source.rs` |
| `its column families are …, and a GroveDB that this command reads has …` | the directory is a RocksDB database that is no GroveDB | `lignin-tools/src/migrate/source.rs` |
| `a process has it open. A migration is made offline: stop the node, or migrate a checkpoint of it` | stop the node | `lignin-tools/src/migrate/source.rs` |
| `a process opened it for writing while this command began to read it. A migration is made offline: stop the node, or migrate a checkpoint that nothing opens` | stop the node, and run again | `lignin-tools/src/migrate/source.rs` |
| `its LOCK is no file: it is no RocksDB database` | check the path | `lignin-tools/src/migrate/source.rs` |
| `its LOCK file: …` | the lock could not be probed; the system's words follow | `lignin-tools/src/migrate/source.rs` |
| `it changed while it was read (…): something wrote it. Nothing of this run is to be used` | something wrote the source during the run: stop it, and run again | `lignin-tools/src/migrate/source.rs` |
| `the source …: the records of … are not what they were when the migration first read them: something wrote the source` | the same | `lignin-tools/src/migrate/walk.rs` |
| `the source is not what its survey found: …: …` | the same | `lignin-tools/src/migrate/compare.rs` |
| `the source … holds what a migration does not carry (the lines NOT CARRIED above): nothing was written by this run` | the survey's `NOT CARRIED` lines name it; such a source stays on RocksDB | `lignin-tools/src/migrate/mod.rs` |
| `the source … projects a peak of about … MiB of memory, above the limit of … MiB (…). Nothing was written by this run: …` | the words after it say whether a larger `--memory-limit`, or a smaller `--batch` or `--cache`, can take it | `lignin-tools/src/migrate/mod.rs` |
| `the target … has no name of its own` | give a target with a last name | `lignin-tools/src/migrate/mod.rs` |
| `the target … has a name that ends in .creating, under which LigninDB opens no database: give it another name` | as it says | `lignin-tools/src/migrate/mod.rs` |
| `the target … and the source … are one inside the other: a migration writes nothing into its source` | put the target outside the source | `lignin-tools/src/migrate/mod.rs` |
| `the target … and …, where its database is made, are one directory` | give another target | `lignin-tools/src/migrate/mod.rs` |
| `the directory … that the target … is to be in is not there: make it first` | as it says | `lignin-tools/src/migrate/mod.rs` |
| `… is found by that name, and the directory … does not list it under that name: give the name as the directory has it` | give the target's name in the spelling the directory lists | `lignin-tools/src/migrate/mod.rs` |
| `the target … is there and …. A migration makes its database in … and gives it the name of the target at its end, once it was compared and verified: a database that a migration left at the target is whole` | a target that is there and holds anything is never written: an empty directory at the target is taken for none, and the migration's database takes its name. Where a run of this migration gave the database that name (a run stopped after its rename, or one whose name could not be made durable, leaves it so), the database is whole: keep it, and check it with `lignin verify` and step 7 of the runbook. Otherwise give another target; remove nothing you did not make | `lignin-tools/src/migrate/mod.rs` |
| `… is there and is not the work of a migration: …. Remove it, or give another target` | the work directory beside the target holds what the command does not take for its own work (the words after it say what); the command leaves it as it is. Give another target. Remove that directory only where you know what it holds and that nothing needs it ([step 6](migration.md#6-migrate)) | `lignin-tools/src/migrate/mod.rs` |
| `… holds a migration of another source, or of another state of this one (RocksDB identity …, last write …, root hash …). Remove that directory to migrate anew` | the work directory is of another run: remove it | `lignin-tools/src/migrate/mod.rs` |
| `the load in … was begun with --unreachable …, and goes on with that alone. Give it, or remove that directory to migrate anew` | give the choice the first run had | `lignin-tools/src/migrate/mod.rs` |
| `of the prefixes of the source that nothing leads to, … has … carried and nothing of …, and this run has --unreachable …. Give what the run that loaded it had, or remove that directory to migrate anew` | the same | `lignin-tools/src/migrate/mod.rs` |
| `the place that the journal of the target states does not fit the source: …. The target was made from another state of the source: remove the directory it is in and migrate anew` | the work directory was made from another state of the source: remove it | `lignin-tools/src/migrate/walk.rs` |
| `the journal of the target …: the directory was not made by this build of the command, or is damaged. Remove it and migrate anew` | as it says | `lignin-tools/src/migrate/journal.rs` |
| `the database in … was written after it was compared: something else opened it. Run the command again: it compares anew` | nothing else is to open the work directory | `lignin-tools/src/migrate/mod.rs` |
| `the target … is there, and … is this command's and holds no database: a run may have given the target its name and stopped before the name was durable. The syncs that make it so failed (…). Run the same command again` | as it says | `lignin-tools/src/migrate/mod.rs` |
| `the database is at …, loaded, compared and verified, and its name there could not be made durable (…): a power cut can take the name back and leave the database in …. Run the same command again: it makes the name durable and then refuses the target as one that is there, or, where a power cut took the name back meanwhile, goes on from that directory` | the one failure that leaves the database at `TARGET`: run the same command again, which makes the name durable | `lignin-tools/src/migrate/mod.rs` |
| `the work directory … was not removed (…): it holds nothing of the target, and can be removed` | a line after a run that succeeded: remove the work directory yourself | `lignin-tools/src/migrate/mod.rs` |
| `the source …: the grove is more than … trees deep at …: this command does not walk it` | the source is deeper than the migrator walks; it stays on RocksDB, and report it | `lignin-tools/src/migrate/walk.rs` |
| `the source …: its … keyspace holds the key …, which is shorter than the prefix of a subtree: it is no GroveDB that this command reads` | a key of the source is no GroveDB key | `lignin-tools/src/migrate/walk.rs` |
| `the source …: the root node of the root Merk is no node of a Merk: …` | the source's root is damaged | `lignin-tools/src/migrate/survey.rs` |
| `the source …: a record of the roots keyspace is no record of a root key` | the source holds a record GroveDB does not write | `lignin-tools/src/migrate/load.rs` |
| `a read of the target, of …: …` | the comparison could not read the target: run again; report it if it stays | `lignin-tools/src/migrate/compare.rs` |

The source's other refusals, which say that it is not a GroveDB the
migrator reads as one (a pending-drop record that does not decode, an
element that does not, a tree whose links do not hold), name the record
and the reason: such a source stays on RocksDB, and the report is a
defect to file with the survey.

## What this chapter leaves out

- Errors that GroveDB's operations give for what a caller asked (a path
  that is not there, a reference that does not resolve): they are the
  same on both engines, and are the application's.
- The refusals of a write transaction, a batch and a write scope
  (`lignindb::txn`, `lignindb::batch`, `lignindb::scope`), which answer
  the application's use of a transaction; GroveDB passes them on as its
  own errors, and a commit that they refuse leaves the database as it
  was.
- `lignindb::backup`, which no GroveDB call and no command uses yet: a
  backup is made as [backup](backup-and-full-disk.md) says.
- The words of the fragments that reclaiming walks
  (`lignin_tree::fragment`), which follow `subtree drop: ` where the
  reclaiming of a dropped subtree fails on one: they are not listed one
  by one, and the row of `subtree drop: …` says what to do.

Every message of the error types of the open, the recovery, a commit,
the end of a session, a close, a checkpoint, the tool, and of every
layer below them (the segment store, the log, the page table, the value
log, the loader, the page ids, the buffer pool, write-back, the cleaner,
the flat keyspaces, the subtree directory, the drops of subtrees and
the formats) is quoted above, and so is every reason the code hands to one of them as words: `lignin-tools/tests/operator_docs.rs`
names those types, and fails where one of their messages is not quoted.
