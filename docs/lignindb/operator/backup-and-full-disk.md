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

Run the blocks in a shell of their own (`bash`), which the first block
sets to stop at the first command that fails: a block refuses a path
with `false`, and only `set -e` makes that stop the rest. The block also
defines what the later ones use to write a path again with every link
followed (`cd -P`, so that `..` is taken after the links), and to refuse
two paths of which one is the other or lies inside it, compared by the
directories they are (`-ef`) and by names that differ in case alone.
A copy claims its directory first with `mkdir`, which fails where
anything is there already, and then copies the files into it, so that a
copy never lands inside a directory that something else made. The claim
also refuses a name that ends in `.creating`, which LigninDB takes for
the build of a checkpoint and whose deletion removes, and a path inside
a directory of such a name or one that holds a `MANIFEST` or a
`MANIFEST.deleting` (LigninDB's) or a `CURRENT` (RocksDB's), a database
or checkpoint whose deletion would remove the copy with it.
Each procedure below names the paths it uses, and the directory each new
path is to be in has to be there.

<!-- runbook.sh: DB="$TARGET"; mkdir "$WORK/backup-chapter"; BACKUP="$WORK/backup-chapter/backup"; RESTORE="$WORK/backup-chapter/restore" -->
```sh
set -euo pipefail
plain_path() {
  case "$1" in *$'\n'*) echo "a path with a line break in it is not taken: $1" >&2; return 1 ;; esac
}
real_dir() {
  local d
  plain_path "$1" || return 1
  d=$(cd -P -- "$1" && pwd -P && echo .) || return 1
  d=${d%$'\n.'}
  plain_path "$d" || return 1
  printf '%s\n' "$d"
}
outside_databases() {
  local d
  case "$1" in
    /*) d=$(dirname -- "$1") ;;
    *) d=$(dirname -- "$(pwd -P)/$1") ;;
  esac
  while [ ! -d "$d" ]; do
    case "$(basename -- "$d" | tr '[:upper:]' '[:lower:]')" in
      *.creating) echo "$1 lies inside $d, which LigninDB takes for the build of a checkpoint: choose a path outside it" >&2; return 1 ;;
    esac
    d=$(dirname -- "$d")
  done
  d=$(real_dir "$d") || return 1
  while :; do
    case "$(basename -- "$d" | tr '[:upper:]' '[:lower:]')" in
      *.creating) echo "$1 lies inside $d, which LigninDB takes for the build of a checkpoint: choose a path outside it" >&2; return 1 ;;
    esac
    if [ -e "$d/MANIFEST" ] || [ -e "$d/MANIFEST.deleting" ]; then echo "$1 lies inside $d, a database directory: choose a path outside it" >&2; return 1; fi
    if [ -e "$d/CURRENT" ]; then echo "$1 lies inside $d, a RocksDB database directory: choose a path outside it" >&2; return 1; fi
    if [ "$d" = / ]; then return 0; fi
    d=$(dirname -- "$d")
  done
}
claim_dir() {
  plain_path "$1" || return 1
  case "$(basename -- "$1" | tr '[:upper:]' '[:lower:]')" in
    *.creating) echo "$1 ends in .creating, under which LigninDB builds a checkpoint: choose another path" >&2; return 1 ;;
  esac
  outside_databases "$1" || return 1
  mkdir -- "$1" || { echo "$1 is there already, or cannot be made: choose another path" >&2; return 1; }
}
real_new() {
  local d
  plain_path "$1" || return 1
  d=$(real_dir "$(dirname -- "$1")") || return 1
  printf '%s/%s\n' "${d%/}" "$(basename -- "$1")"
}
same_place() {
  if [ -e "$1" ] && [ "$1" -ef "$2" ]; then return 0; fi
  [ "$(dirname -- "$1")" -ef "$(dirname -- "$2")" ] && [ "$(basename -- "$1" | tr '[:upper:]' '[:lower:]')" = "$(basename -- "$2" | tr '[:upper:]' '[:lower:]')" ]
}
within() {
  local d=$1
  while :; do
    if same_place "$d" "$2"; then return 0; fi
    if [ "$d" = / ] || [ "$d" = . ] || [ -z "$d" ]; then return 1; fi
    d=$(dirname -- "$d")
  done
}
apart() {
  local i j
  local -a p=("$@")
  for ((i = 0; i < ${#p[@]}; i++)); do
    for ((j = 0; j < ${#p[@]}; j++)); do
      if [ "$i" -ne "$j" ] && within "${p[i]}" "${p[j]}"; then echo "${p[i]} is ${p[j]}, or lies inside it: give paths apart"; return 1; fi
    done
  done
}
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
: "${DB:?set DB to the directory of the database}" "${BACKUP:?set BACKUP to where the backup is to be}"
DB=$(real_dir "$DB")
BACKUP=$(real_new "$BACKUP")
apart "$DB" "$BACKUP"
lignin manifest "$DB"
claim_dir "$BACKUP"
cp -a "$DB/." "$BACKUP/"
sync
lignin manifest "$BACKUP"
```

