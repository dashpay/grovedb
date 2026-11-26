#!/usr/bin/env python3
"""What the measuring thread of `lignin-bench latency` waited for, from a trace
of `trace.bt` beside the run.

    waits.py TRACE [MIN_US]

MIN_US is the shortest wait that is counted, 1000 by default. Three tables:

1. Every wait of the measuring thread inside a block, by where it was (the
   commit, or the operations before it) and by what the thread waited for:
   the system call it was in and the place in the kernel it left a processor
   at, or the lock of the process it asked for.
2. For the waits inside the file system during a write to a file of
   LigninDB's log: which syncs of the process were under way at a moment of
   the wait, by what they are of.
3. For the waits for a lock of the process: which thread woke the measuring
   thread, and what that thread was off a processor for meanwhile. This table
   needs a trace that follows every thread (`THREADS=all traced.sh`), and
   is left out of one that follows the measuring thread alone.

A wait is one OFFCPU record: the thread left a processor, in a state other
than runnable, and was back after the time the record says. A record of a
thread that was taken off a processor while it could run is counted apart.
"""
import collections
import re
import sys

# Frames that say only that the thread slept.
SLEEP = re.compile(r"^(__schedule|schedule|io_schedule|schedule_timeout|io_schedule_timeout|"
                   r"schedule_preempt_disabled|bit_wait|bit_wait_io|__wait_on_bit|"
                   r"out_of_line_wait_on_bit|__wait_for_common|wait_for_completion.*)$")
SYSCALL = re.compile(r"^__(?:arm64|x64|ia32)_sys_(\w+)$")


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


