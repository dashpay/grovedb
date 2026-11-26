#!/usr/bin/env python3
"""What the measuring thread of `lignin-bench latency` waited for, from a trace
of `trace.bt` beside the run.

    waits.py TRACE [MIN_US]

MIN_US is the shortest wait that is counted, 1000 by default.

**Which waits are counted.** A wait is one OFFCPU record of the measuring
thread: the thread left a processor and was back after the time the record
says. Only the waits of the measured blocks are counted: those between the
harness's lines `lignin-mark measured begin` and `lignin-mark measured end`,
which the trace has as MARK records. The load and the warm blocks, which the
same thread applies through the same calls, are left out. A wait with no user
stack cannot be given a place, the commit or the operations before it, and is
counted apart; so is a wait of the harness between two blocks, where it reads
the engine's counts, and a wait in which the thread was taken off a
processor while it could run.

Printed:

1. What was counted, and the commits of each ext4 journal during the measured
   blocks.
2. Every counted wait, by where it was and by what the thread waited for: the
   system call it was in and the place in the kernel it left a processor at,
   or the lock of the process it asked for.
3. For the waits inside the kernel during a write to a file of LigninDB's
   log, two things that are not the same. First, the syncs of the process
   that had a moment in common with the wait, by what they are of: that is
   an overlap in time, and a wait can have several. Beside it is what chance
   gives: how many of the waits would have had a sync beside them had each
   begun at a moment of the measured blocks taken at random. Second, for a wait
   that is for a commit of an ext4 journal by its kernel stack (the journal
   is writing the block, or its transaction is being closed), the commit of
   that journal that was under way when the wait began, and the syncs of the
   process that waited for that commit (the SYNC record names the
   transaction it waited for, and JSTART and JEND say when its commit ran).
   A wait for room in the journal is for no commit, and is counted apart.
4. With a trace that follows every thread (`THREADS=all traced.sh`): for the
   waits for a lock of the process, the thread that woke the measuring
   thread and what that thread was off a processor for meanwhile. The thread
   that wakes is the one that gave the lock up last; the table does not say
   who held the lock.

A trace of an earlier form of the tracer has no line for the measured blocks.
Its waits are then taken by their user stack: the measured blocks are applied
inside `std::thread::scope` and the others are not.
"""
import collections
import re
import sys

# Frames that say only that the thread slept.
SLEEP = re.compile(r"^(__schedule|schedule|io_schedule|schedule_timeout|io_schedule_timeout|"
                   r"schedule_preempt_disabled|bit_wait|bit_wait_io|__wait_on_bit|"
                   r"out_of_line_wait_on_bit|__wait_for_common|wait_for_completion.*)$")
SYSCALL = re.compile(r"^__(?:arm64|x64|ia32)_sys_(\w+)$")
# Where a thread waits for a commit of the journal: for the commit's write
# of a block, and for a transaction that the commit is closing.
FOR_A_COMMIT = ("do_get_write_access", "wait_transaction_locked")
FOR_ROOM = "__jbd2_log_wait_for_space"
# The states of a thread that left a processor while it could run: running,
# and what the tracepoint reports for a preempted thread (TASK_REPORT_MAX).
RUNNABLE = ("0", "256")
ALLOCATOR = "the lock of the C library's allocator"
ENGINE = "a lock of the engine"


def field(line, name):
    found = re.search(r"\b%s=(\S+)" % name, line)
    return found.group(1) if found else None


def frames(lines):
    out = []
    for line in lines:
        found = re.match(r"\s+[0-9a-f]+ (\S+?)\+\d+", line)
        if found and (not out or out[-1] != found.group(1)):
            out.append(found.group(1))
    return out


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


def by_chance(waits, syncs, window):
    """How many of the waits would have a sync beside them had each begun at a
    moment of the window taken at random, and the share of the window's time
    at which a sync was under way."""
    lo, hi = window
    union = []
    for begin, end in sorted((max(sync["begin"], lo), min(sync["end"], hi)) for sync in syncs):
        if end <= begin:
            continue
        if union and begin <= union[-1][1]:
            union[-1][1] = max(union[-1][1], end)
        else:
            union.append([begin, end])
    under_way = sum(end - begin for begin, end in union)
    # A wait of some length has a sync beside it if it begins during one, or
    # less than its length before one: the time between two syncs counts up
    # to that length.
    gaps = [after[0] - before[1] for before, after in zip([[lo, lo]] + union, union)]
    expected = sum(min((under_way + sum(min(gap, wait["end"] - wait["begin"]) for gap in gaps)) / (hi - lo), 1)
                   for wait in waits)
    return expected, under_way / (hi - lo)


