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

**Time.** A run counts as its elements the records of the data
keyspaces it carries: Merk nodes, records that no tree links and flat
records alike (`elements=…` in its line `migrate: done target=…`). The survey of step 5
counts every record of the data keyspaces under the prefixes it reaches
in `data records:`, the flat records among them (`flat records:` gives
those apart), and the records under prefixes that nothing leads to in
`prefixes that nothing leads to: …, with … records`, which a run
carries only with `--unreachable carry`. A source of many flat records
takes longer than its `Merk nodes:` alone says. The plan's acceptance for the migrator (OPS-02 in
[task register 3](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583786))
is at least 40,000 elements a second on the reference shape, measured on
a rented machine with dedicated processors (the least of 20 runs was
40,357); the plan records that a database of many small Merks loads
about 5% slower at the default setting. Before its last line, every run prints a line that says
what it took, `migrate: done target=… seconds=…`, with
`elements_per_second=…` for a run that made the whole target, so the rehearsal of step 3 on your own
machine gives you a rate to plan with.
The node is stopped from step 4 to step 10: add the survey of step 5,
which reads the whole source, `lignin verify` in steps 7 and 9, each of
which reads the whole store again, and the copies of steps 8 and 9,
which are as long as copying the database's bytes.

**Disk.** While it runs, the migration needs about twice the bytes of
the source's records plus 256 MiB on the device of the target, and says
so before the load begins:
`the load needs about … MiB on the device of … while it runs; …`, with
how much is free there. That figure is the tool's estimate from the
records the survey counts under the prefixes it reaches and from the
root keys. With `--unreachable carry`, add twice the bytes the survey
gives for the records under prefixes that nothing leads to, which a run
then carries too. The records of pending drops and the marks a run
writes for them (the survey's line
`pending drops: …, which leave … records behind and … marks`) are not
in it either. The RocksDB database stays where it is, beside
the new one. Step 8 needs room for one copy of the new database, best on
another device, and step 9 for one more where `RESTORE` is, until it is
removed.

**Memory.** A Merk is handed over whole, so the largest Merk of the
source is the part of the peak that grows with one Merk. The survey
keeps, besides, the prefixes of every subtree and of the aux and meta
records, which grow with the number of subtrees, and a run writes the
records of pending drops and their marks in its last commit, all held
at once; neither is in the projection, so a source of very many
subtrees or pending drops needs more than it says. The
survey says what peak the run projects
(`the migration projects a peak of about … MiB of memory`): 256 MiB, the
commit size (`--batch`, 160 MiB unless given), the buffer pool
(`--cache`, 64 MiB unless given), and about 1.5 KB with its key and value
for each node of the largest of what it hands over whole: the largest
Merk, or, with `--unreachable carry`, a prefix that nothing leads to,
whose aux and meta records count as nodes with three times their bytes. A run whose projection is above the
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
To go on after a failure, start `bash` again, run this block and the
block of step 2 again (it clones nothing that is there and builds only
what changed, and puts `lignin` on the path), `export` the four paths as
step 4 printed them, and go on with the step that failed. Steps 3, 8 and 9 claim a directory
first, and run again they refuse the one that the failed run claimed:
remove that directory, which the step made and which holds nothing but
what it built or copied (`$WORK/rehearsal` for step 3, `BACKUP` or
`RESTORE` for steps 8 and 9), and run the step again. Do not run step 4
again once `TARGET` is there: its first check refuses it.

