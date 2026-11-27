#!/usr/bin/env python3
"""The device reads of one run by the kind of file they read, from what read-split.bt printed.

usage: read-split.py BPFTRACE-OUTPUT OPERATIONS

BPFTRACE-OUTPUT holds lines `@calls[DIRECTORY, FILE]: N` and `@pages[DIRECTORY, FILE]: N`.
A file is classed by the directory it is in, for the directories of a LigninDB database (`val`:
the value log; `seg`: the segments, which hold page images and delta runs; `wal`: the log;
`table`: the table checkpoints), and else by its suffix, for RocksDB (`sst`, `log`). Every other
file is `other`, and one of those is named when it has more than a hundredth of the calls.

Prints, for each kind: how many files were read, the calls and the pages, and the calls and the
bytes (pages of 4,096 bytes) for one of OPERATIONS, the number of transitions or reads of the run.
A call is one time ext4 was asked to fill the page cache; the device may be handed fewer
requests, since neighbouring ones are merged.
"""
import collections
import re
import sys


def main():
    output, operations = sys.argv[1], int(sys.argv[2])
    totals = collections.defaultdict(lambda: [0, 0])
    files = collections.defaultdict(set)
    others = collections.Counter()
    pattern = re.compile(r"@(calls|pages)\[(.*), (.*)\]: (\d+)$")
    with open(output) as lines:
        for line in lines:
            match = pattern.match(line.rstrip("\n"))
            if not match:
                continue
            what, directory, name = match.group(1), match.group(2), match.group(3)
            count = int(match.group(4))
            if directory in ("val", "seg", "wal", "table"):
                kind = directory
            elif name.endswith(".sst"):
                kind = "sst"
            elif name.endswith(".log"):
                kind = "log"
            else:
                kind = "other"
                if what == "calls":
                    others[directory + "/" + name] += count
            totals[kind][0 if what == "calls" else 1] += count
            files[kind].add((directory, name))
    all_calls = sum(row[0] for row in totals.values())
    all_pages = sum(row[1] for row in totals.values())
    print(f"{operations} operations")
    print("%-8s %7s %12s %12s %14s %14s %7s" % (
        "kind", "files", "calls", "pages", "calls/op", "bytes/op", "share"))
    for kind in sorted(totals, key=lambda kind: -totals[kind][0]):
        calls, pages = totals[kind]
        print("%-8s %7d %12d %12d %14.2f %14.0f %6.1f%%" % (
            kind, len(files[kind]), calls, pages, calls / operations,
            pages * 4096 / operations, 100 * calls / max(all_calls, 1)))
    print("%-8s %7s %12d %12d %14.2f %14.0f" % (
        "all", "", all_calls, all_pages, all_calls / operations, all_pages * 4096 / operations))
    for name, calls in others.most_common(8):
        if calls * 100 >= all_calls:
            print(f"other: {name}: {calls} calls")


if __name__ == "__main__":
    main()
