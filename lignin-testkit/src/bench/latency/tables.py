#!/usr/bin/env python3
"""The tables of a campaign of `campaign.sh`, as Markdown.

    tables.py OUT

OUT is the campaign's directory. Printed, in this order: the machine, what
the campaign was asked and the builds; for the runs of each part, a row for
every engine of every run and the median of the runs' values with their
range; the control and each experiment held against it; the root hash that
the runs of one stream ended at; and, for each traced run, what its measuring
thread waited for (`waits.py`).

It fails, and prints no table, where a report has another number of blocks
than the campaign was asked for, where a part of the campaign has some of
its runs and not all (every name of the part, under every background, in
every round, with both engines), and where two runs of one stream ended at
different root hashes: a file of tables is of one campaign that did what it
says.

A traced run that did not end well (`traced.sh` leaves a file that says it
did), that has no line for its measured blocks, or whose waits cannot be
placed gets no table of waits: its place says why, the tables of the runs
are printed all the same, and the script ends with 3.

A percentile is the smallest sample with that share of the samples at or
below it. A commit's time off a processor is its time less what its thread
ran, by the thread's own clock.

The rule an experiment is held to: it lowers the 99.9th percentile of
LigninDB's commit by more than two runs of one binary differ. Here that is
read as: the median of the experiment's runs is below the median of the
control's runs (the one binary under its names) by more than the control's
runs are apart, their highest value less their lowest, under one background.
The median of an even number of runs is the mean of the two middle ones. A
control of fewer than two runs, or an experiment of fewer than two, gets no
answer.
"""
import collections
import glob
import json
import math
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PARTS = ("its wait for the commit lock", "its wait for the lock on the counts of values",
         "its append to the log", "placing values after it", "reclaiming versions after it",
         "its wait for room in write-back")


def quantile(ordered, share):
    if not ordered:
        return 0
    wanted = math.ceil(len(ordered) * share)
    return ordered[min(max(wanted, 1), len(ordered)) - 1]


def four(samples):
    ordered = sorted(samples)
    return [quantile(ordered, 0.5), quantile(ordered, 0.99), quantile(ordered, 0.999),
            ordered[-1] if ordered else 0]


def us(nanos):
    return "%.0f" % (nanos / 1e3)


def median(values):
    """The median of the values of some runs: the middle one, or the mean of the two middle ones."""
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


def ranged(values):
    return "%s (%s to %s)" % (us(median(values)), us(min(values)), us(max(values)))


def read(directory):
    """A row for every engine of every report of a directory of runs."""
    rows = []
    for path in sorted(glob.glob(os.path.join(directory, "*.json"))):
        named = re.match(r"(.+)-([a-z]+)-r(\d+)\.json$", os.path.basename(path))
        if not named:
            continue
        report = json.load(open(path))
        if (report.get("label"), report["background"]) != (named.group(1), named.group(2)):
            sys.exit("tables.py: %s says of itself that it is %s under %s" % (
                path, report.get("label"), report["background"]))
        made_by = path[:-len(".json")] + ".build"
        binary = open(made_by).read().strip() if os.path.exists(made_by) else None
        for engine in report["engines"]:
            off = [max(commit - ran, 0) for commit, ran in zip(engine["commit_ns"], engine["ran_ns"])]
            clock, parts = engine["clock"], engine.get("parts_ns")
            rows.append(dict(
                label=named.group(1), background=named.group(2), round=int(named.group(3)),
                engine=engine["engine"], blocks=len(engine["commit_ns"]),
                commit=four(engine["commit_ns"]), off=four(off),
                append=four([part[2] for part in parts]) if parts else None,
                room=sum(1 for part in parts if part[5] > 0) if parts else None,
                turns=engine.get("turns"),
                late=[clock["late"][at] / max(clock["sleeps"], 1) for at in range(4)],
                root=engine["root_hash"], build=report["commit"], binary=binary,
                stream=(report["seed"], report["documents"], report["owners"],
                        report["warm_blocks"], len(engine["commit_ns"]))))
    return rows