```sh
set -euo pipefail
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "${CARGO_HOME:-$HOME/.cargo}/env"
fi
cargo --version
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

The functions at the end of the block are what the later steps use to
write a path again with every link followed (`cd -P`, so that a `..` is
taken after the links), to refuse a path with a line break in it, to
refuse two paths of which one is the other or lies inside it (compared
by the directories they are, `-ef`, and by names that differ in case
alone), and to claim a new directory with `mkdir`, which fails where
anything is there already. The claim also refuses a name that ends in
`.creating`, under which LigninDB builds a checkpoint, and a path inside
a directory of such a name or one that holds a `MANIFEST` or a
`MANIFEST.deleting` (LigninDB's) or a `CURRENT` (RocksDB's): a
database or checkpoint, whose deletion would remove what is inside it.

## 2. Set the variables

Set two variables before the next block:

- `GROVEDB_REF`: the tag of the GroveDB release that the node is to run
  on LigninDB, in `https://github.com/dashpay/grovedb`: a release with
  the package `lignin-tools`, which the build of this block needs, and
  that reads the node's RocksDB database. A release without it fails the
  build (cargo finds no package `lignin-tools`). The tool is built from
  that release, so that it writes what the node will read. A branch is
  taken as it stands when the block runs, and can move between two runs
  of it: a migration that is gone on with after a failure is then gone
  on with by another build. The block prints the commit it builds, on
  the line that begins with the words building GroveDB at; on a run of
  it after a failure it is to print the same one.
- `WORK`: a directory for the source code, the build and the reports of
  each step. The build takes a few GiB.

The block makes `WORK` absolute, since it changes into the source
directory to build. It clones the repository where no clone is there
yet, and refuses a `grovedb` that is a link, one that is no clone of
its own (a directory inside another repository, as `WORK` being a
GroveDB checkout makes it), one without the mark the block sets in the
clone's configuration when it makes it (`grovedb.runbook`), one whose
`origin` is not the repository's address, and one with changes of its
own: a checkout that is not the one this block made is not moved. It
also refuses a `WORK` with a colon in it, which `PATH` would split, and
a `WORK` or `reports` that holds a link or a file with a second name, or
a `reports` that is a link, since the reports are written through `tee`
and the rehearsal's through `lignin-bench`, which write where the name
leads; nothing but these steps is to write into `WORK`. It then fetches `GROVEDB_REF` from the
repository's address and checks it out, so that a clone that a run
before left behind is moved to the release as well. It names
where Cargo builds (`CARGO_TARGET_DIR`), removes the `lignin` a build
before left there, and refuses to go on where the `lignin` first on the
path is not the one this build made. Give the paths of step 4 as
absolute paths too.

