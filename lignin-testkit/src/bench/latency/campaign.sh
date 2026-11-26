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
#    other: `base` and `base-again` (one binary under two names: the
#    control), each with both engines in the one run, and `allocated` and
#    `covered` (the experiments, on LigninDB). The order of the four is
#    turned by one from round to round, and so is the order of the two
#    engines of a run.
# 3. `filesystems`: the same rounds on three file systems that the script
#    makes in files under OUT and mounts through loop devices with direct
#    I/O: ext4, ext4 with `lazytime`, and XFS. `ext4` and `xfs` are `base`
#    with both engines, `ext4-lazytime` and `ext4-lazytime-allocated` are
#    `base` and the first experiment on LigninDB. A sync of such a file
#    system ends in a sync of its file on OUT's file system, so these rows
#    are held against one another and not against those of part 2.
# 4. `traces`: runs of $TRACE_BLOCKS blocks with the kernel trace of
#    `trace.bt` beside them (`traced.sh`): each engine under each background
#    on OUT's file system, LigninDB once more with every thread followed,
#    the two experiments, and LigninDB on each file system of part 3.
# 5. `tables`: OUT/tables.md, from all of it (`tables.py`, `waits.py`).
#
#   BLOCKS         the measured blocks of a run (default 100000)
#   ROUNDS         the rounds (default 2)
#   BACKGROUNDS    the backgrounds (default `quiet heavy`)
#   TRACE_BLOCKS   the measured blocks of a traced run (default 30000)
#   LOOP_GB        the size of each file system of part 3 in GiB; their
#                  files are sparse (default 64)
#   PARTS          the parts to make (default: all five, in their order)
#   LATENCY        further options of `lignin-bench latency` for every run
#   BUILDS         where the builds are made (default: OUT/builds; about
#                  12 GB)
#
# Under `quiet` LigninDB keeps what a run writes, about 10 GB for 100,000
# blocks: where a file system has less room, name other backgrounds.
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
[ "$(uname -s)" = Linux ] || refuse "the traces and the file systems want Linux"
sudo -n true 2> /dev/null || refuse "sudo asks for a password"
[ -z "$(git -C "$repo" status --porcelain --untracked-files=no)" ] ||
  refuse "the checkout has changes, and the builds are of its commit"
commit=$(git -C "$repo" rev-parse HEAD)
for patch in log-allocated-ahead fewer-syncs; do
  git -C "$repo" apply --check "$here/$patch.patch" ||
    refuse "$patch.patch does not apply to this commit"
done

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
      % (took[500] / 1e6, took[989] / 1e6, took[-1] / 1e6))
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
if has builds; then
  mkdir -p "$BUILDS"
  [ -s "$BUILDS/base/sha256" ] || REPO=$repo "$here/build.sh" base "$commit"
  [ -s "$BUILDS/exp1/sha256" ] ||
    REPO=$repo "$here/build.sh" exp1 "$commit" "$here/log-allocated-ahead.patch"
  [ -s "$BUILDS/exp2/sha256" ] ||
    REPO=$repo "$here/build.sh" exp2 "$commit" "$here/fewer-syncs.patch"
  for build in base exp1 exp2; do
    line="$build: commit $(cat "$BUILDS/$build/commit"), sha256 $(cat "$BUILDS/$build/sha256")"
    if [ -f "$BUILDS/$build/patch-sha256" ]; then
      line="$line, which is $commit with a patch of sha256 $(cat "$BUILDS/$build/patch-sha256")"
    fi
    echo "$line"
  done > "$out/builds.txt"
fi

# Removes a run's database, and gives what it took of a file system of part 3
# back to the file that holds it.
cleared() {
  local dir=${1:?}
  rm -rf "$dir/scratch"
  case "$dir" in "$out/mnt/"*) sudo fstrim "$dir" 2> /dev/null || true ;; esac
}

# One run: run FILE BUILD ENGINES DIRECTORY BACKGROUND LABEL SETTINGS
run() {
  local file=$1 build=$2 engines=$3 dir=${4:?} background=$5 label=$6 settings=$7 flags="" engine
  [ -s "$file.json" ] && return 0
  for engine in $engines; do flags="$flags --engine $engine"; done
  rm -rf "$dir/scratch"
  findmnt -no SOURCE,FSTYPE,OPTIONS -T "$dir" > "$file.mount"
  # shellcheck disable=SC2086
  env $settings "$BUILDS/$build/lignin-bench" latency --scratch "$dir/scratch" $flags \
    --background "$background" --blocks "$blocks" --label "$label" \
    --commit "$(cat "$BUILDS/$build/commit")" ${LATENCY:-} --out "$file.json.part" \
    > "$file.txt" 2> "$file.err"
  mv "$file.json.part" "$file.json"
  cleared "$dir"
  say "$(basename "$file"): $(grep -E '^\| [^B-]' "$file.txt" | tr '\n' ' ')"
}

