#!/usr/bin/env bash
# Runs GroveDB with its storage crate replaced by the cost model.
#
#   check.sh                      replay the golden corpus on the model
#   check.sh replay --only NAME   any arguments of `lignin-golden` follow
#   check.sh dump --out DIR ...
#   check.sh test -p grovedb-merk any arguments of `cargo test` follow: the
#                                 test suite of a crate, run on the model
#
# The script lays out a second workspace under $LIGNIN_COST_MODEL_WORK
# (default: target/cost-model) in which every crate is this checkout's and
# `storage` is `lignin-testkit/cost-model-storage`, and runs Cargo there.
# A replay fails if RocksDB is among the crates that build, if a step
# differs from the corpus, or if the model met a node whose link sizes are
# not the ones Merk gave with it. The rule counts of the run are printed
# last and kept in $LIGNIN_COST_MODEL_WORK/rule-hits.tsv.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
work=${LIGNIN_COST_MODEL_WORK:-$repo/target/cost-model}
workspace=$work/workspace
storage=$workspace/storage

rm -rf "$workspace"
mkdir -p "$storage"

ln -s "$repo/Cargo.toml" "$workspace/Cargo.toml"
for file in rust-toolchain.toml .rustfmt.toml .cargo; do
    [ -e "$repo/$file" ] && ln -s "$repo/$file" "$workspace/$file"
done
[ -e "$repo/Cargo.lock" ] && cp "$repo/Cargo.lock" "$workspace/Cargo.lock"
for manifest in "$repo"/*/Cargo.toml; do
    member=$(basename "$(dirname "$manifest")")
    [ "$member" = storage ] || ln -s "$repo/$member" "$workspace/$member"
done
ln -s "$here/manifest.toml" "$storage/Cargo.toml"
ln -s "$here/src" "$storage/src"

export CARGO_TARGET_DIR=$work/target
hits=$work/rule-hits.tsv
rm -f "$hits"
cd "$workspace"

if [ "${1:-}" = test ]; then
    shift
    LIGNIN_COST_MODEL_HITS=$hits exec cargo test "$@"
fi

if cargo tree -p lignin-testkit -e normal,build --prefix none | grep -qi rocksdb; then
    echo "RocksDB is among the crates lignin-testkit builds in this workspace" >&2
    exit 2
fi

[ $# -gt 0 ] || set -- replay
status=0
LIGNIN_COST_MODEL_HITS=$hits cargo run --release -p lignin-testkit --bin lignin-golden -- \
    "$@" --corpus "$repo/lignin-testkit/corpus" || status=$?

if [ -e "$hits" ]; then
    echo
    echo "rule	times applied	case"
    cat "$hits"
    if ! grep -q "^N2	0	.*are not the node's$" "$hits"; then
        echo "the link sizes Merk gave with a node differ from the node's" >&2
        [ "$status" -ne 0 ] || status=3
    fi
fi
exit "$status"
