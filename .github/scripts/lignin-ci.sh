#!/usr/bin/env bash
# The LigninDB jobs of CI, one command each, so that every job can be run by
# hand from any directory of a checkout. docs/lignindb/ci.md says which job
# runs which, where and when.
#
#   lignin-ci.sh crates              the LigninDB crates, one a line
#   lignin-ci.sh machine [--every S] the machine: cores, memory, swap, free disk;
#                                    with --every, again every S seconds (S a
#                                    whole number above 0)
#   lignin-ci.sh rings               whether the machine gives io_uring
#                                    instances, in one line, and
#                                    LIGNIN_IO_URING set to it for the tests
#                                    of the later steps of a job
#   lignin-ci.sh test [ARGS]         `rings`, then the tests of every LigninDB
#                                    crate on this machine (ARGS follow
#                                    `cargo test`), then those of Merk's node
#                                    store on LigninDB and of GroveDB's
#                                    LigninDB engine
#   lignin-ci.sh features            the feature sets that must build apart:
#                                    GroveDB with no engine and on each engine
#                                    alone, Merk on each engine alone, the
#                                    engine crates on their own; and the cfg
#                                    of GroveDB's default engine across
#                                    GroveDB's feature sets
#   lignin-ci.sh docs                rustdoc with warnings denied, for the
#                                    crates whose documentation builds clean
#   lignin-ci.sh loom                the loom models
#   lignin-ci.sh miri [--full]       Miri over the file layer, the ring of its
#                                    direct I/O path, the buffer pool and the
#                                    software path of the checksum; --full
#                                    adds the file layer under Tree Borrows
#                                    and every test of the checksum
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
#   lignin-ci.sh crash [ARGS]             the store crash campaign: the
#                                         defects it must find, then the
#                                         campaign; ARGS follow
#                                         `lignin-crash run`
#   lignin-ci.sh crash-engine [ARGS]      the engine crash campaign: the
#                                         defects it must find, lineages that
#                                         wipe again and again, then the
#                                         campaign; ARGS follow
#                                         `lignin-crash engine run`, and a
#                                         `--save DIR` among them is given to
#                                         all three
#   lignin-ci.sh schedules [POINTS]       one step of the engine's background
#                                         work, a reader or an open of a
#                                         checkpoint against commits let in at
#                                         one of its points: each sweep at
#                                         every point, or at no more than
#                                         POINTS where it samples them
#   lignin-ci.sh stress SECONDS           the engine's stress test: readers,
#                                         writers, write-back, the reclaimer,
#                                         the cleaner and checkpoints at once
#   lignin-ci.sh tsan SECONDS             the same under ThreadSanitizer, with
#                                         the threaded tests of the buffer
#                                         pool and of the engine
#
# Miri, cargo-fuzz and ThreadSanitizer run on the toolchain $LIGNIN_NIGHTLY
# (default nightly-2026-02-24), everything else on the one
# rust-toolchain.toml names.
# Test databases go to the temporary directory: TMPDIR says where. `test`,
# `stress` and `tsan` raise the limit of open files, whose default of 256 on
# macOS is less than the RocksDB databases of the test kit's tests hold at
# once, and less than the engine's stress test does.
set -euo pipefail
shopt -s inherit_errexit 2>/dev/null || true

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"
nightly=${LIGNIN_NIGHTLY:-nightly-2026-02-24}

say() { printf '== %s\n' "$*"; }
# Raises the soft limit of open files to the hard limit, at most 65536.
raise_open_files() {
    local limit
    limit=$(ulimit -Hn)
    if [ "$limit" = unlimited ] || [ "$limit" -gt 65536 ]; then
        limit=65536
    fi
    ulimit -n "$limit"
}
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

# What command "$@" prints, or `unreadable` when it fails or prints nothing.
probe() {
    local out
    if out=$("$@" 2>/dev/null) && [ -n "$out" ]; then
        echo "$out"
    else
        echo unreadable
    fi
}

linux_memory() {
    LC_ALL=C free -m | awk '
        /^Mem:/ { memory = sprintf("memory %d of %d MiB used", $3, $2) }
        /^Swap:/ { swap = sprintf(", swap %d of %d MiB", $3, $2) }
        END { if (memory != "") print memory swap }'
}

macos_memory() {
    local bytes
    bytes=$(sysctl -n hw.memsize) && [[ $bytes =~ ^[0-9]+$ ]] &&
        echo "memory $((bytes / 1048576)) MiB installed"
}

