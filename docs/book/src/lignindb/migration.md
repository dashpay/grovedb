<!-- This chapter is docs/lignindb/operator/migration.md, written here by lignin-tools/tests/operator_docs.rs: edit that file, then run `LIGNIN_OPERATOR_DOCS=write cargo test -p lignin-tools --test operator_docs`. -->

# Migrating a node from RocksDB to LigninDB

This runbook is for the person who runs a node that embeds GroveDB and
moves its database from the RocksDB engine to LigninDB. It is part of
task OPS-05 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).
Everything here is done with GroveDB's own tool, `lignin`, whose
subcommand `migrate` reads a RocksDB GroveDB and writes the same grove
as a new LigninDB database. How a node is stopped, started and pointed
at a database is the application's to say; this runbook says where those
steps come.

The migration is offline: the node is stopped for the whole run. The
RocksDB database is read and never written, so it stays the way back
until you remove it.

| You will | Step |
| --- | --- |
| install what the build needs, and build `lignin` | [1](#1-build-the-tool), [2](#2-set-the-variables) |
| practise on a database the repository makes (optional, recommended) | [3](#3-rehearse-on-a-database-of-the-repository) |
| stop the node and survey its database | [4](#4-stop-the-node-and-name-the-directories), [5](#5-survey-the-source) |
| migrate, and check the result | [6](#6-migrate), [7](#7-verify) |
| back up the new database and restore the backup once | [8](#8-back-up-the-new-database), [9](#9-restore-the-backup-once-and-open-it) |
| start the node on LigninDB, or go back | [10](#10-start-the-node-on-lignindb), [11](#11-the-way-back) |

Every command below is in a block marked `sh`. They are written for
`bash` on Linux or macOS (the platforms of the plan's DEC-06: Linux on
x86_64 and aarch64, macOS on aarch64; there is no Windows build), and
`docs/lignindb/operator/runbook.sh` runs them all, in this order, on a
database that step 3 makes: the commands here are the ones that were run.

## What it takes

**Time.** The survey of step 5 prints how many Merk nodes the source
holds (`Merk nodes:`). The plan's acceptance for the migrator (OPS-02 in
[task register 3](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583786))
is at least 40,000 elements a second on the reference shape, measured on
a rented machine with dedicated processors, with a database of many
small Merks reported beside it. Every run ends with a line that says what it took,
`migrate: done target=… seconds=…`, with `elements_per_second=…` for a
run that made the whole target, so the rehearsal of step 3 on your own
machine gives you a rate to plan with.
Add the time of `lignin verify` in step 7, which reads the whole store
again, and of the copies of steps 8 and 9, which are as long as copying
the database's bytes.

**Disk.** While it runs, the migration needs about twice the bytes of
the source's records plus 256 MiB on the device of the target, and says
so before the load begins:
`the load needs about … MiB on the device of … while it runs; …`, with
how much is free there. The RocksDB database stays where it is, beside
the new one. Step 8 needs room for one copy of the new database, best on
another device, and step 9 for one more where `RESTORE` is, until it is
removed.

**Memory.** A Merk is handed over whole, so the largest Merk of the
source decides the peak. The survey says what peak the run projects
(`the migration projects a peak of about … MiB of memory`): 256 MiB, the
commit size (`--batch`, 160 MiB unless given), the buffer pool
(`--cache`, 64 MiB unless given), and about 1.5 KB with its key and value
for each node of the largest Merk. A run whose projection is above the
limit is refused before it writes anything. On Linux the limit is half of
the machine's memory as `/proc/meminfo` states it, unless
`--memory-limit` says another; where that file is not there, as on
macOS, no limit is held unless `--memory-limit` is given. In a container
the file can state the host's memory: give `--memory-limit` there.

## 1. Build the tool

You need `git`, `curl`, a C and C++ compiler and `clang` with its
library (`libclang`): `migrate` builds RocksDB from source, and its
bindings are made with `libclang`. On Debian or Ubuntu those are the
packages `git`, `curl`, `build-essential`, `clang` and `libclang-dev`; on
macOS, the Xcode command line tools. You also need Rust. If `cargo` is
not on your path, the first block installs it with `rustup`, the
installer the Rust project publishes, for your user alone; the installer
also adds Cargo to your shell's profile files.

Run the blocks in a shell of their own: start `bash`, and give it the
first line of the block below, `set -euo pipefail`, before anything
else. From then on a command that fails, a variable that is not set and
a pipeline whose first command fails (before its report reaches `tee`)
end that shell, so that no later command runs on what a failed one left.
To go on after a failure, start `bash` again, run this first block,
`export` the variables of steps 2 and 4 again, and go on with the step
that failed.

```sh
set -euo pipefail
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "${CARGO_HOME:-$HOME/.cargo}/env"
fi
cargo --version
```

## 2. Set the variables

Set two variables before the next block:

- `GROVEDB_REF`: the GroveDB release your node runs, as a tag or a
  branch of `https://github.com/dashpay/grovedb`. The tool is built from
  the same release, so that it reads and writes what your node does.
- `WORK`: a directory for the source code, the build and the reports of
  each step. The build takes a few GiB.

The block makes `WORK` absolute, since it changes into the source
directory to build. It clones the repository where no clone is there
yet, and then fetches `GROVEDB_REF` and checks it out, so that a clone
that a run before left behind is moved to the release as well. It names
where Cargo builds (`CARGO_TARGET_DIR`), so that the `lignin` it puts
first on the path is the one it built. Give the paths of step 4 as
absolute paths too.

```sh
: "${GROVEDB_REF:?set GROVEDB_REF to the GroveDB release your node runs: a tag or a branch}"
: "${WORK:?set WORK to a directory for the build and the reports}"
mkdir -p "$WORK/reports"
WORK=$(cd "$WORK" && pwd)
if [ ! -d "$WORK/grovedb" ]; then git clone --depth 1 --branch "$GROVEDB_REF" https://github.com/dashpay/grovedb.git "$WORK/grovedb"; fi
cd "$WORK/grovedb"
git fetch --depth 1 origin "$GROVEDB_REF"
git checkout --detach FETCH_HEAD
git log -1 --format='building GroveDB at %H'
export CARGO_TARGET_DIR="$WORK/grovedb/target"
cargo build --release -p lignin-tools --features migrate
export PATH="$CARGO_TARGET_DIR/release:$PATH"
lignin help
```

`lignin help` prints every subcommand. A build without the feature
`migrate` has every subcommand but `migrate`, and says so when it is
asked for it.

## 3. Rehearse on a database of the repository

This step is optional, and worth the few minutes: it runs every later
step on a small RocksDB GroveDB that the repository's benchmark tool
makes, so that you see each report before you meet it on your node's
database. The tool is `lignin-bench` (`docs/lignindb/benchmarks.md`),
which writes the plan's reference workload: a tree of documents and an
index of a reference to each by its owner, here 20,000 documents over
200 owners, with creates, updates and deletes after them. With
`lignin-bench run --keep` its database stays in
`$WORK/rehearsal/rocksdb`. **The benchmark removes what it finds at
`rocksdb` and `rocksdb-checkpoint` in its scratch directory**, so the
block refuses a `$WORK/rehearsal` that is there already. Its report goes
to a file; nothing in it is needed below.

```sh
if [ -e "$WORK/rehearsal" ]; then echo "$WORK/rehearsal is there already, and the benchmark would remove what it holds: choose another WORK"; false; fi
cargo build --release -p lignin-testkit --bin lignin-bench
lignin-bench run --engine rocksdb --keep --scratch "$WORK/rehearsal" --out "$WORK/rehearsal.json" --documents 20000 --owners 200 --creates 200 --updates 200 --deletes 200 --point-reads 20 --reference-reads 5 --warmup-reads 20 --cold-reads 1 --checkpoints 1 --reopens 1 > "$WORK/reports/rehearsal.txt"
SOURCE="$WORK/rehearsal/rocksdb"
TARGET="$WORK/rehearsal-lignindb/database"
BACKUP="$WORK/rehearsal-backup/database"
RESTORE="$WORK/rehearsal-restore/database"
mkdir "$WORK/rehearsal-lignindb" "$WORK/rehearsal-backup" "$WORK/rehearsal-restore"
```

For your node's own migration, skip this block and set the four
variables of step 4 yourself.

## 4. Stop the node and name the directories

Stop the node, as its own documentation says, and make sure that no
other process uses its database: not the node, not a second instance of
it, not a backup job. Where the source has a `LOCK` file, `migrate`
holds a lock on it for as long as it runs: a process that has the source
open is refused (`a process has it open`), and a node that is started
meanwhile cannot open it. A writer whose `LOCK` file was replaced while
it ran is not seen that way, since the new file says nothing of it.

Then name four paths. `SOURCE` is there; `TARGET`, `BACKUP` and
`RESTORE` are not there yet, and the directory each of them is to be in
has to be there: make it first.

- `SOURCE`: the directory of the node's RocksDB GroveDB, the one that
  holds `CURRENT`, `MANIFEST-…` and the `.sst` files. A RocksDB
  checkpoint of it can be the source as well: the migration then holds
  the state of that checkpoint, and a node started on it goes on from
  there.
- `TARGET`: where the LigninDB database is to be.
- `BACKUP`: where the backup of step 8 is to be, best on another device.
- `RESTORE`: where step 9 restores that backup to check it.

```sh
: "${SOURCE:?set SOURCE to the RocksDB directory of the stopped node}"
: "${TARGET:?set TARGET to where the LigninDB database is to be}"
: "${BACKUP:?set BACKUP to where the backup of the new database is to be}"
: "${RESTORE:?set RESTORE to where the backup is to be restored once}"
real_dir() { (cd -P -- "$1" && pwd -P); }
real_new() { printf '%s/%s\n' "$(cd -P -- "$(dirname -- "$1")" && pwd -P)" "$(basename -- "$1")"; }
same_place() {
  if [ -e "$1" ] && [ "$1" -ef "$2" ]; then return 0; fi
  [ "$(dirname -- "$1")" -ef "$(dirname -- "$2")" ] && [ "$(basename -- "$1" | tr '[:upper:]' '[:lower:]')" = "$(basename -- "$2" | tr '[:upper:]' '[:lower:]')" ]
}
within() {
  local d=$1
  while :; do
    if same_place "$d" "$2"; then return 0; fi
    if [ "$d" = / ]; then return 1; fi
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
for path in "$TARGET" "$BACKUP" "$RESTORE"; do
  if [ -e "$path" ] || [ -L "$path" ]; then echo "$path is there already: choose another path"; false; fi
  if [ ! -d "$(dirname -- "$path")" ]; then echo "$(dirname -- "$path") is not there: make it first"; false; fi
done
SOURCE=$(real_dir "$SOURCE")
TARGET=$(real_new "$TARGET")
BACKUP=$(real_new "$BACKUP")
RESTORE=$(real_new "$RESTORE")
apart "$SOURCE" "$TARGET" "$BACKUP" "$RESTORE"
printf 'SOURCE  %s\nTARGET  %s\nBACKUP  %s\nRESTORE %s\n' "$SOURCE" "$TARGET" "$BACKUP" "$RESTORE"
```

The block writes each path again with every link followed (`cd -P` and
`pwd -P`, so that a `..` is taken after the links), and then refuses
four paths of which one is another or lies inside another. It compares
them by the directories they are (`-ef`), and takes two names that
differ in case alone for one, as a file system that ignores case does.
Use the paths it prints from here on. A second mount of one directory
is not seen. The migrator
also refuses a `TARGET` inside `SOURCE`, or around it, before it opens
anything.

## 5. Survey the source

The survey reads every record of the source, says what it holds and
whether it can be migrated, and writes nothing.

```sh
lignin migrate "$SOURCE" --survey | tee "$WORK/reports/survey.txt"
```

Read its report before you go on:

- The first line names the source and its RocksDB identity, and says
  whether its lock is held:
  `no process has it open for writing, and none can open it while this runs`.
  A source with no `LOCK` file is migrated as well, and the line says
  that nothing keeps a writer away.
- `root hash:` is the root hash of the grove, as GroveDB gives it from
  the source. The migration compares the target's with it (step 7).
- The counts after it say what is in the source. A line that begins
  `NOT CARRIED` names something the migrator does not carry, and the
  run is then refused with exit status 1 and
  `the source … holds what a migration does not carry`. The survey
  refuses a keyed child under a tree of flat records (grove versions 1
  to 3 let a caller write one), the mark of a state-sync restore that
  did not finish (such a database is to be discarded, not migrated),
  and records in places where GroveDB writes none; with
  `--unreachable refuse` or `carry`, also the prefixes that nothing leads
  to which that choice does not take.
- `prefixes that nothing leads to:` counts records under prefixes that no
  element leads to (a tree that a batch's plain delete removed while it
  held an element leaves them). Unless `--unreachable` says otherwise
  they are left behind (`leave`), and the target is what a database
  written on LigninDB holds, with the same root hash. What can then
  answer otherwise than on RocksDB is a read by the path of such a tree,
  a tree made again at that path, and a cursor that steps from no key.
  `--unreachable refuse` refuses such a source; `--unreachable carry`
  carries them as records that no tree links, and the engine then refuses
  a write below a tree that is made again at such a path. The module
  documentation of `lignin_tools::migrate` ("Records that nothing leads
  to") says each in full. Choose before step 6 whether your node can meet
  any of these.
- `root-key records that do not name the root of their Merk (GroveDB #1065):`
  counts the records that keep a Merk's root key and name another node
  than that Merk's root. Each Merk is named on a line
  `AT …: THE MERK IS READ AS ITS RECORD NAMES IT, ON ROCKSDB AS HERE, AND LIGNINDB TAKES NO FURTHER WRITE OF A NODE OF IT`.
  Such a source migrates, compares and verifies, and the target reads
  that Merk as RocksDB does; **but LigninDB refuses every later write of
  a node of that Merk, which RocksDB takes**. With a count above 0, do
  not start the node on the target unless you know that it writes
  nothing more into those Merks: keep it on RocksDB, and report the
  survey.
- `the migration projects a peak of about … MiB of memory`, and the
  limit it holds ("What it takes").
- The last line is `the source can be migrated` and the exit status is 0.

## 6. Migrate

```sh
lignin migrate "$SOURCE" "$TARGET" | tee "$WORK/reports/migrate.txt"
```

The run surveys the source again, loads the target a commit at a time,
compares every record of the source that it carries with what the target
answers through GroveDB, checks the target with the physical invariant
checker, and only then gives the database the name `TARGET`. Until that last step it works
in the directory `TARGET.migrating`, so nothing is at `TARGET` that is
not whole, compared and verified. Lines that begin `migrate:` hold
`name=value` words for a script; the others are for you. Every five
seconds (or as `--progress` says) a line says where the load stands:
`migrate: phase=… subtrees=… merks=… nodes=… records=… bytes=… commits=… seconds=…`.

| Exit status | What it means | What to do |
| --- | --- | --- |
| 0 | the database is at `TARGET`, loaded, compared and verified | go on with step 7 |
| 1 | the run failed or refused the source or the target; the error says why. Nothing is at `TARGET`, unless the error says the database got that name and the name could not be made durable | read the error ([the messages](messages.md#lignin-migrate)), fix what it names, and run the same command again. In the one case where the database got its name and the name could not be made durable, keep this report and send the next run's to another file: that run makes the name durable and then refuses the target as one that is there, with status 1 again, which is what it does with a whole database at `TARGET`. This report has every line that step 7 looks for but the last: run step 7's other commands on it, and `lignin verify` on the target |
| 3 | the target differs from the source (`THE TARGET IS NOT ITS SOURCE`) or a rule of the store does not hold (`THE TARGET IS NOT SOUND`). It is left in `TARGET.migrating/database` and is not to be used | keep the report, report it as a defect of GroveDB, and keep running on RocksDB |
| 4 | the device of the target is full (`THE DEVICE OF … IS FULL`) | make room on that device and run the same command again: it goes on from where it stood |

**A run that stops** (a kill, a power cut, a full device, an error) is
gone on with by the same command, with the same `--unreachable`. What
`TARGET.migrating` holds says where the run stood, and a load goes on
from its last commit. To start again from nothing instead, remove
`TARGET.migrating`, but only where a run of the command made it: the
command goes on with its own work directory, and refuses one it did not
make with `… is there and is not the work of a migration: …`, which it
leaves as it is. It tells its own by what its file `MIGRATION` holds to
the byte, not by the names in the directory, so do not remove a
directory of that name that a run refused, or that you cannot tell was
made by a run for this `TARGET`. Nothing else is to open
`TARGET.migrating` while the command runs.

**The options**, for when the defaults do not fit:

| Option | Default | When to give it |
| --- | --- | --- |
| `--memory-limit MIB` | half the machine's memory as `/proc/meminfo` states it; none where that file does not state it | on a machine shared with other work, in a container, or on macOS where no limit is read |
| `--batch MIB` | 160 | about how much one commit of the load holds; a smaller one lowers the projected peak when the memory limit refuses the run and says that a smaller `--batch` or `--cache` can take it |
| `--cache MIB` | 64 | the buffer pool of the target during the run; the same remark |
| `--progress SECS` | 5 | how often the progress line is printed; 0 prints it after every commit |
| `--unreachable refuse\|leave\|carry` | `leave` | see step 5 |

A run that goes on with a work directory must be given the
`--unreachable` choice the run that began it had; it says so if it is
not.

## 7. Verify

The migration has checked its own work. Three things in its report say
so, and the next commands check each of them:

1. The line `migrate: compared subtrees=…` with `differing=0`: every
   record of the source that the migration carries reads the same from
   the target, and the root hash that GroveDB gives the target is the
   source's. The comparison reads the target's root hash through
   GroveDB and counts it among `differing` where it is another; the
   `root_hash=…` that the line prints is the source's, from the survey.
   It also says so in words:
   `every record of the source that this migration carries reads the same from the target`.
2. The line `migrate: verify sound=… left_hashes=…` with `sound=true`:
   the physical invariant checker found every rule of the store to
   hold, with every stored hash made again by Merk's rules but those the
   rules take as they are stored (the value hash of a reference).
3. The last line, `migrated … into …: loaded, compared and verified`.

```sh
grep -E '^migrate: compared .* differing=0 ' "$WORK/reports/migrate.txt"
grep '^migrate: verify sound=true ' "$WORK/reports/migrate.txt"
grep ': loaded, compared and verified$' "$WORK/reports/migrate.txt"
survey_hash=$(sed -n 's/^  root hash: //p' "$WORK/reports/survey.txt")
target_hash=$(grep -o 'root_hash=[0-9a-f]*' "$WORK/reports/migrate.txt" | cut -d= -f2)
if [ -n "$survey_hash" ] && [ "$survey_hash" = "$target_hash" ]; then echo "the same root hash: $target_hash"; else echo "THE ROOT HASHES DIFFER: survey $survey_hash, target $target_hash"; false; fi
if [ "$(sed -n '1s/;.*//p' "$WORK/reports/survey.txt")" = "$(sed -n '1s/;.*//p' "$WORK/reports/migrate.txt")" ]; then echo "the same RocksDB identity and last write in both runs"; else echo "THE SOURCE CHANGED BETWEEN THE TWO RUNS"; false; fi
```

Each of these prints the line it found, or fails: `grep` with no line
found, the last two commands with the words THE ROOT HASHES DIFFER or
THE SOURCE CHANGED BETWEEN THE TWO RUNS. The first of the two holds the
root hash that the survey of step 5 read from the source to the one the
migration read from it. The second holds the first line of each report,
up to its `;`, to the other: the source's path, its RocksDB identity and
the sequence number of its last write. Where they are the same, no
write reached the source between the two runs. That the target has the source's root hash is
what `differing=0` says; the migration also refuses a source that
changes while it reads it. Do not start the node on a target for which
one of them fails.

Then check the target once more on its own, with the checker of
`lignin verify`. It opens the database, closes it, and checks the store
as the close left it: every page, every value, and every stored hash
made again by Merk's rules, but those the rules take as stored. Exit
status 0 is a sound store, 3 a store in
which a rule does not hold, and 4 a database that holds the mark of a
state-sync restore that did not finish (the survey refuses a source with
that mark, so a migration leaves none).

```sh
lignin verify "$TARGET" | tee "$WORK/reports/verify.txt"
```

A grove written on grove versions 1 to 3 can hold value hashes that a
delete below a tree left (GroveDB #686). RocksDB holds them too, they are
no damage, and `verify` counts them after its report without calling the
store unsound.

## 8. Back up the new database

Before the node opens the new database for the first time, copy it. The
database is closed and no process has it open, so a copy of its
directory is a consistent backup ([backup and full disk](backup-and-full-disk.md)).
`cp -a` copies every file with the `LOCK` file, which an open needs. `cp`
returns before its bytes are on the device: on Linux, `sync` returns once
the system has written them. On macOS `sync` can return before they are
written, so a copy made there is durable once its device has been
ejected or the system shut down.

```sh
if [ -e "$BACKUP" ] || [ -L "$BACKUP" ]; then echo "$BACKUP is there already: the copy would go into it"; false; fi
cp -a "$TARGET" "$BACKUP"
sync
lignin manifest "$BACKUP"
```

`lignin manifest` reads the backup's manifest without opening the
database, and lists the files it names with the length the manifest
gives each; a file that is not there is marked `MISSING`, and one that
is shorter `SHORT`. It exits 0 either way, and reads no page: step 9 is
the check of the backup.

## 9. Restore the backup once, and open it

A backup that is to be relied on is checked once. Restore it into a new
directory, open it with `lignin verify`, which checks every page and
hash, and read its counts with `lignin stats`, which opens it again.

```sh
if [ -e "$RESTORE" ] || [ -L "$RESTORE" ]; then echo "$RESTORE is there already: the copy would go into it"; false; fi
cp -a "$BACKUP" "$RESTORE"
sync
lignin verify "$RESTORE" | tee "$WORK/reports/verify-restore.txt"
lignin stats "$RESTORE"
```

Every open writes to the directory it opens (the recovery starts a log
file of its own, and the close writes back), so the check is made on the
restore and not on the backup. The restore is a copy of the database
that nothing needs: remove it once you have read the two reports, by
the path that step 4 printed for `RESTORE`.

## 10. Start the node on LigninDB

Point the node at `TARGET` and select the LigninDB engine, as the
application's configuration says. On GroveDB's side, an application
opens a database on LigninDB with
`GroveDb::from_engine(EitherEngine::open_with(path, &EngineChoice::Lignin(options))?)`,
or with `LigninEngine::open_with(path, &options)`
([engine.md](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/engine.md#an-engine-chosen-at-the-open)); the options are
those of [tuning](tuning.md). Start the node.

From its first commit on, the node's state is in `TARGET` alone. Keep
`SOURCE` until you have decided not to go back.

## 11. The way back

The source was never written, so it is what it was when the migration
read it: the node's database as it was when the node was stopped in
step 4, or, where `SOURCE` was a RocksDB checkpoint, the state of that
checkpoint. In the second case the node's own RocksDB directory can hold
commits after the checkpoint, and it is that directory, and not the
checkpoint, that the node goes back to.

- **Before the node has committed anything on LigninDB**, going back is
  pointing the node at its RocksDB database again and selecting RocksDB.
  `TARGET` can then be removed.
- **After it has committed on LigninDB**, `SOURCE` is still the state at
  the migration, and what the node committed since is in `TARGET` alone.
  GroveDB has no tool that migrates a LigninDB database back to RocksDB. A
  node that goes back starts from `SOURCE` and has to apply again what it
  applied since the migration, which is the application's to do.
- **A migration that failed** left nothing at `TARGET` (exit status 1
  and 4, unless the error says otherwise) or left its database in
  `TARGET.migrating` (exit status 3): start the node on `SOURCE` as
  before, and remove `TARGET.migrating` when it is no longer needed for a
  report, after looking into it as step 6 says.

Remove `SOURCE` only when the node has run on LigninDB for as long as you
want a way back, and after a backup of the LigninDB database
([backup and full disk](backup-and-full-disk.md)). **The migration
carries GroveDB's records and nothing else**: a file that the
application keeps in the RocksDB directory beside RocksDB's own is not
in `TARGET`. Move what the application keeps there before `SOURCE` goes,
as its documentation says.