class Trace:
    def __init__(self, path):
        self.waits, self.syncs, self.marks = [], [], []
        self.starts, self.ends = {}, {}
        record, part = None, None
        for line in open(path, errors="replace"):
            line = line.rstrip("\n")
            if line.startswith("OFFCPU"):
                thread = re.search(r"thread=(.*)/(\d+) woken_by=(.*)/(\d+)$", line)
                # An earlier form of the tracer followed the measuring thread
                # alone and did not name it.
                waker = thread or re.search(r"()()woken_by=(.*)/(\d+)$", line)
                record = dict(begin=int(field(line, "begin")), end=int(field(line, "end")),
                              us=int(field(line, "us")), state=field(line, "state"),
                              dev=field(line, "dev"), tid=int(thread.group(2)) if thread else 0,
                              waker=waker.group(3), waker_tid=int(waker.group(4)), kernel=[], user=[])
                self.waits.append(record)
                part = None
            elif record is not None and line.startswith(" left at:"):
                part = "kernel"
            elif record is not None and line.startswith(" called from:"):
                part = "user"
            elif record is not None and line.startswith(" woken at:"):
                part = None
            elif line.startswith("SYNC"):
                self.syncs.append(dict(begin=int(field(line, "begin")), end=int(field(line, "end")),
                                       us=int(field(line, "us")), file=field(line, "file"),
                                       dev=field(line, "dev"), transaction=field(line, "transaction")))
                record = None
            elif line.startswith("MARK"):
                self.marks.append((int(field(line, "at")), int(field(line, "tid") or 0),
                                   line.split("lignin-mark ", 1)[-1]))
                record = None
            elif line.startswith("JSTART"):
                self.starts[(field(line, "dev"), field(line, "transaction"))] = int(field(line, "at"))
            elif line.startswith("JEND"):
                self.ends[(field(line, "dev"), field(line, "transaction"))] = int(field(line, "at"))
            elif re.match(r"(WRITE|JBD2|BLK)", line):
                record = None
            elif record is not None and part:
                record[part].append(line)
        for wait in self.waits:
            wait["kernel"], wait["user"] = frames(wait["kernel"]), frames(wait["user"])
        begins = [(at, tid) for at, tid, what in self.marks if what.startswith("measured begin")]
        ends = [at for at, _, what in self.marks if what.startswith("measured end")]
        # One engine a trace: its measured blocks are one stretch.
        self.measured = (begins[0][0], ends[0]) if begins and ends else None
        self.measuring = begins[0][1] if begins else 0

    def of_the_measuring_thread(self, wait):
        return not self.measuring or not wait["tid"] or wait["tid"] == self.measuring

    def in_the_measured_blocks(self, wait):
        if self.measured:
            return self.measured[0] <= wait["begin"] and wait["end"] <= self.measured[1]
        return any("scoped5scope" in frame or "scoped::scope" in frame for frame in wait["user"])


def in_a_block(wait):
    return any("latency5block" in frame or "latency::block" in frame for frame in wait["user"])


def in_the_commit(wait):
    return any("5Block6commit" in frame or "commit_transaction" in frame
               or "Block>::commit" in frame for frame in wait["user"])


def in_the_log(wait):
    return any("lignin_store3wal" in frame or "lignin_store::wal" in frame for frame in wait["user"])


def what(wait):
    """What a wait was for, in words."""
    kernel, user = wait["kernel"], wait["user"]
    if wait["state"] in RUNNABLE:
        return "taken off the processor while it could run"
    call = next((found.group(1) for found in map(SYSCALL.match, kernel) if found), "?")
    if any("futex" in frame for frame in kernel):
        if any(frame.startswith("__lll_lock_wait_private") for frame in user[:2]):
            return ALLOCATOR
        if any("wait_for_room" in frame or "wait_for_turn" in frame for frame in user):
            return "a turn of write-back, for room"
        return ENGINE
    if "file_modified" in kernel or "kiocb_modified" in kernel or "touch_atime" in kernel:
        asked = "the file's times"
    elif "ext4_da_write_end" in kernel or "ext4_write_end" in kernel:
        asked = "the file's length"
    else:
        asked = "a change of the file"
    if "do_get_write_access" in kernel:
        return "%s: ext4 writes %s to the journal, which is writing that block of inodes" % (call, asked)
    if "__jbd2_log_wait_for_space" in kernel:
        return "%s: ext4 writes %s to the journal, which has no room, and the thread makes room" % (call, asked)
    if "wait_transaction_locked" in kernel:
        return "%s: ext4 writes %s to the journal, whose transaction is being closed" % (call, asked)
    rest = [frame for frame in kernel if not SLEEP.match(frame)]
    if "xfs_ilock" in rest and asked == "the file's times":
        return "%s: XFS stamps %s, and waits for the lock of the inode (xfs_ilock)" % (call, asked)
    return "%s: %s" % (call, " < ".join(rest[:3]))