The block reads the database's manifest first, so a `DB` whose manifest
cannot be read (a RocksDB directory given by mistake) is refused before
anything is copied; a directory with a manifest and files missing is
copied all the same, and its restore's check finds it. `cp -a` copies every file of the directory
with the `LOCK` file. `cp` returns before its bytes are on the device. On Linux `sync` returns once
the system has written them, and tells of no write that failed; on
macOS `sync` can return before, so a copy made there is written once
its device has been ejected or the system shut down. Either way, a
backup is relied on once a restore of it has passed `lignin verify`,
and that check can be answered from the system's cache: a restore on
another machine reads what the device holds. `lignin manifest`
reads the backup's manifest without opening it and lists the files it
names, marking one that is not there `, MISSING` and one that is
shorter `, SHORT: … bytes on disk`; it exits 0 either way, and reads no page. A backup is relied on
once a restore of it has passed `lignin verify` (below).

## Back up a running node

1. **Make a checkpoint.** The application asks GroveDB for one:
   `GroveDb::create_checkpoint(path)`, where `path` is not there and the
   directory above it is, outside the node's database directory (the
   copy's block below refuses a checkpoint inside it). A node that makes checkpoints for its own use
   makes one more for the backup. When the call returns `Ok`, the
   checkpoint is durable as far as the directory above `path` is: that
   directory's own name is the application's to have made durable.
   **A checkpoint holds the engine's files and nothing else**:
   `GroveDb::create_checkpoint` carries no other file, so a file that the
   application keeps in the database directory is not in it, and is
   backed up as the application's documentation says.
2. **Do not open the checkpoint** before it is copied: every open writes
   to the directory it opens. `lignin manifest` and `lignin segments` read
   files only, and can be run on it.
