#!/usr/bin/env bash
# The LigninDB jobs of CI, one command each, so that every job can be run by
# hand from any directory of a checkout. docs/lignindb/ci.md says which job
# runs which, where and when.
#
#   lignin-ci.sh crates              the LigninDB crates, one a line
#   lignin-ci.sh machine [--every S] the machine: cores, memory, swap, free disk;
#                                    with --every, again every S seconds (S a
#                                    whole number above 0)
#   lignin-ci.sh test [ARGS]         the tests of every LigninDB crate on this
#                                    machine (ARGS follow `cargo test`), then
#                                    those of Merk's node store on LigninDB
#   lignin-ci.sh features            the feature sets that must build apart:
#                                    GroveDB with no engine, Merk on each engine
#                                    alone, the engine crates on their own
#   lignin-ci.sh docs                rustdoc with warnings denied, for the
#                                    crates whose documentation builds clean
#   lignin-ci.sh loom                the loom models
#   lignin-ci.sh miri [--full]       Miri over the file layer, the buffer pool
#                                    and the software path of the checksum;
#                                    --full adds the file layer under Tree
#                                    Borrows and every test of the checksum
#   lignin-ci.sh fuzz-targets        build the fuzz targets of lignin-format
#   lignin-ci.sh cost-model          the golden corpus replayed on the cost model
#
# The nightly runs, which take longer:
#
#   lignin-ci.sh diff-fuzz SECONDS [ARGS] the differential fuzzer; ARGS
#                                         follow `lignin-diff fuzz`
#   lignin-ci.sh corpus-reference [ARGS]  the corpus of the reference scale;
#                                         ARGS follow `lignin-golden replay`
#   lignin-ci.sh corpus-at-pin [ARGS]     the corpus recorded again from the
#                                         engine it is pinned to, compared
#                                         with the committed one; ARGS follow
#                                         `lignin-golden record`
#   lignin-ci.sh fuzz-decoders SECONDS    each fuzz target of lignin-format
#   lignin-ci.sh miri-schedules SEEDS     the pool's races under SEEDS of
#                                         Miri's schedules
#
# Miri and cargo-fuzz run on the toolchain $LIGNIN_NIGHTLY (default
# nightly-2026-02-24), everything else on the one rust-toolchain.toml names.
# Test databases go to the temporary directory: TMPDIR says where. `test`
# raises the limit of open files, whose default of 256 on macOS is less than
# the RocksDB databases of the test kit's tests hold at once.
set -euo pipefail
shopt -s inherit_errexit 2>/dev/null || true

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"
nightly=${LIGNIN_NIGHTLY:-nightly-2026-02-24}

say() { printf '== %s\n' "$*"; }
fail() {
    printf 'lignin-ci: %s\n' "$*" >&2
    exit 1
}

# The LigninDB crates (the members whose name begins with lignin), or those
# of them for which the jq condition $1 holds.
crates() {
    cargo metadata --no-deps --format-version 1 |
        jq -r ".packages[] | select((.name | startswith(\"lignin\")) and (${1:-true})) | .name" |
        sort
}

# The lines command "$@" prints. It fails when the command fails or prints
# nothing, so that a list is never taken as empty.
listed() {
    local lines
    lines=$("$@") || fail "$* failed"
    [ -n "$lines" ] || fail "$* lists nothing"
    echo "$lines"
}

# `-p CRATE` for each LigninDB crate but those named, into the array packages.
package_args() {
    local names crate
    names=$(listed crates)
    packages=()
    for crate in $names; do
        [[ " $* " == *" $crate "* ]] || packages+=(-p "$crate")
    done
}

# The crates of the build of package $2 with the arguments that follow,
# through the dependency kinds $1.
build_of() {
    local edges=$1 package=$2
    shift 2
    cargo tree -p "$package" "$@" -e "$edges" --prefix none --format '{p}' |
        awk '{ print $1 }' | sort -u
}

