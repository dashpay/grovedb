<!-- This chapter is docs/lignindb/operator/backup-and-full-disk.md, written here by lignin-tools/tests/operator_docs.rs: edit that file, then run `LIGNIN_OPERATOR_DOCS=write cargo test -p lignin-tools --test operator_docs`. -->

# Backup, restore, and a full disk

This chapter is for the person who runs a node on GroveDB's LigninDB
engine. It says how to back a database up and restore it, and what to do
when the device the database is on fills. It is part of task OPS-05 of
the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).
The rules behind it, with their numbers, are the design text
[disk-full-and-backup.md](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/disk-full-and-backup.md) (BK1 to BK11 for
backups, DF1 to DF13 for a full device); this chapter is what an
operator does with them. The words of each error are in
[the messages](messages.md).

The commands use these variables, as absolute paths:

| Variable | Is |
| --- | --- |
| `DB` | the directory of the LigninDB database |
| `CHECKPOINT` | the directory of a checkpoint of it |
| `BACKUP` | where a backup is to be, best on another device; not there yet |
| `RESTORE` | where a backup is to be restored; not there yet |

`lignin` is the tool of the [migration runbook](migration.md#1-build-the-tool),
built the same way (the feature `migrate` is not needed here).

```sh
: "${DB:?set DB to the directory of the database}"
: "${BACKUP:?set BACKUP to where the backup is to be}"
: "${RESTORE:?set RESTORE to where a backup is to be restored}"
mkdir -p "$(dirname "$BACKUP")" "$(dirname "$RESTORE")"
```

## What a backup is

- **A backup is a copy of a database directory that nobody had open while
  it was copied**, and that shares no file with the database. It opens
  like any database, at one committed version.
- **A stopped node's database is backed up by copying its directory**,
  with its `LOCK` file.
- **A running node's database is backed up from a checkpoint.** A
  checkpoint is one committed version in a directory of its own, made in
  the time of a few hard links, which holds back no commit. It is no
  backup by itself: its files are the live database's under a second
  name, on the same device.
- **Never copy the directory of a database that is open.** Its manifest
  and its files change while they are read, and the copy is no state of
  the database.

## Back up a stopped database

With the node stopped (or any process that had the database open
closed):

```sh
cp -a "$DB" "$BACKUP"
sync
lignin manifest "$BACKUP"
```

`cp -a` copies every file of the directory with the `LOCK` file. `cp`
returns before its bytes are on the device: `sync` makes the copy
durable. `lignin manifest` reads the backup's manifest without opening
it and lists the files it names; a copy that was cut short has no
manifest, and the command then fails.

## Back up a running node

1. **Make a checkpoint.** The application asks GroveDB for one:
   `GroveDb::create_checkpoint(path)`, where `path` is not there and the
   directory above it is. A node that makes checkpoints for its own use
   makes one more for the backup. When the call returns `Ok`, the
   checkpoint is durable.
2. **Do not open the checkpoint** before it is copied: every open writes
   to the directory it opens. `lignin manifest` and `lignin segments` read
   files only, and can be run on it.
3. **Copy its directory**, and make the copy durable by syncing the
   copy's files and nothing else. On Linux, `sync` given files (GNU
   coreutils) syncs those files alone, so `find` hands it every file and
   directory of the copy:

   ```sh
   : "${CHECKPOINT:?set CHECKPOINT to the directory of the checkpoint}"
   lignin manifest "$CHECKPOINT"
   cp -a "$CHECKPOINT" "$BACKUP"
   find "$BACKUP" -exec sync {} +
   ```

   Do not run `sync` with no file while the node runs: it syncs every
   file of the system, the checkpoint's log files among them, and those
   are the live database's own. A sync of a log file through another
   name, which fails, is not seen by the database that writes it
   ([BK7](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/disk-full-and-backup.md#62-the-procedure)). macOS's `sync`
   takes no file and always syncs everything: there, sync the copy after
   the node has stopped.
4. **Delete the checkpoint.** Until it is deleted it keeps the bytes of
   every file the live database has let go of since it was made. The
   application calls `GroveDb::delete_checkpoint(path)`, or removes the
   directory once no handle has it open:

   ```sh
   rm -rf "${CHECKPOINT:?}"
   ```

## Check a backup, and restore one

A backup that is to be relied on is checked once, on a restore of it:
the copy of a directory reads no page, and the check reads them all. The
restore is synced file by file, as the copy of a checkpoint is, so that
it can be made beside a running node.

```sh
cp -a "$BACKUP" "$RESTORE"
find "$RESTORE" -exec sync {} +
lignin verify "$RESTORE"
```

`lignin verify` opens the restore, closes it and checks every page,
every value and every stored hash. Exit status 0 is a sound store. A
restore is a database like any other, at the version the backup holds:
point the node at it, or keep it as the check and remove it.

**A restored directory that does not open, with
`holds a database and no LOCK file`,** was copied by a tool that left
`LOCK` out. Put an empty file of that name there, once you know that no
process has the directory open:

```sh
touch "$RESTORE/LOCK"
lignin stats "$RESTORE"
```

**What a backup holds** is what was copied. `cp -a` copies the whole
directory: the engine's files, the reserve (`RESERVE`, below) and
whatever the application keeps there.

### What a cut-short copy or checkpoint leaves

| Left | What it is | What to do |
| --- | --- | --- |
| a target with no `MANIFEST` (empty, with `LOCK` alone, or with some files) | a copy that was cut short | remove it, and copy again |
| a directory whose name ends in `.creating`, beside a checkpoint's path | a checkpoint whose creation was cut short or failed. It is never opened. | leave it: the next creation at the same path takes it over, and `GroveDb::delete_checkpoint(path)` removes it |
| a directory that holds `MANIFEST.deleting` | a deletion that was cut short | `GroveDb::delete_checkpoint(path)` finishes it |

## The device is full

### What you see

Commits fail and the node stops at a block. The error of the commit has
the system's "no space left" among its causes; its words end with what
the system said, such as "No space left on device (os error 28)".
Reads go on.

**What is true then.** Nothing that was made durable is lost, nothing
that was committed is damaged, and nothing needs a repair. **Do not
remove any file of the database directory by hand**: each one is needed,
or the engine removes it when it is not. The one exception is the
reserve, below.

### What to do, in this order

1. **Make room.**
   - If the database keeps a reserve (`RESERVE` in its directory, below)
     and the node is stopped, or its session keeps none, remove it with
     what a fill of it left behind:

     ```sh
     rm -f "$DB/RESERVE" "$DB/RESERVE.new"
     ```

     A running session that keeps a reserve removes it itself, at the
     first refusal for room it is told of.
   - Delete checkpoints that are no longer needed: each keeps every file
     the database has let go of since it was made.
   - End what holds old versions: a snapshot kept for long, a write
     transaction left open (the application's).
   - Move away what is not the database's.
2. **Let the node tell you whether its session goes on.** An error whose
   words say
   `the session takes no write, and the database has to be opened again`
   means a sync of the log or of a segment was refused while the device
   was full, and the session has ended: stop the node
   and start it again once there is room. Nothing that was made durable
   is lost. The open can land on an earlier commit than the last one the
   node published, since a sync that the device refused makes no promise
   of what it was for; the node goes on from where the open landed.
   Otherwise the session goes on by itself: the next commit is taken.
3. **Let the engine give room back.** In a running session, write-back
   takes a table checkpoint and drops the log files it covers, and the
   cleaner drops the segment and value-log files that are dead and moves
   what is live out of the others. On a stopped database, `lignin stats`
   shows where the room is, and `lignin compact` has the cleaner work
   until a whole pass moves nothing (600 s at most, unless `--timeout`
   says otherwise):

   ```sh
   df -h "$DB"
   lignin stats "$DB"
   lignin compact "$DB"
   ```

   The lines `log held:` and `on disk:` of `lignin stats` are the log that
   a recovery would read and the bytes of each kind of file. `compact`
   needs room for the copies it makes before it gives files back.
4. **The reserve fills again by itself** in a session that keeps one,
   once commits are taken, a table checkpoint was installed and the
   cleaner is at rest.
5. **If the device is full of live data**, the steps above give back
   less than the node needs. Stop the node, back the database up as a
   stopped database (above), and restore it on a larger device.

**A database that does not open on a full device** needs the same first
step, and then opens. An open needs room of its own (a manifest, the
header of a new log file, and the end of the last log file written
again). An open that was refused changed nothing that the next open does
not expect.

### The reserve

A reserve is a file of written bytes, `RESERVE`, that a session keeps in
the database directory so that it has room to give back when the device
fills: it removes the file at the first refusal for room, and writes it
again when room is back. It is set by `Options::reserve`
([tuning](tuning.md)), in bytes, and **is 0 by default: no reserve**.
How large it should be has not been measured for a node's database; the
design text's first estimate is one log file and one segment at their
configured lengths, 128 MiB with the defaults
([section 5.3](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/disk-full-and-backup.md#53-how-much-room-is-a-little)).

### The log grows and does not shrink

The log is kept from the last table checkpoint on, and write-back bounds
it. Three things take the bound away:

- **An open write transaction** holds the log from the commit before its
  first copy of a page for as long as it lives. A transaction that the
  application forgot holds it until it is committed or rolled back.
- **A page that cannot be written** makes the log grow until the
  database is opened again: restart the node. If the same page fails
  again after the restart, its error names it; that is a defect to
  report with what `lignin verify` says, and the database is restored
  from a backup.
- **A session that has ended** (step 2 above) takes no write, so its log
  grows no more.

### Space that does not come back

- **A checkpoint that is kept** keeps the files it shares with the live
  database. Delete it once its backup is made.
- **A crash while a dropped Merk was being reclaimed** leaves the rest of
  that Merk's cells in their pages: space, and no state. `lignin verify`
  counts them, and they do not make the store unsound. A database that is
  built again from its data (a state sync, a migration) has none.
