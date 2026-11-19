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
# The engine of a suite is the engine of the package's unit tests. Its
# integration tests and doc tests use the package as a dependent does, on
# the engine it exports by default. So a run on the first engine is of every
# target, and a run on another engine is of the unit tests (`--lib`).
#
# A suite is a package, the variable that selects its engine and its
# registry: a file with one engine a line, its name first, and the default
# engine first.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

suites() {
    cat <<'SUITES'
grovedb GROVEDB_TEST_ENGINE grovedb/test-engines.txt
grovedb-merk MERK_TEST_ENGINE merk/test-engines.txt
SUITES
}

engines() {
    grep -vE '^[[:space:]]*(#|$)' "$repo/$1" | awk '{ print $1 }'
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
        targets=()
        for engine in $(engines "$registry"); do
            echo "== $package on $engine"
            env "$variable=$engine" cargo test -p "$package" "${targets[@]}" "$@"
            targets=(--lib)
        done
    done < <(suites)
    ;;
*)
    sed -n '2,18p' "${BASH_SOURCE[0]}" >&2
    exit 2
    ;;
esac
