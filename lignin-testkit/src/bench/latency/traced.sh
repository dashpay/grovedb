#!/usr/bin/env bash
# traced.sh OUT SCRATCH [options of `lignin-bench latency`]
#
# One latency run with the kernel trace of trace.bt beside it. Leaves
# OUT.json and OUT.txt (the run), OUT.trace (the trace) and OUT.pid (the
# process and its measuring thread). Wants bpftrace and sudo.
#
#   BIN       the lignin-bench to run (default: target/release/lignin-bench)
#   LONG_US   the bound for WRITE, SYNC and BLK lines (default 1000)
#   OFF_US    the bound for OFFCPU lines (default 200)
#   THREADS   `all` to follow every thread of the process, not the measuring
#             one alone
set -euo pipefail
out=$1 scratch=$2; shift 2
here=$(cd "$(dirname "$0")" && pwd)
bin=${BIN:-target/release/lignin-bench}
ulimit -n 65536
"$bin" latency --scratch "$scratch" "$@" --out "$out.json" > "$out.txt" &
pid=$!
tracer=""
# Whatever ends the run, the tracer is stopped.
stop() {
  [ -z "$tracer" ] || sudo pkill -INT -x bpftrace || true
  [ -z "$tracer" ] || wait "$tracer" || true
}
trap stop EXIT
sleep 1
# A thread that an engine starts from the measuring thread has its name too:
# the measuring thread is the first of that name.
tid=$(ps -L -o tid=,comm= -p "$pid" | awk '$2 == "lignin-commit" { print $1; exit }')
echo "$pid $tid" > "$out.pid"
[ "${THREADS:-}" = all ] && tid=0
sudo BPFTRACE_MAX_MAP_KEYS=262144 bpftrace "$here/trace.bt" "$pid" "${LONG_US:-1000}" "${OFF_US:-200}" "$tid" \
  > "$out.trace" 2> "$out.trace.err" &
tracer=$!
wait "$pid"
sleep 1