def read(path):
    """The OFFCPU and SYNC records of a trace."""
    waits, syncs, record, part = [], [], None, None
    for line in open(path, errors="replace"):
        line = line.rstrip("\n")
        if line.startswith("OFFCPU"):
            thread = re.search(r"thread=(.*)/(\d+) woken_by=(.*)/(\d+)$", line)
            # A trace of an earlier form of the tracer has no thread: it
            # followed the measuring thread alone.
            waker = thread or re.search(r"()()woken_by=(.*)/(\d+)$", line)
            record = dict(begin=int(field(line, "begin")), end=int(field(line, "end")),
                          us=int(field(line, "us")), state=field(line, "state"),
                          tid=int(thread.group(2)) if thread else 0,
                          waker=waker.group(3), waker_tid=int(waker.group(4)),
                          kernel=[], user=[])
            waits.append(record)
            part = None
        elif record is not None and line.startswith(" left at:"):
            part = "kernel"
        elif record is not None and line.startswith(" called from:"):
            part = "user"
        elif record is not None and line.startswith(" woken at:"):
            part = None
        elif line.startswith("SYNC"):
            syncs.append((int(field(line, "begin")), int(field(line, "end")),
                          int(field(line, "us")), field(line, "file")))
            record = None
        elif re.match(r"(WRITE|MARK|JBD2|BLK)", line):
            record = None
        elif record is not None and part:
            record[part].append(line)
    for wait in waits:
        wait["kernel"], wait["user"] = frames(wait["kernel"]), frames(wait["user"])
    return waits, syncs


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
    if wait["state"] == "0":
        return "taken off the processor while it could run"
    call = next((found.group(1) for found in map(SYSCALL.match, kernel) if found), "?")
    if any("futex" in frame for frame in kernel):
        if any(frame.startswith("__lll_lock_wait_private") for frame in user[:2]):
            return "the lock of the C library's allocator"
        if any("wait_for_room" in frame or "wait_for_turn" in frame for frame in user):
            return "a turn of write-back, for room"
        return "a lock of the engine"
    asked = "the file's length" if "ext4_da_write_end" in kernel or "ext4_write_end" in kernel else "the file's times"
    if any(frame == "do_get_write_access" for frame in kernel):
        return "%s: ext4 writes %s to the journal, which is writing that block of inodes" % (call, asked)
    if any(frame == "__jbd2_log_wait_for_space" for frame in kernel):
        return "%s: ext4 writes %s to the journal, which has no room, and the thread makes room" % (call, asked)
    if any(frame == "wait_transaction_locked" for frame in kernel):
        return "%s: ext4 writes %s to the journal, whose transaction is being closed" % (call, asked)
    rest = [frame for frame in kernel if not SLEEP.match(frame)]
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
    waits, syncs = read(sys.argv[1])
    whole = len({wait["tid"] for wait in waits}) > 1
    mine = [wait for wait in waits if wait["us"] >= floor and in_a_block(wait)]

    total, count, longest = collections.Counter(), collections.Counter(), collections.Counter()
    for wait in mine:
        key = ("the commit" if in_the_commit(wait) else "the operations", what(wait))
        total[key] += wait["us"]
        count[key] += 1
        longest[key] = max(longest[key], wait["us"])
    print("| Where | What the thread waited for | Waits of %.1f ms or more | In all, ms | Longest, ms |" % (floor / 1e3))
    print("| --- | --- | --- | --- | --- |")
    for key, us in sorted(total.items(), key=lambda item: (item[0][0], -item[1])):
        print("| %s | %s | %d | %.1f | %.1f |" % (key[0], key[1], count[key], us / 1e3, longest[key] / 1e3))

    appends = [wait for wait in mine if in_the_log(wait) and "vfs_write" in wait["kernel"]
               and any(frame in wait["kernel"] for frame in
                       ("do_get_write_access", "__jbd2_log_wait_for_space", "wait_transaction_locked"))]
    beside_kind, with_none, longest_beside = collections.Counter(), 0, []
    for wait in appends:
        beside = [sync for sync in syncs if sync[1] > wait["begin"] and sync[0] < wait["end"]]
        with_none += not beside
        for kind in {of(sync[3]) for sync in beside}:
            beside_kind[kind] += 1
        if beside:
            longest_beside.append(max(sync[2] for sync in beside))
    print("\n%d waits inside the file system during a write to a file of the log, %.1f ms in all; %d of them "
          "with no listed sync under way at any moment of the wait" %
          (len(appends), sum(wait["us"] for wait in appends) / 1e3, with_none))
    if beside_kind:
        print("\n| A sync under way at a moment of the wait, of | Waits |\n| --- | --- |")
        for kind, waits_beside in beside_kind.most_common():
            print("| %s | %d |" % (kind, waits_beside))
        longest_beside.sort()
        print("\nThe longest sync beside a wait took a median of %.1f ms, and %.1f ms at most." %
              (longest_beside[len(longest_beside) // 2] / 1e3, longest_beside[-1] / 1e3))

    if not whole:
        return
    by_thread = collections.defaultdict(list)
    for wait in waits:
        by_thread[wait["tid"]].append(wait)
    held, held_us = collections.Counter(), collections.Counter()
    for wait in mine:
        asked = what(wait)
        if asked not in ("the lock of the C library's allocator", "a lock of the engine"):
            continue
        meanwhile = [other for other in by_thread.get(wait["waker_tid"], [])
                     if other["end"] > wait["begin"] and other["begin"] < wait["end"] and other["us"] >= 200]
        overlap = lambda other: min(other["end"], wait["end"]) - max(other["begin"], wait["begin"])
        doing = what(max(meanwhile, key=overlap)) if meanwhile else "nothing: it ran, or was kept from running"
        key = (asked, wait["waker"] or "an interrupt", doing)
        held[key] += 1
        held_us[key] += wait["us"]
    print("\n| The thread waited for | Woken by | What that thread was off a processor for meanwhile | Waits | In all, ms |")
    print("| --- | --- | --- | --- | --- |")
    for key, us in sorted(held_us.items(), key=lambda item: (item[0][0], -item[1])):
        print("| %s | %s | %s | %d | %.1f |" % (key[0], key[1], key[2], held[key], us / 1e3))


main()
