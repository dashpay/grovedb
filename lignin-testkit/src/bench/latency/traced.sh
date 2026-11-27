#!/usr/bin/env bash
# traced.sh OUT SCRATCH [options of `lignin-bench latency`]
#
# One latency run of one engine with the kernel trace of trace.bt beside it.
# Leaves OUT.json and OUT.txt (the run), OUT.trace (the trace), OUT.pid
# (the process and its measuring thread), OUT.waits.md (what `waits.py`
# makes of the trace) and, last and only if nothing below failed, OUT.ok: a
# traced run without it is not one. Wants bpftrace, python3 and sudo.
#
# The run does nothing before the tracer is attached. It is started with
# `--hold`: its measuring thread says that it is there, the tracer is
# attached to it, and only then is the run let go. Each side waits for the
# other for a time and no longer: this script a minute for the run and a
# minute for the tracer, the run two minutes to be let go.
#
# It fails, and says why, where the run fails or does not say that it is
# ready, where the tracer has not attached within a minute or ends before
# the run does, where the trace has no line for the beginning and for the
# end of the measured blocks, where the tracer did not write its last line
# or did not end by itself with success after one interrupt, where it says
# that it lost events, where `waits.py --marked` refuses the trace, and
# where the trace has not a line for every block of the report: a trace
# that cannot say which waits were of the measured blocks, or that has not
# all of them, is none.
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
ulimit -n 65536 2> /dev/null || {
  echo "traced.sh: the limit of open files is $(ulimit -n) and cannot be raised to 65536" >&2
  exit 1
}

run="" tracer=""
# The tracer is this script's own: `sudo` is its child, and bpftrace `sudo`'s.
# `sudo` runs as another user, so whether it lives is read from /proc.
tracing() { [ -n "$tracer" ] && [ -d "/proc/$tracer" ]; }
# Where `sudo` runs the command in its own place there is no child, and the
# process itself is the tracer.
signal_tracer() { # signal_tracer SIGNAL
  sudo pkill "-$1" -P "$tracer" 2> /dev/null || sudo kill "-$1" "$tracer" 2> /dev/null || true
}
# One interrupt has bpftrace run its END, write out what its buffers hold and
# end. It is given ten minutes for that and is not signalled again meanwhile:
# a second signal can cut the writing short. `tracer_ended` is `clean` if it
# ended by itself with success, and says what happened otherwise.
tracer_ended=""
stop_tracer() {
  local tries status=0
  [ -n "$tracer" ] || return 0
  tracing && signal_tracer INT
  for tries in $(seq "${1:-1200}"); do
    tracing || break
    sleep 0.5
  done
  if tracing; then
    tracer_ended="it did not end after an interrupt and was ended"
    signal_tracer KILL
    sleep 1
  fi
  wait "$tracer" 2> /dev/null || status=$?
  [ -n "$tracer_ended" ] || { [ "$status" = 0 ] && tracer_ended=clean || tracer_ended="it ended with $status"; }
  tracer=""
}
ended() {
  [ -z "$run" ] || kill "$run" 2> /dev/null || true
  # Where the script is left early the trace is none: twenty seconds.
  stop_tracer 40
}
trap ended EXIT
refuse() {
  echo "traced.sh: $*" >&2
  exit 1
}

sudo -n true 2> /dev/null || refuse "sudo asks for a password"
rm -f "$out.ok" "$out.go" "$out.go.ready"
"$bin" latency --scratch "$scratch" --mark-blocks --hold "$out.go" "$@" --out "$out.json" > "$out.txt" &
run=$!
# The run says that it is there, and which its measuring thread is.
for _ in $(seq 600); do
  [ -s "$out.go.ready" ] && break
  kill -0 "$run" 2> /dev/null || break
  sleep 0.1
done
[ -s "$out.go.ready" ] || refuse "the run did not say within a minute that it was ready: $(tail -2 "$out.txt")"
read -r said tid < "$out.go.ready"
[ "$said" = "$run" ] && [ "${tid:-0}" -gt 0 ] ||
  refuse "the run says it is the process $said with the measuring thread ${tid:-none}, and it is $run"
echo "$run $tid" > "$out.pid"
measuring=$tid
[ "${THREADS:-}" = all ] && tid=0
# Room for what a busy run prints at once: 16 MiB a processor.
sudo env BPFTRACE_MAX_MAP_KEYS=262144 BPFTRACE_PERF_RB_PAGES=4096 bpftrace "$here/trace.bt" "$run" \
  "${LONG_US:-1000}" "${OFF_US:-200}" "$tid" "$measuring" > "$out.trace" 2> "$out.trace.err" &
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
# The tracer has the run: it is let go.
touch "$out.go"

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
[ "$tracer_ended" = clean ] ||
  refuse "the tracer did not end as it should, so the trace may be cut off: $tracer_ended"
grep -q "^trace ended$" "$out.trace" ||
  refuse "the tracer did not write its last line: the trace was cut off"
lost=$(grep -hE "^Lost [0-9]+ events" "$out.trace" "$out.trace.err" | head -3 | tr '\n' ' ' || true)
[ -z "$lost" ] || refuse "the tracer lost events, so the trace has too few lines: $lost"
rm -f "$out.go" "$out.go.ready"
# What a table would be made of is read here, before the run counts as made.
python3 "$here/waits.py" --marked "$out.trace" > "$out.waits.md" 2> "$out.waits.err" ||
  refuse "the trace cannot be read: $(tail -1 "$out.waits.err")"
marked=$(sed -n '1s/.*and \([0-9]*\) of them are marked.*/\1/p' "$out.waits.md")
blocks=$(python3 -c 'import json, sys; print(len(json.load(open(sys.argv[1]))["engines"][0]["commit_ns"]))' "$out.json")
[ "${marked:-0}" = "$blocks" ] ||
  refuse "the trace has the lines of ${marked:-no} blocks, and the run was of $blocks"
touch "$out.ok"
