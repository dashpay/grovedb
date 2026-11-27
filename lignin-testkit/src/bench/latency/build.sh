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
# The paths of the checkout and of the target directory are written as
# `/src` and `/target` in what is compiled, so that a crate that no patch
# touches comes out the same in every build, and `$BUILDS/NAME/libraries`
# lists every library of the build by its name and sha256: two builds can
# then be held against one another library by library.
#
# Every build keeps its frame pointers, in Rust and in what is compiled from
# C and C++: the trace of `trace.bt` reads a user stack by them, and on a
# target that leaves them out by default a stack ends at its first frame.
#
# One writer at a time has a directory of builds: the script takes the lock
# `builds.lock` there, which a campaign holds for as long as it runs, and a
# campaign says to the builds it makes itself that it has it
# (`BUILDS_LOCK_HELD`).
#
# A build that is there is replaced only if this script made it: it leaves
# `made-by-build.sh` in a directory it makes, and refuses to remove one that
# is not empty and has not that file.
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
if [ -z "${BUILDS_LOCK_HELD:-}" ]; then
  exec 8> "$builds/builds.lock"
  flock -n 8 || {
    echo "build.sh: a campaign or another build has $builds" >&2
    exit 1
  }
fi
if [ -e "$to" ] && [ ! -f "$to/made-by-build.sh" ] && [ -n "$(ls -A "$to" 2> /dev/null)" ]; then
  echo "build.sh: $to is there and this script did not make it: it is left as it is" >&2
  exit 1
fi
rm -rf "$to"
mkdir -p "$to"
touch "$to/made-by-build.sh"
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
  RUSTFLAGS="${RUSTFLAGS:-} -C force-frame-pointers=yes --remap-path-prefix=$to/src=/src --remap-path-prefix=$to/target=/target" \
  CFLAGS="${CFLAGS:-} -fno-omit-frame-pointer -ffile-prefix-map=$to/src=/src -ffile-prefix-map=$to/target=/target" \
  CXXFLAGS="${CXXFLAGS:-} -fno-omit-frame-pointer -ffile-prefix-map=$to/src=/src -ffile-prefix-map=$to/target=/target" \
  cargo build --release -p lignin-testkit --bin lignin-bench 2>&1 | tail -2)
[ -f "$builds/Cargo.lock" ] || cp "$to/src/Cargo.lock" "$builds/Cargo.lock"
cmp "$builds/Cargo.lock" "$to/src/Cargo.lock"
cp "$to/target/release/lignin-bench" "$to/lignin-bench"
git -C "$to/src" rev-parse HEAD > "$to/commit"
sha256sum "$to/lignin-bench" | cut -d' ' -f1 > "$to/sha256"
# Wherever under the target directory this cargo keeps them. The list is in
# the order of bytes, whatever the caller's language, and has its name only
# once it is whole.
find "$to/target/release" -name 'lib*.rlib' | while read -r library; do
  # A library whose checksum cannot be taken ends the build: the list is of
  # every library or is none.
  sum=$(sha256sum "$library" | cut -d' ' -f1) && [ -n "$sum" ] || exit 1
  echo "$(basename "$library" | sed -E 's/^lib(.*)-[0-9a-f]+\.rlib$/\1/') $sum"
done | LC_ALL=C sort > "$to/libraries.part"
[ -s "$to/libraries.part" ] || {
  echo "build.sh: no library of the build was found under $to/target/release" >&2
  exit 1
}
mv "$to/libraries.part" "$to/libraries"
line="$name: commit $(cat "$to/commit"), sha256 $(cat "$to/sha256")"
if [ -n "$patch" ]; then
  line="$line, which is $commit with $(basename "$patch") (sha256 $(cat "$to/patch-sha256"))"
fi
echo "$line"
