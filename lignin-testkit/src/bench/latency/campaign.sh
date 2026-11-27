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
# It wants git, cargo and what GroveDB's build wants, python3, bpftrace 0.19
# or later, xfsprogs, e2fsprogs and util-linux. On Ubuntu 24.04:
#
#   sudo apt-get install build-essential clang libclang-dev cmake pkg-config \
#     git python3 bpftrace xfsprogs
#
# The bpftrace of Ubuntu 22.04 and of Debian 12 is older than 0.19 and has
# not the clock the tracer reads: the script says so before it builds, and
# `PARTS="builds main filesystems tables"` makes the tables of the runs, which
# need no trace, with any bpftrace or none.
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
#    `trace.bt` beside them (`traced.sh`), in which the harness marks every
#    block and reads LigninDB's counts at each commit, as the runs of parts
#    2 and 3 do not: each engine under each background
#    on OUT's file system, LigninDB once more with every thread followed,
#    the two experiments, and LigninDB on ext4, on ext4 with `lazytime` and
#    on XFS in files as in part 3, and the first experiment on ext4 with
#    `lazytime`.
# 5. `tables`: OUT/tables.md, from all of it (`tables.py`, `waits.py`).
#
# **Several hosts.** A campaign is of one host. To split the work over hosts
# of one kind, give each its own OUT and the PARTS and BACKGROUNDS it is to
# make, with a background's control and its experiments on one host: every
# OUT is then a campaign of its own, with its own controls and its own
# tables, which name its host. No figure of one is to be held against a
# figure of another.
#
# Before part 2, where traces are asked for, a traced run of 1,000 blocks of
# each engine is made and read, so that a tracer that cannot attach here, or
# user stacks that come out cut short, end the campaign in its first minutes
# and not after its runs. That the stacks are whole wants twenty waits inside
# the blocks to show it; where the short run has fewer, one of 6,000 blocks
# is made, and where that has too few as well the script says that the
# question is open on this machine, and goes on.
#
# **What a campaign assumes of its host.** A host that is given to it alone
# for as long as it runs. A fresh OUT, with nothing mounted under it that
# the campaign did not mount. One copy of the builds a host, on a file
# system of that host. Its reports, traces and the files beside them left
# where the script wrote them, and none copied by hand from one directory
# into another. What the script checks of this it refuses by name; it does
# not look for every way a host can be otherwise.
#
# **What the script touches.** One call at a time has OUT, and one a
# directory of builds: each is locked for as long as the script runs. A
# campaign is of one host and of one file system under OUT (`host-id.txt`):
# a call from another is refused, and so is a report made elsewhere by the
# tables. Under OUT/mnt the script removes and trims only on a file system
# it has verified to be its own: mounted at that very directory, of the kind
# it makes there, from a loop device over its own file under OUT/fs. What a
# stopped call left mounted is verified the same way, used, and unmounted
# when the script ends; a mount it does not know is refused and left alone.
# `build.sh` replaces only a directory that it made.
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
refuse() {
  echo "campaign.sh: $*" >&2
  exit 1
}
mkdir -p "$out"
out=$(cd "$out" && pwd)
# One call at a time has a campaign's directory: the lock is taken before
# anything in it is read, written or removed, and goes with the script.
command -v flock > /dev/null || refuse "not found: flock"
exec 9> "$out/campaign.lock"
flock -n 9 || refuse "another call of campaign.sh has $out: a campaign's directory is one call's at a time"
blocks=${BLOCKS:-100000} rounds=${ROUNDS:-2} backgrounds=${BACKGROUNDS:-quiet heavy}
trace_blocks=${TRACE_BLOCKS:-30000} loop_gb=${LOOP_GB:-64}
parts=${PARTS:-builds main filesystems traces tables}
[ -n "${parts// /}" ] || refuse "PARTS names no part"
for part in $parts; do
  case "$part" in
    builds | main | filesystems | traces | tables) ;;
    *) refuse "PARTS names $part, and the parts are builds, main, filesystems, traces and tables" ;;
  esac
done