```sh
: "${GROVEDB_REF:?set GROVEDB_REF to the GroveDB release your node runs: a tag or a branch}"
: "${WORK:?set WORK to a directory for the build and the reports}"
case "/$WORK/" in */../*) echo "$WORK has a .. in it, which mkdir -p could make lead elsewhere: give WORK without one"; false ;; esac
outside_databases "$WORK/reports/survey.txt"
mkdir -p "$WORK/reports"
WORK=$(real_dir "$WORK")
case "$WORK" in *:*) echo "$WORK has a colon in it, which PATH takes for a separator: give a WORK without one"; false ;; esac
if [ -L "$WORK/reports" ] || [ -n "$(find "$WORK" -maxdepth 1 \( -type l -o -type f -links +1 \); find "$WORK/reports" \( -type l -o -type f -links +1 \))" ]; then echo "$WORK or $WORK/reports holds a link or a file with a second name: give a WORK of your own"; false; fi
if [ -L "$WORK/grovedb" ]; then echo "$WORK/grovedb is a link: give a WORK with no grovedb in it"; false; fi
if [ ! -e "$WORK/grovedb" ]; then git clone --depth 1 --branch "$GROVEDB_REF" https://github.com/dashpay/grovedb.git "$WORK/grovedb" && git -C "$WORK/grovedb" config grovedb.runbook clone; fi
if [ "$(git -C "$WORK/grovedb" rev-parse --show-toplevel)" != "$(real_dir "$WORK/grovedb")" ]; then echo "$WORK/grovedb is no clone of its own (it lies inside another repository): give a WORK with no grovedb in it"; false; fi
if [ "$(git -C "$WORK/grovedb" config --get grovedb.runbook)" != clone ]; then echo "$WORK/grovedb is not the clone this block makes: give a WORK with no grovedb in it"; false; fi
if [ "$(git -C "$WORK/grovedb" remote get-url origin)" != https://github.com/dashpay/grovedb.git ]; then echo "$WORK/grovedb is no clone of https://github.com/dashpay/grovedb.git: give a WORK with no grovedb in it"; false; fi
cd "$WORK/grovedb"
if [ -n "$(git status --porcelain)" ]; then echo "$WORK/grovedb has changes of its own: give a WORK with no clone in it"; false; fi
git fetch --depth 1 https://github.com/dashpay/grovedb.git "$GROVEDB_REF"
git checkout --detach FETCH_HEAD
git log -1 --format='building GroveDB at %H'
export CARGO_TARGET_DIR="$WORK/grovedb/target"
rm -f "$CARGO_TARGET_DIR/release/lignin"
cargo build --release -p lignin-tools --features migrate
export PATH="$CARGO_TARGET_DIR/release:$PATH"
if [ "$(command -v lignin)" != "$CARGO_TARGET_DIR/release/lignin" ]; then echo "the lignin on the path is not the one this block built: a build target set in the configuration of cargo puts it elsewhere; run this block where that configuration sets none"; false; fi
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
block claims `$WORK/rehearsal` with `mkdir` first, which fails where it
is there already. Its report goes
to a file; nothing in it is needed below.

```sh
claim_dir "$WORK/rehearsal"
cargo build --release -p lignin-testkit --bin lignin-bench
lignin-bench run --engine rocksdb --keep --scratch "$WORK/rehearsal" --out "$WORK/rehearsal.json" --documents 20000 --owners 200 --creates 200 --updates 200 --deletes 200 --point-reads 20 --reference-reads 5 --warmup-reads 20 --cold-reads 1 --checkpoints 1 --reopens 1 > "$WORK/reports/rehearsal.txt"
SOURCE="$WORK/rehearsal/rocksdb"
TARGET="$WORK/rehearsal-lignindb/database"
BACKUP="$WORK/rehearsal-backup/database"
RESTORE="$WORK/rehearsal-restore/database"
mkdir -p "$WORK/rehearsal-lignindb" "$WORK/rehearsal-backup" "$WORK/rehearsal-restore"
```

For your node's own migration, skip this block and set the four
variables of step 4 yourself.

## 4. Stop the node and name the directories

Stop the node, as its own documentation says, and make sure that no
other process uses its database: not the node, not a second instance of
it, not a backup job. Where the source has a `LOCK` file, `migrate`
holds a shared lock on it for as long as it runs: a process that has
the source open for writing is refused (`a process has it open`), and a
node that is started meanwhile cannot open it to write. Other readers
are not refused, which is why nothing else is to use the database. A writer whose `LOCK` file was replaced while
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
for path in "$TARGET" "$BACKUP" "$RESTORE"; do
  if [ -e "$path" ] || [ -L "$path" ]; then echo "$path is there already: choose another path"; false; fi
  if [ ! -d "$(dirname -- "$path")" ]; then echo "$(dirname -- "$path") is not there: make it first"; false; fi
done
SOURCE=$(real_dir "$SOURCE")
TARGET=$(real_new "$TARGET")
BACKUP=$(real_new "$BACKUP")
RESTORE=$(real_new "$RESTORE")
apart "$SOURCE" "$TARGET" "$BACKUP" "$RESTORE"
outside_databases "$TARGET"
if within "$WORK" "$SOURCE"; then echo "$WORK lies inside $SOURCE: give a WORK outside the source"; false; fi
printf 'SOURCE  %s\nTARGET  %s\nBACKUP  %s\nRESTORE %s\n' "$SOURCE" "$TARGET" "$BACKUP" "$RESTORE"
```

The block writes each path again with every link followed (`cd -P` and
`pwd -P`, so that a `..` is taken after the links), and then refuses
four paths of which one is another or lies inside another. It compares
them by the directories they are (`-ef`), and takes two names that
differ in case alone for one, as a file system that ignores case does.
It refuses a `WORK` inside `SOURCE`, which the reports would be
written into; step 2 has already refused one inside any directory that
holds a database, before it writes anything. It also refuses a `TARGET` inside a directory that holds a `MANIFEST`
or a `MANIFEST.deleting`, or a `CURRENT`, a LigninDB or RocksDB
database or checkpoint whose deletion would remove it, or inside one
whose name ends in `.creating`.
Use the paths it prints from here on. A second mount of one directory
is not seen. The migrator
also refuses a `TARGET` inside `SOURCE`, or around it, before it opens
anything.

## 5. Survey the source

