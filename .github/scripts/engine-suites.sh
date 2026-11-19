#!/usr/bin/env bash
# The test suites that run once for every engine registered for them.
#
#   engine-suites.sh matrix         the runs CI adds to the workspace run, as
#                                   the JSON of a job matrix: one for each
#                                   engine of a suite but its first, which the
#                                   workspace run already covers
#   engine-suites.sh run [ARGS]     every suite on every engine, one after the
#                                   other; ARGS follow `cargo test -p PACKAGE`
#
# A suite is a package, the variable that selects its engine and its
# registry: a file with one engine name a line, the default first.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

suites() {
    cat <<'SUITES'
grovedb GROVEDB_TEST_ENGINE grovedb/test-engines.txt
grovedb-merk MERK_TEST_ENGINE merk/test-engines.txt
SUITES
}

engines() {
    grep -vE '^[[:space:]]*(#|$)' "$repo/$1"
}

case "${1:-}" in
matrix)
    rows=""
    while read -r package variable registry; do
        for engine in $(engines "$registry" | tail -n +2); do
            rows="$rows{\"package\":\"$package\",\"variable\":\"$variable\",\"engine\":\"$engine\"},"
        done
    done < <(suites)
    echo "[${rows%,}]"
    ;;
run)
    shift
    cd "$repo"
    while read -r package variable registry; do
        for engine in $(engines "$registry"); do
            echo "== $package on $engine"
            env "$variable=$engine" cargo test -p "$package" "$@"
        done
    done < <(suites)
    ;;
*)
    sed -n '2,12p' "${BASH_SOURCE[0]}" >&2
    exit 2
    ;;
esac
