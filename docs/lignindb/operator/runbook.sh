#!/usr/bin/env bash
# Runs every command of the operator documents, as they are written, in one
# shell: each block marked `sh` of migration.md (the rehearsal of its step 3
# among them), then of backup-and-full-disk.md and of tuning.md. It prints
# each command after `$ ` and then what it printed, so its output is the
# transcript of a run. It stops at the first command that fails.
#
#   GROVEDB_REF=<tag or branch> runbook.sh WORK
#
# WORK is the directory the runbook's WORK names: empty or not there. The
# runbook builds the tool from GROVEDB_REF, cloned from GitHub, so the run
# needs the network and that ref pushed. A line `<!-- runbook.sh: ... -->`
# of a document is run before the block after it and printed after `# `:
# the variables that the document asks its reader to set, for this run.
#
# For a run on a clean machine, start it with no Rust on the path and with
# CARGO_HOME and RUSTUP_HOME naming empty directories: the runbook then
# installs Rust itself, and every crate is fetched again.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
documents=(migration.md backup-and-full-disk.md tuning.md)
others=(architecture.md messages.md)

if [ $# -ne 1 ]; then
    echo "usage: GROVEDB_REF=<tag or branch> $0 WORK" >&2
    exit 2
fi
: "${GROVEDB_REF:?set GROVEDB_REF to the tag or branch the runbook builds from}"
work=$1
case "$work" in *$'\n'*)
    echo "runbook.sh: a WORK with a line break in it is not taken" >&2
    exit 2
    ;;
esac
if [ -e "$work" ] && [ -n "$(ls -A "$work")" ]; then
    echo "runbook.sh: $work is not empty" >&2
    exit 2
fi
mkdir -p "$work"
work=$(cd "$work" && pwd -P && echo .)
work=${work%$'\n.'}
case "$work" in *$'\n'*)
    echo "runbook.sh: WORK resolves to a path with a line break in it" >&2
    exit 2
    ;;
esac

for document in "${others[@]}"; do
    if grep -q '^ *```sh' "$here/$document"; then
        echo "runbook.sh: $document has a block marked sh, which this script does not run" >&2
        exit 2
    fi
done

# The blocks of each document, in order, as `@@ KIND DOCUMENT LINE`, their
# lines, and `@@ end`: a block of commands, or the line a directive is on.
extract() {
    awk -v document="$1" '
        /^ *<!-- runbook\.sh: .* -->$/ && !open {
            text = $0
            sub(/^ *<!-- runbook\.sh: /, "", text)
            sub(/ -->$/, "", text)
            printf "@@ set %s %d\n%s\n@@ end\n", document, NR, text
            next
        }
        /^ *```sh[[:space:]]*$/ && !open {
            open = 1
            indent = index($0, "`") - 1
            printf "@@ run %s %d\n", document, NR + 1
            next
        }
        /^ *```[[:space:]]*$/ && open {
            open = 0
            print "@@ end"
            next
        }
        open { print substr($0, indent + 1) }
    ' "$here/$1"
}

script=$(mktemp "${TMPDIR:-/tmp}/runbook.XXXXXX")
trap 'rm -f "$script"' EXIT
{
    echo 'set -euo pipefail'
    echo "export GROVEDB_REF=$(printf '%q' "$GROVEDB_REF") WORK=$(printf '%q' "$work")"
    echo 'cd "$WORK"'
    echo 'where=start'
    echo 'trap '\''echo; echo "runbook.sh: the command at $where failed" >&2'\'' ERR'
} >"$script"
pending=''
# Writes the command in `pending` to the script: printed, then run.
flush() {
    local first=1 line
    printf "where=%q\n" "$place" >>"$script"
    while IFS= read -r line; do
        if [ $first = 1 ]; then
            printf "printf '%%s\\\\n' %q\n" "\$ $line" >>"$script"
            first=0
        else
            printf "printf '%%s\\\\n' %q\n" "> $line" >>"$script"
        fi
    done <<<"$pending"
    printf '%s\n' "$pending" >>"$script"
    pending=''
}
for document in "${documents[@]}"; do
    printf "printf '\\\\n## %s\\\\n'\n" "$document" >>"$script"
    kind='' place=''
    while IFS= read -r line; do
        case "$line" in
            '@@ run '* | '@@ set '*)
                read -r _ kind name number <<<"$line"
                place="$name:$number"
                ;;
            '@@ end')
                if [ -n "$pending" ]; then
                    echo "runbook.sh: an incomplete command at the end of the block at $place" >&2
                    exit 2
                fi
                kind=''
                ;;
            *)
                if [ "$kind" = set ]; then
                    printf "printf '%%s\\\\n' %q\n" "# set by runbook.sh: $line" >>"$script"
                    printf '%s\n' "$line" >>"$script"
                    continue
                fi
                pending=${pending:+$pending$'\n'}$line
                # A command is complete when the shell parses it and its last
                # line does not go on in the next.
                if [ "${line%\\}" = "$line" ] && bash -n <<<"$pending" 2>/dev/null; then
                    flush
                fi
                ;;
        esac
    done < <(extract "$document")
done
echo "printf '\\nrunbook.sh: every command ran\\n'" >>"$script"

echo "runbook.sh: GROVEDB_REF=$GROVEDB_REF WORK=$work, $(uname -sm)"
bash "$script"
