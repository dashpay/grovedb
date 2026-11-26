#!/usr/bin/env bash
# campaign.sh [OUT]
#
# Every table of PERF-07-F1, on the machine it is run on: the commit of a
# block on both engines in one run, the control, the three experiments, and
# what a long commit waited for. Run it from a clean checkout, on Linux, as
# a user whom `sudo` asks for no password. OUT (default: `perf07f1` under the
# current directory) is where everything goes, and its file system is the
# one that is measured: put it on the disk and the file system in question
# (ext4 on a local disk, for the reference class of the plan).
#
# It wants git, cargo and what GroveDB's build wants, python3, bpftrace,
# xfsprogs, e2fsprogs and util-linux. On Ubuntu:
#
#   sudo apt-get install build-essential clang libclang-dev cmake pkg-config \
#     git python3 bpftrace xfsprogs
#
# What it does, each run only where its report is not there yet, so that a
# campaign that was stopped goes on where it was:
#
# 1. `builds`: three builds of `lignin-bench`, each from a clean checkout of
#    this checkout's commit in a target directory of its own (`build.sh`):
#    `base`, and the two experiments, each of which is `base` with one patch
#    of this directory (`log-allocated-ahead.patch`, `fewer-syncs.patch`).
# 2. `main`: on OUT's own file system, $ROUNDS rounds under each background
#    of $BACKGROUNDS, each of four runs of $BLOCKS blocks, one after the
#    other, every run with both engines: `base` and `base-again` (one binary
#    under two names: the control), `allocated` and `covered` (the
#    experiments).
# 3. `filesystems`: the same rounds of three runs, every run with both
#    engines, on two file systems that the script makes in files under OUT
#    and mounts through loop devices with direct I/O: `ext4` and
#    `ext4-again` (`base` under two names on ext4: the control) and `xfs`
#    (`base` on XFS). A sync of such a file system ends in a sync of its
#    file on OUT's file system, so these rows are held against one another
#    and not against those of part 2.
# 4. `traces`: runs of $TRACE_BLOCKS blocks with the kernel trace of
#    `trace.bt` beside them (`traced.sh`): each engine under each background
#    on OUT's file system, LigninDB once more with every thread followed,
#    the two experiments, and LigninDB on ext4, on ext4 with `lazytime` and
#    on XFS in files as in part 3, and the first experiment on ext4 with
#    `lazytime`.
# 5. `tables`: OUT/tables.md, from all of it (`tables.py`, `waits.py`).
#
# Before part 2, where traces are asked for, a traced run of 1,000 blocks of
# each engine is made and read, so that a tracer that cannot attach here, or
# user stacks that come out cut short, end the campaign in its first minutes
# and not after its runs.
#
# The tables that an experiment is held by are of parts 2 and 3 and need no
# trace. Where a traced run fails, or its waits cannot be placed, the script
# does not say that it finished; `PARTS=tables` then makes OUT/tables.md with
# the tables of the runs and, in the place of that traced run, why it has
# none.
#
# **The order.** In a round the runs are made in the order above, turned by
# one for every two rounds; an even round makes them in the reverse of the
# round before it, so that in two rounds every two runs have been made in
# both orders. The two engines of a run exchange their order from round to
# round.
#
#   BLOCKS         the measured blocks of a run (default 100000)
#   ROUNDS         the rounds (default 2; an even number has every order twice)
#   BACKGROUNDS    the backgrounds (default `quiet heavy`)
#   TRACE_BLOCKS   the measured blocks of a traced run (default 30000)
#   LOOP_GB        the size of each file system of parts 3 and 4 in GiB;
#                  their files are sparse (default 64)
#   PARTS          the parts to make (default: all five, in their order)
#   LATENCY        further options of `lignin-bench latency` for every run
#   BUILDS         where the builds are made (default: OUT/builds; about
#                  12 GB)
#
# Under `quiet` LigninDB keeps what a run writes, which is taken here as
# 100 KB a block, and the campaign asks for room for two such runs and for
# its builds before it begins: where a file system has less, name fewer
# blocks or other backgrounds. OUT keeps what the campaign was asked
# (`campaign.txt`), and a campaign that is asked something else there, or
# for another commit, is refused: a directory holds the runs of one
# campaign. A build that is there is used only if it is of this commit, of
# this patch, and the binary its checksum names.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
out=${1:-$PWD/perf07f1}
mkdir -p "$out"
out=$(cd "$out" && pwd)
blocks=${BLOCKS:-100000} rounds=${ROUNDS:-2} backgrounds=${BACKGROUNDS:-quiet heavy}
trace_blocks=${TRACE_BLOCKS:-30000} loop_gb=${LOOP_GB:-64}
parts=${PARTS:-builds main filesystems traces tables}
export BUILDS=${BUILDS:-$out/builds}
export PATH=$PATH:/usr/sbin:/sbin
ulimit -n 65536

