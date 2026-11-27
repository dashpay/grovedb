#!/usr/bin/env bash
# What ext4 reads into the page cache while one `lignin-bench` call runs under a memory limit, by
# the kind of file.
#
# usage: read-split.sh MEMORY-MAX|none DIVISOR OUT -- LIGNIN-BENCH COMMAND [OPTIONS...]
#
#   MEMORY-MAX   the limit of the call's cgroup (`MemoryMax` of a systemd unit, as `233M`), with
#                no swap; `none` for no limit.
#   DIVISOR      what the counts are divided by. The counts are of the time the probes are in
#                place, from before the call is started until after it has ended, and of the
#                whole machine: the call from its start to its end, the programs that start
#                it, and whatever else reads files meanwhile. For `writes` with one phase the
#                phase's transitions are a fitting divisor if the kept database it copies is
#                read from the page cache; 1 gives the totals.
#   OUT          a prefix: OUT.bt is what the probes counted, OUT.split the table, OUT.log the
#                call's output.
#   after `--`   the call, as `target/release/lignin-bench writes --engine lignindb ...`.
#
# With DROP_PAGE_CACHE=1 the system's page cache is emptied before the call, for a call that is
# to read from storage a database that was written before it (`reads`): what of such files
# lies in the page cache is read from memory. Whatever else is then read into the page cache
# from a file on ext4 while the probes are in place, as the programs that start the call may
# be, is in the counts too, under the kind `other`.
#
# It runs the call as a transient systemd unit with memory and I/O accounting on, and
# `read-split.bt` beside it for as long as the call runs. It needs root (bpftrace, systemd-run),
# Linux with systemd and cgroup v2, a database on ext4 (the probes are ext4's), and a machine at
# rest: the probes count every process's reads. The times of such a call are no result: the
# probes take time.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
limit=${1:?the memory limit, or none}
operations=${2:?the divisor}
out=${3:?the prefix of the output files}
[ "${4:-}" = "--" ] || { echo "read-split.sh: \`--\` and the call are missing" >&2; exit 2; }
shift 4
[ $# -gt 0 ] || { echo "read-split.sh: the call is missing" >&2; exit 2; }

properties=(-p MemoryAccounting=yes -p IOAccounting=yes)
[ "$limit" = none ] || properties+=(-p "MemoryMax=$limit" -p MemorySwapMax=0)

# Ends the tracer if it still runs, and waits for it; its status is this function's. A tracer
# that does not end when it is asked to is killed: what it had printed by then is not its counts,
# and its status says so.
stop_tracer() {
    if kill -0 "$tracer" 2>/dev/null; then
        kill -INT "$tracer" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$tracer" 2>/dev/null || break
            sleep 0.2
        done
        kill -KILL "$tracer" 2>/dev/null || true
    fi
    wait "$tracer" 2>/dev/null
}

# The capture is a new file before the tracer starts, so that nothing a call before this one
# left under the same prefix is read as this tracer's.
: > "$out.bt"
: > "$out.bt.err"
bpftrace -B line "$here/read-split.bt" >> "$out.bt" 2>> "$out.bt.err" &
tracer=$!
trap 'stop_tracer || true' EXIT
# The script says `probes ready` from a probe of its own, which runs once all of them are in place.
attached=0
for _ in $(seq 1 300); do
    if grep -q '^probes ready$' "$out.bt" 2>/dev/null; then
        attached=1
        break
    fi
    kill -0 "$tracer" 2>/dev/null || break
    sleep 0.2
done
if [ "$attached" != 1 ]; then
    cat "$out.bt.err" >&2
    echo "read-split.sh: the probes were not attached" >&2
    exit 1
fi
if [ "${DROP_PAGE_CACHE:-0}" = 1 ]; then
    sync
    echo 3 > /proc/sys/vm/drop_caches
fi
status=0
systemd-run --quiet --wait --collect --same-dir "${properties[@]}" \
    -p StandardOutput="file:$out.log" -p StandardError="file:$out.log" \
    "$@" || status=$?
# The counts are the call's only if the tracer ran until the call had ended.
through=1
kill -0 "$tracer" 2>/dev/null || through=0
traced=0
stop_tracer || traced=$?
trap - EXIT
if [ "$through" != 1 ] || [ "$traced" != 0 ]; then
    cat "$out.bt.err" >&2
    echo "read-split.sh: the probes ended before the call did, or with an error (status $traced)" >&2
    exit 1
fi
python3 "$here/read-split.py" "$out.bt" "$operations" "$(getconf PAGESIZE)" > "$out.split"
cat "$out.split"
exit "$status"