# What the campaign is asked. It is said before anything else is looked at
# or set: up to here the script has found its checkout, which wants git, and
# made OUT and taken its lock, and nothing more.
commit=$(git -C "$repo" rev-parse HEAD) || refuse "$repo is no checkout of GroveDB"
asked="directory $out
commit $commit
blocks $blocks
rounds $rounds
backgrounds $backgrounds
trace blocks $trace_blocks
file systems of $loop_gb GiB
further options ${LATENCY:-none}"
# The parts the campaign is of are all that it was ever asked for. A call to
# a campaign that is there adds its parts to them here, before anything that
# can refuse the call: a call that is refused or stopped has said what it
# set out to make, and the tables say which of the campaign's parts are not
# made.
plan() {
  local part
  for part in $parts; do
    grep -qsw "$part" "$out/parts.txt" || echo "$part" >> "$out/parts.txt"
  done
}
if [ -s "$out/campaign.txt" ]; then
  [ "$(cat "$out/campaign.txt")" = "$asked" ] ||
    refuse "$out holds another campaign; what it was asked is in $out/campaign.txt"
  plan
fi

# What is set from here on can refuse a call, and a call to a campaign that
# is there has its parts written down by now.
export BUILDS=${BUILDS:-$out/builds}
export PATH=$PATH:/usr/sbin:/sbin
ulimit -n 65536 2> /dev/null ||
  refuse "the limit of open files is $(ulimit -n) and cannot be raised to 65536, which the engines want"

# A directory of builds is one call's at a time as well, whichever campaign
# it serves.
case " $parts " in *" builds "* | *" main "* | *" filesystems "* | *" traces "*)
  mkdir -p "$BUILDS"
  exec 8> "$BUILDS/builds.lock"
  flock -n 8 || refuse "another call of campaign.sh has the builds under $BUILDS"
  ;;
esac
# The switches of the experiments are the campaign's to set: a run under one
# that the caller's environment has would be another run.
switches=$(env | grep -E '^LIGNIN_[A-Za-z0-9_]*=' | cut -d= -f1 | tr '\n' ' ' || true)
[ -z "$switches" ] || refuse "the environment has $switches, and the campaign sets the switches of its experiments itself: unset them"

missing=""
tools="cargo python3 sha256sum sudo mkfs.xfs mkfs.ext4 losetup findmnt blkid fstrim nproc"
case " $parts " in *" traces "*) tools="$tools bpftrace" ;; esac
for tool in $tools; do
  command -v "$tool" > /dev/null || missing="$missing $tool"
done
[ -z "$missing" ] || refuse "not found:$missing"
# The script keeps lists of paths in words.
case "$out$BUILDS" in *[!A-Za-z0-9/._+@:,=~-]*) refuse "OUT and BUILDS take a path of letters, digits and / . _ - + @ : , = ~ alone: $out, $BUILDS" ;; esac
# What a run is of is the campaign's to say, and its tables go by it.
for flag in --engine --background --blocks --label --commit --out --scratch --watch-files --mark-blocks --parts; do
  case " ${LATENCY:-} " in *" $flag "*) refuse "LATENCY cannot give $flag: the campaign does" ;; esac
done
[ "$(uname -s)" = Linux ] || refuse "the traces and the file systems want Linux"
sudo -n true 2> /dev/null || refuse "sudo asks for a password"
[ -z "$(git -C "$repo" status --porcelain --untracked-files=no)" ] ||
  refuse "the checkout has changes, and the builds are of its commit"
for patch in log-allocated-ahead fewer-syncs; do
  git -C "$repo" apply --check "$here/$patch.patch" ||
    refuse "$patch.patch does not apply to this commit"
done

# A campaign that is not there yet begins here, where nothing has refused it.
if [ ! -s "$out/campaign.txt" ]; then
  most=$((blocks > trace_blocks ? blocks : trace_blocks))
  case " $backgrounds " in
    *" quiet "*) wanted=$((most * 200 + 2097152)) ;;
    *) wanted=$((most * 40 + 4194304)) ;;
  esac
  case "$BUILDS/" in "$out/"*) [ -s "$BUILDS/exp2/sha256" ] || wanted=$((wanted + 12582912)) ;; esac
  # The inode tables and the journals of the ext4 file systems it makes,
  # which are written out when they are made.
  case " $parts " in *" filesystems "* | *" traces "*) wanted=$((wanted + loop_gb * 65536)) ;; esac
  free=$(df -k --output=avail "$out" | tail -1)
  [ "$free" -ge "$wanted" ] ||
    refuse "$out has $((free / 1048576)) GiB free, and this campaign wants $((wanted / 1048576 + 1))"
  echo "$asked" > "$out/campaign.txt"
  plan