missing=""
for tool in git cargo python3 sha256sum sudo bpftrace mkfs.xfs mkfs.ext4 losetup findmnt fstrim; do
  command -v "$tool" > /dev/null || missing="$missing $tool"
done
refuse() {
  echo "campaign.sh: $*" >&2
  exit 1
}
[ -z "$missing" ] || refuse "not found:$missing"
# The script keeps lists of paths in words.
case "$out$BUILDS" in *[[:space:]]*) refuse "OUT and BUILDS take a path without a space: $out, $BUILDS" ;; esac
# What a run is of is the campaign's to say, and its tables go by it.
for flag in --engine --background --blocks --label --commit --out --scratch --watch-files; do
  case " ${LATENCY:-} " in *" $flag "*) refuse "LATENCY cannot give $flag: the campaign does" ;; esac
done
[ "$(uname -s)" = Linux ] || refuse "the traces and the file systems want Linux"
sudo -n true 2> /dev/null || refuse "sudo asks for a password"
[ -z "$(git -C "$repo" status --porcelain --untracked-files=no)" ] ||
  refuse "the checkout has changes, and the builds are of its commit"
commit=$(git -C "$repo" rev-parse HEAD)
for patch in log-allocated-ahead fewer-syncs; do
  git -C "$repo" apply --check "$here/$patch.patch" ||
    refuse "$patch.patch does not apply to this commit"
done

asked="commit $commit
blocks $blocks
rounds $rounds
backgrounds $backgrounds
trace blocks $trace_blocks
file systems of $loop_gb GiB
further options ${LATENCY:-none}"
if [ -s "$out/campaign.txt" ]; then
  [ "$(cat "$out/campaign.txt")" = "$asked" ] ||
    refuse "$out holds another campaign; what it was asked is in $out/campaign.txt"
else
  most=$((blocks > trace_blocks ? blocks : trace_blocks))
  case " $backgrounds " in
    *" quiet "*) wanted=$((most * 200 + 2097152)) ;;
    *) wanted=$((most * 40 + 4194304)) ;;
  esac
  case "$BUILDS/" in "$out/"*) [ -s "$BUILDS/exp2/sha256" ] || wanted=$((wanted + 12582912)) ;; esac
  free=$(df -k --output=avail "$out" | tail -1)
  [ "$free" -ge "$wanted" ] ||
    refuse "$out has $((free / 1048576)) GiB free, and this campaign wants $((wanted / 1048576 + 1))"
  echo "$asked" > "$out/campaign.txt"
fi

has() {
  case " $parts " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}
say() { echo "$(date -u +%FT%TZ) $*" | tee -a "$out/progress.log"; }

# How long a sync takes on the file system of a directory.
probe() {
  python3 - "$1" << 'PROBE'
import os, sys, time
path = os.path.join(sys.argv[1], "probe.bin")
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
took = []
for _ in range(1000):
    os.write(fd, b"x" * 4096)
    began = time.perf_counter_ns()
    os.fdatasync(fd)
    took.append(time.perf_counter_ns() - began)
os.close(fd)
os.unlink(path)
took.sort()
print("a sync of a 4 KiB append, of 1,000: median %.2f ms, 99%% %.2f ms, longest %.2f ms"
      % (took[499] / 1e6, took[989] / 1e6, took[-1] / 1e6))
PROBE
}
describe() { # describe NAME DIRECTORY
  local source
  source=$(findmnt -no SOURCE -T "$2")
  {
    echo "$1: $(findmnt -no SOURCE,FSTYPE,OPTIONS -T "$2")"
    case "$(findmnt -no FSTYPE -T "$2")" in
      ext4) sudo dumpe2fs -h "$source" 2> /dev/null | grep -E "^Total journal size" | sed "s/^/$1: /" || true ;;
      xfs) xfs_info "$2" 2> /dev/null | grep -E "^log" | sed "s/^/$1: /" || true ;;
    esac
    echo "$1: $(probe "$2")"
  } >> "$out/host.txt"
}
if [ ! -s "$out/host.txt" ]; then
  {
    echo "commit $commit"
    echo "kernel $(uname -srm)"
    lscpu | grep -E "^(Model name|CPU\(s\)|Thread\(s\) per core|Hypervisor vendor|Virtualization type)" | sed 's/  */ /g' || true
    echo "memory $(awk '/MemTotal/ { printf "%.0f GiB", $2 / 1048576 }' /proc/meminfo)"
    echo "load average $(cut -d' ' -f1-3 /proc/loadavg)"
    lsblk -n -o NAME,SIZE,ROTA,TYPE,MODEL -s "$(findmnt -no SOURCE -T "$out")" 2> /dev/null |
      sed 's/^/device (name, size, rotational, type, model) /' || true
  } > "$out/host.txt"
  describe "the file system of the runs" "$out"
