#!/usr/bin/env bash
# The device reads of one `lignin-bench` call by the kind of file they read, under a memory limit.
#
# usage: read-split.sh MEMORY-MAX|none OPERATIONS OUT -- LIGNIN-BENCH COMMAND [OPTIONS...]
#
#   MEMORY-MAX   the limit of the call's cgroup (`MemoryMax` of a systemd unit, as `233M`), with
#                no swap; `none` for no limit. Under a limit that leaves the page cache little
#                room, a read that the engine's own cache does not answer reaches the device.
#   OPERATIONS   the transitions or reads the call makes, to divide by.
#   OUT          a prefix: OUT.bt is what the probes counted, OUT.split the table, OUT.log the
#                call's output.
#   after `--`   the call, as `target/release/lignin-bench writes --engine lignindb ...`.
#
# With DROP_PAGE_CACHE=1 the system's page cache is emptied before the call. A call that reads a
# database that was written before it (`reads`, or `writes` under no limit) needs that: such
# files lie in the page cache on another cgroup's account, and are read from memory.
#
# It runs the call as a transient systemd unit with memory and I/O accounting on, and
# `read-split.bt` beside it for as long as the call runs. It needs root (bpftrace, systemd-run),
# Linux with systemd and cgroup v2, a database on ext4 (the probes are ext4's), and a machine at
# rest: the probes count every process's reads. The times of such a call are no result, since
# every read of the device goes through a probe.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
limit=${1:?the memory limit, or none}
operations=${2:?the number of operations}
out=${3:?the prefix of the output files}
[ "${4:-}" = "--" ] || { echo "read-split.sh: \`--\` and the call are missing" >&2; exit 2; }
shift 4
[ $# -gt 0 ] || { echo "read-split.sh: the call is missing" >&2; exit 2; }

properties=(-p MemoryAccounting=yes -p IOAccounting=yes)
[ "$limit" = none ] || properties+=(-p "MemoryMax=$limit" -p MemorySwapMax=0)

bpftrace "$here/read-split.bt" > "$out.bt" 2> "$out.bt.err" &
tracer=$!
trap 'kill -INT "$tracer" 2>/dev/null || true' EXIT
for _ in $(seq 1 150); do
    grep -q Attaching "$out.bt" "$out.bt.err" 2>/dev/null && break
    kill -0 "$tracer" 2>/dev/null || { cat "$out.bt.err" >&2; exit 1; }
    sleep 0.2
done
if [ "${DROP_PAGE_CACHE:-0}" = 1 ]; then
    sync
    echo 3 > /proc/sys/vm/drop_caches
fi
status=0
systemd-run --quiet --wait --collect --same-dir "${properties[@]}" \
    -p StandardOutput="file:$out.log" -p StandardError="file:$out.log" \
    "$@" || status=$?
kill -INT "$tracer"
wait "$tracer" 2>/dev/null || true
trap - EXIT
python3 "$here/read-split.py" "$out.bt" "$operations" | tee "$out.split"
exit "$status"
