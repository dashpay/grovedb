#!/usr/bin/env bash
# build.sh NAME COMMIT [PATCH]
#
# A build of `lignin-bench` for a comparison: a clean checkout of COMMIT of
# $REPO (default: the repository this script is in) under $BUILDS/NAME/src,
# built in $BUILDS/NAME/target. With PATCH, the patch is applied to the
# checkout and committed there first, under a fixed name and date, so that
# one patch on one commit gives one commit. Leaves $BUILDS/NAME/lignin-bench
# with its commit, its sha256 and the sha256 of the patch beside it, and
# prints them.
#
# Every build keeps its frame pointers, in Rust and in what is compiled from
# C and C++: the trace of `trace.bt` reads a user stack by them, and on a
# target that leaves them out by default a stack ends at its first frame.
#
# The first build of a $BUILDS leaves its lock file there and every later
# one is made with it, so that two builds differ in the crates their commits
# changed and in nothing that was resolved anew between them.
set -euo pipefail
name=${1:?a name for the build} commit=${2:?the commit to build} patch=${3:-}
repo=${REPO:-$(git -C "$(dirname "$0")" rev-parse --show-toplevel)}
builds=${BUILDS:-$HOME/builds}
[ -z "$patch" ] || patch=$(cd "$(dirname "$patch")" && pwd)/$(basename "$patch")
to=${builds:?}/${name:?}
mkdir -p "$builds"
rm -rf "$to"
mkdir -p "$to"
git clone -q --no-hardlinks "$repo" "$to/src"
git -C "$to/src" checkout -q --detach "$commit"
if [ -n "$patch" ]; then
  git -C "$to/src" apply --index "$patch"
  GIT_AUTHOR_NAME=experiment GIT_AUTHOR_EMAIL=experiment@localhost \
    GIT_AUTHOR_DATE=2000-01-01T00:00:00Z GIT_COMMITTER_NAME=experiment \
    GIT_COMMITTER_EMAIL=experiment@localhost GIT_COMMITTER_DATE=2000-01-01T00:00:00Z \
    git -C "$to/src" commit -q -m "experiment: $(basename "$patch")"
  sha256sum "$patch" | cut -d' ' -f1 > "$to/patch-sha256"
fi
[ -z "$(git -C "$to/src" status --porcelain)" ]
if [ -f "$builds/Cargo.lock" ]; then cp "$builds/Cargo.lock" "$to/src/Cargo.lock"; fi
(cd "$to/src" && CARGO_TARGET_DIR="$to/target" \
  RUSTFLAGS="${RUSTFLAGS:-} -C force-frame-pointers=yes" \
  CFLAGS="${CFLAGS:-} -fno-omit-frame-pointer" CXXFLAGS="${CXXFLAGS:-} -fno-omit-frame-pointer" \
  cargo build --release -p lignin-testkit --bin lignin-bench 2>&1 | tail -2)
[ -f "$builds/Cargo.lock" ] || cp "$to/src/Cargo.lock" "$builds/Cargo.lock"
cmp "$builds/Cargo.lock" "$to/src/Cargo.lock"
cp "$to/target/release/lignin-bench" "$to/lignin-bench"
git -C "$to/src" rev-parse HEAD > "$to/commit"
sha256sum "$to/lignin-bench" | cut -d' ' -f1 > "$to/sha256"
line="$name: commit $(cat "$to/commit"), sha256 $(cat "$to/sha256")"
if [ -n "$patch" ]; then
  line="$line, which is $commit with $(basename "$patch") (sha256 $(cat "$to/patch-sha256"))"
fi
echo "$line"