3. **Copy its directory**, and make the copy durable by syncing the
   copy's files and nothing else. On Linux, `sync` given files (GNU
   coreutils) syncs those files alone: `find` hands it every file and
   directory of the copy, and no link, which `sync` would follow to a
   file outside it, and the loop after it syncs each directory the
   copy is in, up to and with the root, which hold the copy's name and the
   names above it. A sync of a file that `cp` has written and closed
   cannot report a write that the system had already failed before it:
   the check of a restore (below) reads the copy back, which the system
   can answer from its cache. macOS's `sync` takes no file and syncs every
   file of the system, so there the block makes no sync:

   <!-- runbook.sh: CHECKPOINT="$WORK/backup-chapter/checkpoint"; cp -a "$DB" "$CHECKPOINT"; BACKUP="$WORK/backup-chapter/backup-of-checkpoint" -->
   ```sh
   : "${DB:?set DB to the database directory of the node}" "${CHECKPOINT:?set CHECKPOINT to the directory of the checkpoint}" "${BACKUP:?set BACKUP to where the backup is to be}"
   DB=$(real_dir "$DB")
   CHECKPOINT=$(real_dir "$CHECKPOINT")
   BACKUP=$(real_new "$BACKUP")
   apart "$DB" "$CHECKPOINT" "$BACKUP"
   CHECKPOINT_ID="$(df -P -- "$CHECKPOINT" | awk 'NR == 2 {print $1}') $(ls -di -- "$CHECKPOINT" | awk '{print $1}')"
   printf 'CHECKPOINT    %s\nCHECKPOINT_ID %s\nBACKUP        %s\n' "$CHECKPOINT" "$CHECKPOINT_ID" "$BACKUP"
   lignin manifest "$CHECKPOINT"
   claim_dir "$BACKUP"
   cp -a "$CHECKPOINT/." "$BACKUP/"
   if [ "$(uname -s)" = Linux ]; then
     find "$BACKUP" \( -type f -o -type d \) -exec sync {} +
     d=$(dirname -- "$BACKUP")
     while :; do sync "$d"; if [ "$d" = / ]; then break; fi; d=$(dirname -- "$d"); done
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
4. **Check the copy**: restore it once and verify it, as the next section
   says.
5. **Then delete the checkpoint** ([below](#delete-the-checkpoint-of-a-backup)),
   and not before: until the copy is durable and its restore has passed
   `lignin verify`, the checkpoint is the one whole copy of that version.

## Check a backup, and restore one

A backup that is to be relied on is checked once, on a restore of it:
the copy of a directory reads no page, and the check reads them all. The
restore is synced as the copy of a checkpoint is, file by file on Linux
and not at all elsewhere, so that it can be made beside a running node.
A restore that a node is to run on is made with that node stopped, and
is durable once synced as a stopped database's backup is. Where
`CHECKPOINT` is set, as the copy's block sets it, the restore is also
kept apart from the checkpoint, whose deletion would remove it.

```sh
: "${BACKUP:?set BACKUP to the backup}" "${RESTORE:?set RESTORE to where it is to be restored}"
BACKUP=$(real_dir "$BACKUP")
RESTORE=$(real_new "$RESTORE")
apart "$BACKUP" "$RESTORE" ${CHECKPOINT:+"$CHECKPOINT"}
claim_dir "$RESTORE"
cp -a "$BACKUP/." "$RESTORE/"
if [ "$(uname -s)" = Linux ]; then
  find "$RESTORE" \( -type f -o -type d \) -exec sync {} +
  d=$(dirname -- "$RESTORE")
  while :; do sync "$d"; if [ "$d" = / ]; then break; fi; d=$(dirname -- "$d"); done
