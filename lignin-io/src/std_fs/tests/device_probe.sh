#!/bin/bash
# What a synced set leaves on its device: the experiment that
# device_probe.rs describes. macOS only, and no root is needed.
#
#   device_probe.sh FILE_SYSTEM WAY [WHOLE]
#
# FILE_SYSTEM is what hdiutil makes: APFS, HFS+J. WAY is one of together,
# alone, shared, handed, none. With a third argument the step that checks
# fails unless everything is there.
set -euo pipefail
file_system=${1:?the file system of the image: APFS or HFS+J}
way=${2:?how the changes are synced: together, alone, shared, handed, none}
whole=${3:-}

cd "$(dirname "$0")/../../../.."
test_name=std_fs::tests::device_probe::probe
binary=$(cargo test -p lignin-io --lib --no-run 2>&1 | sed -n 's/.*(\(.*\)).*/\1/p' | tail -1)
work=$(mktemp -d "${TMPDIR:-/tmp}/device-probe.XXXXXX")
attached=""
cleanup() {
    [[ -n $attached ]] && hdiutil detach "$attached" -force -quiet || true
    rm -rf "$work"
}
trap cleanup EXIT

# Runs one step, shows what it says, and ends as the step ended.
step() {
    local ended=0
    LIGNIN_IO_PROBE=$1 LIGNIN_IO_PROBE_DIR=$2 \
        LIGNIN_IO_PROBE_IMAGE=$work/image.dmg LIGNIN_IO_PROBE_COPY=$work/copy.dmg \
        "$binary" --ignored --exact "$test_name" --nocapture --test-threads=1 \
        >"$work/said" 2>&1 || ended=$?
    grep -o "probe: .*" "$work/said" || true
    if ((ended != 0)); then
        grep "panicked\|assertion" -A3 "$work/said" || true
    fi
    return "$ended"
}

hdiutil create -size 64m -fs "$file_system" -volname probe -type UDIF -quiet "$work/image.dmg"
mkdir "$work/mounted" "$work/copied"
hdiutil attach -nobrowse -quiet -mountpoint "$work/mounted" "$work/image.dmg"
attached=$work/mounted
step base "$work/mounted"
step "$way" "$work/mounted"
hdiutil detach "$work/mounted" -force -quiet
attached=""

# Not read-only: a journal that was cut off is played back at the mount.
hdiutil attach -nobrowse -quiet -mountpoint "$work/copied" "$work/copy.dmg" || {
    echo "$file_system, synced as $way: the copy of the image does not mount"
    exit 1
}
attached=$work/copied
echo "$file_system, synced as: $way"
if [[ -n $whole ]]; then
    export LIGNIN_IO_PROBE_WHOLE=1
fi
step check "$work/copied"
