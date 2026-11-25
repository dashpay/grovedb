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
another device.

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

```sh
set -o pipefail
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "${CARGO_HOME:-$HOME/.cargo}/env"
fi
cargo --version
```

`set -o pipefail` makes a pipeline fail when the command before `tee`
fails, so a failed step below is not hidden by the copy of its report.

## 2. Set the variables

Set two variables before the next block:

- `GROVEDB_REF`: the GroveDB release your node runs, as a tag or a
  branch of `https://github.com/dashpay/grovedb`. The tool is built from
  the same release, so that it reads and writes what your node does.
- `WORK`: a directory for the source code, the build and the reports of
  each step. The build takes a few GiB.

The block makes `WORK` absolute, since it changes into the source
directory to build. Give the paths of step 4 as absolute paths too.

```sh
: "${GROVEDB_REF:?set GROVEDB_REF to the GroveDB release your node runs: a tag or a branch}"
: "${WORK:?set WORK to a directory for the build and the reports}"
mkdir -p "$WORK/reports"
WORK=$(cd "$WORK" && pwd)
git clone --depth 1 --branch "$GROVEDB_REF" https://github.com/dashpay/grovedb.git "$WORK/grovedb"
cd "$WORK/grovedb"
cargo build --release -p lignin-tools --features migrate
export PATH="$WORK/grovedb/target/release:$PATH"
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
`$WORK/rehearsal/rocksdb`. Its report goes to a file; nothing in it is
needed below.

```sh
cargo build --release -p lignin-testkit --bin lignin-bench
lignin-bench run --engine rocksdb --keep --scratch "$WORK/rehearsal" --out "$WORK/rehearsal.json" --documents 20000 --owners 200 --creates 200 --updates 200 --deletes 200 --point-reads 20 --reference-reads 5 --warmup-reads 20 --cold-reads 1 --checkpoints 1 --reopens 1 > "$WORK/reports/rehearsal.txt"
SOURCE="$WORK/rehearsal/rocksdb"
TARGET="$WORK/rehearsal-lignindb/database"
BACKUP="$WORK/rehearsal-backup/database"
RESTORE="$WORK/rehearsal-restore/database"
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

Then name four paths. None of them is to be there yet; the directory
each is in can be, and is made below.

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
for path in "$TARGET" "$BACKUP" "$RESTORE"; do
  if [ -e "$path" ]; then echo "$path is there already: choose another path"; false; fi
