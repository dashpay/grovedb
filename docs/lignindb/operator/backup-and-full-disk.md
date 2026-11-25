# Backup, restore, and a full disk

This chapter is for the person who runs a node on GroveDB's LigninDB
engine. It says how to back a database up and restore it, and what to do
when the device the database is on fills. It is part of task OPS-05 of
the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).
The rules behind it, with their numbers, are the design text
[disk-full-and-backup.md](../disk-full-and-backup.md) (BK1 to BK11 for
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

The directory each of `BACKUP` and `RESTORE` is in has to be there. The
block writes the three paths again with every link followed, so that a
copy is never made of a link or into the database, and refuses three
paths of which one is another or lies inside another.

<!-- runbook.sh: DB="$TARGET"; mkdir "$WORK/backup-chapter"; BACKUP="$WORK/backup-chapter/backup"; RESTORE="$WORK/backup-chapter/restore" -->
```sh
: "${DB:?set DB to the directory of the database}"
: "${BACKUP:?set BACKUP to where the backup is to be}"
: "${RESTORE:?set RESTORE to where a backup is to be restored}"
for path in "$BACKUP" "$RESTORE"; do
  if [ ! -d "$(dirname "$path")" ]; then echo "$(dirname "$path") is not there: make it first"; false; fi
done
DB=$(cd "$DB" && pwd -P)
BACKUP="$(cd "$(dirname "$BACKUP")" && pwd -P)/$(basename "$BACKUP")"
RESTORE="$(cd "$(dirname "$RESTORE")" && pwd -P)/$(basename "$RESTORE")"
for path in "$DB" "$BACKUP" "$RESTORE"; do
  n=$(printf '%s\n' "$DB" "$BACKUP" "$RESTORE" | awk -v p="$path" '$0 == p || index($0, p "/") == 1' | wc -l)
  if [ "$n" -ne 1 ]; then echo "$path is another of the three paths, or a directory that one of them is in: give three paths apart"; false; fi
done
printf 'DB      %s\nBACKUP  %s\nRESTORE %s\n' "$DB" "$BACKUP" "$RESTORE"
```

## What a backup is

- **A backup is a copy of a database directory that nobody had open while
  it was copied**, and that shares no file with the database. It opens
  like any database, at one committed version.
- **A stopped node's database is backed up by copying its directory**,
  with its `LOCK` file.
- **A running node's database is backed up from a checkpoint.** A
  checkpoint is one committed version in a directory of its own. Its
  files are hard links to the live database's where the file system
  allows it, and copies where it does not: a checkpoint on another file
  system than the database copies the whole database. Its creation holds
  the commit lock for one step, and commits go on while the files are
  linked or copied. A checkpoint of links is no backup by itself: its
  files are the live database's under a second name, on the same device.
- **Never copy the directory of a database that is open.** Its manifest
  and its files change while they are read, and the copy is no state of
  the database.

## Back up a stopped database

With the node stopped (or any process that had the database open
closed):

```sh
if [ -e "$BACKUP" ] || [ -L "$BACKUP" ]; then echo "$BACKUP is there already: the copy would go into it"; false; fi
cp -a "$DB" "$BACKUP"
sync
lignin manifest "$BACKUP"
```

`cp -a` copies every file of the directory with the `LOCK` file. `cp`
returns before its bytes are on the device. On Linux `sync` returns once
the system has written them, and so makes the copy durable; on macOS
`sync` can return before, so a copy made there is durable once its
device has been ejected or the system shut down. `lignin manifest`
reads the backup's manifest without opening it and lists the files it
names, marking one that is not there `MISSING` and one that is shorter
`SHORT`; it exits 0 either way, and reads no page. A backup is relied on
once a restore of it has passed `lignin verify` (below).

## Back up a running node

1. **Make a checkpoint.** The application asks GroveDB for one:
   `GroveDb::create_checkpoint(path)`, where `path` is not there and the
   directory above it is. A node that makes checkpoints for its own use
   makes one more for the backup. When the call returns `Ok`, the
   checkpoint is durable as far as the directory above `path` is: that
   directory's own name is the application's to have made durable.
   **A checkpoint holds the engine's files and nothing else**, unless the
   application names files for it to carry: a file that the application
   keeps in the database directory is not in it, and is backed up as the
   application's documentation says.
2. **Do not open the checkpoint** before it is copied: every open writes
   to the directory it opens. `lignin manifest` and `lignin segments` read
   files only, and can be run on it.
3. **Copy its directory**, and make the copy durable by syncing the
   copy's files and nothing else. On Linux, `sync` given files (GNU
   coreutils) syncs those files alone: `find` hands it every file and
   directory of the copy, and the loop after it syncs each directory the
   copy is in, up to the root, which hold the copy's name and the names
   above it. macOS's `sync` takes no file and syncs every file of the
   system, so there the block makes no sync:

   <!-- runbook.sh: CHECKPOINT="$WORK/backup-chapter/checkpoint"; cp -a "$DB" "$CHECKPOINT"; BACKUP="$WORK/backup-chapter/backup-of-checkpoint" -->
   ```sh
   : "${CHECKPOINT:?set CHECKPOINT to the directory of the checkpoint}"
   CHECKPOINT=$(cd "$CHECKPOINT" && pwd -P)
   lignin manifest "$CHECKPOINT"
   if [ -e "$BACKUP" ] || [ -L "$BACKUP" ]; then echo "$BACKUP is there already: the copy would go into it"; false; fi
   cp -a "$CHECKPOINT" "$BACKUP"
   if [ "$(uname -s)" = Linux ]; then
     find "$BACKUP" -exec sync {} +
     d=$(dirname "$BACKUP"); while [ "$d" != / ]; do sync "$d"; d=$(dirname "$d"); done
   else
     echo "no sync here: make the copy durable once the node has stopped"
   fi
   ```

   Do not run `sync` with no file while the node runs: it syncs every
   file of the system, the checkpoint's log files among them, and those
   are the live database's own. A sync of a log file through another
   name, which fails, is not seen by the database that writes it
   ([BK7](../disk-full-and-backup.md#62-the-procedure)). On macOS, where
   `sync` cannot be given a file and can return before the files are
   written, take a copy as durable once the node has stopped and the
   copy's device has been ejected, or the system shut down.
4. **Delete the checkpoint.** Until it is deleted it keeps the bytes of
   every file the live database has let go of since it was made. The
   application deletes it with the deletion of the engine it runs:
   `GroveDb::<LigninEngine>::delete_checkpoint_with_engine(path)`, or
   `EitherEngine::delete_checkpoint_of(EngineKind::Lignin, path)` where it
   chooses the engine at the open. `GroveDb::delete_checkpoint` deletes
   with the build's default engine, which is RocksDB in a build of both
   engines unless the build sets it otherwise ([engine.md](../engine.md#the-default-engine)).
   LigninDB's deletion can remove more than a checkpoint, the directory
   `path.creating` beside it among what it removes: it is given the
   checkpoint's path and nothing else. Or the checkpoint's directory is
   removed once no handle has it open:

   ```sh
   rm -rf "${CHECKPOINT:?}"
   ```

## Check a backup, and restore one

A backup that is to be relied on is checked once, on a restore of it:
the copy of a directory reads no page, and the check reads them all. The
restore is synced as the copy of a checkpoint is, file by file on Linux
and not at all elsewhere, so that it can be made beside a running node.
A restore that a node is to run on is made with that node stopped, and
is durable once synced as a stopped database's backup is.

```sh
if [ -e "$RESTORE" ] || [ -L "$RESTORE" ]; then echo "$RESTORE is there already: the copy would go into it"; false; fi
cp -a "$BACKUP" "$RESTORE"
if [ "$(uname -s)" = Linux ]; then
  find "$RESTORE" -exec sync {} +
  d=$(dirname "$RESTORE"); while [ "$d" != / ]; do sync "$d"; d=$(dirname "$d"); done
fi
lignin verify "$RESTORE"
```

`lignin verify` opens the restore, closes it and checks every page and
every value, and makes every stored hash again by Merk's rules but those
the rules take as stored (the value hash of a reference). Exit status 0
is a sound store. A restore is a database like any other, at the version
the backup holds: point the node at it, or keep it as the check and
remove it.

**A restored directory that does not open, with
`holds a database and no LOCK file`,** was copied by a tool that left
`LOCK` out. Put an empty file of that name there, once you know that no
process has the directory open, and run the check that could not run:

<!-- runbook.sh: rm "$RESTORE/LOCK" -->
```sh
touch "$RESTORE/LOCK"
lignin verify "$RESTORE"
```

**What a backup holds** is what was copied. A copy of a stopped
database's directory with `cp -a` holds the whole directory: the
engine's files, the reserve (`RESERVE`, below) and whatever the
application keeps there. A copy of a checkpoint holds what the
checkpoint holds: the engine's files, and the files the application
named for it to carry.

### What a cut-short copy or checkpoint leaves

| Left | What it is | What to do |
| --- | --- | --- |
| a copy that `cp` or `sync` did not finish, with or without a `MANIFEST` | a copy that was cut short: `cp` writes the files in no set order, so its manifest says nothing of the rest | remove it, and copy again |
| a directory whose name ends in `.creating`, beside a checkpoint's path | what a creation of a checkpoint at that path left, when it was cut short or failed. It is never opened, and it holds disk space. | with no checkpoint being made at that path, remove that directory by itself. Keep nothing of your own under a name that ends in `.creating`: a creation at the path takes over whatever such a directory holds, and the deletion of a checkpoint at the path removes it |
| a directory that holds `MANIFEST.deleting` and no `MANIFEST` | a deletion that was cut short, where that entry is a manifest and a deletion of this directory was under way: a deletion renames the manifest to that name before it removes any file. The names alone do not say that a deletion was meant | where you know that this directory was being deleted, the deletion of [step 4](#back-up-a-running-node) finishes it (it refuses the directory where the entry is no manifest). Where you do not know that, delete and rename nothing: some of its files may be gone, and the database is restored from a backup |
| a directory that holds both `MANIFEST.deleting` and `MANIFEST` | no deletion leaves this, since it renames the manifest first: something else made the entry beside a database | if the database is to be kept, remove the entry `MANIFEST.deleting` and nothing else. The deletion of [step 4](#back-up-a-running-node) would remove the whole database: it goes by the manifest it finds, not by the entry's name |

## The device is full

### What you see

Commits fail. The error of the commit has the system's "no space left"
among its causes (`lignindb::space::out_of_space` says so of an error),
and its words end with what the system said, such as "No space left on
device (os error 28)". Reads go on.

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
2. **Let the node tell you whether its session goes on.** The session
   has ended when an error of a commit or of a flush says so:
   - its words say
     `the session takes no write, and the database has to be opened again`,
     or `the session takes no commit: …`, or, from a flush,
     `a sync of … failed, so the log cannot be made durable in this session: …`;
   - or GroveDB answers the commit with
     `not durable: the commit happened, but it is not known to survive a crash`
     or `unsettled: the engine could not find out whether the commit happened`.
     These two give the engine's words no further: [the messages](messages.md#how-a-message-reaches-you).

   Then stop the node, and start it again once there is room. Nothing
   that was made durable is lost. Where a sync was refused, the open can
   land on an earlier commit than the last one the node published, since
   a sync that the device refused makes no promise of what it was for;
   the open also settles whether an unsettled commit happened.
   `Db::version` says where the open landed. With none of these, the
   session goes on by itself, and the next commit is taken once there is
   room.
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

   The line `on disk:` of `lignin stats` gives the files and bytes of each
   kind. Its other counts are those of the session that `lignin stats`
   opens, and not the node's: the node's own are `Db::stats` of its
   handle while it runs. `compact` needs room for the copies it makes
   before it gives files back.
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
([section 5.3](../disk-full-and-backup.md#53-how-much-room-is-a-little)).

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
  built again from its data, by a state sync, has none.