# Fails when a crate whose name matches $1 is among the crates on stdin.
refuse() {
    local pattern=$1 what=$2 found
    found=$(grep -E "$pattern" || true)
    [ -z "$found" ] || fail "$what:"$'\n'"$found"
}

# Cores, memory and swap (on Linux used of total; on macOS the memory
# installed and the swap in use), and the disk free under the checkout and
# the temporary directory. A value that cannot be read is printed as such.
machine() {
    local cores memory
    if [ -r /proc/meminfo ]; then
        cores=$(nproc || echo unreadable)
        memory=$(free -m | awk '/^Mem:/ { printf "memory %d of %d MiB used", $3, $2 } /^Swap:/ { printf ", swap %d of %d MiB", $3, $2 }') ||
            memory="memory unreadable"
    else
        cores=$(sysctl -n hw.ncpu || echo unreadable)
        memory="memory $(($(sysctl -n hw.memsize || echo 0) / 1048576)) MiB installed, swap $(sysctl -n vm.swapusage || echo unreadable)"
    fi
    printf '== %s machine: %s cores, %s; free disk: %s here, %s in %s\n' "$(date -u +%H:%M:%S)" \
        "$cores" "$memory" "$(free_disk .)" "$(free_disk "${TMPDIR:-/tmp}")" "${TMPDIR:-/tmp}"
}

free_disk() {
    df -Pm "$1" | awk 'NR == 2 { print $4 " MiB" }' || echo unreadable
}

case "${1:-}" in
crates)
    listed crates
    ;;

machine)
    case "$#:${2:-}" in
    1:) machine ;;
    3:--every)
        [[ $3 =~ ^[1-9][0-9]*$ ]] || fail "usage: lignin-ci.sh machine [--every SECONDS]"
        while machine; do sleep "$3" >/dev/null 2>&1; done
        ;;
    *) fail "usage: lignin-ci.sh machine [--every SECONDS]" ;;
    esac
    ;;

test)
    shift
    package_args
    limit=$(ulimit -Hn)
    if [ "$limit" = unlimited ] || [ "$limit" -gt 65536 ]; then
        limit=65536
    fi
    ulimit -n "$limit"
    say "cargo test ${packages[*]} --all-features $* (open files: $(ulimit -n))"
    status=0
    cargo test "${packages[@]}" --all-features --no-fail-fast "$@" || status=$?
    # Merk's node store on LigninDB is LigninDB code in grovedb-merk, behind
    # its feature lignin_storage. It runs whether or not the crates passed.
    store=(-p grovedb-merk --features lignin_storage --lib store::lignin)
    say "cargo test ${store[*]}"
    if ! found=$(cargo test "${store[@]}" -- --list); then
        printf 'lignin-ci: the tests of grovedb-merk with lignin_storage do not build\n' >&2
        status=1
    elif grep -q ': test$' <<<"$found"; then
        cargo test "${store[@]}" || status=$?
    else
        printf 'lignin-ci: no test of grovedb-merk matches store::lignin\n' >&2
        status=1
    fi
    exit "$status"
    ;;

features)
    say "GroveDB with no engine: no RocksDB and no LigninDB among its crates"
    build_of normal,build,dev grovedb --no-default-features --features estimated_costs |
        refuse 'rocksdb|^lignin' \
            "an engine is among the crates of grovedb with no engine"
    if grep -rn 'feature = "minimal"' grovedb/src grovedb/benches grovedb/tests; then
        fail "a gate in grovedb says minimal: it is engine_generic or rocksdb_storage (docs/lignindb/engine.md)"
    fi
    cargo check -p grovedb --no-default-features --features engine_generic
    cargo test -p grovedb --no-default-features --features estimated_costs --lib --tests
    cargo test -p grovedb-merk --no-default-features --features minimal --lib

    say "Merk on LigninDB alone: no RocksDB among its crates"
    build_of normal,build grovedb-merk --no-default-features --features lignin_storage |
        refuse 'rocksdb' "RocksDB is among the crates of Merk on LigninDB"
    cargo check -p grovedb-merk --no-default-features --features lignin_storage

    say "Merk on RocksDB alone: no LigninDB among its crates"
    build_of normal,build grovedb-merk --no-default-features --features rocksdb_storage |
        refuse '^lignin' "LigninDB is among the crates of Merk on RocksDB"
    cargo check -p grovedb-merk --no-default-features --features rocksdb_storage

    say "The LigninDB crates that have features, with none and with every one"
    names=$(listed crates '.features | length > 0')
    for crate in $names; do
        cargo check -p "$crate" --no-default-features
        cargo check -p "$crate" --all-features
    done
    ;;

