#!/usr/bin/env bash
# Records the golden corpus from the engine it is pinned to, with the test
# kit of this checkout.
#
#   record-at-pin.sh DIR                  the standard scale, into DIR
#   record-at-pin.sh DIR --reference      the reference scale
#   record-at-pin.sh DIR --only NAME ...  any arguments of `lignin-golden
#                                         record` follow the directory
#
# DIR is a corpus directory: this checkout's `lignin-testkit/corpus` to add
# to the corpus, any other to compare a recording with it.
#
# The script makes a detached worktree of the pinned commit and gives it the
# workspace manifest and the LigninDB crates of this checkout, as they are
# committed, and its `Cargo.lock`. Of the LigninDB crates, the worktree's
# workspace lists only lignin-testkit as a member. The script takes out of
# the worktree's copy of `lignin-testkit/Cargo.toml` what the pinned engine
# cannot resolve, records there and removes the worktree. It exits with what `record` exits with.
# When it stops before recording it says why, and exits with 2 or with what
# git or Cargo exited with.
#
# The pinned commit is the one the traces of this checkout's corpus name, or
# $LIGNIN_PIN. The worktree and its build are under $LIGNIN_PIN_WORK
# (default: target/record-at-pin), for one run at a time; the build is kept
# for the next run.
# shellcheck disable=SC2016
set -euo pipefail

fail() {
    echo "record-at-pin: $1" >&2
    shift
    [ $# -eq 0 ] || printf '%s\n' "$@" >&2
    exit 2
}

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
testkit=lignin-testkit/Cargo.toml

case "${1:-}" in
"" | -*) fail "usage: record-at-pin.sh DIR [arguments of \`lignin-golden record\`]" ;;
esac
mkdir -p "$1"
into=$(cd "$1" && pwd)
shift

named=${LIGNIN_PIN:-}
if [ -z "$named" ]; then
    named=$(grep -rh --include='*.trace' -m1 '^engine ' "$here" | sort -u || true)
    [ "$(grep -c . <<<"$named")" = 1 ] || fail \
        "the traces of $here do not name one engine commit:" "$named" \
        "Say which to record from with LIGNIN_PIN=<commit>."
    named=${named#engine }
fi
pin=$(git -C "$repo" rev-parse --verify --quiet "$named^{commit}") ||
    fail "the pinned commit $named is not in this repository: fetch it"
head=$(git -C "$repo" rev-parse HEAD)

# The members of the workspace manifest on standard input, one a line.
members_of() {
    { echo && cat; } | sed 's/#.*//' | tr '\n' ' ' |
        sed -n 's/.*[[:space:]]members[[:space:]]*=[[:space:]]*\[\([^]]*\)\].*/\1/p' |
        tr -d '"[:space:]' | tr ',' '\n' | grep .
}

members=$(git -C "$repo" show "$head:Cargo.toml" | members_of) ||
    fail "the workspace manifest of $head lists no members"
taken=(Cargo.toml)
kept=()
left_out=()
while read -r member; do
    case $member in
    lignin-testkit) taken+=("$member") kept+=("$member") ;;
    lignin*) taken+=("$member") left_out+=("$member") ;;
    *)
        kept+=("$member")
        git -C "$repo" cat-file -e "$pin:$member/Cargo.toml" 2>/dev/null || fail \
            "the workspace member $member is not a LigninDB crate and is not in the pinned commit $pin:" \
            "the engine of this checkout is not the pinned one"
        ;;
    esac
done <<<"$members"
[[ " ${kept[*]} " == *" lignin-testkit "* ]] ||
    fail "the workspace manifest of $head does not list lignin-testkit among its members"

changed=$(git -C "$repo" status --porcelain -- "${taken[@]}" ':(exclude)lignin-testkit/corpus')
[ -z "$changed" ] || fail \
    "the recording takes these as they are committed, and they have changes that are not:" "$changed"
[ -e "$repo/Cargo.lock" ] || fail \
    "this checkout has no Cargo.lock: build it once (cargo check -p lignin-testkit)," \
    "so that the pinned engine is built with the versions of the dependencies this checkout is built with"

work=${LIGNIN_PIN_WORK:-$repo/target/record-at-pin}
mkdir -p "$work"
work=$(cd "$work" && pwd)
worktree=$work/worktree
mkdir "$work/lock" 2>/dev/null || fail \
    "another run of this script is using $work." \
    "If none is, one was killed: remove $work/lock."
leave() {
    status=$?
    cd "$repo" || true
    if [ -e "$worktree" ]; then
        git worktree remove --force "$worktree" ||
            echo "record-at-pin: $worktree could not be removed" >&2
    fi
    rmdir "$work/lock" || true
    exit "$status"
}
trap leave EXIT

if [ -e "$worktree" ]; then
    git -C "$repo" worktree remove --force "$worktree" ||
        fail "$worktree is in the way and git could not remove it"
fi
git -C "$repo" worktree add --force --quiet --detach "$worktree" "$pin"
git -C "$worktree" checkout --quiet "$head" -- "${taken[@]}"
cp "$repo/Cargo.lock" "$worktree/Cargo.lock"

# The LigninDB crates carry the version of the workspace they are committed
# in, and ask the GroveDB crates and each other for it. The pinned engine can
# be of an earlier release, and then Cargo selects no version for what the
# test kit asks. The worktree's copies of their manifests are given the pinned
# engine's version, for themselves and for what they ask. Nothing recorded
# holds a version of a crate.
version_of() {
    sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | sed -n 1p
}
ours=$(version_of "$worktree/$testkit")
theirs=$(version_of "$worktree/grovedb/Cargo.toml")
[ -n "$ours" ] && [ -n "$theirs" ] || fail \
    "the version of the test kit ($ours) or of the pinned engine ($theirs) could not be read from its manifest"