disk_free() {
    df -Pm "$1" | awk 'NR == 2 && $4 ~ /^[0-9]+$/ { print $4 " MiB" }'
}

# Cores, memory and swap (on Linux used of total; on macOS the memory
# installed and the swap in use), and the disk free under the checkout and
# the temporary directory. A value that cannot be read is printed as such.
machine() {
    local cores memory
    if [ -r /proc/meminfo ]; then
        cores=$(probe nproc)
        memory=$(probe linux_memory)
    else
        cores=$(probe sysctl -n hw.ncpu)
        memory="$(probe macos_memory), swap $(probe sysctl -n vm.swapusage)"
    fi
    memory=${memory/#unreadable/memory unreadable}
    printf '== %s machine: %s cores, %s; free disk: %s here, %s in %s\n' "$(date -u +%H:%M:%S)" \
        "$cores" "$memory" "$(probe disk_free .)" "$(probe disk_free "${TMPDIR:-/tmp}")" "${TMPDIR:-/tmp}"
}

# What the system answers when it is asked for an io_uring instance of the
# size the file layer asks for: `given`, or `refused: ` and the error. A
# program in C makes the call, so the answer is not that of the code the
# tests are of.
ring_answer() {
    local dir compiler answer=""
    dir=$(mktemp -d)
    cat >"$dir/probe.c" <<'PROBE'
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_io_uring_setup
#define __NR_io_uring_setup 425
#endif

int main(void) {
    /* struct io_uring_params is 120 bytes: all zero asks for no flag. */
    unsigned long long params[15];
    long ring;

    memset(params, 0, sizeof params);
    ring = syscall(__NR_io_uring_setup, 32, params);
    if (ring < 0) {
        printf("refused: %s\n", strerror(errno));
        return 0;
    }
    close((int)ring);
    puts("given");
    return 0;
}
PROBE
    for compiler in "${CC:-cc}" clang gcc; do
        if "$compiler" -o "$dir/probe" "$dir/probe.c" >/dev/null 2>&1; then
            answer=$("$dir/probe") || answer=""
            break
        fi
    done
    rm -rf "$dir"
    echo "$answer"
}

# Says in one line whether this machine gives io_uring instances, which the
# tests of the file layer's direct I/O path make, and sets LIGNIN_IO_URING
# to what it found: for the tests this script runs, and in $GITHUB_ENV for
# the later steps of a job. The tests hold the machine to the variable:
# with `present` those of a ring fail without one, with `absent` they fail
# with one. A value the variable has already is kept.
rings() {
    local answer found
    if [ "$(uname -s)" != Linux ]; then
        say "io_uring: this is not Linux, the direct I/O path is not built and no test makes a ring"
        return
    fi
    answer=$(ring_answer)
    case $answer in
    given) found=present ;;
    "refused: "*) found=absent ;;
    *) fail "no answer to a request for an io_uring instance (is there a C compiler?): $answer" ;;
    esac
    if [ -n "${LIGNIN_IO_URING+set}" ]; then
        case $LIGNIN_IO_URING in
        present | absent) ;;
        *) fail "LIGNIN_IO_URING is present, absent or not set: $LIGNIN_IO_URING" ;;
        esac
        say "io_uring: $answer. LIGNIN_IO_URING=$LIGNIN_IO_URING was set before, and the tests hold the machine to it"
        return
    fi
    export LIGNIN_IO_URING=$found
    [ -z "${GITHUB_ENV:-}" ] || echo "LIGNIN_IO_URING=$found" >>"$GITHUB_ENV"
    if [ "$found" = present ]; then
        say "io_uring: given. LIGNIN_IO_URING=present: the tests of a ring run on rings of this machine"
    else
        say "io_uring: $answer. LIGNIN_IO_URING=absent: the tests of a ring check that the machine refuses one and end there, the rest of the direct I/O path runs on single reads"
    fi
}

case "${1:-}" in
crates)
    listed crates
    ;;

