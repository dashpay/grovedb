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
this chapter lists says what to do**; the parts after it say where, down
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

Through the tool, every error is one line that begins `lignin: `, and the
exit status is 1 (2 for arguments that are no subcommand).

## At an open

An open is `Db::open`: what a node does when it starts, what
`GroveDb::open_checkpoint` does, and what every `lignin` subcommand but
`manifest`, `segments` and `migrate` does. Through `lignin` the words
come after `lignin: the database did not open: `.

The first group says that the directory is not one this build opens.
Each is found before anything is written: **the directory is as it was**.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… holds no database` | the directory has no database, and the open was told not to make one (`lignin` and `open_checkpoint` never make one) | check the path | `lignindb/src/db/error.rs` |
| `… was written by another storage engine (…: …)` | the directory holds an entry that only another engine makes (RocksDB's `CURRENT`, `IDENTITY`, `MANIFEST-…`, `OPTIONS-…` or an `.sst` file), or a `MANIFEST` that is no LigninDB file | point the node at the LigninDB database; a RocksDB database is migrated first ([the runbook](migration.md)) | `lignindb/src/db/error.rs` |
| `… is a LigninDB database of format version …, which this build does not read` | a newer GroveDB wrote this database | run the GroveDB release that wrote it, or a later one | `lignindb/src/db/error.rs` |
| `… has files of a database (…) and no manifest` | the files of a database without the manifest that says what they hold: a copy that was cut short, or files removed by hand | restore from a backup; a cut-short copy is removed and made again ([backup](backup-and-full-disk.md#what-a-cut-short-copy-or-checkpoint-leaves)) | `lignindb/src/db/error.rs` |
| `… lists … as something other than a regular file (a named pipe is not opened and a symbolic link is not followed): no database is opened or made` | `LOCK`, `MANIFEST`, `RESERVE` or a file the manifest names is a link, a pipe or a directory | put the real file back; the engine follows no link | `lignindb/src/db/error.rs` |
| `… lists … as something other than a directory of its own (a symbolic link is not followed): no database is opened or made beside it` | `wal`, `seg`, `val` or `table` is a link or a file | the four directories are the database's own and cannot be links to elsewhere | `lignindb/src/db/error.rs` |
| `… was changed while it was being opened: the database that was seen there is gone, or the lock file that was taken is not the one at the path any more. No database is opened or made there` | something removed, replaced or emptied the directory during the open | find what did, and open again | `lignindb/src/db/error.rs` |
| `… holds an entry named MANIFEST.deleting, the name a removal gives the manifest of what it removes: no database is opened or made beside it` | a removal of this directory (`GroveDb::delete_checkpoint`) was cut short | finish it with `GroveDb::delete_checkpoint`; if nothing removed it and the database is to be kept, remove that entry and nothing else | `lignindb/src/db/error.rs` |
| `… has a final file name with the suffix reserved for checkpoint creation (.creating, ignoring ASCII case): no database was opened or made` | the path is where a checkpoint is built before it gets its name, and is never opened | open the checkpoint's own path once its creation has returned | `lignindb/src/db/error.rs` |
| `… holds a database and no LOCK file (a copy left it out, or it was removed), and the file is made in no directory that holds a database` | a copy left `LOCK` out | with no process having the directory open, put an empty file `LOCK` there ([backup](backup-and-full-disk.md#check-a-backup-and-restore-one)) | `lignindb/src/db/error.rs` |
| `… has pages of … bytes, and … were asked for` | the options name a page size the database was not made with | leave `page_size` unset ([tuning](tuning.md)): a database keeps its page size | `lignindb/src/db/error.rs` |

The rest:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… is open in another handle` | another process (or another handle of this one) has the database open: a node that runs, a second instance, or `lignin` | stop the other process; run `lignin` on a stopped node or on a checkpoint | `lignindb/src/db/error.rs` |
| `cannot … …: …` | the file layer refused an operation: the word after `cannot` is `lock`, `list`, `create`, `look at` or `sync the directory`, then the path and the system's words | fix what the system says (permissions, a device that is full or read-only), and open again | `lignindb/src/db/error.rs` |
| `manifest: …` | the manifest could not be read, written or replaced; the words of [the files](#the-files-at-an-open) follow | as those words say | `lignindb/src/db/error.rs` |
| `recovery: …` | the store could not be recovered; the words of [the recovery](#in-a-recovery) follow | as those words say | `lignindb/src/db/error.rs` |
| `the first commit of the database (…): …` | the first commit of a new database failed. The next open makes it again | fix what the words after it say, and open again | `lignindb/src/db/error.rs` |
| `the record of the keyspaces: …` | the record of where the flat keyspaces are could not be read, or is not what this build writes | a damaged database: restore from a backup, and report it with what `lignin verify` says | `lignindb/src/db/error.rs` |
| `cannot start the background work: …` | the system refused a thread | raise the process's thread limit, and open again | `lignindb/src/db/error.rs` |
| `… was changed while it was being opened, and a lock file may have been made in it: …` | a new database was to be made, and something wrote into the directory meanwhile | find what did; the words after it say what the directory is now | `lignindb/src/db/error.rs` |

Through `EitherEngine`, the engine chosen at the open, two refusals come
before either engine is called, and change nothing:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| ``the directory … holds a database of … (its entry `…`) and … was chosen for it: it is not opened, and nothing was written to it`` | the configuration chose one engine and the directory holds the other's database | choose the engine the directory holds, or migrate it | `grovedb/src/engine/either.rs` |
| ``the directory … holds entries of both engines (`…` is …'s and `…` is …'s): it is opened as neither`` | files of both engines in one directory | find out which is the database; nothing opens it as it is | `grovedb/src/engine/either.rs` |

### The files at an open

These follow `manifest: `, `recovery could not go on: ` or
`the log was not recovered: `. They name a file of the database.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… has no MANIFEST` | the manifest is gone | restore from a backup | `lignin-store/src/segment/error.rs` |
| `a replacement of the manifest of … did not finish; open it again` | a replacement of the manifest was cut off, and which of the two is in force is found out by an open | open again | `lignin-store/src/segment/error.rs` |
| `… is … bytes long, shorter than its valid length of …` | a file is shorter than the manifest says it was made durable at: it was cut, or copied in part | restore from a backup | `lignin-store/src/segment/error.rs` |
| `… has format version …, newer than the manifest's …` | a file of a newer build sits in an older database | restore from a backup | `lignin-store/src/segment/error.rs` |
| `… is … bytes long, but the manifest does not name it and records no id that high` | a file that this database did not make is in one of its directories | move that file out of the database directory, and open again | `lignin-store/src/segment/error.rs` |
| `the manifest no longer names …, but it could not be deleted: …` | a file the database let go of could not be removed | fix what the system says (permissions), and open again | `lignin-store/src/segment/error.rs` |
| `… at offset …: …` | a file does not decode where it should: damage | restore from a backup, and report it | `lignin-store/src/segment/error.rs` |
| `… is damaged at offset …, below its valid length of …: …` | a log file is damaged inside what was made durable | restore from a backup, and report it | `lignin-store/src/wal/error.rs` |
| `the valid length … of … lies inside what begins at offset …` | the manifest's length of a log file ends inside a record: damage | restore from a backup, and report it | `lignin-store/src/wal/error.rs` |
| `the manifest vouches for … bytes of …, and the log ends before that file` | a log file that the manifest needs is missing | restore from a backup | `lignin-store/src/wal/error.rs` |
| `the header of … says it is log file …` | a log file under another file's name | restore from a backup | `lignin-store/src/wal/error.rs` |
| `every log file id has been used` | the log has used the last id a log file can have (2³² − 2) | a database that is built again (a state sync, or a migration from a backup) begins its ids again | `lignin-store/src/wal/error.rs` |

## In a recovery

Every open is a recovery: it loads the table checkpoint the manifest
names, replays the log on it up to the last commit whose group the log
holds whole, and begins a log file of its own. It lands on one committed
version. These words follow `recovery: `.

Two kinds are told apart (`RecoveryError::is_damage`). **A file
operation that failed** (the first group below) leaves the directory as
the next open expects it: fix what the system says, and open again. The
buffer pool's refusals are in that group too, and say what to do.
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
| `a buffer pool of … bytes does not hold one frame of … bytes` | after the line above: `cache_size` is below one page. Give a larger one ([tuning](tuning.md#memory)) | `lignin-store/src/cache/error.rs` |
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
answered. **What to do is the same for each: close the database and open
it again** (stop the node and start it), once the cause is gone (for a
full device, [make room](backup-and-full-disk.md#what-to-do-in-this-order)).
Nothing that was made durable is lost; the open lands on a committed
version, which can be earlier than the last one published after a
refused sync.

| Message | What it means | Code |
| --- | --- | --- |
| `the session takes no write, and the database has to be opened again: ` | the start of every such refusal; the reason follows | `lignindb/src/db/error.rs` |
| `a sync of the log failed with version … published (…)` | the log makes nothing durable any more in this session | `lignindb/src/db/error.rs` |
| `a sync of a segment failed, and no table checkpoint is installed until then` | write-back could not make a segment durable | `lignindb/src/db/error.rs` |
| `every log file id has been used, and the log can be given no further file` | the log's ids are used up; the next open fails for the same reason ([the files](#the-files-at-an-open)) | `lignindb/src/db/error.rs` |
| `whether commit … is in the log is not known (…)` | a commit's group may or may not be in the log; the next open settles it | `lignindb/src/commit/error.rs` |
| `commit … is in the log and could not be installed (…)` | the commit is in the log and the next open recovers it | `lignindb/src/commit/error.rs` |

A commit that meets the end itself says so in words of its own:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `the session takes no commit: …` | the commit was refused because the session had ended | open again | `lignindb/src/commit/error.rs` |
| `commit … is published and not known to be durable: …` | published; its sync failed | open again | `lignindb/src/commit/error.rs` |
| `whether commit … is in the log is not known, and the session takes no further commit: …` | as above | open again | `lignindb/src/commit/error.rs` |
| `commit … is in the log and could not be installed, and the session takes no further commit: …` | as above | open again | `lignindb/src/commit/error.rs` |
| `the commit was not made: …` | the commit did not happen and never will; the session goes on | nothing, unless the words say the device is full: then [make room](backup-and-full-disk.md#what-to-do-in-this-order) | `lignindb/src/commit/error.rs` |
| `the session takes no commit yet: …` | the session waits for the log to be cut after a failed append, or for a manifest in doubt to be settled; the transaction is whole | make room where the words say the device is full; where they name a directory that was removed or cannot be written, stop the node | `lignindb/src/commit/error.rs` |
| `the log would give the commit version …, and the latest published version is …` | the log holds a group the session never published: the session has stopped | open again | `lignindb/src/commit/error.rs` |

## At a close

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `write-back: …` | write-back failed at the close; the next open replays what the close did not cover | open again; if the words say the device is full, [make room](backup-and-full-disk.md#what-to-do-in-this-order) first | `lignindb/src/db/error.rs` |
| `a sync of a segment failed in this session, and no table checkpoint is installed until the store is opened again` | after `write-back: `: a segment could not be made durable in the session | open again | `lignin-store/src/flush/error.rs` |
| `the cleaner: …` | the cleaner could not close the files it was moving into; what it moved stands | open again | `lignindb/src/db/error.rs` |
| `segment …, which takes the moved blocks, could not be made durable: …` | after `the cleaner: `: the cleaner's segment could not be made durable | open again | `lignin-store/src/cleaner/error.rs` |
| `the … thread panicked` | a background thread of the engine panicked: what it held is as a crash leaves it | open again, and report it | `lignindb/src/db/error.rs` |
| `… was not found to hold this database any more: it was removed or replaced while the database was open, and the close finished nothing in it` | the directory was removed or replaced under the open handle | nothing for the database: it is gone from that path. A checkpoint that the node removed while it was open ends so | `lignindb/src/db/error.rs` |

## At a checkpoint

A checkpoint is made by `GroveDb::create_checkpoint(path)` and removed by
`GroveDb::delete_checkpoint(path)`; on LigninDB these are `Db::checkpoint`
and `lignindb::checkpoint::delete`. A creation that fails has put nothing
at `path`, with one exception that the message says. Through GroveDB the
words follow `lignindb: `; a refusal for the name is `not supported: `.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… is there already, and a checkpoint is made in a new directory` | the path is taken: a checkpoint, a file or an empty directory | give a new path, or remove what is there | `lignindb/src/checkpoint/error.rs` |
| `… has no directory above it to make the checkpoint in, or no name there` | the directory above the path is not there | make it first | `lignindb/src/checkpoint/error.rs` |
| `… has a name that checkpoints are built under, and none is made there` | the path ends in `.creating` | give another name | `lignindb/src/checkpoint/error.rs` |
| `… is locked, as a directory that a checkpoint is being built in` | another creation at the same path is running | wait for it | `lignindb/src/checkpoint/error.rs` |
| `… was not found to be the directory whose lock is held, with what was put into it, and nothing was given the name of the target` | the build directory was replaced during the creation | make the checkpoint again | `lignindb/src/checkpoint/error.rs` |
| `… is not listed as a file, and none was made there: the lock of its directory was not taken` | the lock file of the build directory is no file | remove what is there, and make the checkpoint again | `lignindb/src/checkpoint/error.rs` |
| `the session takes no commit, and no checkpoint is made of it: …` | the session has ended ([When a session ends](#when-a-session-ends)); through GroveDB it ends `: no checkpoint is made before the database is opened again` | open again | `lignindb/src/checkpoint/error.rs` |
| `: no checkpoint is made before the database is opened again` | GroveDB's end of the two refusals that need an open | open again | `grovedb/src/engine/lignin/checkpoint.rs` |
| `the log could not be made durable: …` | the creation's sync of the log failed: the session has ended | open again | `lignindb/src/checkpoint/error.rs` |
| `the manifest was in doubt, and settling it answered an error: …` | a replacement of the manifest had not finished, and settling it failed | make the checkpoint again; make room first if the device is full | `lignindb/src/checkpoint/error.rs` |
| `the manifest could not be read: …` | the manifest was in doubt again when the creation read it | make the checkpoint again | `lignindb/src/checkpoint/error.rs` |
| `the log holds commit …, and the latest published version is …` | the log is ahead of what the session published | open again | `lignindb/src/checkpoint/error.rs` |
| `the checkpoint's log ends at …, and the log is durable …` | the checkpoint would need log that is not durable | make the checkpoint again | `lignindb/src/checkpoint/error.rs` |
| `the manifest of the checkpoint could not be written: …` | the checkpoint's manifest could not be encoded | report it | `lignindb/src/checkpoint/error.rs` |
| `could not … …: …` | the file layer refused an operation of the creation or the removal; the system's words follow | fix what the system says; then make the checkpoint again | `lignindb/src/checkpoint/error.rs` |

A removal (`GroveDb::delete_checkpoint`) refuses, and removes nothing, with:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `… is not a directory of its own, and nothing was removed` | the path is a link or a file | remove it yourself if it is to go | `lignindb/src/checkpoint/error.rs` |
| `… is no checkpoint, and nothing of it was removed: …` | the directory holds files and no manifest, or what is no database of this engine | look at it before anything removes it | `lignindb/src/checkpoint/error.rs` |
| `… is open, and nothing of it was removed` | a handle has the checkpoint open | close it first | `lignindb/src/checkpoint/error.rs` |

## lignin

The tool's own words, besides the engine's above.

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `the database did not open: …` | the open failed: the words of [At an open](#at-an-open) follow | as they say | `lignin-tools/src/error.rs` |
| `… is no database: …` | the directory is no database that the tool reads; the reason follows | as the reason says | `lignin-tools/src/error.rs` |
| `its final name ends in .creating: it is a checkpoint that is being made, or the part of one that a creation cut short left, and it is never opened` | the reason, for a path that ends in `.creating` | run `lignin` on the checkpoint's own path once its creation has returned | `lignin-tools/src/open.rs` |
| `it holds MANIFEST.deleting: a removal of it is under way, or one was cut short and some of its files may be gone` | the reason, for a directory being removed | `GroveDb::delete_checkpoint` finishes the removal; where no removal made that entry and the database is to be kept, remove the entry and nothing else | `lignin-tools/src/open.rs` |
| `this build of lignin has no …: build lignin-tools with the Cargo feature …` | `migrate` was asked of a build without the feature `migrate` | build as [the runbook](migration.md#2-set-the-variables) does | `lignin-tools/src/error.rs` |
| `the background work failed: …` | `lignin compact`: a turn of write-back or the cleaner failed; its report is written | as the words say | `lignin-tools/src/error.rs` |

`lignin verify` prints the checker's report and exits with 0, 3 or 4:

| Message | What it means | What to do | Code |
| --- | --- | --- | --- |
| `checked …, …` | the first line: where the check ran (`the still store, after the close`, or `beside the background work`) and whether every hash was made again | | `lignin-tools/src/verify.rs` |
| `sound: every rule holds` | exit status 0 | nothing | `lignindb/src/verify/report.rs` |
| `INCOMPLETE RESTORE: the root holds the mark of a state-sync restore that did not finish` | exit status 4: a state sync into this database did not finish, so it is unsound by its own statement | discard the database; sync again or restore from a backup | `lignin-tools/src/verify.rs` |
| `value hashes of trees are the ones grove versions 1 to 3 left after a delete below them (GroveDB #686), not the ones the rules make` | counted after the report; no damage, and not unsound | nothing | `lignin-tools/src/verify.rs` |

A report with a rule that does not hold (exit status 3) names the rule
and where. Keep the database as it is, restore from a backup, and report
it with the whole report.

## lignin migrate

The exit statuses are in [the runbook](migration.md#6-migrate). The run
writes these lines on its way; lines that begin `migrate:` are for
scripts and hold `name=value` words.

| Message | What it means | Code |
| --- | --- | --- |
| `the source …: RocksDB identity …, last write …; …` | the first line: the source and its lock | `lignin-tools/src/migrate/mod.rs` |
| `no process has it open for writing, and none can open it while this runs` | the source's lock is held for the run | `lignin-tools/src/migrate/mod.rs` |
| `it has no LOCK file, so nothing keeps a process from opening it for writing while this runs` | a source with no `LOCK` file: make sure nothing opens it | `lignin-tools/src/migrate/mod.rs` |
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
| `the records of … are not what they were when the migration first read them: something wrote the source` | the same | `lignin-tools/src/migrate/walk.rs` |
| `the source is not what its survey found: …: …` | the same | `lignin-tools/src/migrate/compare.rs` |
| `holds what a migration does not carry (the lines NOT CARRIED above): nothing was written by this run` | the survey's `NOT CARRIED` lines name it; such a source stays on RocksDB | `lignin-tools/src/migrate/mod.rs` |
| `projects a peak of about … MiB of memory, above the limit of … MiB (…). Nothing was written by this run: …` | the words after it say whether a larger `--memory-limit`, or a smaller `--batch` or `--cache`, can take it | `lignin-tools/src/migrate/mod.rs` |
| `the target … has no name of its own` | give a target with a last name | `lignin-tools/src/migrate/mod.rs` |
| `the target … has a name that ends in .creating, under which LigninDB opens no database: give it another name` | as it says | `lignin-tools/src/migrate/mod.rs` |
| `the target … and the source … are one inside the other: a migration writes nothing into its source` | put the target outside the source | `lignin-tools/src/migrate/mod.rs` |
| `the target … and …, where its database is made, are one directory` | give another target | `lignin-tools/src/migrate/mod.rs` |
| `the directory … that the target … is to be in is not there: make it first` | as it says | `lignin-tools/src/migrate/mod.rs` |
| `is found by that name, and the directory … does not list it under that name: give the name as the directory has it` | give the target's name in the spelling the directory lists | `lignin-tools/src/migrate/mod.rs` |
| `the target … is there and …. A migration makes its database in … and gives it the name of the target at its end, once it was compared and verified: a database that a migration left at the target is whole` | a target that is there is never written: give another, or remove it | `lignin-tools/src/migrate/mod.rs` |
| `… is there and is not the work of a migration: …. Remove it, or give another target` | the work directory beside the target holds something else | `lignin-tools/src/migrate/mod.rs` |
| `holds a migration of another source, or of another state of this one (RocksDB identity …, last write …, root hash …). Remove that directory to migrate anew` | the work directory is of another run: remove it | `lignin-tools/src/migrate/mod.rs` |
| `the load in … was begun with --unreachable …, and goes on with that alone. Give it, or remove that directory to migrate anew` | give the choice the first run had | `lignin-tools/src/migrate/mod.rs` |
| `of the prefixes of the source that nothing leads to, … has … carried and nothing of …, and this run has --unreachable …` | the same | `lignin-tools/src/migrate/mod.rs` |
| `the place that the journal of the target states does not fit the source: …` | the work directory was made from another state of the source: remove it | `lignin-tools/src/migrate/walk.rs` |
| `the journal of the target …: the directory was not made by this build of the command, or is damaged. Remove it and migrate anew` | as it says | `lignin-tools/src/migrate/journal.rs` |
| `the database in … was written after it was compared: something else opened it. Run the command again: it compares anew` | nothing else is to open the work directory | `lignin-tools/src/migrate/mod.rs` |
| `the target … is there, and … is this command's and holds no database: a run may have given the target its name and stopped before the name was durable. The syncs that make it so failed (…). Run the same command again` | as it says | `lignin-tools/src/migrate/mod.rs` |
| `the database is at …, loaded, compared and verified, and its name there could not be made durable (…)` | the one failure that leaves the database at `TARGET`: run the same command again, which makes the name durable | `lignin-tools/src/migrate/mod.rs` |
| `the work directory … was not removed (…): it holds nothing of the target, and can be removed` | a line after a run that succeeded: remove the work directory yourself | `lignin-tools/src/migrate/mod.rs` |
| `the grove is more than … trees deep at …: this command does not walk it` | the source is deeper than the migrator walks; it stays on RocksDB, and report it | `lignin-tools/src/migrate/walk.rs` |
| `which is shorter than the prefix of a subtree: it is no GroveDB that this command reads` | a key of the source is no GroveDB key | `lignin-tools/src/migrate/walk.rs` |
| `the root node of the root Merk is no node of a Merk: …` | the source's root is damaged | `lignin-tools/src/migrate/survey.rs` |
| `a record of the roots keyspace is no record of a root key` | the source holds a record GroveDB does not write | `lignin-tools/src/migrate/load.rs` |
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
- The words of the layers below those listed here (the formats, the
  buffer pool, the page table), which follow one of the messages above
  and do not change what to do.
- `lignindb::backup`, which no GroveDB call and no command uses yet: a
  backup is made as [backup](backup-and-full-disk.md) says.
