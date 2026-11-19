#!/usr/bin/env bash
# The crates a GroveDB release publishes to crates.io, in the order they are
# published (.github/workflows/release-crates.yml). docs/lignindb/publishing.md
# says what the LigninDB crates among them need before a release.
#
#   release-crates.sh list               the crates, one a line, in order
#   release-crates.sh check              the list against the workspace
#   release-crates.sh check --release    the same, and every listed crate can
#                                        be published now
#
# `check` fails when a listed crate is not a member of the workspace, when a
# member that can be published is not listed, or when a listed crate depends
# on a member that is not listed before it. A dependency counts if a build
# of the published crate resolves it on crates.io: a normal or build
# dependency, optional or not, and a dev-dependency that names a version
# (Cargo drops one that names a path alone when it publishes).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

usage() {
    awk 'NR > 1 && /^set -euo/ { exit } NR > 1' "${BASH_SOURCE[0]}" >&2
    exit 2
}

list() {
    # Each crate comes after the workspace crates it depends on. cargo waits
    # for each upload to reach the index before the next publish resolves
    # it, so no sleeps are needed.
    #
    # grovedb-commitment-tree depends on the dashpay orchard fork by git tag
    # with no version, which crates.io rejects, and grovedb depends on
    # grovedb-commitment-tree. Both stay last so everything else publishes;
    # they succeed once orchard is a registry crate.
    cat <<'CRATES'
grovedb-costs
grovedb-path
grovedb-visualize
grovedb-version
grovedbg-types
grovedb-bincode-derive
grovedb-bincode
grovedb-storage
grovedb-epoch-based-storage-flags
grovedb-query
grovedb-element
grovedb-merkle-mountain-range
grovedb-dense-fixed-sized-merkle-tree
grovedb-bulk-append-tree
grovedb-private-document-store
lignin-io
lignin-format
lignin-store
lignin-tree
lignindb
grovedb-merk
grovedb-commitment-tree
grovedb
CRATES
}

case "${1:-}" in
list)
    list
    ;;
check)
    release=false
    case "${2:-}" in
    "") ;;
    --release) release=true ;;
    *) usage ;;
    esac
    # The crates of the list are the arguments of jq, one a word.
    # shellcheck disable=SC2046
    problems=$(cargo metadata --no-deps --format-version 1 |
        jq -r --argjson release "$release" '
            $ARGS.positional as $order
            | (.packages | map({key: .name, value: .}) | from_entries) as $by_name
            | ($order | to_entries | map({key: .value, value: .key}) | from_entries) as $place
            | (
                ($order[] | select($by_name[.] == null)
                    | "\(.) is listed and is not a member of the workspace"),
                (.packages[] | select(.publish == null and ($place[.name] == null))
                    | "\(.name) can be published and is not listed"),
                ($order[] | . as $crate | select($by_name[$crate] != null)
                    | $by_name[$crate].dependencies[]
                    | select(.path != null and $by_name[.name] != null)
                    | select((.kind // "normal") != "dev" or .req != "*")
                    | select(($place[.name] // 1e9) > $place[$crate])
                    | "\($crate) depends on \(.name) (\(.kind // "normal")\(if .optional then ", optional" else "" end)), which is not listed before it"),
                (if $release then
                    ($order[] | select($by_name[.].publish != null and ($by_name[.].publish | index("crates-io") | not))
                        | "\(.) is listed and cannot be published to crates.io (`publish` in its manifest)")
                 else empty end)
            )' --args $(list))
    if [ -n "$problems" ]; then
        echo "release-crates: the list does not match the workspace:" >&2
        echo "$problems" >&2
        exit 1
    fi
    echo "release-crates: $(list | wc -l) crates, in an order crates.io can resolve"
    ;;
*)
    usage
    ;;
esac