rings)
    [ $# -eq 1 ] || fail "usage: lignin-ci.sh rings"
    rings
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
    raise_open_files
    rings
    say "cargo test ${packages[*]} --all-features $* (open files: $(ulimit -n))"
    status=0
    cargo test "${packages[@]}" --all-features --no-fail-fast "$@" || status=$?
    # Merk's node store on LigninDB and GroveDB's LigninDB engine are
    # LigninDB code in grovedb-merk and in grovedb, each behind the feature
    # lignin_storage of its crate. They run whether or not the crates passed.
    while read -r package filter; do
        beside=(-p "$package" --features lignin_storage --lib "$filter")
        say "cargo test ${beside[*]}"
        if ! found=$(cargo test "${beside[@]}" -- --list); then
            printf 'lignin-ci: the tests of %s with lignin_storage do not build\n' "$package" >&2
            status=1
        elif grep -q ': test$' <<<"$found"; then
            cargo test "${beside[@]}" || status=$?
        else
            printf 'lignin-ci: no test of %s matches %s\n' "$package" "$filter" >&2
            status=1
        fi
    done <<'BESIDE'
grovedb-merk store::lignin
grovedb engine::lignin
BESIDE
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

    say "GroveDB on LigninDB alone: no RocksDB among its crates"
    build_of normal,build grovedb --no-default-features --features lignin_storage |
        refuse 'rocksdb' "RocksDB is among the crates of GroveDB on LigninDB"
    cargo check -p grovedb --no-default-features --features lignin_storage
    cargo check -p grovedb --no-default-features --features lignin_storage --lib --tests
    # No other job lints this feature set: the workspace lint has every
    # feature on, where a line that is compiled for LigninDB alone is left
    # out.
    cargo clippy -p grovedb --no-default-features --features lignin_storage --all-targets -- -D warnings
    # The suite has no engine in that build unless one is selected: with
    # LigninDB selected its tests are type-checked for LigninDB alone.
    GROVEDB_TEST_ENGINE=lignin cargo check -p grovedb --no-default-features --features lignin_storage,estimated_costs --lib --tests

    say "GroveDB on RocksDB alone: no LigninDB among its crates"
    build_of normal,build grovedb --no-default-features --features rocksdb_storage |
        refuse '^lignin' "LigninDB is among the crates of GroveDB on RocksDB"
    cargo check -p grovedb --no-default-features --features rocksdb_storage

    say "Merk on LigninDB alone: no RocksDB among its crates"
    build_of normal,build grovedb-merk --no-default-features --features lignin_storage |
        refuse 'rocksdb' "RocksDB is among the crates of Merk on LigninDB"
    cargo check -p grovedb-merk --no-default-features --features lignin_storage

    say "Merk on RocksDB alone: no LigninDB among its crates"
    build_of normal,build grovedb-merk --no-default-features --features rocksdb_storage |
        refuse '^lignin' "LigninDB is among the crates of Merk on RocksDB"
    cargo check -p grovedb-merk --no-default-features --features rocksdb_storage

    say "The engine chosen at the open as GroveDB's default: by the cfg, and only with both engines"
    # The cfg is to change nothing and warn of nothing where the two engines
    # are not both compiled in. `default_engine` asserts which engine a
    # `GroveDb` that names none is on in the build it is compiled in.
    the_cfg_is_silent() {
        local said
        said=$("$@" 2>&1) || fail "$* failed:"$'\n'"$said"
        if grep -q 'grovedb_default_engine' <<<"$said"; then
            fail "$* names the cfg in its output:"$'\n'"$(grep -B2 -A8 'grovedb_default_engine' <<<"$said")"
        fi
    }
    export GROVEDB_DEFAULT_ENGINE=either
    the_cfg_is_silent cargo check -p grovedb --no-default-features --features verify
    the_cfg_is_silent cargo check -p grovedb --no-default-features --features engine_generic
    the_cfg_is_silent cargo test -p grovedb --no-default-features --features rocksdb_storage --test default_engine
    the_cfg_is_silent cargo test -p grovedb --no-default-features --features lignin_storage --test default_engine
    # Which default the test of the default engine was built for is asked
    # of the names of its tests, apart from the cfg they are compiled under.
    built_for() {
        cargo test -p grovedb --features lignin_storage --test default_engine -- --list |
            grep -q "^$1: test\$" || fail "default_engine has no test $1 in this build"
    }
    # With both engines it is in force: the integration tests and the doc
    # tests are built with the engine chosen at the open as the default.
    built_for chosen_at_the_open::with_no_engine_named_a_database_is_on_the_engine_that_is_chosen_at_the_open
    cargo test -p grovedb --features lignin_storage --test '*'
    cargo test -p grovedb --features lignin_storage --doc
    unset GROVEDB_DEFAULT_ENGINE
    # With neither the variable nor a cfg handed to the compiler, the
    # default engine of a build with both engines is RocksDB.
    built_for with_no_engine_named_a_database_is_on_rocksdb
    cargo test -p grovedb --features lignin_storage --test default_engine
    # The cfg handed to the compiler reaches every crate of a build, which
    # none but grovedb reads.
    RUSTFLAGS="${RUSTFLAGS:-} --cfg grovedb_default_engine=\"either\"" \
        the_cfg_is_silent cargo check -p grovedb --no-default-features --features verify
    # The value `other`, of the variable and of the cfg among the flags
    # of the build, is an error of the build and not silence.
    if GROVEDB_DEFAULT_ENGINE=other cargo check -p grovedb --no-default-features --features verify 2>/dev/null; then
        fail "GROVEDB_DEFAULT_ENGINE=other built: the variable is either, empty or not set"
    fi
    if RUSTFLAGS="${RUSTFLAGS:-} --cfg grovedb_default_engine=\"other\"" \
        cargo check -p grovedb --no-default-features --features verify 2>/dev/null; then
        fail "--cfg grovedb_default_engine=\"other\" built: the one value of the cfg is either"
    fi

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
lignindb models::
MODELS
    ;;

miri)
    # Without its feature direct_io the file layer has no unsafe code of its
    # own, so a pull request runs it under one borrow model and the night
    # under both. The unsafe code of that feature is the ring's, and runs
    # on a model of the system's side under both on every run, as the
    # pool's does.
    case "$#:${2:-}" in
    1:) file_layer=("") checksum=checksum::tests::crc32c_matches ;;
    2:--full) file_layer=("" -Zmiri-tree-borrows) checksum=checksum:: ;;
    *) fail "usage: lignin-ci.sh miri [--full]" ;;
    esac
    # Every set runs, and every test target of a set, whatever failed
    # before it: one run shows the failures of every set.
    status=0
    for borrows in "${file_layer[@]}"; do
        say "Miri: the file layer ${borrows:-(Stacked Borrows)}"
        MIRIFLAGS="${MIRIFLAGS:-} -Zmiri-disable-isolation $borrows" \
            cargo "+$nightly" miri test -p lignin-io --no-fail-fast || status=$?
    done
    for borrows in "" -Zmiri-tree-borrows; do
        say "Miri: the ring of the direct I/O path ${borrows:-(Stacked Borrows)}, on a model of the system's side: no ring of the system is made"
        MIRIFLAGS="${MIRIFLAGS:-} $borrows" \
            cargo "+$nightly" miri test -p lignin-io --features direct_io --lib --no-fail-fast linux:: ||
            status=$?
    done
    for borrows in "" -Zmiri-tree-borrows; do
        say "Miri: the buffer pool ${borrows:-(Stacked Borrows)}"
        MIRIFLAGS="${MIRIFLAGS:-} $borrows" \
            cargo "+$nightly" miri test -p lignin-store --lib --no-fail-fast cache:: ||
            status=$?
    done
    # crc32c takes the instructions of the machine where it finds them
    # (x86_64, aarch64) and its tables elsewhere. A big-endian target has
    # neither, so this runs the tables, on the other byte order. A pull
    # request runs the published vectors and the random lengths; the night
    # adds the test that walks 128 MiB.
    say "Miri: the checksum's software path, big-endian ($checksum)"
    cargo "+$nightly" miri test -p lignin-format --lib --no-fail-fast \
        --target s390x-unknown-linux-gnu "$checksum" || status=$?
    exit "$status"
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