docs)
    # Not lignin-testkit, whose documentation had warnings when this step was
    # added (two ambiguous links in `diff`), nor the older crates
    # (grovedb-merk had 33 errors). A crate joins when its documentation
    # builds clean.
    package_args lignin-testkit
    say "RUSTDOCFLAGS=-D warnings cargo doc ${packages[*]} --all-features --no-deps"
    RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings" \
        cargo doc "${packages[@]}" --all-features --no-deps
    ;;

loom)
    # A build of its own: the cfg changes every crate it compiles. Under it
    # every test of the library compiles, and only the models run in loom,
    # so each run names the modules of the models.
    export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$repo/target}/loom
    export RUSTFLAGS="${RUSTFLAGS:-} --cfg loom"
    while read -r crate models; do
        say "loom: cargo test -p $crate --release --lib $models"
        found=$(cargo test -p "$crate" --release --lib "$models" -- --list)
        grep -q ': test$' <<<"$found" || fail "no test of $crate matches $models"
        cargo test -p "$crate" --release --lib "$models"
    done <<'MODELS'
lignin-store cache::tests::races
lignindb scope::tests::model
lignindb snapshot::tests::loom
MODELS
    ;;

miri)
    # The file layer has no unsafe code of its own, so a pull request runs
    # it under one borrow model and the night under both. The pool's unsafe
    # code runs under both on every run.
    case "$#:${2:-}" in
    1:) file_layer=("") checksum=checksum::tests::crc32c_matches ;;
    2:--full) file_layer=("" -Zmiri-tree-borrows) checksum=checksum:: ;;
    *) fail "usage: lignin-ci.sh miri [--full]" ;;
    esac
    for borrows in "${file_layer[@]}"; do
        say "Miri: the file layer ${borrows:-(Stacked Borrows)}"
        MIRIFLAGS="${MIRIFLAGS:-} -Zmiri-disable-isolation $borrows" \
            cargo "+$nightly" miri test -p lignin-io
    done
    for borrows in "" -Zmiri-tree-borrows; do
        say "Miri: the buffer pool ${borrows:-(Stacked Borrows)}"
        MIRIFLAGS="${MIRIFLAGS:-} $borrows" \
            cargo "+$nightly" miri test -p lignin-store --lib cache::
    done
    # crc32c takes the instructions of the machine where it finds them
    # (x86_64, aarch64) and its tables elsewhere. A big-endian target has
    # neither, so this runs the tables, on the other byte order. A pull
    # request runs the published vectors and the random lengths; the night
    # adds the test that walks 128 MiB.
    say "Miri: the checksum's software path, big-endian ($checksum)"
    cargo "+$nightly" miri test -p lignin-format --lib \
        --target s390x-unknown-linux-gnu "$checksum"
    ;;

fuzz-targets)
    manifest=lignin-format/fuzz/Cargo.toml
    if [ ! -e "$manifest" ]; then
        say "no $manifest: nothing to build"
        exit 0
    fi
    say "cargo check --manifest-path $manifest --bins"
    cargo "+$nightly" check --manifest-path "$manifest" --bins
    ;;

cost-model)
    # check.sh keeps the lock file it copied once; dependencies are resolved
    # afresh for each run, as for every other job.
    rm -f "${LIGNIN_COST_MODEL_WORK:-$repo/target/cost-model}/workspace/Cargo.lock"
    say "lignin-testkit/cost-model-storage/check.sh"
    lignin-testkit/cost-model-storage/check.sh
    ;;