fi

# Part 1.
# Whether a build that is there is the one that would be made: of this
# commit, with this patch or none, and the binary its checksum names.
built() { # built NAME [PATCH]
  local to=$BUILDS/$1 patch=${2:-} parent
  [ -s "$to/sha256" ] && [ -x "$to/lignin-bench" ] || return 1
  [ "$(sha256sum "$to/lignin-bench" | cut -d' ' -f1)" = "$(cat "$to/sha256")" ] || return 1
  if [ -z "$patch" ]; then
    [ "$(cat "$to/commit")" = "$commit" ] && [ ! -e "$to/patch-sha256" ]
  else
    parent=$(git -C "$to/src" rev-parse "$(cat "$to/commit")^" 2> /dev/null) || return 1
    [ "$parent" = "$commit" ] &&
      [ "$(cat "$to/patch-sha256" 2> /dev/null)" = "$(sha256sum "$patch" | cut -d' ' -f1)" ]
  fi
}
if has builds; then
  mkdir -p "$BUILDS"
  built base || REPO=$repo "$here/build.sh" base "$commit"
  built exp1 "$here/log-allocated-ahead.patch" ||
    REPO=$repo "$here/build.sh" exp1 "$commit" "$here/log-allocated-ahead.patch"
  built exp2 "$here/fewer-syncs.patch" ||
    REPO=$repo "$here/build.sh" exp2 "$commit" "$here/fewer-syncs.patch"
  for build in base exp1 exp2; do
    line="$build: commit $(cat "$BUILDS/$build/commit"), sha256 $(cat "$BUILDS/$build/sha256")"
    if [ -f "$BUILDS/$build/patch-sha256" ]; then
      line="$line, which is $commit with a patch of sha256 $(cat "$BUILDS/$build/patch-sha256")"
    fi
    echo "$line"
  done > "$out/builds.txt"
fi

if has main || has filesystems || has traces; then
  { built base && built exp1 "$here/log-allocated-ahead.patch" && built exp2 "$here/fewer-syncs.patch"; } ||
    refuse "the builds under $BUILDS are not those of this commit and these patches: make the part builds"
fi

# Removes a run's database, and gives what it took of a file system of part 3
# back to the file that holds it.
cleared() {
  local dir=${1:?}
  rm -rf "$dir/scratch"
  case "$dir" in
    "$out/mnt/"*)
      # What was freed can be given back once the file system has written
      # that it is free.
      sync -f "$dir"
      sudo fstrim "$dir"
      ;;
  esac
}

# Ends the campaign at a run that failed, with what the run said.
failed() {
  echo "campaign.sh: the run $1 failed; the end of $1.err:" >&2
  tail -5 "$1.err" >&2
  exit 1
}

# Whether a run that is there was made by the build that would make it now:
# a campaign is of one set of builds, and a report says which made it.
made_by() { # made_by FILE BUILD
  [ "$(cat "$1.build" 2> /dev/null)" = "$(cat "$BUILDS/$2/sha256")" ] ||
    refuse "$1 was made by another build than $BUILDS/$2: a campaign is of one set of builds"
}

# One run: run FILE BUILD ENGINES DIRECTORY BACKGROUND LABEL SETTINGS
run() {
  local file=$1 build=$2 engines=$3 dir=${4:?} background=$5 label=$6 settings=$7 flags="" engine
  if [ -s "$file.json" ]; then
    made_by "$file" "$build"
    return 0
  fi
  for engine in $engines; do flags="$flags --engine $engine"; done
  rm -rf "$dir/scratch"
  findmnt -no SOURCE,FSTYPE,OPTIONS -T "$dir" > "$file.mount"
  # shellcheck disable=SC2086
  env $settings "$BUILDS/$build/lignin-bench" latency --scratch "$dir/scratch" $flags \
    --background "$background" --blocks "$blocks" --label "$label" \
    --commit "$(cat "$BUILDS/$build/commit")" ${LATENCY:-} --out "$file.json.part" \
    > "$file.txt" 2> "$file.err" || failed "$file"
  cp "$BUILDS/$build/sha256" "$file.build"
  mv "$file.json.part" "$file.json"
  cleared "$dir"
  say "$(basename "$file"): $(grep -E '^\| [^B-]' "$file.txt" | tr '\n' ' ')"
}