crash)
    shift
    # The campaign's own acceptance test first: a campaign that does not
    # find a planted defect has shown nothing by finding none.
    say "lignin-crash planted"
    cargo run --release -p lignin-testkit --bin lignin-crash -- planted
    say "lignin-crash run $*"
    cargo run --release -p lignin-testkit --bin lignin-crash -- run "$@"
    ;;

crash-engine)
    shift
    # A seed gives the streams of a run of this campaign, and not its crash
    # states, so each of the three commands is told where to keep the files
    # of a crash state in which it finds a violation: a directory of its own
    # under the one the caller names. `planted` keeps only those of a run
    # that has a violation with no defect planted.
    # (An empty array is unset to the bash of macOS under `set -u`, hence
    # the `+` forms below.)
    kept=
    for ((at = 1; at <= $#; at++)); do
        if [ "${!at}" = --save ]; then
            next=$((at + 1))
            kept=${!next:?"lignin-ci.sh crash-engine: --save needs a directory"}
        fi
    done
    planted=()
    wiped=()
    if [ -n "$kept" ]; then
        planted=(--save "$kept/planted")
        wiped=(--save "$kept/wipe")
    fi
    # As for the store's campaign: what is planted has to be found first.
    # Then the second sentence of the campaign's acceptance test by itself:
    # a crash at any point during a wipe leaves the database as it was or
    # empty.
    say "lignin-crash engine planted"
    cargo run --release -p lignin-testkit --bin lignin-crash -- engine planted --jobs 4 ${planted[@]+"${planted[@]}"}
    say "lignin-crash engine wipe"
    cargo run --release -p lignin-testkit --bin lignin-crash -- engine wipe --runs 4 --jobs 4 --quiet ${wiped[@]+"${wiped[@]}"}
    say "lignin-crash engine run $*"
    cargo run --release -p lignin-testkit --bin lignin-crash -- engine run "$@"
    ;;

miri-schedules)
    seeds=${2:?"usage: lignin-ci.sh miri-schedules SEEDS"}
    for borrows in "" -Zmiri-tree-borrows; do
        say "Miri: the pool's races under $seeds schedules ${borrows:-(Stacked Borrows)}"
        MIRIFLAGS="${MIRIFLAGS:-} -Zmiri-many-seeds=0..$seeds $borrows" \
            cargo "+$nightly" miri test -p lignin-store --lib cache::tests::races
    done
    ;;

schedules)
    points=${2:-all}
    [[ $points == all || $points =~ ^[1-9][0-9]*$ ]] ||
        fail "usage: lignin-ci.sh schedules [POINTS]"
    export LIGNIN_SCHEDULE_POINTS=$points
    say "the schedules of the engine, $points points of each step"
    sweeps=(cargo test -p lignindb --lib db::tests::schedules::)
    found=$("${sweeps[@]}" -- --list)
    grep -q ': test$' <<<"$found" || fail "no test of lignindb is a schedule"
    "${sweeps[@]}" -- --nocapture
    ;;