def of(path):
    """What a synced file is, by its directory."""
    directory, _, name = path.partition("/")
    if name == "MANIFEST.tmp":
        return "the manifest's temporary file"
    kinds = {"wal": "a log file", "val": "a value-log file", "seg": "a segment", "table": "a table checkpoint"}
    if directory in kinds:
        return kinds[directory]
    return "the names of %s/" % name if name in kinds else "the names of the database directory"


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    floor = int(sys.argv[2]) if len(sys.argv) > 2 else 1000
    trace = Trace(sys.argv[1])
    long = [wait for wait in trace.waits if wait["us"] >= floor and trace.of_the_measuring_thread(wait)]
    if trace.measured:
        seconds = (trace.measured[1] - trace.measured[0]) / 1e9
        print("The measured blocks took %.0f s in the trace. Waits of the measuring thread of %.1f ms or more: "
              "%d in the measured blocks and %d before or after them, which are not counted." % (
                  seconds, floor / 1e3, sum(1 for wait in long if trace.in_the_measured_blocks(wait)),
                  sum(1 for wait in long if not trace.in_the_measured_blocks(wait))))
        outside = [wait for wait in long if trace.in_the_measured_blocks(wait) and not in_a_block(wait)]
        unplaced = [wait for wait in outside if not wait["user"]]
        between = [wait for wait in outside if wait["user"]]
        print("Of those in the measured blocks, %d (%.1f ms in all) have no user stack, so no place can be given "
              "them, and %d (%.1f ms) are of the harness between two blocks, where it reads the engine's counts. "
              "Neither is in a row below." % (len(unplaced), sum(wait["us"] for wait in unplaced) / 1e3,
                                              len(between), sum(wait["us"] for wait in between) / 1e3))
        for dev in sorted({dev for dev, _ in trace.starts}):
            took = [trace.ends[key] - at for key, at in trace.starts.items()
                    if key[0] == dev and key in trace.ends and trace.measured[0] <= at <= trace.measured[1]]
            if took:
                print("The journal of the file system %s committed %d times in the measured blocks, %.1f a second; "
                      "a commit took a median of %.2f ms and %.1f ms at most." % (
                          dev, len(took), len(took) / seconds, median(took) / 1e6, max(took) / 1e6))
    else:
        print("The trace has no line for the measured blocks: its waits are taken by their user stack. "
              "%d waits of %.1f ms or more have no user stack and are in no row below." % (
                  sum(1 for wait in long if not wait["user"]), floor / 1e3))
    mine = [wait for wait in long if trace.in_the_measured_blocks(wait) and in_a_block(wait)]
    # The time of the measured blocks: between the harness's two lines, or
    # for an earlier trace from the first of their waits to the last.
    counted = [wait for wait in trace.waits
               if trace.of_the_measuring_thread(wait) and trace.in_the_measured_blocks(wait)]
    span = trace.measured or (min((wait["begin"] for wait in counted), default=0),
                              max((wait["end"] for wait in counted), default=1))

    total, count, longest = collections.Counter(), collections.Counter(), collections.Counter()
    for wait in mine:
        key = ("the commit" if in_the_commit(wait) else "the operations", what(wait))
        total[key] += wait["us"]
        count[key] += 1
        longest[key] = max(longest[key], wait["us"])
    print("\n| Where | What the thread waited for | Waits of %.1f ms or more | In all, ms | Longest, ms |" % (floor / 1e3))
    print("| --- | --- | --- | --- | --- |")
    for key, us in sorted(total.items(), key=lambda item: (item[0][0], -item[1], item[0])):
        print("| %s | %s | %d | %.1f | %.1f |" % (key[0], key[1], count[key], us / 1e3, longest[key] / 1e3))

    appends = [wait for wait in mine if in_the_log(wait) and "vfs_write" in wait["kernel"]
               and wait["state"] not in RUNNABLE]
    print("\n%d waits inside the kernel during a write to a file of the log, %.1f ms in all." %
          (len(appends), sum(wait["us"] for wait in appends) / 1e3))
    beside_kind, with_none, longest_beside = collections.Counter(), 0, []
    for wait in appends:
        beside = [sync for sync in trace.syncs if sync["end"] > wait["begin"] and sync["begin"] < wait["end"]]
        with_none += not beside
        for kind in {of(sync["file"]) for sync in beside}:
            beside_kind[kind] += 1
        if beside:
            longest_beside.append(max(sync["us"] for sync in beside))
    if appends:
        print("\nBy time alone: %d of them had no listed sync of the process under way at any moment of the wait. "
              "The others, by what a sync beside them was of:\n" % with_none)
        print("| A sync with a moment in common with the wait, of | Waits |\n| --- | --- |")
        for kind, waits_beside in sorted(beside_kind.items(), key=lambda item: (-item[1], item[0])):
            print("| %s | %d |" % (kind, waits_beside))
        if longest_beside:
            print("\nThe longest sync beside a wait took a median of %.1f ms, and %.1f ms at most." %
                  (median(longest_beside) / 1e3, max(longest_beside) / 1e3))
        expected, under_way = by_chance(appends, trace.syncs, span)
        print("\nWhat chance gives: a listed sync was under way for %.0f%% of the time of the measured blocks, and "
              "had each of these waits begun at a moment of them taken at random, %.1f of the %d would have had "
              "one beside it, where %d had." % (100 * under_way, expected, len(appends), len(appends) - with_none))
    for_a_commit = [wait for wait in appends if any(frame in wait["kernel"] for frame in FOR_A_COMMIT)]
    for_room = sum(1 for wait in appends if FOR_ROOM in wait["kernel"])
    if appends and trace.starts:
        for_kind, in_a_commit, with_a_sync = collections.Counter(), 0, 0
        for wait in for_a_commit:
            under_way = {key for key, at in trace.starts.items()
                         if key[0] == wait["dev"] and at <= wait["begin"] < trace.ends.get(key, wait["begin"] + 1)}
            in_a_commit += bool(under_way)
            waited_for = [sync for sync in trace.syncs if (sync["dev"], sync["transaction"]) in under_way]
            with_a_sync += bool(waited_for)
            for kind in {of(sync["file"]) for sync in waited_for}:
                for_kind[kind] += 1
        print("\nBy the journal's transactions: %d of the %d waits were for a commit of an ext4 journal by their "
              "kernel stack, and %d were for room in the journal, which is for no commit. Of the %d, %d began "
              "while a commit of the journal of the written file's file system was under way, and for %d of "
              "them a sync of the process waited for that commit. By what those syncs were of:\n" % (
                  len(for_a_commit), len(appends), for_room, len(for_a_commit), in_a_commit, with_a_sync))
        print("| A sync that waited for the commit that the wait began in, of | Waits |\n| --- | --- |")
        for kind, waits_for in sorted(for_kind.items(), key=lambda item: (-item[1], item[0])):
            print("| %s | %d |" % (kind, waits_for))

    if len({wait["tid"] for wait in trace.waits}) <= 1:
        return
    by_thread = collections.defaultdict(list)
    for wait in trace.waits:
        by_thread[wait["tid"]].append(wait)
    woke, woke_us = collections.Counter(), collections.Counter()
    for wait in mine:
        asked = what(wait)
        if asked not in (ALLOCATOR, ENGINE):
            continue
        meanwhile = [other for other in by_thread.get(wait["waker_tid"], [])
                     if other["end"] > wait["begin"] and other["begin"] < wait["end"] and other["us"] >= 200]
        overlap = lambda other: min(other["end"], wait["end"]) - max(other["begin"], wait["begin"])
        doing = what(max(meanwhile, key=overlap)) if meanwhile else "nothing: it ran, or was kept from running"
        key = (asked, wait["waker"] or "an interrupt", doing)
        woke[key] += 1
        woke_us[key] += wait["us"]
    print("\n| The thread waited for | Woken by | What that thread was off a processor for meanwhile | Waits | In all, ms |")
    print("| --- | --- | --- | --- | --- |")
    for key, us in sorted(woke_us.items(), key=lambda item: (item[0][0], -item[1], item[0])):
        print("| %s | %s | %s | %d | %.1f |" % (key[0], key[1], key[2], woke[key], us / 1e3))


main()
