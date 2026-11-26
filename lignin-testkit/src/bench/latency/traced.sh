#!/usr/bin/env bash
# traced.sh OUT SCRATCH [options of `lignin-bench latency`]
#
# One latency run of one engine with the kernel trace of trace.bt beside it.
# Leaves OUT.json and OUT.txt (the run), OUT.trace (the trace), OUT.pid
# (the process and its measuring thread) and, last and only if nothing below
# failed, OUT.ok: a traced run without it is not one. Wants bpftrace and sudo.
#
# It fails, and says why, where the run fails, where the tracer has not
# attached within a minute or ends before the run does, and where the trace
# has no line for the beginning and for the end of the measured blocks: a
# trace that cannot say which waits were of the measured blocks is none.
#
#   BIN       the lignin-bench to run (default: target/release/lignin-bench)
#   LONG_US   the bound for WRITE, SYNC and BLK lines (default 1000)
#   OFF_US    the bound for OFFCPU lines (default 200)
#   THREADS   `all` to follow every thread of the process, not the measuring
#             one alone
set -euo pipefail
out=${1:?where the run and its trace go} scratch=${2:?the scratch directory}
shift 2
here=$(cd "$(dirname "$0")" && pwd)
bin=${BIN:-target/release/lignin-bench}
ulimit -n 65536

run="" tracer=""
# The tracer is this script's own: `sudo` is its child, and bpftrace `sudo`'s.
# `sudo` runs as another user, so whether it lives is read from /proc.
tracing() { [ -n "$tracer" ] && [ -d "/proc/$tracer" ]; }
stop_tracer() {
  local tries signal
  [ -n "$tracer" ] || return 0
  for tries in $(seq 40); do
    tracing || break
    # An interrupt has bpftrace end its script; after ten seconds it is ended.
    # Where `sudo` runs the command in its own place there is no child, and
    # the process itself is the tracer.
    if [ "$tries" -le 20 ]; then signal=INT; else signal=KILL; fi
    sudo pkill "-$signal" -P "$tracer" 2> /dev/null || sudo kill "-$signal" "$tracer" 2> /dev/null || true
    sleep 0.5
  done
  if tracing; then echo "traced.sh: the tracer $tracer did not end" >&2; else wait "$tracer" 2> /dev/null || true; fi
  tracer=""
}
ended() {
  [ -z "$run" ] || kill "$run" 2> /dev/null || true
  stop_tracer
}
trap ended EXIT
refuse() {
  echo "traced.sh: $*" >&2
  exit 1
}

sudo -n true 2> /dev/null || refuse "sudo asks for a password"
rm -f "$out.ok"
"$bin" latency --scratch "$scratch" "$@" --out "$out.json" > "$out.txt" &
run=$!
sleep 1
# A thread that an engine starts from the measuring thread has its name too:
# the measuring thread is the first of that name.
tid=$(ps -L -o tid=,comm= -p "$run" | awk '$2 == "lignin-commit" { print $1; exit }')
[ -n "$tid" ] || refuse "the run has no measuring thread: $(tail -2 "$out.txt")"
echo "$run $tid" > "$out.pid"
# The pause the harness makes after a commit, which the tracer leaves out.
pause_us=5000 before=""
for word in "$@"; do
  [ "$before" = --pause-us ] && pause_us=$word
  before=$word
done
measuring=$tid
[ "${THREADS:-}" = all ] && tid=0
sudo env BPFTRACE_MAX_MAP_KEYS=262144 bpftrace "$here/trace.bt" "$run" "${LONG_US:-1000}" \
  "${OFF_US:-200}" "$tid" "$measuring" "$((pause_us * 1000))" > "$out.trace" 2> "$out.trace.err" &
tracer=$!
attached=""
for _ in $(seq 120); do
  if grep -q "^tracing pid" "$out.trace" 2> /dev/null; then
    attached=yes
    break
  fi
  tracing || break
  sleep 0.5
done
[ -n "$attached" ] || refuse "the tracer did not attach: $(tail -3 "$out.trace.err")"

status=0
wait "$run" || status=$?
run=""
tracing || refuse "the tracer ended before the run: $(tail -3 "$out.trace.err")"
sleep 1
stop_tracer
[ "$status" = 0 ] || refuse "the run failed with $status"
grep -q "lignin-mark measured begin" "$out.trace" ||
  refuse "the trace has no line for the beginning of the measured blocks: the tracer attached after it"
grep -q "lignin-mark measured end" "$out.trace" ||
  refuse "the trace has no line for the end of the measured blocks"
touch "$out.ok"