fi
lignin verify "$RESTORE"
```

`lignin verify` opens the restore, closes it and checks every page and
every value, and makes every stored hash of the Merks in use again by
Merk's rules but those the rules take as stored (the value hash of a
reference); the cells of a dropped Merk that waits to be reclaimed are
walked, and their hashes not made again. Exit status 0
is a sound store. A restore is a database like any other, at the version
the backup holds: point the node at it, with the application's files of
the same state (a copy of a checkpoint holds none, below), or keep it as
the check and
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
application keeps there. A link in the directory is copied as a link,
and not what it points to: a file of the application's that is kept
elsewhere and linked from there is backed up where it is kept. A copy
of a checkpoint holds what the checkpoint holds: the engine's files and
no file of the application's. A copy of a checkpoint made while the node
runs holds whole files, and the files a checkpoint shares with the live
database go on growing: the copy can hold bytes that the live database
appended after the checkpoint, values and log records of later commits,
behind the lengths its manifest gives. They are no part of the version
the copy restores, and a recovery reads none of them
([BK6](../disk-full-and-backup.md#62-the-procedure)), but they are in the
copy: give a backup only where the database as it was when the copy was
made may go.

### Delete the checkpoint of a backup

Once the copy of a checkpoint is durable (on macOS: the node stopped and
the copy's device ejected, or the system shut down) and its restore has
passed `lignin verify`, delete the checkpoint. Until it is deleted it
keeps the files it links, which the live database would otherwise let
go of as it writes on. The application deletes it with the deletion of the engine it
runs: `GroveDb::<LigninEngine>::delete_checkpoint_with_engine(path)`, or
`EitherEngine::delete_checkpoint_of(EngineKind::Lignin, path)` where it
chooses the engine at the open. `GroveDb::delete_checkpoint` deletes
with the build's default engine, which is RocksDB in a build of both
engines unless the build sets it otherwise ([engine.md](../engine.md#the-default-engine)).
LigninDB's deletion can remove more than a checkpoint, the directory
`path.creating` beside it among what it removes: it is given the
checkpoint's path and nothing else. It removes the database that is at
that path when it runs, whatever made it: it cannot tell a checkpoint
from another database, and nothing keeps the path from leading to
another directory meanwhile. So the application makes its checkpoints
at paths of their own, where nothing else makes, moves or restores a
directory, and deletes each by the path it made it at.

Or the checkpoint's directory, as the copy's block wrote it with its
links followed, is removed once no handle has it open. Give the block
that path and its identity as the copy's block printed them
(`CHECKPOINT`, `CHECKPOINT_ID`: the device and the inode), and the
node's database directory as `DB`: it and the copy's block refuse a
checkpoint that is the node's database, or lies inside it or around it.
It refuses a path that is a link now, no directory, another path once its links are
followed, another directory than the one that was copied, or a
directory that holds anything but the entries a checkpoint of GroveDB
has (`LOCK`, `MANIFEST`, `wal`, `seg`, `val` and `table`, with nothing
but files in those four), such as a restore made inside it: what is
there is then not what was copied. The
checks and the removal are two steps: nothing else is to make, move or
remove a directory at that path between them.

```sh
: "${CHECKPOINT:?set CHECKPOINT to the directory of the checkpoint, as the copy block printed it}"
: "${DB:?set DB to the database directory of the node}"
plain_path "$CHECKPOINT"
DB=$(real_dir "$DB")
apart "$DB" "$CHECKPOINT"
if [ -L "$CHECKPOINT" ] || [ ! -d "$CHECKPOINT" ]; then echo "$CHECKPOINT is a link or no directory now: it is not what was copied"; false; fi
if [ "$(real_dir "$CHECKPOINT")" != "$CHECKPOINT" ]; then echo "$CHECKPOINT is not the path the copy's block wrote: give that one"; false; fi
: "${CHECKPOINT_ID:?set CHECKPOINT_ID as the copy block printed it}"
if [ "$(df -P -- "$CHECKPOINT" | awk 'NR == 2 {print $1}') $(ls -di -- "$CHECKPOINT" | awk '{print $1}')" != "$CHECKPOINT_ID" ]; then echo "$CHECKPOINT is another directory than the one that was copied"; false; fi
for entry in "$CHECKPOINT"/* "$CHECKPOINT"/.[!.]* "$CHECKPOINT"/..?*; do
  if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then continue; fi
  case "${entry##*/}" in
    LOCK | MANIFEST | wal | seg | val | table) ;;
    *) echo "$CHECKPOINT holds ${entry##*/}, which a checkpoint does not: it is not what was copied"; false ;;
  esac