def runs(rows):
    print("| Rows | Engine | Background | Round | Blocks | Median, µs | 99%, µs | 99.9%, µs | Longest, µs "
          "| Off a processor: 99%, µs | 99.9%, µs | longest, µs | Sleeps late by 1 ms or more | by 5 ms |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    order = lambda row: (row["background"], row["label"], row["engine"], row["round"])
    for row in sorted(rows, key=order):
        print("| %s | %s | %s | %d | %d | %s | %s | %s | %s | %s | %s | %s | %.2f%% | %.2f%% |" % (
            row["label"], row["engine"], row["background"], row["round"], row["blocks"],
            *map(us, row["commit"]), *map(us, row["off"][1:]),
            100 * row["late"][1], 100 * row["late"][2]))
    groups = collections.OrderedDict()
    for row in sorted(rows, key=order):
        groups.setdefault((row["label"], row["engine"], row["background"]), []).append(row)
    print("\nThe median of the runs' values, and their range:\n")
    print("| Rows | Engine | Background | Runs | Median, µs | 99%, µs | 99.9%, µs | Longest, µs "
          "| Off a processor: 99%, µs | 99.9%, µs | longest, µs |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for (label, engine, background), group in groups.items():
        cells = [ranged([row["commit"][at] for row in group]) for at in range(4)]
        cells += [ranged([row["off"][at] for row in group]) for at in range(1, 4)]
        print("| %s | %s | %s | %d | %s |" % (label, engine, background, len(group), " | ".join(cells)))
    lignin = [row for row in sorted(rows, key=order) if row["append"]]
    if lignin:
        print("\nLigninDB's own counts of a run: the commit's append to the log, the commits that "
              "waited for room in write-back, and the turns of write-back a block:\n")
        print("| Rows | Background | Round | Append: 99%, µs | 99.9%, µs | longest, µs | Waited for room | Turns a block |")
        print("| --- | --- | --- | --- | --- | --- | --- | --- |")
        for row in lignin:
            print("| %s | %s | %d | %s | %s | %s | %d | %.2f |" % (
                row["label"], row["background"], row["round"], *map(us, row["append"][1:]),
                row["room"], (row["turns"] or 0) / max(row["blocks"], 1)))


def held(rows, control, experiments):
    """Each experiment against the control, by the rule of the module's text."""
    print("| Background | Rows | Runs | 99.9% of LigninDB's commit, µs: median (range) | Below the control's median by, µs "
          "| The control's runs are apart by, µs | By more than that |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    lignin = [row for row in rows if row["engine"] == "lignindb"]
    for background in sorted({row["background"] for row in lignin}):
        of = lambda labels: sorted(row["commit"][2] for row in lignin
                                   if row["background"] == background and row["label"] in labels)
        base = of(control)
        if not base:
            continue
        apart, middle = base[-1] - base[0], median(base)
        print("| %s | %s (the control) | %d | %s | | %s | |" % (
            background, ", ".join(control), len(base), ranged(base), us(apart)))
        for label in experiments:
            values = of([label])
            if not values:
                continue
            below = middle - median(values)
            if len(base) < 2 or len(values) < 2:
                verdict = "too few runs to say"
            else:
                verdict = "yes" if below > apart else "no"
            print("| %s | %s | %d | %s | %s | %s | %s |" % (
                background, label, len(values), ranged(values), us(below), us(apart), verdict))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    main_rows = read(os.path.join(out, "runs", "main"))
    other_rows = read(os.path.join(out, "runs", "filesystems"))
    asked = {}
    asked_path = os.path.join(out, "campaign.txt")
    if os.path.exists(asked_path):
        for line in open(asked_path):
            for key in ("commit", "blocks", "rounds", "backgrounds", "trace blocks"):
                if line.startswith(key + " "):
                    asked[key] = line[len(key) + 1:].strip()
        if len(asked) < 5:
            sys.exit("tables.py: %s does not say what the campaign was asked" % asked_path)
    if "blocks" in asked:
        short = sorted({"%s-%s-r%d" % (row["label"], row["background"], row["round"])
                        for row in main_rows + other_rows if row["blocks"] != int(asked["blocks"])})
        if short:
            sys.exit("tables.py: the campaign was asked for %s blocks a run, and these reports have another "
                     "number: %s" % (asked["blocks"], ", ".join(short)))
    parts = (("main", main_rows, ("base", "base-again", "allocated", "covered")),
             ("filesystems", other_rows, ("ext4", "ext4-again", "xfs")))
    if asked:
        for part, rows, labels in parts:
            have = {(row["label"], row["background"], row["round"], row["engine"]) for row in rows}
            want = {(label, background, number, engine) for label in labels
                    for background in asked["backgrounds"].split()
                    for number in range(1, int(asked["rounds"]) + 1) for engine in ("lignindb", "rocksdb")}
            if have and have != want:
                sys.exit("tables.py: the runs of %s are not those the campaign was asked for; missing: %s; "
                         "not asked for: %s" % (part, ", ".join("%s-%s-r%d %s" % key for key in sorted(want - have))
                                                or "none", ", ".join("%s-%s-r%d %s" % key for key in sorted(have - want))
                                                or "none"))
    # The build of each name, and what `builds.txt` says the campaign's builds are.
    of_build = collections.defaultdict(lambda: "base", allocated="exp1", covered="exp2")
    builds = {}
    builds_path = os.path.join(out, "builds.txt")
    if os.path.exists(builds_path):
        for line in open(builds_path):
            found = re.match(r"(\w+): commit (\w+), sha256 (\w+)", line)
            if found:
                builds[found.group(1)] = (found.group(2), found.group(3))
    if asked and len(builds) < 3:
        sys.exit("tables.py: %s does not name the three builds of the campaign" % builds_path)
    for row in main_rows + other_rows if builds else []:
        if (row["build"], row["binary"]) != builds[of_build[row["label"]]]:
            sys.exit("tables.py: the report %s-%s-r%d was made by the commit %s and the binary %s, and the "
                     "campaign's build %s is %s and %s" % (
                         row["label"], row["background"], row["round"], row["build"], row["binary"],
                         of_build[row["label"]], *builds[of_build[row["label"]]]))
    streams = collections.defaultdict(set)
    for row in main_rows + other_rows:
        streams[row["stream"]].add(row["root"])
    for stream, roots in streams.items():
        if len(roots) > 1:
            sys.exit("tables.py: runs of one stream ended at different root hashes: " + ", ".join(sorted(roots)))
    waited, refused = [], []
    # The traced runs of a campaign, where it made any: those of
    # `campaign.sh`, by name.
    there = {os.path.basename(path).split(".")[0] for path in glob.glob(os.path.join(out, "traces", "*"))}
    wanted = []
    if there and asked:
        grounds = asked["backgrounds"].split()
        wanted = ["%s-%s" % (engine, ground) for ground in grounds for engine in ("lignindb", "rocksdb")]
        wanted += ["%s-%s%s" % (name, grounds[0], at) for name, at in (
            ("lignindb", "-every-thread"), ("allocated", ""), ("covered", ""), ("lignindb", "-on-ext4"),
            ("lignindb", "-on-ext4-lazytime"), ("lignindb", "-on-xfs"), ("allocated", "-on-ext4-lazytime"))]
    for name in sorted(set(wanted) | there):
        trace = os.path.join(out, "traces", name + ".trace")
        stem = trace[:-len(".trace")]
        if wanted and name not in wanted:
            refused.append((trace, "it is no traced run of this campaign"))
            continue
        if not (os.path.exists(stem + ".ok") and os.path.exists(trace) and os.path.exists(stem + ".json")):
            refused.append((trace, "the traced run was not made or did not end well: its trace, its report and "
                                   "the file .ok that `traced.sh` leaves last are not all there"))
            continue
        report = json.load(open(stem + ".json"))
        binary = open(stem + ".build").read().strip() if os.path.exists(stem + ".build") else None
        # The measuring thread that `traced.sh` found for the run is the
        # one whose line begins the measured blocks in the trace.
        thread = open(stem + ".pid").read().split()[-1] if os.path.exists(stem + ".pid") else None
        begun = next((re.search(r"tid=(\d+)", line).group(1) for line in open(trace, errors="replace")
                      if line.startswith("MARK") and "measured begin" in line), None)
        if thread is None or thread != begun:
            refused.append((trace, "the trace is not of this traced run: its measured blocks are begun by the "
                                   "thread %s, and the run's measuring thread was %s" % (begun, thread)))
            continue
        build = of_build[name.split("-")[0]]
        if asked and (report.get("label") != name or "-%s" % report["background"] not in name
                      or len(report["engines"][0]["commit_ns"]) != int(asked["trace blocks"])
                      or (report["commit"], binary) != builds[build]):
            refused.append((trace, "its report is not of this traced run of this campaign: it says %s under %s, "
                                   "%d blocks, commit %s, binary %s" % (
                                       report.get("label"), report["background"],
                                       len(report["engines"][0]["commit_ns"]), report["commit"], binary)))
            continue
        made = subprocess.run([sys.executable, os.path.join(HERE, "waits.py"), "--marked", trace],
                              capture_output=True, text=True)
        if made.returncode != 0:
            refused.append((trace, made.stderr.strip()))
            continue
        waited.append((trace, made.stdout.rstrip()))

    print("# The tables of PERF-07-F1\n")
    for title, name in (("The machine", "host.txt"), ("What the campaign was asked", "campaign.txt"),
                        ("The builds", "builds.txt")):
        path = os.path.join(out, name)
        if os.path.exists(path):
            print("## %s\n\n```text\n%s\n```\n" % (title, open(path).read().rstrip()))
    if main_rows:
        print("## The commit of a block, on the file system of the runs\n")
        runs(main_rows)
        print("\n### The control and the two experiments\n")
        held(main_rows, ["base", "base-again"], ["allocated", "covered"])
        print()
    if other_rows:
        print("## The commit of a block, on file systems in files of that one\n")
        runs(other_rows)
        print("\n### A second file system: XFS against ext4\n")
        held(other_rows, ["ext4", "ext4-again"], ["xfs"])
        print()
    if streams:
        print("## One stream, one root hash\n")
        for stream, roots in sorted(streams.items()):
            count = sum(1 for row in main_rows + other_rows if row["stream"] == stream)
            print("- %d measurements of seed %d, %d documents of %d owners, %d warm and %d measured blocks "
                  "ended at one root hash, `%s`" % (count, *stream, next(iter(roots))))
        print()
    if waited or refused:
        print("## What the measuring thread waited for, in the traced runs\n")
    for trace, why in refused:
        print("### %s\n\n**No table of this traced run:** %s\n" % (os.path.basename(trace)[:-len(".trace")], why))
    for trace, tables in waited:
        name = os.path.basename(trace)[:-len(".trace")]
        report_path = trace[:-len(".trace")] + ".json"
        print("### %s\n" % name)
        if os.path.exists(report_path):
            engine = json.load(open(report_path))["engines"][0]
            blocks = max(len(engine["commit_ns"]), 1)
            off = [max(commit - ran, 0) for commit, ran in zip(engine["commit_ns"], engine["ran_ns"])]
            print("%d blocks; the commit, µs: median %s, 99%% %s, 99.9%% %s, longest %s; off a processor: "
                  "99%% %s, 99.9%% %s, longest %s.\n" % (blocks, *map(us, four(engine["commit_ns"])),
                                                         *map(us, four(off)[1:])))
            if engine.get("syncs"):
                counted = sorted(engine["syncs"].items(), key=lambda item: -item[1][0])
                print("Syncs a block, counted by the harness's file layer, %.2f in all: %s.\n" % (
                    sum(count for _, (count, _) in counted) / blocks,
                    ", ".join("%.2f of %s" % (count / blocks, of) for of, (count, _) in counted)))
        print(tables)
        print()
    if refused:
        for trace, why in refused:
            print("tables.py: no table of %s: %s" % (trace, why), file=sys.stderr)
        sys.exit(3)


main()
