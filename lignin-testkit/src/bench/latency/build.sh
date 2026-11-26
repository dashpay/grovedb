#!/usr/bin/env bash
# build.sh NAME COMMIT
#
# A build of `lignin-bench` for a comparison: a clean checkout of COMMIT of
# $REPO (default: the repository this script is in) under $BUILDS/NAME/src,
# built in $BUILDS/NAME/target. Leaves $BUILDS/NAME/lignin-bench with its
# commit and its sha256 beside it, and prints both.
#
# The first build of a $BUILDS leaves its lock file there and every later
# one is made with it, so that two builds differ in the crates their commits
# changed and in nothing that was resolved anew between them.
set -euo pipefail
name=$1 commit=$2
repo=${REPO:-$(git -C "$(dirname "$0")" rev-parse --show-toplevel)} builds=${BUILDS:-$HOME/builds}
mkdir -p "$builds"; rm -rf "$builds/$name"; mkdir -p "$builds/$name"
git clone -q --no-hardlinks "$repo" "$builds/$name/src"
git -C "$builds/$name/src" checkout -q --detach "$commit"
[ -z "$(git -C "$builds/$name/src" status --porcelain)" ]
if [ -f "$builds/Cargo.lock" ]; then cp "$builds/Cargo.lock" "$builds/$name/src/Cargo.lock"; fi
(cd "$builds/$name/src" && CARGO_TARGET_DIR="$builds/$name/target" \
   cargo build --release -p lignin-testkit --bin lignin-bench 2>&1 | tail -2)
[ -f "$builds/Cargo.lock" ] || cp "$builds/$name/src/Cargo.lock" "$builds/Cargo.lock"
cmp "$builds/Cargo.lock" "$builds/$name/src/Cargo.lock"
cp "$builds/$name/target/release/lignin-bench" "$builds/$name/lignin-bench"
git -C "$builds/$name/src" rev-parse HEAD > "$builds/$name/commit"
sha256sum "$builds/$name/lignin-bench" | cut -d' ' -f1 > "$builds/$name/sha256"
echo "$name $(cat "$builds/$name/commit") sha256 $(cat "$builds/$name/sha256")"