done
mkdir -p "$(dirname "$TARGET")" "$(dirname "$BACKUP")" "$(dirname "$RESTORE")"
```

`TARGET` must not be inside `SOURCE`, and `SOURCE` not inside it: the
migration refuses that before it opens anything.

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
- `root hash:` is the root hash of the grove, as GroveDB gives it. Step 7
  compares the target's with it.
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
  written on LigninDB holds, with the same root hash.
  `--unreachable refuse` refuses such a source; `--unreachable carry`
  carries them as records that no tree links. The module documentation of
  `lignin_tools::migrate` ("Records that nothing leads to") says what
  each choice gives a later operation.
- `the migration projects a peak of about … MiB of memory`, and the
  limit it holds ("What it takes").
- The last line is `the source can be migrated` and the exit status is 0.

## 6. Migrate

```sh
lignin migrate "$SOURCE" "$TARGET" | tee "$WORK/reports/migrate.txt"
```

The run surveys the source again, loads the target a commit at a time,
compares every record of the source that it carries with what the target
answers through GroveDB, checks the target with the physical invariant checker, and only
then gives the database the name `TARGET`. Until that last step it works
in the directory `TARGET.migrating`, so nothing is at `TARGET` that is
not whole, compared and verified. Lines that begin `migrate:` hold
`name=value` words for a script; the others are for you. Every five
seconds (or as `--progress` says) a line says where the load stands:
`migrate: phase=… subtrees=… merks=… nodes=… records=… bytes=… commits=… seconds=…`.

| Exit status | What it means | What to do |
| --- | --- | --- |
| 0 | the database is at `TARGET`, loaded, compared and verified | go on with step 7 |
| 1 | the run failed or refused the source or the target; the error says why. Nothing is at `TARGET`, unless the error says the database got that name and the name could not be made durable | read the error ([the messages](messages.md#lignin-migrate)), fix what it names, and run the same command again |
| 3 | the target differs from the source (`THE TARGET IS NOT ITS SOURCE`) or a rule of the store does not hold (`THE TARGET IS NOT SOUND`). It is left in `TARGET.migrating/database` and is not to be used | keep the report, report it as a defect of GroveDB, and keep running on RocksDB |
| 4 | the device of the target is full (`THE DEVICE OF … IS FULL`) | make room on that device and run the same command again: it goes on from where it stood |

**A run that stops** (a kill, a power cut, a full device, an error) is
gone on with by the same command, with the same `--unreachable`. What
`TARGET.migrating` holds says where the run stood, and a load goes on
from its last commit. To start again from nothing instead, remove
`TARGET.migrating`. Nothing else is to open `TARGET.migrating` while the
command runs.

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

1. The line `migrate: compared subtrees=…` with `differing=0` and
   `root_hash=…`: every record of the source that the migration carries
   reads the same from the target, and the root hash that GroveDB gives
   the target is the source's. It also says so in words:
   `every record of the source that this migration carries reads the same from the target`.
2. The line `migrate: verify sound=… left_hashes=…` with `sound=true`:
   the physical invariant checker found every rule of the store to
   hold, with every stored hash made again by Merk's rules.
3. The last line, `migrated … into …: loaded, compared and verified`.

```sh
grep -E '^migrate: compared .* differing=0 ' "$WORK/reports/migrate.txt"
grep '^migrate: verify sound=true ' "$WORK/reports/migrate.txt"
grep ': loaded, compared and verified$' "$WORK/reports/migrate.txt"
survey_hash=$(sed -n 's/^  root hash: //p' "$WORK/reports/survey.txt")
target_hash=$(grep -o 'root_hash=[0-9a-f]*' "$WORK/reports/migrate.txt" | cut -d= -f2)
if [ -n "$survey_hash" ] && [ "$survey_hash" = "$target_hash" ]; then echo "the same root hash: $target_hash"; else echo "THE ROOT HASHES DIFFER: survey $survey_hash, target $target_hash"; false; fi
```

Each of these prints the line it found, or fails: `grep` with no line
found, the last command with the words THE ROOT HASHES DIFFER. Do not start the
node on a target for which one of them fails.

Then check the target once more on its own, with the checker of
`lignin verify`. It opens the database, closes it, and checks the store
as the close left it: every page, every value, and every stored hash
made again by Merk's rules. Exit status 0 is a sound store, 3 a store in
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
`cp -a` copies every file with the `LOCK` file, which an open needs, and
`sync` makes the copy durable: `cp` alone returns before its bytes are on
the device.

```sh
cp -a "$TARGET" "$BACKUP"
sync
lignin manifest "$BACKUP"
```

`lignin manifest` reads the backup's manifest and the files it names
without opening the database, and lists them.

## 9. Restore the backup once, and open it

A backup that is to be relied on is checked once. Restore it into a new
directory, open it with `lignin verify`, which checks every page and
hash, and read its counts with `lignin stats`, which opens it again.

```sh
cp -a "$BACKUP" "$RESTORE"
sync
lignin verify "$RESTORE" | tee "$WORK/reports/verify-restore.txt"
lignin stats "$RESTORE"
rm -rf "${RESTORE:?}"
```

Every open writes to the directory it opens (the recovery starts a log
file of its own, and the close writes back), so the check is made on the
restore and not on the backup. The last command removes the restore; the
backup stays.

## 10. Start the node on LigninDB

Point the node at `TARGET` and select the LigninDB engine, as the
application's configuration says. On GroveDB's side, an application
opens a database on LigninDB with
`GroveDb::from_engine(EitherEngine::open_with(path, &EngineChoice::Lignin(options))?)`,
or with `LigninEngine::open_with(path, &options)`
([engine.md](../engine.md#an-engine-chosen-at-the-open)); the options are
those of [tuning](tuning.md). Start the node.

From its first commit on, the node's state is in `TARGET` alone. Keep
`SOURCE` until you have decided not to go back.

## 11. The way back

The source was never written, so it is the node's database as it was
when the node was stopped in step 4.

- **Before the node has committed anything on LigninDB**, going back is
  pointing the node at `SOURCE` again and selecting RocksDB. `TARGET`
  can then be removed.
- **After it has committed on LigninDB**, `SOURCE` is still the state at
  the migration, and what the node committed since is in `TARGET` alone.
  GroveDB has no tool that migrates a LigninDB database back to RocksDB. A
  node that goes back starts from `SOURCE` and has to apply again what it
  applied since the migration, which is the application's to do.
- **A migration that failed** left nothing at `TARGET` (exit status 1
  and 4, unless the error says otherwise) or left its database in
  `TARGET.migrating` (exit status 3): start the node on `SOURCE` as
  before, and remove `TARGET.migrating` when it is no longer needed for a
  report.

Remove `SOURCE` only when the node has run on LigninDB for as long as you
want a way back, and after a backup of the LigninDB database
([backup and full disk](backup-and-full-disk.md)).