# Rounds of entries, each `label|build|engines|directory|settings`, one run
# after the other, the order turned by one from round to round and the two
# engines of an entry that has both exchanged.
rounds_of() { # rounds_of DIRECTORY ENTRY...
  local to=$1
  shift
  local entries=("$@") round background index entry label build engines dir settings
  mkdir -p "$to"
  for round in $(seq 1 "$rounds"); do
    for background in $backgrounds; do
      for index in $(seq 0 $((${#entries[@]} - 1))); do
        entry=${entries[$(((index + round - 1) % ${#entries[@]}))]}
        IFS='|' read -r label build engines dir settings <<< "$entry"
        if [ "$engines" = both ]; then
          if [ $((round % 2)) = 1 ]; then engines="rocksdb lignindb"; else engines="lignindb rocksdb"; fi
        fi
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
    "base|base|both|$out/native|" \
    "base-again|base|both|$out/native|" \
    "allocated|exp1|lignindb|$out/native|$allocated" \
    "covered|exp2|lignindb|$out/native|$covered"
fi

# The file systems of parts 3 and 4, in files under OUT.
mounted=""
unmount() {
  local dir
  for dir in $mounted; do sudo umount "$dir" 2> /dev/null || true; done
}
trap unmount EXIT
filesystem() { # filesystem NAME MKFS MOUNT-OPTIONS
  local dir=$out/mnt/$1 image=$out/fs/$1.img device
  mkdir -p "$dir" "$out/fs"
  if ! findmnt -n "$dir" > /dev/null; then
    if [ ! -f "$image" ]; then
      truncate -s "${loop_gb}G" "$image"
      # shellcheck disable=SC2086
      $2 "$image" > /dev/null 2>&1
    fi
    device=$(sudo losetup --find --show --direct-io=on "$image")
    sudo mount ${3:+-o "$3"} "$device" "$dir"
    sudo chown "$(id -u):$(id -g)" "$dir"
  fi
  mounted="$mounted $dir"
  grep -q "^$1: " "$out/host.txt" || describe "$1" "$dir"
}
if has filesystems || has traces; then
  filesystem ext4 "mkfs.ext4 -q -F" ""
  filesystem ext4-lazytime "mkfs.ext4 -q -F" "lazytime"
  filesystem xfs "mkfs.xfs -q -f" ""
fi

# Part 3.
if has filesystems; then
  rounds_of "$out/runs/filesystems" \
    "ext4|base|both|$out/mnt/ext4|" \
    "xfs|base|both|$out/mnt/xfs|" \
    "ext4-lazytime|base|lignindb|$out/mnt/ext4-lazytime|" \
    "ext4-lazytime-allocated|exp1|lignindb|$out/mnt/ext4-lazytime|$allocated"
fi

# Part 4. One traced run: traced FILE BUILD ENGINE DIRECTORY BACKGROUND THREADS SETTINGS
traced() {
  local file=$1 build=$2 engine=$3 dir=${4:?} background=$5 threads=$6 settings=$7 watch=""
  [ -s "$file.json" ] && [ -s "$file.trace" ] && return 0
  [ "$engine" = lignindb ] && watch="--watch-files"
  rm -rf "$dir/scratch"
  findmnt -no SOURCE,FSTYPE,OPTIONS -T "$dir" > "$file.mount"
  # shellcheck disable=SC2086
  env $settings BIN="$BUILDS/$build/lignin-bench" THREADS="$threads" "$here/traced.sh" "$file" \
    "$dir/scratch" --engine "$engine" --background "$background" --blocks "$trace_blocks" \
    $watch --label "$(basename "$file")" --commit "$(cat "$BUILDS/$build/commit")" ${LATENCY:-} \
    2> "$file.err"
  cleared "$dir"
  say "traced $(basename "$file"): $(grep -E '^\| [^B-]' "$file.txt" | tr '\n' ' ')"
}
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
  python3 "$here/tables.py" "$out" > "$out/tables.md.part"
  mv "$out/tables.md.part" "$out/tables.md"
fi
echo "finished: the tables are in $out/tables.md"
