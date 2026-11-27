#!/usr/bin/env bash
# pairs.sh OUT ROUNDS SLOTS
#
# Latency runs, two at a time, in rounds: for a machine that other work
# shares, where two runs made one after the other are not made under the
# same conditions.
#
# SLOTS has a line for each pair, two entries with `;` between them, each
# `label|build|engines|settings`: the name the rows get, the build under
# $BUILDS (as `build.sh` leaves it), the engines in the order they are
# measured (`rocksdb`, `lignindb`, or both with a comma between), and
# settings of the environment for the run (or `none`). A pair of one build
# under two labels is a control.
#
# The two runs of a pair are started together, each on a file system of its
# own ($FS_A and $FS_B), and the pair has ended when both have: whatever the
# machine does at a moment, it does beside both. From round to round the
# two exchange their file systems, unless SWAP=0, and the order of the slots
# is turned by one. In each round every slot is run under each background of
# $BACKGROUNDS (default: `default heavy`). $LATENCY holds further options of
# `lignin-bench latency`.
#
# A pair whose reports are there already is not run again, so a campaign
# that was stopped goes on where it was. A run that fails ends the campaign. Leaves OUT/builds.txt (each build
# with its commit and sha256), a report and its text for every run with the
# mount of its file system beside it, OUT/progress.log, and OUT/done at the
# end.
set -uo pipefail
out=$1 rounds=$2 slots=$3
root=${BUILDS:-$HOME/builds}
fs_a=${FS_A:?the first file system} fs_b=${FS_B:?the second file system}
# The two runs of a pair each remove their scratch directory, and are held
# against one another as runs on two file systems.
if [ "$fs_a" -ef "$fs_b" ]; then
  echo "pairs.sh: FS_A ($fs_a) and FS_B ($fs_b) are one directory: the two runs of a pair want one each" >&2
  exit 1
fi
mkdir -p "$out"; rm -f "$out/done"; ulimit -n 65536
mapfile -t lines < <(grep -v -E '^(#|$)' "$slots")
for line in "${lines[@]}"; do
  for entry in "${line%%;*}" "${line##*;}"; do
    IFS='|' read -r label build _ _ <<< "$entry"
    echo "$label $build $(cat "$root/$build/commit") sha256 $(sha256sum "$root/$build/lignin-bench" | cut -d' ' -f1)"
  done
done | sort -u > "$out/builds.txt"

one() { # one ENTRY FS FILE BACKGROUND
  local label build engines settings flags="" engine
  IFS='|' read -r label build engines settings <<< "$1"
  [ "$settings" = none ] && settings=""
  for engine in ${engines//,/ }; do flags="$flags --engine $engine"; done
  rm -rf "${2:?}/scratch"
  findmnt -no SOURCE,FSTYPE,OPTIONS -T "$2" > "$3.mount"
  # shellcheck disable=SC2086
  env $settings "$root/$build/lignin-bench" latency --scratch "$2/scratch" $flags \
    --background "$4" --label "$label" --commit "$(cat "$root/$build/commit")" \
    ${LATENCY:-} --out "$3.json.part" > "$3.txt" 2> "$3.err"
  local status=$?
  [ "$status" = 0 ] && mv "$3.json.part" "$3.json"
  rm -rf "${2:?}/scratch"
  return "$status"
}

for round in $(seq 1 "$rounds"); do
  for background in ${BACKGROUNDS:-default heavy}; do
    for index in $(seq 0 $((${#lines[@]} - 1))); do
      slot=$(((index + round - 1) % ${#lines[@]}))
      line=${lines[$slot]}
      first=${line%%;*} second=${line##*;}
      if [ "${SWAP:-1}" = 0 ] || [ $((round % 2)) = 1 ]; then one_fs=$fs_a two_fs=$fs_b
      else one_fs=$fs_b two_fs=$fs_a; fi
      one_file="$out/${first%%|*}-$background-r$round-s$slot-$(basename "$one_fs")"
      two_file="$out/${second%%|*}-$background-r$round-s$slot-$(basename "$two_fs")"
      [ -s "$one_file.json" ] && [ -s "$two_file.json" ] && continue
      sudo fstrim "$one_fs" 2>/dev/null; sudo fstrim "$two_fs" 2>/dev/null
      one "$first" "$one_fs" "$one_file" "$background" &
      one_run=$!
      one "$second" "$two_fs" "$two_file" "$background" &
      two_run=$!
      failed=""
      wait "$one_run" || failed="$one_file"
      wait "$two_run" || failed="$failed $two_file"
      if [ -n "$failed" ]; then
        echo "pairs.sh: a run of the pair failed:$failed" >&2
        exit 1
      fi
      echo "$(date -u +%FT%TZ) round $round $background slot $slot: $(grep -h -E '^\| [^B-]' "$one_file.txt" "$two_file.txt" | tr '\n' ' ')" >> "$out/progress.log"
    done
  done
done
date -u +%FT%TZ > "$out/done"