done
if [ -n "$(find "$CHECKPOINT" -mindepth 2 ! -type f)" ]; then echo "$CHECKPOINT holds a directory or a link inside its own directories, which a checkpoint does not: it is not what was copied"; false; fi
rm -rf -- "$CHECKPOINT"
```

A removal that the system stops part way, as a directory that cannot be
written, leaves part of the checkpoint, with its `MANIFEST` or without
it: what is left is no whole checkpoint either way. Fix what the system
said, and run the block again, which finds the same
directory by its identity and removes what is left.


### What a cut-short copy or checkpoint leaves

| Left | What it is | What to do |
| --- | --- | --- |
| a copy that `cp` or `sync` did not finish, with or without a `MANIFEST` | a copy that was cut short: `cp` writes the files in no set order, so its manifest says nothing of the rest | remove it, and copy again |
| a directory whose name ends in `.creating`, beside a checkpoint's path | what a creation of a checkpoint at that path left, when it was cut short or failed, where one was made there. It is never opened, and it holds disk space. The name alone does not say who made the directory | the engine takes any directory of that name for a creation's, whoever made it. Remove it only where you know that nothing but checkpoints of GroveDB is ever made under that name at that path, and that none is being made now; otherwise remove nothing you cannot account for. Keep nothing of your own under a name that ends in `.creating`: a creation at the path takes over whatever such a directory holds, and the deletion of a checkpoint at the path removes it |
| a directory that holds `MANIFEST.deleting` and no `MANIFEST` | a deletion that was cut short, where that entry is a manifest and a deletion of this directory was under way: a deletion renames the manifest to that name before it removes any file. The names alone do not say that a deletion was meant | where you know that this directory was being deleted, the deletion of [the checkpoint](#delete-the-checkpoint-of-a-backup) finishes it (it refuses the directory where the entry is no manifest). Where you do not know that, delete and rename nothing: some of its files may be gone, and the database is restored from a backup |
| a directory that holds both `MANIFEST.deleting` and `MANIFEST` | no deletion leaves this, since it renames the manifest first: something else made one of the two entries, and either can be the database's manifest | remove and rename nothing, and do not open the directory: restore the database from a backup, or report it with what each entry is. The deletion of [the checkpoint](#delete-the-checkpoint-of-a-backup) would remove the whole database: it goes by the manifest it finds, not by the entry's name |

## The device is full

### What you see

Commits fail. The error of the commit has the system's "no space left"
among its causes (`lignindb::space::out_of_space` says so of an error),
and its words end with what the system said, such as "No space left on
device (os error 28)", or "Disk quota exceeded" where a quota of the
user or the project that writes there is used up: `df` then shows room,
and the room to make is inside that quota, or the quota is raised. A commit beside write-back that met the full
device first can say only `the log has no file that takes items`, with
no word of the device: where commits fail so, look at the free space
too. Reads go on.

**What is true then.** Nothing that was made durable is lost, nothing
that was committed is damaged, and nothing needs a repair. **Do not
remove any file of the database directory by hand**: each one is needed,
or the engine removes it when it is not. The one exception is the
reserve, below.

### What to do, in this order

1. **Make room.**
   - If the node's sessions keep a reserve (`Options::reserve` above 0,
     below), or kept one, and the node is stopped, or its session keeps
     none, remove the reserve with what a fill of it left behind. A
     session with a reserve of 0 writes no file of those two names and
     leaves alone any it finds, so where no session of the node ever kept
     a reserve, a `RESERVE` or `RESERVE.new` in the directory is not the
     engine's: leave it.

     ```sh
     : "${DB:?set DB to the directory of the database}"
     rm -f "$DB/RESERVE" "$DB/RESERVE.new"
     ```

     A running session that keeps a reserve removes it itself, at the
     first refusal for room it is told of.
   - Delete checkpoints that are no longer needed, as
     [their deletion](#delete-the-checkpoint-of-a-backup) says: a
     checkpoint of a backup only once its copy is durable and its restore
     has passed `lignin verify`. A checkpoint keeps the files it links,
     which the database would otherwise let go of as it writes on.
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

   Then stop the node, and start it again once there is room; but where
   the reason is `every log file id has been used, and the log can be given no further file`,
   keep the node's handle for reads, as
   [the messages](messages.md#when-a-session-ends) say. Nothing
   that was made durable is lost. Where a sync was refused, the open can
   land on an earlier commit than the last one the node published, since
   a sync that the device refused makes no promise of what it was for;
   the open also settles whether an unsettled commit happened.
   `Db::version` says where the open landed. With none of these, the
   session goes on by itself, and the next commit is taken once there is
   room and the engine has also cut the log and replaced its manifest.
   Where commits still answer `the session takes no commit yet: …` with
   room on the device (the directory itself refuses a write, as a
   directory that cannot be written), close the database and open it
   again once that is dealt with.
3. **Let the engine give room back.** In a running session, write-back
   takes a table checkpoint and drops the log files it covers, and the
   cleaner drops the segment and value-log files that are dead and moves
   what is live out of the others. On a stopped database, `lignin stats`
   shows where the room is, and `lignin compact` has the cleaner work
   until a whole pass moves nothing, and starts no further turn once
   600 s have passed (or what `--timeout` says); the turn under way and
   the close end after that:

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

- **A checkpoint that is kept** keeps the files it links, which the
  live database would otherwise let go of. Delete it as
  [its deletion](#delete-the-checkpoint-of-a-backup) says.
- **A crash while a dropped Merk was being reclaimed** leaves the rest of
  that Merk's cells in their pages: space, and no state. `lignin verify`
  counts them, and they do not make the store unsound. A database that is
  built again from its data, by a state sync, has none.