The survey reads every record of the source, says what it holds and
whether it can be migrated, and writes nothing.

```sh
lignin migrate "$SOURCE" --survey 2>&1 | tee "$WORK/reports/survey.txt"
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
- `prefixes that nothing leads to:` counts the prefixes that no element
  leads to, and then the records and bytes under them (a tree that a batch's plain delete removed while it
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
  limit it holds ("What it takes"). The survey projects with the default
  `--batch` and `--cache`, and takes neither. Where it refuses the
  source for memory, and the words after the refusal say that a smaller
  `--batch` or `--cache` can take it, survey again with a
  `--memory-limit` that lets it through, and give step 6 the smaller
  `--batch` and `--cache` with the limit you mean: the migration checks
  its own projection against that limit before it writes anything.
- The last line is `the source can be migrated` and the exit status is 0.

## 6. Migrate

```sh
lignin migrate "$SOURCE" "$TARGET" 2>&1 | tee "$WORK/reports/migrate.txt"
```

The run surveys the source again, loads the target a commit at a time,
compares every record of the source that it carries with what the target
answers through GroveDB, checks the target with the physical invariant
checker, and only then gives the database the name `TARGET`. Until that last step it works
in the directory `TARGET.migrating`, so nothing is at `TARGET` that is
not whole, compared and verified. Lines that begin `migrate:` hold
words of the form name=value for a script; the others are for you. After a
commit of the load, once five seconds (or what `--progress` says) have
gone by since the last such line, a line says where the load stands, so
a long commit can keep it longer, and the last commit of the load says
it whatever the interval:
`migrate: phase=… subtrees=… merks=… nodes=… records=… bytes=… commits=… seconds=…`.

| Exit status | What it means | What to do |
| --- | --- | --- |
| 0 | the database is at `TARGET`, loaded, compared and verified | go on with step 7 |
| 1 | the run failed or refused the source or the target; the error says why. The run made nothing at `TARGET`, but where the error says that the database got that name and the name could not be made durable. What was at `TARGET` before the run is there still: a run that looks at `TARGET` refuses one that is there and holds anything (`the target … is there and …`; an empty directory is taken for none), which is also what a run answers that was given a `TARGET` an earlier run had named, and a run refused before it looks there, as at its source, says nothing of it | read the error ([the messages](messages.md#lignin-migrate)), fix what it names, and run the same command again, with its report going to another file so that this one is kept. Where the database got its name and the name could not be made durable, or a run was stopped after it gave the database its name and before its last line, the next run makes the name durable where it has to and refuses the target as one that is there: that database is whole, but no report of a run that finished is held to it (step 7), so migrate again into a new `TARGET`, and leave that one where it is until you know what it holds. Where a power cut took the name back, the database is in `TARGET.migrating` again, and the same command goes on with it: where it ends with status 0 and a whole report, go on with step 7 |
| 3 | the target differs from the source (`THE TARGET IS NOT ITS SOURCE`) or a rule of the store does not hold (`THE TARGET IS NOT SOUND`). It is left in `TARGET.migrating/database` and is not to be used | keep the report, report it as a defect of GroveDB, and keep running on RocksDB |
| 4 | the device of the target is full (`THE DEVICE OF … IS FULL`), or a quota there is used up | make room on that device, or inside that quota, and run the same command again: it goes on from where it stood |

The status is the pipeline's: a `tee` that cannot write the report (a
device of `WORK` that is full or read-only) fails it too, whatever the
migration did. Where the terminal then shows the last line
`migrated … into …: loaded, compared and verified`, the migration was
made, but no report holds it and step 7 cannot be held to it: make room
for the reports, and migrate again into a new `TARGET`.

**A run that stops** (a kill, a power cut, a full device, an error) is
gone on with by the same command, with the same `--unreachable`. What
`TARGET.migrating` holds says where the run stood, and a load goes on
from its last commit. To start again from nothing instead, remove
`TARGET.migrating`, but only where a run of the command made it: the
command goes on with its own work directory, which it tells by what its
file `MIGRATION` holds to the byte. It also takes over an empty
directory of that name, and one that holds nothing but a `MIGRATION.new`
of the kind a stop of it can leave, and refuses any other with
`… is there and is not the work of a migration: …`, which it leaves as
it is. So keep nothing of your own under that name, and do not remove a
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

A run that goes on with a work directory is to be given the
`--unreachable` choice the run that began it had. Where the source has
prefixes that nothing leads to and the first commit of the load was
made, it refuses another and says so; a run stopped before that commit
holds no choice yet, and where the source has no such prefix, the choice
changes nothing and is not held.

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
survey_hash=$(sed -n 's/^  root hash: //p' "$WORK/reports/survey.txt")
target_hash=$(sed -n 's/^migrate: compared .* root_hash=\([0-9a-f]*\) seconds=.*/\1/p' "$WORK/reports/migrate.txt")
if [ -n "$survey_hash" ] && [ "$survey_hash" = "$target_hash" ]; then echo "the same root hash: $target_hash"; else echo "THE ROOT HASHES DIFFER: survey $survey_hash, target $target_hash"; false; fi
survey_source=$(sed -n '1s/^\(the source .*: RocksDB identity [^,]*, last write [0-9][0-9]*\); .*$/\1/p' "$WORK/reports/survey.txt")
migrate_source=$(sed -n '1s/^\(the source .*: RocksDB identity [^,]*, last write [0-9][0-9]*\); .*$/\1/p' "$WORK/reports/migrate.txt")
case "$survey_source" in "the source $SOURCE: RocksDB identity "*) ;; *) survey_source= ;; esac
if [ -n "$survey_source" ] && [ "$survey_source" = "$migrate_source" ]; then echo "the same RocksDB identity and last write in both runs: $survey_source"; else echo "THE SOURCE CHANGED BETWEEN THE TWO RUNS, or a report does not begin with this SOURCE"; false; fi
last_line="migrated $SOURCE into $TARGET: loaded, compared and verified"
if [ "$(tail -n 1 "$WORK/reports/migrate.txt")" = "$last_line" ]; then echo "the report ends with: $last_line"; else echo "THE REPORT DOES NOT END WITH: $last_line"; false; fi
```