# Before the runs: that a traced run can be made here and its waits placed.
# Every time a thread is off a processor for 20 µs counts, so that a short
# run has enough of them to say.
preflight() { # preflight ENGINE
  local file=$out/preflight/$1 waits
  [ -e "$file.read" ] && return 0
  mkdir -p "$out/preflight" "$out/native"
  rm -rf "$out/native/scratch"
  OFF_US=20 BIN="$BUILDS/base/lignin-bench" "$here/traced.sh" "$file" "$out/native/scratch" \
    --engine "$1" --background heavy --blocks 1000 --label preflight \
    --commit "$commit" 2> "$file.err" || failed "$file"
  rm -rf "$out/native/scratch"
  python3 "$here/waits.py" --marked "$file.trace" 20 > "$file.waits.md" ||
    refuse "the trace of a short run of $1 cannot be read, so no traced run would be: $file.trace"
  touch "$file.read"
  waits=$(sed -n '1s/.*: \([0-9]*\) in the measured blocks.*/\1/p' "$file.waits.md")
  if [ "${waits:-0}" -ge 10 ]; then
    say "a traced run of $1 can be made here, and its $waits waits can be placed"
  else
    say "a traced run of $1 can be made here; it had ${waits:-0} waits, too few to say whether stacks are whole"
  fi
}
if has traces && { has main || has filesystems; }; then
  preflight lignindb
  preflight rocksdb
fi

# Rounds of entries, each `label|build|directory|settings`, one run after the
# other, every run with both engines. The order of a round is the order
# given, turned by one for every two rounds, and an even round is the
# reverse of the round before it. The two engines exchange their order from
# round to round.
rounds_of() { # rounds_of DIRECTORY ENTRY...
  local to=$1
  shift
  local entries=("$@") count=$# round background place index entry label build dir settings engines
  mkdir -p "$to"
  for round in $(seq 1 "$rounds"); do
    if [ $((round % 2)) = 1 ]; then engines="rocksdb lignindb"; else engines="lignindb rocksdb"; fi
    for background in $backgrounds; do
      for place in $(seq 0 $((count - 1))); do
        index=$(((place + (round - 1) / 2) % count))
        [ $((round % 2)) = 1 ] || index=$(((count - 1 - place + (round - 1) / 2) % count))
        entry=${entries[$index]}
        IFS='|' read -r label build dir settings <<< "$entry"
        run "$to/$label-$background-r$round" "$build" "$engines" "$dir" "$background" "$label" "$settings"
      done
    done
  done
}
allocated="LIGNIN_LOG_ALLOCATE=fallocate" covered="LIGNIN_COVER_BYTES=1048576"

# Part 2.
if has main; then
  mkdir -p "$out/native"
  rounds_of "$out/runs/main" \
    "base|base|$out/native|" \
    "base-again|base|$out/native|" \
    "allocated|exp1|$out/native|$allocated" \
    "covered|exp2|$out/native|$covered"
fi

# The file systems of parts 3 and 4, in files under OUT. What this run of the
# script mounted, it unmounts, and it frees the loop devices it took.
mounted="" devices=""
unmount() {
  local dir device
  for dir in $mounted; do sudo umount "$dir" 2> /dev/null || true; done
  for device in $devices; do sudo losetup -d "$device" 2> /dev/null || true; done
}
trap unmount EXIT
filesystem() { # filesystem NAME MKFS MOUNT-OPTIONS
  local dir=$out/mnt/$1 image=$out/fs/$1.img device
  mkdir -p "$dir" "$out/fs"
  if ! findmnt -n "$dir" > /dev/null; then
    # The file system is made on the loop device and not in the file: with
    # direct I/O the device has the sectors of the disk under OUT, and a
    # file system made for other sectors is refused at its mount.
    [ -f "$image.made" ] || { rm -f "$image" && truncate -s "${loop_gb}G" "$image"; }
    device=$(sudo losetup --find --show --direct-io=on "$image")
    devices="$devices $device"
    if [ ! -f "$image.made" ]; then
      # shellcheck disable=SC2086
      sudo $2 "$device" > /dev/null 2>&1 || refuse "$2 failed for $1"
      touch "$image.made"
    fi
    sudo mount ${3:+-o "$3"} "$device" "$dir" || refuse "$1 could not be mounted"
    mounted="$mounted $dir"
    sudo chown "$(id -u):$(id -g)" "$dir"
  fi
  grep -q "^$1: " "$out/host.txt" || describe "$1" "$dir"
}
if has filesystems || has traces; then
  filesystem ext4 "mkfs.ext4 -q -F" ""
  filesystem xfs "mkfs.xfs -q -f" ""