fi

# The tracer stamps its lines with the monotonic clock by name, which
# bpftrace has from 0.19 on. It is asked for here, before anything is built.
case " $parts " in *" traces "*)
  sudo bpftrace -e 'BEGIN { printf("%llu\n", nsecs(monotonic)); exit(); }' > /dev/null 2>&1 ||
    refuse "this bpftrace ($(bpftrace --version 2> /dev/null)) has no nsecs(monotonic): the traces want bpftrace 0.19 or later, and PARTS without traces makes the tables of the runs without one"
  ;;
esac

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
# The description of the host, for the tables: written with its identity, by
# the first call that makes a run, and so of the host the runs are made on.
described() {
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
}

# Part 1.
# Whether a build that is there is the one that would be made: of this
# commit, with this patch or none, and the binary its checksum names.
built() { # built NAME [PATCH]
  local to=$BUILDS/$1 patch=${2:-} parent
  [ -s "$to/sha256" ] && [ -x "$to/lignin-bench" ] && [ -s "$to/libraries" ] || return 1
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
  # The lock of the builds is this script's already, and its builds are
  # made under it.
  built base || BUILDS_LOCK_HELD=1 REPO=$repo "$here/build.sh" base "$commit"
  built exp1 "$here/log-allocated-ahead.patch" ||
    BUILDS_LOCK_HELD=1 REPO=$repo "$here/build.sh" exp1 "$commit" "$here/log-allocated-ahead.patch"
  built exp2 "$here/fewer-syncs.patch" ||
    BUILDS_LOCK_HELD=1 REPO=$repo "$here/build.sh" exp2 "$commit" "$here/fewer-syncs.patch"
fi

# The builds a campaign's runs are made by are said in its directory, by the
# first call that finds them there, a call that makes no build as well. A
# later call is held to them: builds of the same commit made anew can be
# other binaries, and a campaign is of one set. What the directory says is
# not changed by a call that is refused.
if has builds || has main || has filesystems || has traces; then
  { built base && built exp1 "$here/log-allocated-ahead.patch" && built exp2 "$here/fewer-syncs.patch"; } ||
    refuse "the builds under $BUILDS are not those of this commit and these patches: make the part builds"
  for build in base exp1 exp2; do
    line="$build: commit $(cat "$BUILDS/$build/commit"), sha256 $(cat "$BUILDS/$build/sha256")"
    if [ -f "$BUILDS/$build/patch-sha256" ]; then
      line="$line, which is $commit with a patch of sha256 $(cat "$BUILDS/$build/patch-sha256")"
    fi
    echo "$line"
  done > "$out/builds.txt.now"
  # Which libraries an experiment's build has otherwise than the base's:
  # every build writes the paths of its checkout the same way, so a crate
  # that the patch does not reach comes out the same.
  for build in exp1 exp2; do
    changed=$(LC_ALL=C comm -3 "$BUILDS/base/libraries" "$BUILDS/$build/libraries" | awk '{ print $1 }' |
      LC_ALL=C sort -u | tr '\n' ' ')
    echo "$build differs from base in the libraries: ${changed:-none}" >> "$out/builds.txt.now"
  done
  if [ -s "$out/builds.txt" ]; then
    cmp -s "$out/builds.txt" "$out/builds.txt.now" || {
      rm -f "$out/builds.txt.now"
      refuse "the builds under $BUILDS are not the binaries this campaign was made with, which $out/builds.txt names: a campaign is of one set of builds"
    }
    rm -f "$out/builds.txt.now"
  else
    mv "$out/builds.txt.now" "$out/builds.txt"
  fi
fi

# The file systems of parts 3 and 4, in files under OUT: those this call has
# verified to be the campaign's or has mounted itself, and their loop
# devices. What is in the lists is unmounted and freed when the script ends.
mounted="" devices=""
unmount() {
  local dir device
  for dir in $mounted; do sudo umount "$dir" 2> /dev/null || true; done
  for device in $devices; do sudo losetup -d "$device" 2> /dev/null || true; done
}
trap unmount EXIT
# Nothing is removed or trimmed under a directory for a file system that this
# call has not verified.
own() { # own DIRECTORY
  case "$1" in
    "$out/mnt/"*)
      case " $mounted " in
        *" $1 "*) ;;
        *) refuse "$1 was not verified to be a file system of this campaign: nothing is removed under it" ;;
      esac
      ;;
    "$out/native")
      # The runs of part 2 are on OUT's own file system, in a directory of
      # it: a mount at that directory is none of the campaign's.
      ! findmnt -rn --mountpoint "$1" > /dev/null ||
        refuse "$1 is a mount point, which the campaign did not make: the runs on OUT's own file system are not made on it, and nothing is removed under it"
      ;;
  esac
}