if [ "$ours" != "$theirs" ]; then
    asked=$(printf '%s' "$ours" | sed 's/\./\\./g')
    for member in "${taken[@]}"; do
        [ "$member" = Cargo.toml ] && continue
        sed "s/\"$asked\"/\"$theirs\"/g" "$worktree/$member/Cargo.toml" > "$worktree/$member/Cargo.toml.pinned"
        mv "$worktree/$member/Cargo.toml.pinned" "$worktree/$member/Cargo.toml"
    done
    echo "record-at-pin: the LigninDB crates are at $ours and the pinned engine at $theirs:" \
        "the worktree's copies of their manifests say $theirs" >&2
fi

# The recording builds the test kit alone. Of the LigninDB crates, the
# worktree's workspace lists only lignin-testkit as a member; the others are
# in it as the test kit's path dependencies, or not at all. A LigninDB crate
# the test kit does not depend on there may ask of the pinned engine what it
# does not have, as lignin-tools asks Merk for `lignin_storage`: the one line
# by which the test kit asks for lignin-tools is taken out below.
list=$(printf '"%s", ' "${kept[@]}")
awk -v list="${list%, }" '
    !done && /^[[:space:]]*members[[:space:]]*=[[:space:]]*\[/ {
        inside = 1
        print "members = [" list "]"
    }
    inside { if (/\]/) { inside = 0; done = 1 }; next }
    { print }' "$worktree/Cargo.toml" > "$worktree/Cargo.toml.pinned"
mv "$worktree/Cargo.toml.pinned" "$worktree/Cargo.toml"
[ "$(members_of < "$worktree/Cargo.toml")" = "$(printf '%s\n' "${kept[@]}")" ] || fail \
    "the members of the worktree's workspace manifest did not come out as" "${kept[@]}" \
    "The manifest of $head lists them in a form this script does not read: bring it up to date."
[ ${#left_out[@]} -eq 0 ] ||
    echo "record-at-pin: not listed as members of the worktree's workspace, which has them" \
        "only as path dependencies of lignin-testkit, if at all: ${left_out[*]}" >&2

# GroveDB and Merk at the pin have no feature `lignin_storage`. The replay
# through the LigninDB node store (tests/merk_store.rs) asks Merk for it, and
# the engine crash campaign (src/crash/engine) asks both: in a table of its
# own, [target.'cfg(all())'.dependencies], so that each asking is a line that
# can go. The tests of the migrator (tests/migrate.rs) ask for lignin-tools
# with its feature `migrate`, and with that feature the tool asks GroveDB for
# `lignin_storage`. Cargo resolves every dependency of a member whatever is
# built.
asks='/^\[/ { section = $0 }
    { asked = ((section == "[dev-dependencies]" || section == "[target.'"'"'cfg(all())'"'"'.dependencies]") &&
        /^grovedb(-merk)?[ =]/ && /"lignin_storage"/) ||
        (section == "[dev-dependencies]" && /^lignin-tools[ =]/ && /"migrate"/) }'
found=$(awk "$asks asked { found++ } END { print found + 0 }" "$worktree/$testkit")
[ "$found" = 4 ] || fail \
    "expected four lines of $testkit that ask of the pinned engine what it does not have. Three ask" \
    "for the feature \`lignin_storage\`: grovedb-merk under [dev-dependencies], and grovedb and" \
    "grovedb-merk under [target.'cfg(all())'.dependencies]. One asks for lignin-tools with its feature" \
    "\`migrate\`, under [dev-dependencies]: with it the tool asks grovedb for \`lignin_storage\`." \
    "Found $found." \
    "The manifest has changed: see what it asks of the pinned engine now, and bring this script" \
    "and the section of the README that names it up to date."
awk "$asks !asked" "$worktree/$testkit" > "$worktree/$testkit.pinned"
mv "$worktree/$testkit.pinned" "$worktree/$testkit"

export CARGO_TARGET_DIR=$work/target
cd "$worktree"
if ! unresolved=$(cargo metadata --format-version 1 2>&1 >/dev/null); then
    dependency=$(sed -n \
        -e 's/.*failed to select a version for `\([^`]*\)`.*/\1/p' \
        -e 's/.*failed to select a version for the requirement `\([^ `]*\).*/\1/p' \
        -e 's/.*failed to get `\([^`]*\)` as a dependency.*/\1/p' \
        -e 's/.*failed to load manifest for dependency `\([^`]*\)`.*/\1/p' \
        -e 's/.*failed to load source for dependency `\([^`]*\)`.*/\1/p' \
        -e 's/.*no matching package named `\([^`]*\)` found.*/\1/p' <<<"$unresolved")
    dependency=${dependency%%$'\n'*}
    fail "the workspace does not resolve against the pinned engine $pin." \
        "The dependency Cargo could not resolve: ${dependency:-see below}." \
        "This script lists only lignin-testkit among the LigninDB members and takes out of the pinned" \
        "copy of $testkit only the lines that ask grovedb and grovedb-merk for \`lignin_storage\` and the" \
        "line that asks for lignin-tools with \`migrate\`; what else the pinned engine cannot give has to" \
        "be added to it." \
        "Cargo said:" "$unresolved"
fi

echo "recording from the engine at $pin with the test kit of $head into $into" >&2
cargo run --release -p lignin-testkit --bin lignin-golden -- record --corpus "$into" "$@"