fi
if has traces; then
  filesystem ext4-lazytime "mkfs.ext4 -q -F" "lazytime"
fi

# Part 3.
if has filesystems; then
  rounds_of "$out/runs/filesystems" \
    "ext4|base|$out/mnt/ext4|" \
    "ext4-again|base|$out/mnt/ext4|" \
    "xfs|base|$out/mnt/xfs|"
fi

# Part 4. One traced run: traced FILE BUILD ENGINE DIRECTORY BACKGROUND THREADS SETTINGS
traced() {
  local file=$1 build=$2 engine=$3 dir=${4:?} background=$5 threads=$6 settings=$7 watch="" status=0
  if [ -e "$file.ok" ] && [ -s "$file.trace" ] && [ -s "$file.json" ]; then
    made_by "$file" "$build"
    return 0
  fi
  [ "$engine" = lignindb ] && watch="--watch-files"
  rm -rf "$dir/scratch"
  findmnt -no SOURCE,FSTYPE,OPTIONS -T "$dir" > "$file.mount"
  # shellcheck disable=SC2086
  env $settings BIN="$BUILDS/$build/lignin-bench" THREADS="$threads" "$here/traced.sh" "$file" \
    "$dir/scratch" --engine "$engine" --background "$background" --blocks "$trace_blocks" \
    $watch --label "$(basename "$file")" --commit "$(cat "$BUILDS/$build/commit")" ${LATENCY:-} \
    2> "$file.err" || status=$?
  cp "$BUILDS/$build/sha256" "$file.build"
  cleared "$dir"
  # The runs that the experiments are held by are made: a traced run that
  # failed is said, the others are made, and the tables say which has none.
  if [ "$status" != 0 ]; then
    rm -f "$file.ok"
    traces_failed="$traces_failed $(basename "$file")"
    say "the traced run $(basename "$file") failed: $(tail -1 "$file.err")"
    return 0
  fi
  say "traced $(basename "$file"): $(grep -E '^\| [^B-]' "$file.txt" | tr '\n' ' ')"
}
traces_failed=""
if has traces; then
  mkdir -p "$out/traces" "$out/native"
  first=${backgrounds%% *}
  for background in $backgrounds; do
    traced "$out/traces/lignindb-$background" base lignindb "$out/native" "$background" "" ""
    traced "$out/traces/rocksdb-$background" base rocksdb "$out/native" "$background" "" ""
  done
  traced "$out/traces/lignindb-$first-every-thread" base lignindb "$out/native" "$first" all ""
  traced "$out/traces/allocated-$first" exp1 lignindb "$out/native" "$first" "" "$allocated"
  traced "$out/traces/covered-$first" exp2 lignindb "$out/native" "$first" "" "$covered"
  for name in ext4 ext4-lazytime xfs; do
    traced "$out/traces/lignindb-$first-on-$name" base lignindb "$out/mnt/$name" "$first" "" ""
  done
  traced "$out/traces/allocated-$first-on-ext4-lazytime" exp1 lignindb \
    "$out/mnt/ext4-lazytime" "$first" "" "$allocated"
fi

# Part 5.
if has tables; then
  made=0
  python3 "$here/tables.py" "$out" > "$out/tables.md.part" || made=$?
  # With 3 the tables of the runs are made and a traced run has none.
  [ "$made" = 0 ] || [ "$made" = 3 ] || exit "$made"
  mv "$out/tables.md.part" "$out/tables.md"
  [ "$made" = 0 ] ||
    refuse "the tables of the runs are in $out/tables.md, and a traced run has no table of its waits: it says why there${traces_failed:+; these traced runs failed:$traces_failed}"
fi
[ -z "$traces_failed" ] || refuse "these traced runs failed:$traces_failed"
echo "finished: the tables are in $out/tables.md"
