#!/usr/bin/env bash
# The LigninDB jobs of CI, one command each, so that every job can be run by
# hand from any directory of a checkout. docs/lignindb/ci.md says which job
# runs which, where and when.
#
#   lignin-ci.sh crates              the LigninDB crates, one a line
#   lignin-ci.sh test [ARGS]         the tests of every LigninDB crate on this
#                                    machine; ARGS follow `cargo test`
#   lignin-ci.sh features            the feature sets that must build apart:
#                                    GroveDB with no engine, Merk on each engine
#                                    alone, the engine crates on their own
#   lignin-ci.sh docs                rustdoc with warnings denied, for the
#                                    crates whose documentation builds clean
#   lignin-ci.sh loom                every loom model
#   lignin-ci.sh miri                Miri over the file layer, the buffer pool
#                                    and the software path of the checksum
#   lignin-ci.sh fuzz-targets        build the fuzz targets of lignin-format
#   lignin-ci.sh cost-model          the golden corpus replayed on the cost model
#
# The nightly runs, which take longer:
#
#   lignin-ci.sh diff-fuzz SECONDS        the differential fuzzer
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
# Test databases go to the temporary directory: TMPDIR says where.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"
nightly=${LIGNIN_NIGHTLY:-nightly-2026-02-24}

say() { printf '== %s\n' "$*"; }
fail() {
    printf 'lignin-ci: %s\n' "$*" >&2
    exit 1
}

crates() {
    cargo metadata --no-deps --format-version 1 |
        jq -r '.packages[].name | select(startswith("lignin"))' | sort
}

each_crate() {
    local crate
    for crate in $(crates); do
        printf -- '-p\n%s\n' "$crate"
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

# The LigninDB crates that have Cargo features of their own.
crates_with_features() {
    cargo metadata --no-deps --format-version 1 |
        jq -r '.packages[] | select((.name | startswith("lignin")) and (.features | length > 0)) | .name' |
        sort
}

# Fails when a crate whose name matches $1 is among the crates on stdin.
refuse() {
    local pattern=$1 what=$2 found
    found=$(grep -E "$pattern" || true)
    [ -z "$found" ] || fail "$what:"$'\n'"$found"
}

case "${1:-}" in
crates)
    crates
    ;;

test)
    shift
    mapfile -t packages < <(each_crate)
    say "cargo test ${packages[*]} --all-features $*"
    cargo test "${packages[@]}" --all-features --no-fail-fast "$@"
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

    say "The LigninDB crates that have features, with none and with every one"
    for crate in $(crates_with_features); do
        cargo check -p "$crate" --no-default-features
        cargo check -p "$crate" --all-features
    done
    ;;

docs)
    # Every LigninDB crate but those whose documentation had warnings when
    # this step was added: lignin-testkit (two ambiguous links in `diff`).
    # The older crates are not in it either (grovedb-merk had 33 errors).
    # A crate leaves this list when its documentation builds clean.
    not_clean=(lignin-testkit)
    packages=()
    for crate in $(crates | grep -vxF -f <(printf '%s\n' "${not_clean[@]}")); do
        packages+=(-p "$crate")
    done
    say "RUSTDOCFLAGS=-D warnings cargo doc ${packages[*]} --all-features --no-deps"
    RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings" \
        cargo doc "${packages[@]}" --all-features --no-deps
    ;;

loom)
    # A build of its own: the cfg changes every crate it compiles.
    export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$repo/target}/loom
    export RUSTFLAGS="${RUSTFLAGS:-} --cfg loom"
    for crate in lignin-store lignindb; do
        say "loom: cargo test -p $crate --release --lib"
        cargo test -p "$crate" --release --lib
    done
    ;;

miri)
    say "Miri: the file layer, under Stacked Borrows and Tree Borrows"
    MIRIFLAGS="${MIRIFLAGS:-} -Zmiri-disable-isolation" \
        cargo "+$nightly" miri test -p lignin-io
    MIRIFLAGS="${MIRIFLAGS:-} -Zmiri-disable-isolation -Zmiri-tree-borrows" \
        cargo "+$nightly" miri test -p lignin-io
    say "Miri: the buffer pool, under Stacked Borrows and Tree Borrows"
    cargo "+$nightly" miri test -p lignin-store --lib cache::
    MIRIFLAGS="${MIRIFLAGS:-} -Zmiri-tree-borrows" \
        cargo "+$nightly" miri test -p lignin-store --lib cache::
    # crc32c takes the instructions of the machine where it finds them
    # (x86_64, aarch64) and its tables elsewhere. A big-endian target has
    # neither, so this runs the tables, on the other byte order.
    say "Miri: the checksum's software path, big-endian"
    cargo "+$nightly" miri test -p lignin-format --lib \
        --target s390x-unknown-linux-gnu checksum::
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
    say "lignin-testkit/cost-model-storage/check.sh"
    lignin-testkit/cost-model-storage/check.sh
    ;;

diff-fuzz)
    seconds=${2:?"usage: lignin-ci.sh diff-fuzz SECONDS"}
    say "lignin-diff fuzz --duration $seconds"
    cargo run --release -p lignin-testkit --bin lignin-diff -- fuzz --duration "$seconds"
    ;;

corpus-reference)
    shift
    say "lignin-golden replay --reference $*"
    cargo run --release -p lignin-testkit --bin lignin-golden -- replay --reference "$@"
    ;;

corpus-at-pin)
    shift
    into=$(mktemp -d)
    trap 'rm -rf "$into"' EXIT
    say "record-at-pin.sh into $into $*"
    lignin-testkit/corpus/record-at-pin.sh "$into" "$@"
    say "the recording against lignin-testkit/corpus"
    differ=0
    while IFS= read -r -d '' trace; do
        name=${trace#"$into"/}
        if ! cmp -s "$trace" "lignin-testkit/corpus/$name"; then
            echo "differs from the committed corpus: $name"
            differ=1
        fi
    done < <(find "$into" -name '*.trace' -print0)
    recorded=$(find "$into" -name '*.trace' | wc -l)
    [ "$recorded" -gt 0 ] || fail "nothing was recorded"
    [ "$differ" = 0 ] || fail "the corpus does not come out of its pinned engine as committed"
    echo "$recorded traces recorded, all as committed"
    ;;

fuzz-decoders)
    seconds=${2:?"usage: lignin-ci.sh fuzz-decoders SECONDS"}
    fuzz=lignin-format/fuzz
    if [ ! -e "$fuzz/Cargo.toml" ]; then
        say "no $fuzz/Cargo.toml: nothing to fuzz"
        exit 0
    fi
    seeds=$(mktemp -d)
    trap 'rm -rf "$seeds"' EXIT
    (cd "$fuzz" && ./seed.sh "$seeds")
    cd "$fuzz"
    cargo "+$nightly" fuzz build -s none --debug-assertions
    for target in $(cargo "+$nightly" fuzz list); do
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