stress | tsan)
    seconds=${2:-}
    [[ $seconds =~ ^[1-9][0-9]*$ ]] || fail "usage: lignin-ci.sh $1 SECONDS"
    # The stress test holds a database and a checkpoint of it open at
    # once, both in small files.
    raise_open_files
    # An optimised build with the assertions of a debug build, in a
    # directory of its own: the profile is not the one of a release.
    export CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true
    export CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true
    export LIGNIN_STRESS_SECONDS=$seconds
    export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$repo/target}/$1
    long=(-p lignindb --release --lib db::tests::stress::an_hour_of_stress)
    runner=(cargo test)
    under=
    if [ "$1" = tsan ]; then
        # ThreadSanitizer needs the standard library built with it, which
        # asks for a target by name. A test binary in which it reported a
        # data race ends with a failure, whatever its tests said.
        host=$(rustc "+$nightly" -vV | sed -n 's/^host: //p')
        [ -n "$host" ] || fail "the toolchain $nightly names no host"
        export RUSTFLAGS="${RUSTFLAGS:-} -Zsanitizer=thread"
        runner=(cargo "+$nightly" test -Zbuild-std --target "$host")
        under="ThreadSanitizer: "
        while read -r crate tests; do
            say "ThreadSanitizer: cargo test -p $crate --release --lib $tests"
            found=$("${runner[@]}" -p "$crate" --release --lib "$tests" -- --list)
            grep -q ': test$' <<<"$found" || fail "no test of $crate matches $tests"
            "${runner[@]}" -p "$crate" --release --lib "$tests"
        done <<'THREADED'
lignin-store cache::tests::
lignindb snapshot::tests::
lignindb scope::tests::
lignindb models::
lignindb db::tests::schedules::
lignindb db::tests::reclaim::
lignindb db::tests::kept::
lignindb commit::tests::cleaner::the_cleaner_runs_beside
THREADED
    fi
    say "${under}the stress test of the engine for $seconds s"
    found=$("${runner[@]}" "${long[@]}" -- --ignored --list)
    grep -q ': test$' <<<"$found" || fail "no test of lignindb is the long stress test"
    "${runner[@]}" "${long[@]}" -- --ignored --nocapture
    ;;

*)
    awk 'NR > 1 && /^set -euo/ { exit } NR > 1' "${BASH_SOURCE[0]}" >&2
    exit 2
    ;;
esac