Each of these prints the line it found, or fails: `grep` with no line
found, the three comparisons with the words THE ROOT HASHES DIFFER,
THE SOURCE CHANGED BETWEEN THE TWO RUNS or THE REPORT DOES NOT END WITH. Step 7 reads the report of the
run that ended with status 0 (where a run before it failed, that is the
report the retry of step 6 wrote: name it in place of `migrate.txt`).
The last comparison requires the report to end with the run's last
line, which names this `SOURCE` and this `TARGET`, so that the report is
held to the database you are about to start the node on. A run that was stopped after it gave
the database its name and before that line leaves a report that cannot
be held to what is at `TARGET`: then migrate again into a new `TARGET`,
and leave the old one where it is until you know what it holds. The
first comparison reads the root hash from the line
`migrate: compared subtrees=…` alone and holds the root hash that the
survey of step 5 read from the source to the one the migration read
from it. The second holds the first line of each report, up to the `;` after
its last write, to the other: the source's path, which has to be this
`SOURCE`, its RocksDB identity and the sequence number of its last
write. Where they are the same, no write of RocksDB reached the source
between the two runs; a file that something else put in the directory
is not seen by it. That the target has the
source's root hash is what `differing=0` says; the migration also
refuses a source that changes while it reads it. Do not start the node
on a target for which one of them fails. These checks hold the report to
the path `TARGET`, not to the bytes there: no command of `lignin` reads
the root hash of a LigninDB database, and `lignin verify` below finds
whether what is at `TARGET` is sound, not whether it is the source's.
So put nothing at `TARGET`, and let nothing write there, between step 6
and the start of the node.

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
It holds GroveDB's records alone: the application's files go into
`TARGET` at step 10, after this copy, and a node started on a restore of
it needs the application's files of the same state, as the
application's documentation says: `SOURCE` keeps them where it is the
node's own directory, and where `SOURCE` was a RocksDB checkpoint, which
holds RocksDB's files alone, they are those the application keeps for
that checkpoint's state (step 10).
The block claims `BACKUP` with `mkdir`, which fails where anything is
there already, and copies the database's files into it; `cp -a` copies
every file with the `LOCK` file, which an open needs. `cp`
returns before its bytes are on the device: on Linux, `sync` returns once
the system has written them, and tells of no write that failed. On macOS
`sync` can return before they are written, so a copy made there is
written once its device has been ejected or the system shut down. Either
way, step 9's restore is what the backup is relied on by.