# Removes the scratch directory under a directory of the campaign: only
# where the directory is the campaign's own, and never across a mount. What
# is mounted at the scratch directory or below it is another's, and a
# removal would go into it.
plain() { # plain PATH
  case "$1" in *[!A-Za-z0-9/._+@:,=~-]*) return 1 ;; esac
}
emptied() { # emptied DIRECTORY
  local dir=${1:?} at
  own "$dir"
  local mounts
  at=$(realpath -m "$dir/scratch")
  # The mount points are read as the system writes them, with its escapes
  # for what is no plain character: a path is looked for among them only
  # where it has none to escape.
  plain "$at" || refuse "$at has a character in it that is no letter, digit or one of / . _ - + @ : , = ~, and nothing is removed under such a path"
  mounts=$(findmnt -rn -o TARGET) || refuse "the mounts cannot be listed, so nothing is removed under $at"
  if echo "$mounts" | at="$at" awk '$0 == ENVIRON["at"] || index($0, ENVIRON["at"] "/") == 1 { found = 1 } END { exit !found }'; then
    refuse "something is mounted at $at or below it: nothing is removed across a mount"
  fi
  rm -rf "$dir/scratch"
}

# Removes a run's database, and gives what it took of a file system of part 3
# back to the file that holds it.
cleared() {
  local dir=${1:?}
  emptied "$dir"
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

# What a directory is mounted from, as a run is held to it: the file
# system's kind, its options and its own id. The id is the file system's and
# not the device's, so a loop device of another number is the same. It is
# read from the device itself: what findmnt says of it is the system's
# record of its devices, which can be without the id of a file system that
# was just made and have it at a later call. A mount that is of no device
# (memory, a dataset, a share) has no id, and is held to its kind and its
# options. A device whose id cannot be read refuses the call, one that is
# not to be seen from here among them: a run on it could be held to no file
# system.
mounted_from() { # mounted_from DIRECTORY
  local source id=""
  source=$(findmnt --nofsroot -no SOURCE -T "$1")
  case "$source" in /dev/*)
    id=$(sudo blkid -p -o value -s UUID "$source" 2> /dev/null) || id=""
    [ -n "$id" ] ||
      refuse "the id of the file system on $source, which $1 is on, could not be read: a run cannot be held to its mount"
    ;;
  esac
  echo "$(findmnt -no FSTYPE,OPTIONS -T "$1") $id"
}
mount_of() { # mount_of DIRECTORY
  local of under
  of=$(mounted_from "$1") || return 1
  # A file system the campaign keeps in a file is also of the mount its
  # file is on: its syncs end in syncs of that file.
  case "$1" in "$out/mnt/"*)
    under=$(mounted_from "$out/fs") || return 1
    of="$of, in a file on $under"
    ;;
  esac
  echo "$of"
}

# The host a campaign is of, and the file system its runs are on: a campaign
# is of one of each. The first call that makes a run writes it down, a later
# call that makes one is held to it, and every run says it beside its report,
# for the tables to hold each to the campaign's.
host_now() {
  local under
  under=$(mount_of "$out") || return 1
  echo "kernel $(uname -srm)"
  echo "machine $(sha256sum /etc/machine-id 2> /dev/null | cut -c1-16)"
  echo "processors $(nproc)"
  echo "memory $(awk '/^MemTotal/ { print $2, $3 }' /proc/meminfo)"
  echo "the file system of the runs: $under"
}
case " $parts " in *" main "* | *" filesystems "* | *" traces "*)
  host=$(host_now) || exit 1
  if [ -s "$out/host-id.txt" ]; then
    [ "$(cat "$out/host-id.txt")" = "$host" ] ||
      refuse "$out was begun on another host or another file system, which $out/host-id.txt names, and this one is: $(echo "$host" | tr '\n' ';'): a campaign is of one host; runs made elsewhere are another campaign, with its own controls"
  else
    echo "$host" > "$out/host-id.txt"
    described
  fi
  ;;
esac

# Whether a run that is there was made by the build that would make it now
# and on the mount that is there now: a campaign is of one set of builds and
# of one mount of each file system, and a run says which made it.
made_by() { # made_by FILE BUILD DIRECTORY
  [ "$(cat "$1.build" 2> /dev/null)" = "$(cat "$BUILDS/$2/sha256")" ] ||
    refuse "$1 was made by another build than $BUILDS/$2: a campaign is of one set of builds"
  local now
  now=$(mount_of "$3") || exit 1
  [ "$(cat "$1.mount" 2> /dev/null)" = "$now" ] ||
    refuse "$1 was made on $(cat "$1.mount" 2> /dev/null), and $3 is now $now: a campaign is of one mount of each file system"
}

# One run: run FILE BUILD ENGINES DIRECTORY BACKGROUND LABEL SETTINGS
run() {
  local file=$1 build=$2 engines=$3 dir=${4:?} background=$5 label=$6 settings=$7 flags="" engine
  if [ -s "$file.json" ]; then
    made_by "$file" "$build" "$dir"
    return 0
  fi
  for engine in $engines; do flags="$flags --engine $engine"; done
  emptied "$dir"
  mount_of "$dir" > "$file.mount" || exit 1
  cp "$out/host-id.txt" "$file.host"
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

# Before the runs: that a traced run can be made here, that its waits can be
# placed, and that its user stacks are whole. The last wants waits inside the
# blocks to show it, twenty of them (`waits.py --stacks`): a short run is
# made first, in which every time the thread is off a processor for 20 µs
# counts, and where that has too few, a longer one in which 5 µs count. A
# trace whose stacks are cut short ends the campaign here. Only a run that
# showed whole stacks leaves the file by which a later call skips this; where
# both runs have too few waits to say, the script says that it is
# inconclusive and goes on, and every traced run says of its own stacks what
# it has.
preflight() { # preflight ENGINE
  local file=$out/preflight/$1 blocks off status said
  [ -e "$file.read" ] && return 0
  mkdir -p "$out/preflight" "$out/native"
  for blocks in 1000 6000; do
    off=20
    [ "$blocks" = 1000 ] || off=5
    emptied "$out/native"
    # shellcheck disable=SC2086
    LONG_US=1000 OFF_US=$off THREADS="" BIN="$BUILDS/base/lignin-bench" "$here/traced.sh" "$file" "$out/native/scratch" \
      --engine "$1" --background heavy --blocks "$blocks" --label preflight \
      --commit "$commit" ${LATENCY:-} 2> "$file.err" || failed "$file"
    emptied "$out/native"
    status=0
    python3 "$here/waits.py" --marked --stacks "$file.trace" "$off" > "$file.waits.md" 2> "$file.waits.err" ||
      status=$?
    said=$(grep -m1 "^User stacks:" "$file.waits.md" || true)
    case "$status" in
      0)
        touch "$file.read"
        say "a traced run of $1 can be made here, and its waits can be placed. $said"
        return 0
        ;;
      4) say "a traced run of $1 of $blocks blocks does not say whether user stacks are whole. $said" ;;
      *) refuse "the trace of a short run of $1 cannot be used, so no traced run would be: $(tail -1 "$file.waits.err")" ;;
    esac
  done
  say "whether user stacks are whole for $1 on this machine is inconclusive: the traced runs are made all the same, and each says of its own stacks what it has"
}
if has traces; then
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

# What is mounted at a directory of the campaign is used only where it is the
# campaign's own: one file system, at that very directory, of the kind the
# campaign makes there, from a loop device over the campaign's own file,
# which the campaign made. Anything else is another's: the script refuses
# it, and neither removes anything under it nor trims it. Prints the loop
# device; returns 2 where nothing is mounted there.
own_mount() { # own_mount NAME KIND
  local dir=$out/mnt/$1 image=$out/fs/$1.img found source kind back
  found=$(findmnt -rn -o SOURCE,FSTYPE --mountpoint "$dir") || return 2
  [ "$(echo "$found" | wc -l)" -eq 1 ] ||
    refuse "more than one file system is mounted at $dir: unmount them, or give the campaign another directory"
  read -r source kind <<< "$found"
  [ "$kind" = "$2" ] ||
    refuse "$dir holds a file system of the kind $kind, where the campaign makes $2: it is not the campaign's, and is left as it is"
  case "$source" in
    /dev/loop*) ;;
    *) refuse "$dir is mounted from $source and not from a file of the campaign: it is left as it is" ;;
  esac
  back=$(sudo losetup -n -O BACK-FILE "$source" 2> /dev/null) || back=""
  [ -f "$image.made" ] && [ -n "$back" ] && [ "$back" -ef "$image" ] ||
    refuse "$dir is mounted from $source, which is over ${back:-no file} and not over $image as the campaign made it: it is left as it is"
  echo "$source"
}
filesystem() { # filesystem NAME MKFS MOUNT-OPTIONS
  local dir=$out/mnt/$1 image=$out/fs/$1.img kind=ext4 device stale status=0 option
  [ "$1" != xfs ] || kind=xfs
  mkdir -p "$dir" "$out/fs"
  device=$(own_mount "$1" "$kind") || status=$?
  case "$status" in
    0)
      # Left mounted by a call that was stopped, and the campaign's own: it
      # is used, and unmounted and freed with the rest when this call ends.
      mounted="$mounted $dir" devices="$devices $device"
      ;;
    2)
      # A loop device that a stopped call left over the campaign's file is
      # freed before another is taken.
      for stale in $(sudo losetup -n -O NAME -j "$image" 2> /dev/null); do
        sudo losetup -d "$stale" 2> /dev/null || true
      done
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
      own_mount "$1" "$kind" > /dev/null || refuse "$dir is not what the campaign has just mounted there"
      ;;
    *) exit 1 ;;
  esac
  for option in ${3//,/ }; do
    case ",$(findmnt -rn -o OPTIONS --mountpoint "$dir")," in
      *",$option,"*) ;;
      *) refuse "$dir is mounted without $option, with which the campaign mounts it" ;;
    esac
  done
  sudo chown "$(id -u):$(id -g)" "$dir"
  grep -q "^$1: " "$out/host.txt" || describe "$1" "$dir"
}
# ext4 is made with its inode tables and its journal written out: left to
# the kernel, they are written after the mount, beside the runs.
ext4="mkfs.ext4 -q -F -E lazy_itable_init=0,lazy_journal_init=0"
if has filesystems || has traces; then
  filesystem ext4 "$ext4" ""
  filesystem xfs "mkfs.xfs -q -f" ""
fi
if has traces; then
  filesystem ext4-lazytime "$ext4" "lazytime"
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
  # A traced run is there with all that the tables read of it: one that has
  # lost its tracer's error output or the ids of its process is made again.
  if [ -e "$file.ok" ] && [ -s "$file.trace" ] && [ -s "$file.json" ] && [ -e "$file.trace.err" ] &&
    [ -s "$file.pid" ]; then
    made_by "$file" "$build" "$dir"
    return 0
  fi
  [ "$engine" = lignindb ] && watch="--watch-files"
  emptied "$dir"
  # What the run is of is written before it: the file that says it was made
  # is the last, and is `traced.sh`'s, after the trace has been read.
  rm -f "$file.ok"
  mount_of "$dir" > "$file.mount" || exit 1
  cp "$out/host-id.txt" "$file.host"
  cp "$BUILDS/$build/sha256" "$file.build"
  # The bounds from which the tracer writes a wait, and a sync, a write or
  # a request, are the campaign's and not the caller's: the tables hold
  # every trace to them.
  # shellcheck disable=SC2086
  env $settings LONG_US=1000 OFF_US=200 BIN="$BUILDS/$build/lignin-bench" THREADS="$threads" "$here/traced.sh" "$file" \
    "$dir/scratch" --engine "$engine" --background "$background" --blocks "$trace_blocks" \
    $watch --parts --label "$(basename "$file")" --commit "$(cat "$BUILDS/$build/commit")" ${LATENCY:-} \
    2> "$file.err" || status=$?
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
    refuse "the tables that could be made are in $out/tables.md, and it says there what is missing and why${traces_failed:+; these traced runs failed:$traces_failed}"
fi
[ -z "$traces_failed" ] || refuse "these traced runs failed:$traces_failed"
if has tables; then
  echo "finished the parts $parts: the tables are in $out/tables.md"
else
  echo "finished the parts $parts: this call was asked for no tables, which PARTS=tables makes of what is there"
fi
