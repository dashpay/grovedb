#!/usr/bin/env bash
# Runs GroveDB with its storage crate replaced by the cost model.
#
#   check.sh                      replay the golden corpus on the model
#   check.sh replay --only NAME   any arguments of `lignin-golden` follow
#   check.sh dump --out DIR ...
#   check.sh test -p grovedb-merk any arguments of `cargo test` follow: the
#                                 test suite of a crate, run on the model
#   check.sh profile [VERSION]    the cost of a few common operations on a
#                                 grove version, with the rules applied
#
# The script lays out a second workspace under $LIGNIN_COST_MODEL_WORK
# (default: target/cost-model) in which every crate is this checkout's and
# `storage` is `lignin-testkit/cost-model-storage`, and runs Cargo there.
# A replay fails if RocksDB is among the crates that build, if a step
# differs from the corpus, if the model met a node whose link sizes are not
# the ones Merk gave with it, or if it left no rule counts. The rule counts
# of a replay are printed last. Those of a replay or a test run are kept in
# $LIGNIN_COST_MODEL_WORK/rule-hits.tsv, added up over the processes of the
# run.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
work=${LIGNIN_COST_MODEL_WORK:-$repo/target/cost-model}
workspace=$work/workspace
storage=$workspace/storage

mkdir -p "$storage"
ln -sfn "$repo/Cargo.toml" "$workspace/Cargo.toml"
for file in rust-toolchain.toml .rustfmt.toml .cargo; do
    [ -e "$repo/$file" ] && ln -sfn "$repo/$file" "$workspace/$file"
done
[ -e "$workspace/Cargo.lock" ] || [ ! -e "$repo/Cargo.lock" ] ||
    cp "$repo/Cargo.lock" "$workspace/Cargo.lock"
for manifest in "$repo"/*/Cargo.toml; do
    member=$(basename "$(dirname "$manifest")")
    [ "$member" = storage ] || ln -sfn "$repo/$member" "$workspace/$member"
done
ln -sfn "$here/manifest.toml" "$storage/Cargo.toml"
ln -sfn "$here/src" "$storage/src"

export CARGO_TARGET_DIR=$work/target
# A test that is about RocksDB itself is compiled out of this workspace.
export RUSTFLAGS="${RUSTFLAGS:-} --cfg lignin_cost_model"
hits=$work/rule-hits.tsv
counts=$work/rule-hits
rm -rf "$hits" "$counts"
mkdir -p "$counts"
cd "$workspace"

add_up_counts() {
    set -- "$counts"/*.tsv
    [ -e "$1" ] || return 0
    awk -F '\t' '
        !($3 in times) { cases[++rows] = $3; rule[$3] = $1 }
        { times[$3] += $2 }
        END { for (row = 1; row <= rows; row++) print rule[cases[row]] "\t" times[cases[row]] "\t" cases[row] }
    ' "$@" > "$hits"
}

if [ "${1:-}" = test ]; then
    shift
    status=0
    LIGNIN_COST_MODEL_HITS=$counts cargo test "$@" || status=$?
    add_up_counts
    exit "$status"
fi

if [ "${1:-}" = profile ]; then
    shift
    mkdir -p "$work/profile/src"
    ln -sfn "$here/profile.rs" "$work/profile/src/main.rs"
    cat > "$work/profile/Cargo.toml" <<MANIFEST
[package]
name = "lignin-cost-model-profile"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
grovedb = { path = "$workspace/grovedb" }
grovedb-costs = { path = "$workspace/costs" }
grovedb-storage = { path = "$storage", features = ["rocksdb_storage"] }
grovedb-version = { path = "$workspace/grovedb-version" }
tempfile = "3"
MANIFEST
    cd "$work/profile"
    exec cargo run --quiet --release -- "$@"
fi

built=$(cargo tree -p lignin-testkit -e normal,build --prefix none)
case "$built" in
*[Rr]ocks[Dd][Bb]*)
    echo "RocksDB is among the crates lignin-testkit builds in this workspace" >&2
    exit 2
    ;;
esac

[ $# -gt 0 ] || set -- replay
status=0
LIGNIN_COST_MODEL_HITS=$counts cargo run --release -p lignin-testkit --bin lignin-golden -- \
    "$@" --corpus "$repo/lignin-testkit/corpus" || status=$?
add_up_counts

if [ -e "$hits" ]; then
    echo
    echo "rule	times applied	case"
    cat "$hits"
    if ! grep -q "^N2	0	.*are not the node's$" "$hits"; then
        echo "the link sizes Merk gave with a node differ from the node's" >&2
        [ "$status" -ne 0 ] || status=3
    fi
elif [ "$1" = replay ] && [ "$status" -eq 0 ]; then
    echo "the replay left no rule counts in $hits: it did not run on the model" >&2
    status=4
fi
exit "$status"