diff-fuzz)
    seconds=${2:?"usage: lignin-ci.sh diff-fuzz SECONDS [ARGS]"}
    shift 2
    say "lignin-diff fuzz --duration $seconds $*"
    cargo run --release -p lignin-testkit --bin lignin-diff -- fuzz --duration "$seconds" "$@"
    ;;

corpus-reference)
    shift
    say "lignin-golden replay --reference $*"
    cargo run --release -p lignin-testkit --bin lignin-golden -- replay --reference "$@"
    ;;

corpus-at-pin)
    shift
    # The script gives the pinned build this checkout's lock file, which is
    # not committed.
    [ -e Cargo.lock ] || cargo generate-lockfile
    into=$(mktemp -d)
    trap 'rm -rf "$into"' EXIT
    say "record-at-pin.sh into $into $*"
    lignin-testkit/corpus/record-at-pin.sh "$into" "$@"
    say "the recording against lignin-testkit/corpus"
    recorded=$(cd "$into" && find . -name '*.trace' | sort)
    [ -n "$recorded" ] || fail "nothing was recorded"
    differ=0
    for name in $recorded; do
        if ! cmp -s "$into/$name" "lignin-testkit/corpus/$name"; then
            echo "differs from the committed corpus: $name"
            differ=1
        fi
    done
    # The whole standard scale, one grove version of it, or the reference
    # scale records every committed trace of it and nothing else. Other selections are checked
    # trace by trace only.
    expected=
    if [ $# -eq 0 ]; then
        expected=$(cd lignin-testkit/corpus && find . -path ./reference -prune -o -name '*.trace' -print | sort)
    elif [ $# -eq 2 ] && [ "$1" = --grove-version ]; then
        expected=$(cd lignin-testkit/corpus && find "./v$2" -name '*.trace' | sort)
    elif [ $# -eq 1 ] && [ "$1" = --reference ]; then
        expected=$(cd lignin-testkit/corpus && find ./reference -name '*.trace' | sort)
    fi
    if [ -n "$expected" ] && [ "$expected" != "$recorded" ]; then
        echo "the traces recorded are not those committed:"
        diff <(echo "$expected") <(echo "$recorded") || true
        differ=1
    fi
    [ "$differ" = 0 ] || fail "the corpus does not come out of its pinned engine as committed"
    echo "$(wc -l <<<"$recorded") traces recorded, all as committed"
    ;;

fuzz-decoders)
    seconds=${2:?"usage: lignin-ci.sh fuzz-decoders SECONDS"}
    if [ ! -e lignin-format/fuzz/Cargo.toml ]; then
        say "no lignin-format/fuzz/Cargo.toml: nothing to fuzz"
        exit 0
    fi
    seeds=$(mktemp -d)
    trap 'rm -rf "$seeds"' EXIT
    cd lignin-format/fuzz
    ./seed.sh "$seeds"
    cargo "+$nightly" fuzz build -s none --debug-assertions
    targets=$(listed cargo "+$nightly" fuzz list)
    for target in $targets; do
        say "cargo fuzz run $target for $seconds s"
        mkdir -p "corpus/$target" "$seeds/$target"
        cargo "+$nightly" fuzz run -s none --debug-assertions "$target" \
            "corpus/$target" "$seeds/$target" -- -max_total_time="$seconds" -rss_limit_mb=2048
    done
    ;;

miri-schedules)
    seeds=${2:?"usage: lignin-ci.sh miri-schedules SEEDS"}
    for borrows in "" -Zmiri-tree-borrows; do
        say "Miri: the pool's races under $seeds schedules ${borrows:-(Stacked Borrows)}"
        MIRIFLAGS="${MIRIFLAGS:-} -Zmiri-many-seeds=0..$seeds $borrows" \
            cargo "+$nightly" miri test -p lignin-store --lib cache::tests::races
    done
    ;;

*)
    awk 'NR > 1 && /^set -euo/ { exit } NR > 1' "${BASH_SOURCE[0]}" >&2
    exit 2
    ;;
esac
