#!/usr/bin/env bash
# Writes the seed corpus of every fuzz target into <dir>/<target>/, built
# from the golden files: each seed is a control byte and the target's input.
#
#   ./seed.sh <dir>
set -euo pipefail

out=${1:?usage: seed.sh <dir>}
golden="$(cd "$(dirname "$0")" && pwd)/golden/v1"

# A control byte, given as two hex digits.
control() { printf "\\x$1"; }

# A piece: a u16 length, little-endian, then the bytes of a file.
piece() {
    local len
    len=$(wc -c <"$1")
    printf "\\x$(printf %02x $((len & 255)))\\x$(printf %02x $((len >> 8)))"
    cat "$1"
}

empty_piece() { printf '\x00\x00'; }

seed() {
    local target=$1 name=$2
    mkdir -p "$out/$target"
    cat >"$out/$target/$name"
}

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
tail -c +21 "$golden/wal" >"$scratch/wal_records"
tail -c +21 "$golden/value_log" >"$scratch/value_records"
printf '\xff\xff\xff\xff\x2d\x00\x00\x00\x00\x00\x00\x00\x1d\x00\x00\x00' >"$scratch/pending"

for file in "$golden"/*; do
    { control 00; cat "$file"; } | seed frame "$(basename "$file")"
done
{ control 00; cat "$golden/manifest"; } | seed manifest manifest
{ control 00; cat "$golden/segment"; } | seed segment segment
{ control 0e; cat "$golden/segment"; } | seed segment segment-max
{ control 00; cat "$golden/page_table"; } | seed table page_table
{ control 06; printf '\x00\x10\x02\x00\x01\x00\x01\x00'; } | seed table expanded
{ control 00; cat "$golden/value_log"; } | seed value_log value_log
{ control 04; cat "$scratch/value_records"; } | seed value_log records
{ control 06; cat "$scratch/pending"; head -c 20 "$scratch/value_records"; } | seed value_log places
{ control 00; cat "$golden/flat_leaf"; printf '\x0auser/alice\x05user/'; } | seed flat_page flat_leaf
{ control 00; cat "$golden/flat_branch"; printf '\x06user/b\x02us'; } | seed flat_page flat_branch
{ control 04; cat "$golden/tree_page"; } | seed tree_page tree_page
{ control 0c; cat "$golden/tree_page"; } | seed tree_page tree_page-cells
{ control 04; piece "$golden/tree_page"; piece "$golden/tree_run"; } | seed delta tree_run
{ control 04; empty_piece; piece "$golden/created_run"; } | seed delta created_run
{ control 04; piece "$golden/tree_page"; piece "$golden/created_run"; piece "$golden/tree_run"; } |
    seed delta both
{ control 04; piece "$golden/flat_leaf"; piece "$golden/flat_leaf_run"; } | seed flat_run leaf
{ control 04; empty_piece; piece "$golden/flat_leaf_run"; } | seed flat_run leaf-image
{ control 04; piece "$golden/flat_branch"; piece "$golden/flat_branch_run"; } | seed flat_run branch
{ control 00; cat "$golden/wal"; } | seed wal wal
{
    control 00
    piece "$scratch/wal_records"
    piece "$golden/tree_page"
    piece "$golden/flat_leaf"
    piece "$scratch/pending"
} | seed wal_record records
