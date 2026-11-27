#!/usr/bin/env python3
"""The device reads of one run by the kind of file they read, from what read-split.bt printed.

usage: read-split.py BPFTRACE-OUTPUT DIVISOR PAGE-BYTES

BPFTRACE-OUTPUT holds lines `@calls[DIRECTORY, FILE]: N` and `@pages[DIRECTORY, FILE]: N`, and
`@attached: 1`, without which it is not the output of probes that ran to their end.
A file is classed by the directory it is in, for the directories of a LigninDB database (`val`:
the value log; `seg`: the segments, which hold page images and delta runs; `wal`: the log;
`table`: the table checkpoints), and else by its suffix, for RocksDB (`sst`, `log`). Every other
file is `other`, and one of those is named when it has more than a hundredth of the calls.

Prints, for each kind: how many files were read, the calls and the pages, and the calls and the
bytes (pages of PAGE-BYTES, the system's page size) divided by DIVISOR. The counts are of the
whole command that ran under the probes, so DIVISOR is whatever that command's reads are to be
set against: the transitions of a call that does nothing else that reads, or 1 for the totals.
A call is one time ext4 was asked to fill the page cache, and the pages are what it was asked
for; neither is a count of the device's requests.
"""
import collections
import re
import sys


def main():
    output, operations, page_bytes = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    if operations <= 0 or page_bytes <= 0:
        sys.exit("read-split.py: the divisor and the page size are positive numbers")
    attached = False
    totals = collections.defaultdict(lambda: [0, 0])
    files = collections.defaultdict(set)
    others = collections.Counter()
    pattern = re.compile(r"@(calls|pages)\[(.*), (.*)\]: (\d+)$")
    with open(output) as lines:
        for line in lines:
            attached = attached or line.strip() == "@attached: 1"
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
    if not attached:
        sys.exit(f"read-split.py: {output} is not the output of probes that ran to their end")
    all_calls = sum(row[0] for row in totals.values())
    all_pages = sum(row[1] for row in totals.values())
    print(f"divided by {operations}; pages of {page_bytes} bytes")
    print("%-8s %7s %12s %12s %14s %14s %7s" % (
        "kind", "files", "calls", "pages", "calls/div", "bytes/div", "share"))
    for kind in sorted(totals, key=lambda kind: -totals[kind][0]):
        calls, pages = totals[kind]
        print("%-8s %7d %12d %12d %14.2f %14.0f %6.1f%%" % (
            kind, len(files[kind]), calls, pages, calls / operations,
            pages * page_bytes / operations, 100 * calls / max(all_calls, 1)))
    print("%-8s %7s %12d %12d %14.2f %14.0f" % (
        "all", "", all_calls, all_pages, all_calls / operations,
        all_pages * page_bytes / operations))
    for name, calls in others.most_common(8):
        if calls * 100 >= all_calls:
            print(f"other: {name}: {calls} calls")


if __name__ == "__main__":
    main()