```sh
claim_dir "$BACKUP"
cp -a "$TARGET/." "$BACKUP/"
sync
lignin manifest "$BACKUP"
```

`lignin manifest` reads the backup's manifest without opening the
database, and lists the files it names with the length the manifest
gives each; a file that is not there is marked `, MISSING`, and one
that is shorter `, SHORT: … bytes on disk`. It exits 0 either way, and reads no page: step 9 is
the check of the backup.

## 9. Restore the backup once, and open it

A backup that is to be relied on is checked once. Restore it into a new
directory, open it with `lignin verify`, which checks every page and
hash, and read its counts with `lignin stats`, which opens it again.

```sh
claim_dir "$RESTORE"
cp -a "$BACKUP/." "$RESTORE/"
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

**First**, put into `TARGET` what the application keeps in its RocksDB
directory beside RocksDB's own files, as the application's
documentation says: the migration carries GroveDB's records and nothing
else, and `SOURCE` keeps its own copy for the way back. Where `SOURCE`
was a RocksDB checkpoint, those files are to be the ones of the state
of that checkpoint, as the application keeps them for it: the files in
the node's own directory can be of commits after it, and step 7 does
not see a mismatch between them and the grove. Copy nothing over
a file that `TARGET` has, and put no file there under a name of the
engine's: `LOCK`, `MANIFEST`, `MANIFEST.tmp`, `MANIFEST.deleting`,
`RESERVE`, `RESERVE.new`, `wal`, `seg`, `val` and `table`. On a file system that ignores case, a name that differs from one
of these in case alone is the same name. The engine
takes a file of such a name for its own (an open removes
`MANIFEST.tmp`, and refuses a directory where `wal`, `seg`, `val` or
`table` is not a directory), and an open refuses a directory that holds
`CURRENT`, `IDENTITY`, or a name that begins `MANIFEST-` or `OPTIONS-`
or ends in `.sst`, which it takes for RocksDB's. A file of the
application's under one of these names goes where the application's
documentation says, outside `TARGET`. Then point the
node at `TARGET` and select the LigninDB engine, as the application's
configuration says. On GroveDB's side, an application
opens a database on LigninDB with
`GroveDb::from_engine(EitherEngine::open_with(path, &EngineChoice::Lignin(options))?)`,
or with `LigninEngine::open_with(path, &options)`
([engine.md](../engine.md#an-engine-chosen-at-the-open)); the options are
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
- **After it has committed on LigninDB**, its RocksDB database is still
  the state at the migration, and what the node committed since is in
  `TARGET` alone. GroveDB has no tool that migrates a LigninDB database
  back to RocksDB. A node that goes back starts from what its RocksDB
  database holds, and is brought forward from that state, which is the
  application's to do: where the migration read a checkpoint, that
  database can already hold commits after the checkpoint's.
- **In either case**, stop the node on LigninDB first, and remove
  `TARGET` only once the node has been started on its RocksDB database
  and you have decided not to go back to LigninDB.
- **A migration that failed** made nothing at `TARGET` (exit status 1
  and 4, but for the cases the table of step 6 names) or left its
  database in `TARGET.migrating` (exit status 3): start the node on its
  RocksDB database as before, and remove `TARGET.migrating` when it is
  no longer needed for a report, after looking into it as step 6 says.

Remove `SOURCE` only when the node has run on LigninDB for as long as you
want a way back, and after a backup of the LigninDB database
([backup and full disk](backup-and-full-disk.md)). **The migration
carries GroveDB's records and nothing else**: a file that the
application keeps in the RocksDB directory beside RocksDB's own was put
into `TARGET` at step 10, or where step 10 sends a file of one of the
engine's names, and the node has kept it there since. The
copies in `SOURCE` are those of the migration: do not copy them over
`TARGET`'s, and keep or remove them with `SOURCE`, as the application's
documentation says.
