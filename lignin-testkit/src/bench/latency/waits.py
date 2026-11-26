#!/usr/bin/env python3
"""What the measuring thread of `lignin-bench latency` waited for, from a trace
of `trace.bt` beside the run.

    waits.py [--marked] TRACE [MIN_US]

MIN_US is the shortest wait that is counted, 1000 by default. With
`--marked`, as a campaign's tables are made, a trace is refused that has no
line for the beginning and the end of the measured blocks or for its
blocks, that did not run to its end, that says it lost events, or in which
the user stacks of a fifth or more of the waits inside a block are cut
short.

**Which waits are counted.** A wait is one OFFCPU record of the measuring
thread: the thread left a processor and was back after the time the record
says. Only the waits of the measured blocks are counted: those between the
harness's lines `lignin-mark measured begin` and `lignin-mark measured end`,
which the trace has as MARK records. The load and the warm blocks, which the
same thread applies through the same calls, are left out.

**Where a wait was** is read from the harness's lines, not from a stack. A
traced run has a line where each block begins (`lignin-mark block N`) and
one after each commit with the commit's length (`lignin-mark commit N NS`).
A wait that ended before the commit began is of the block's operations, one
that ended after it is of the commit, and one after the commit's line is
between two blocks, where the harness reads the engine's counts and pauses.
The thread writes those lines itself, so a wait cannot lie across one. The
stacks say what a wait was for; where a user stack does not reach the
harness it was cut short, as it is where a frame of the process keeps no
frame pointer, and what such a wait was in is known only as far as the
stack goes. A trace of an earlier form of the harness has no line for its
blocks: its waits are placed by their user stacks, by the harness's call of
the block and of the commit, and one whose stack has no frame of the
harness cannot be placed and is counted apart.
A wait in which the thread was taken off a processor while it could run is a
row of its own. The harness's pause after a commit is no wait and is not in
the trace; a sleep that an engine asks for on the measuring thread is one.

Printed:

1. What was counted, and the commits of each ext4 journal during the measured
   blocks.
2. Every counted wait, by where it was and by what the thread waited for: the
   system call it was in and the place in the kernel it left a processor at,
   or the lock of the process it asked for.
3. For the waits inside the kernel during a write to a file of the engine's
   log, or an allocation for one, by a frame of the user stack that says so
   (LigninDB's `lignin_store::wal`, RocksDB's `log::Writer` and
   `WriteToWAL`), two things that are not the same. First, the syncs of the process
   that had a moment in common with the wait, by what they are of: that is
   an overlap in time, and a wait can have several. Beside it is what chance
   gives: how many of the waits would have had a sync beside them had each
   begun at a moment of the measured blocks taken at random. Second, for a wait
   that is for a commit of an ext4 journal by the frames it blocks in (the
   journal is writing the block: a wait for the block's shadow bit under
   `do_get_write_access`; its transaction is being closed; or the thread is
   in `jbd2_log_wait_commit`), the commit of that journal that was under
   way when the wait began, and the syncs of the process that asked to wait
   for that commit before it had ended (the SYNC record names the
   transaction and when the sync asked, and JSTART and JEND say when the
   commit ran; a commit whose end the trace does not have is none). A trace
   of an earlier form of the tracer does not say when a sync asked: for it
   the syncs that named the transaction and whose time has the commit's end
   in it are given, and it is said that this shows less. The wait is linked
   to the commit by its stack and by the time, and not by a transaction's
   number read at the wait. A wait for room in the journal is in the
   journal's checkpoint, by its stack for a buffer or for the device, or
   where the journal's tail is moved on, or for the checkpoint's lock; it
   is for no commit, and is counted apart. Who had a buffer written, or
   held the lock, the trace does not say. A wait under
   `do_get_write_access` for the lock of the block, or for memory, is for
   neither, and is linked to nothing.
4. With a trace that follows every thread (`THREADS=all traced.sh`): for the
   waits for a lock of the process, the thread that woke the measuring
   thread and what that thread was off a processor for meanwhile. The thread
   that wakes is the one that gave the lock up last; the table does not say
   who held the lock. Where the trace has no waking for a wait, the row says
   that it was not recorded.

A trace of an earlier form of the tracer has no line for the measured blocks.
Its waits are then taken by their user stack: the measured blocks are applied
inside `std::thread::scope` and the others are not.
"""
import bisect
import collections
import re
import sys

# Frames that say only that the thread slept.
SLEEP = re.compile(r"^(__schedule|schedule|io_schedule|schedule_timeout|io_schedule_timeout|"
                   r"schedule_preempt_disabled|bit_wait|bit_wait_io|__wait_on_bit|"
                   r"out_of_line_wait_on_bit|__wait_for_common|wait_for_completion.*)$")
SYSCALL = re.compile(r"^__(?:arm64|x64|ia32)_sys_(\w+)$")
FOR_ROOM = "__jbd2_log_wait_for_space"
# Where a wait of the measured blocks was.
COMMIT, OPERATIONS, BETWEEN, UNPLACED = "the commit", "the operations", "between two blocks", "nowhere"
SLEPT = ("nanosleep", "clock_nanosleep")
HARNESS = ("lignin_testkit5bench7latency", "lignin_testkit::bench::latency")
# The harness's file layer is under the engine's calls, inside a block: a
# frame of it does not say that the stack reaches the harness's own loop.
WATCHING = ("latency8watching", "latency::watching")
# The states of a thread that left a processor while it could run: running,
# and what the tracepoint reports for a preempted thread (TASK_REPORT_MAX).
RUNNABLE = ("0", "256")
ALLOCATOR = "the lock of the C library's allocator"
ENGINE = "a lock of the engine"
CONDITION = "a condition variable of the engine"


def field(line, name):
    found = re.search(r"\b%s=(\S+)" % name, line)
    return found.group(1) if found else None


def frames(lines):
    out = []
    for line in lines:
        found = re.match(r"\s+[0-9a-f]+ (.+?)\+\d+(?: \(.*\))?$", line)
        if not found:
            continue
        # A name of C++ comes with its arguments: the name is what is before them.
        name = found.group(1) if found.group(1).startswith("_R") else found.group(1).split("(", 1)[0]
        if not out or out[-1] != name:
            out.append(name)
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
        # What the tracer says of itself: the events it lost, and that it
        # ran to its end.
        self.lost, self.ended = 0, False
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
                              waker=waker.group(3), waker_tid=int(waker.group(4)), kernel=[], user=[],
                              woken=[])
                self.waits.append(record)
                part = None
            elif record is not None and line.startswith(" left at:"):
                part = "kernel"
            elif record is not None and line.startswith(" called from:"):
                part = "user"
            elif record is not None and line.startswith(" woken at:"):
                part = "woken"
            elif line.startswith("Lost "):
                found = re.match(r"Lost (\d+) events", line)
                self.lost += int(found.group(1)) if found else 1
            elif line == "trace ended":
                self.ended = True
            elif line.startswith("SYNC"):
                self.syncs.append(dict(begin=int(field(line, "begin")), end=int(field(line, "end")),
                                       us=int(field(line, "us")), file=field(line, "file"),
                                       dev=field(line, "dev"), transaction=field(line, "transaction"),
                                       asked=int(field(line, "asked") or 0)))
            elif line.startswith("MARK"):
                self.marks.append((int(field(line, "at")), int(field(line, "tid") or 0),
                                   line.split("lignin-mark ", 1)[-1]))
            elif line.startswith("JSTART"):
                self.starts[(field(line, "dev"), field(line, "transaction"))] = int(field(line, "at"))
            elif line.startswith("JEND"):
                self.ends[(field(line, "dev"), field(line, "transaction"))] = int(field(line, "at"))
            elif re.match(r"(WRITE|JBD2|BLK)", line):
                # A record of another kind. The earlier tracer printed a wait
                # in four calls, and such a line can stand between a wait's
                # header and its stacks: the wait goes on after it.
                pass
            elif record is not None and part:
                record[part].append(line)
        for wait in self.waits:
            wait["kernel"], wait["user"] = frames(wait["kernel"]), frames(wait["user"])
            wait["woken"] = frames(wait["woken"])
        begins = [(at, tid) for at, tid, what in self.marks if what.startswith("measured begin")]
        ends = [at for at, _, what in self.marks if what.startswith("measured end")]
        # One engine a trace: its measured blocks are one stretch.
        self.measured = (begins[0][0], ends[0]) if begins and ends else None
        self.measuring = begins[0][1] if begins else 0
        # The blocks that the harness marked: where each began, where its
        # commit began and where the commit's line was written.
        began, committed = {}, {}
        for at, tid, what in self.marks:
            words = what.split()
            if tid != self.measuring or len(words) < 2 or not words[1].isdigit():
                continue
            if words[0] == "block":
                began[int(words[1])] = at
            elif words[0] == "commit" and len(words) > 2 and words[2].isdigit():
                committed[int(words[1])] = (at - int(words[2]), at)
        self.blocks = sorted((began[block], *committed[block]) for block in began if block in committed)
        self.block_begins = [block[0] for block in self.blocks]

    def of_the_measuring_thread(self, wait):
        return not self.measuring or not wait["tid"] or wait["tid"] == self.measuring

    def in_the_measured_blocks(self, wait):
        if self.measured:
            return self.measured[0] <= wait["begin"] and wait["end"] <= self.measured[1]
        return any("scoped5scope" in frame or "scoped::scope" in frame for frame in wait["user"])

    def phase(self, wait):
        """Where a wait of the measured blocks was: by the harness's lines of
        its blocks, and for a trace that has none by the wait's user stack."""
        if self.blocks:
            at = bisect.bisect_right(self.block_begins, wait["begin"]) - 1
            if at < 0 or wait["begin"] > self.blocks[at][2]:
                return BETWEEN
            # The line of a commit is written after it, so the commit began
            # no later than the line's time less the commit's length: a
            # wait of the operations has ended by then.
            return OPERATIONS if wait["end"] <= self.blocks[at][1] else COMMIT
        if in_a_block(wait):
            return COMMIT if in_the_commit(wait) else OPERATIONS
        return BETWEEN if of_the_harness(wait) else UNPLACED

    def the_pause(self, wait):
        """The harness's pause after a commit, which the tracer leaves out
        where it knows it."""
        slept = any(SYSCALL.match(frame) and SYSCALL.match(frame).group(1) in SLEPT for frame in wait["kernel"])
        if self.blocks:
            return slept and self.phase(wait) == BETWEEN
        return slept and of_the_harness(wait) and not in_a_block(wait)


def in_a_block(wait):
    return any("latency5block" in frame or "latency::block" in frame for frame in wait["user"])


def in_the_commit(wait):
    return any("5Block6commit" in frame or "commit_transaction" in frame
               or "Block>::commit" in frame for frame in wait["user"])


def of_the_harness(wait):
    """Whether the user stack reaches the harness: one that does not was cut short."""
    return any(mark in frame and not any(layer in frame for layer in WATCHING)
               for frame in wait["user"] for mark in HARNESS)


def in_the_log(wait):
    """A call on a file of the engine's log, by a frame that says so:
    LigninDB's log store, and RocksDB's writer of its log."""
    return any("lignin_store3wal" in frame or "lignin_store::wal" in frame
               or frame.startswith("rocksdb::log::Writer::") or "::WriteToWAL" in frame for frame in wait["user"])


def journal(wait):
    """What a wait under ext4's journal is for, by the frames it blocks in:
    `commit` where it waits for a commit of the journal, `room` where it
    waits for room in it, `other` under the journal for neither, with the
    words for it; and None outside the journal."""
    kernel = wait["kernel"]
    if FOR_ROOM in kernel:
        inner = kernel[:kernel.index(FOR_ROOM)]
        if "jbd2_log_wait_commit" in inner:
            return "commit", "which has no room, and the thread waits for a commit"
        if "jbd2_log_do_checkpoint" in inner:
            return "room", "which has no room: the thread waits in its checkpoint"
        if "jbd2_cleanup_journal_tail" in inner:
            return "room", "which has no room: the thread moves the journal's tail on, and waits for the device"
        if any("mutex_lock" in frame for frame in inner):
            return "room", "which has no room: the thread waits for the lock of its checkpoint"
        rest = [frame for frame in inner if not SLEEP.match(frame)]
        return "room", "which has no room: " + (" < ".join(rest[:3]) or "the thread gives the processor up")
    if "jbd2_log_wait_commit" in kernel:
        return "commit", "and waits for a commit of it"
    if "wait_transaction_locked" in kernel:
        return "commit", "whose transaction is being closed"
    if "do_get_write_access" in kernel:
        inner = kernel[:kernel.index("do_get_write_access")]
        # The block's lock is waited for with the bit-lock calls, and the
        # journal's write of the block, its shadow, with the plain ones.
        if any(frame in inner for frame in ("__lock_buffer", "__wait_on_bit_lock", "out_of_line_wait_on_bit_lock")):
            return "other", "and waits for the lock of the block it changes"
        if any(frame in inner for frame in ("__wait_on_bit", "out_of_line_wait_on_bit")):
            # The block of the file's inode, where ext4 changes the inode.
            block = "that block of inodes" if "ext4_reserve_inode_write" in kernel else "the block it changes"
            return "commit", "which is writing " + block
        rest = [frame for frame in inner if not SLEEP.match(frame)]
        return "other", "and waits there: " + (" < ".join(rest[:3]) or "it gives the processor up")
    return None, None


def what(wait):
    """What a wait was for, in words."""
    kernel, user = wait["kernel"], wait["user"]
    if wait["state"] in RUNNABLE:
        return "taken off the processor while it could run"
    call = next((found.group(1) for found in map(SYSCALL.match, kernel) if found), "?")
    if call in SLEPT:
        return "a sleep that the engine asked for"
    if any("futex" in frame for frame in kernel):
        if any(frame.startswith("__lll_lock_wait_private") for frame in user[:2]):
            return ALLOCATOR
        if any("wait_for_room" in frame or "wait_for_turn" in frame for frame in user):
            return "a turn of write-back, for room"
        if any("pthread_cond_" in frame or "CondVar" in frame or "Condvar" in frame for frame in user):
            return CONDITION
        return ENGINE
    if "file_modified" in kernel or "kiocb_modified" in kernel or "touch_atime" in kernel:
        asked = "the file's times"
    elif "ext4_da_write_end" in kernel or "ext4_write_end" in kernel:
        asked = "the file's length"
    else:
        asked = "a change of the file"
    _, at_the_journal = journal(wait)
    if at_the_journal:
        return "%s: ext4 writes %s to the journal, %s" % (call, asked, at_the_journal)
    rest = [frame for frame in kernel if not SLEEP.match(frame)]
    if "xfs_ilock" in rest and asked == "the file's times":
        return "%s: XFS stamps %s, and waits for the lock of the inode (xfs_ilock)" % (call, asked)
    return "%s: %s" % (call, " < ".join(rest[:3]))


def of(path):
    """What a synced file is, by its directory."""
    directory, _, name = path.partition("/")
    if name == "MANIFEST.tmp":
        return "the manifest's temporary file"
    # The harness keeps RocksDB's database in a directory of that name.
    if directory == "rocksdb":
        if name.endswith(".sst"):
            return "a table file of RocksDB"
        if name.endswith(".log"):
            return "a log file of RocksDB"
        return "RocksDB's manifest" if name.startswith("MANIFEST") else "another file of RocksDB"
    if name == "rocksdb":
        return "the names of RocksDB's directory"
    kinds = {"wal": "a log file", "val": "a value-log file", "seg": "a segment", "table": "a table checkpoint"}
    if directory in kinds:
        return kinds[directory]
    return "the names of %s/" % name if name in kinds else "the names of the database directory"


def main():
    arguments = [argument for argument in sys.argv[1:] if argument != "--marked"]
    marked = len(arguments) < len(sys.argv) - 1
    if not arguments:
        sys.exit(__doc__)
    floor = int(arguments[1]) if len(arguments) > 1 else 1000
    trace = Trace(arguments[0])
    if marked:
        refused = None
        if not trace.measured:
            refused = "has no line for the beginning and the end of the measured blocks"
        elif not trace.blocks:
            refused = "has no line for its blocks: the harness is run with --mark-blocks for a trace"
        elif not trace.ended:
            refused = "did not run to its end: the tracer was ended before it had written what it held"
        elif trace.lost:
            refused = "says that the tracer lost %d events: its counts are too low by what was lost" % trace.lost
        if refused:
            sys.exit("waits.py: %s %s" % (arguments[0], refused))
    long = [wait for wait in trace.waits
            if wait["us"] >= floor and trace.of_the_measuring_thread(wait) and not trace.the_pause(wait)]
    inside = [wait for wait in long if trace.in_the_measured_blocks(wait)]
    where = {id(wait): trace.phase(wait) for wait in inside}
    mine = [wait for wait in inside if where[id(wait)] in (COMMIT, OPERATIONS)]
    between = [wait for wait in inside if where[id(wait)] == BETWEEN]
    # The waits whose user stack does not say what they were in: with the
    # harness's lines for its blocks, those inside a block whose stack does
    # not reach the harness; without, those that could not be placed at all.
    cut = [wait for wait in mine if not of_the_harness(wait)] if trace.blocks else \
        [wait for wait in inside if where[id(wait)] == UNPLACED]
    if trace.measured:
        seconds = (trace.measured[1] - trace.measured[0]) / 1e9
        print("The measured blocks took %.0f s in the trace%s. Waits of the measuring thread of %.1f ms or more: "
              "%d in the measured blocks and %d before or after them, which are not counted." % (
                  seconds, ", and %d of them are marked" % len(trace.blocks) if trace.blocks else "",
                  floor / 1e3, len(inside), len(long) - len(inside)))
        if trace.blocks:
            print("Of those in the measured blocks, %d (%.1f ms in all) are between two blocks, where the harness "
                  "reads the engine's counts, and are in no row below. Of the %d inside a block, %d (%.1f ms) "
                  "have a user stack that does not reach the harness: the harness's lines say where they were, "
                  "and what they were in is known only as far as the stack goes." % (
                      len(between), sum(wait["us"] for wait in between) / 1e3, len(mine),
                      len(cut), sum(wait["us"] for wait in cut) / 1e3))
        else:
            print("Of those in the measured blocks, %d (%.1f ms in all) have no frame of the harness in their user "
                  "stack, so no place can be given them, and %d (%.1f ms) are of the harness between two blocks, "
                  "where it reads the engine's counts. Neither is in a row below." % (
                      len(cut), sum(wait["us"] for wait in cut) / 1e3,
                      len(between), sum(wait["us"] for wait in between) / 1e3))
        # A wait between two blocks says nothing of the stacks inside a
        # block: the share is of the waits that are not between blocks.
        elsewhere = len(inside) - len(between)
        if marked and len(cut) >= 10 and 5 * len(cut) >= elsewhere:
            sys.exit("waits.py: of the %d waits of %s that are not between two blocks, %d have a user stack that "
                     "is cut short: the build has to keep its frame pointers" % (elsewhere, arguments[0], len(cut)))
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
    # The time of the measured blocks: between the harness's two lines, or
    # for an earlier trace from the first of their waits to the last.
    counted = [wait for wait in trace.waits
               if trace.of_the_measuring_thread(wait) and trace.in_the_measured_blocks(wait)]
    span = trace.measured or (min((wait["begin"] for wait in counted), default=0),
                              max((wait["end"] for wait in counted), default=1))

    total, count, longest = collections.Counter(), collections.Counter(), collections.Counter()
    for wait in mine:
        key = (where[id(wait)], what(wait))
        total[key] += wait["us"]
        count[key] += 1
        longest[key] = max(longest[key], wait["us"])
    print("\n| Where | What the thread waited for | Waits of %.1f ms or more | In all, ms | Longest, ms |" % (floor / 1e3))
    print("| --- | --- | --- | --- | --- |")
    for key, us in sorted(total.items(), key=lambda item: (item[0][0], -item[1], item[0])):
        print("| %s | %s | %d | %.1f | %.1f |" % (key[0], key[1], count[key], us / 1e3, longest[key] / 1e3))

    appends = [wait for wait in mine if in_the_log(wait) and wait["state"] not in RUNNABLE
               and any(frame in wait["kernel"] for frame in ("vfs_write", "vfs_fallocate"))]
    cut_ms = [wait["us"] for wait in cut] if trace.measured else []
    print("\n%d waits inside the kernel during a write to a file of the engine's log, or an allocation for one, "
          "%.1f ms in all.%s" % (len(appends), sum(wait["us"] for wait in appends) / 1e3,
                                " The %d waits whose user stack is cut short (%.1f ms) are not among them, and "
                                "any of them can be such a wait." % (len(cut_ms), sum(cut_ms) / 1e3)
                                if cut_ms else ""))
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
        print("\nWhat a moment taken at random gives: a listed sync was under way for %.0f%% of the time of the "
              "measured blocks, and had each of these waits begun at a moment of them taken at random, %.1f of the "
              "%d would have had one beside it, where %d had. An append is made inside a commit, and the "
              "engine's background is woken by commits, so the moments of appends are not moments taken at random: "
              "the figure is something to read the table against, and no measure of what the syncs do to appends." % (
                  100 * under_way, expected, len(appends), len(appends) - with_none))
    for_a_commit = [wait for wait in appends if journal(wait)[0] == "commit"]
    for_room = sum(1 for wait in appends if journal(wait)[0] == "room")
    for_neither = sum(1 for wait in appends if journal(wait)[0] == "other")
    if appends and trace.starts:
        # A commit is one that began and ended in the trace: one without its
        # end would be under way for every later wait.
        commits = {key: (at, trace.ends[key]) for key, at in trace.starts.items() if key in trace.ends}
        asked_kind, named_kind, in_a_commit, with_asked, with_named = collections.Counter(), collections.Counter(), 0, 0, 0
        # An earlier form of the tracer had the file system of a write and
        # not of an allocation.
        nowhere = sum(1 for wait in for_a_commit if wait["dev"] == "0,0")
        for wait in for_a_commit:
            under_way = {key: ended for key, (began, ended) in commits.items()
                         if key[0] == wait["dev"] and began <= wait["begin"] < ended}
            in_a_commit += bool(under_way)
            named = [sync for sync in trace.syncs if (sync["dev"], sync["transaction"]) in under_way]
            # A sync waited for the commit if it asked before the commit had
            # ended, which only a trace that says when a sync asked shows.
            asked = [sync for sync in named
                     if sync["asked"] and sync["asked"] < under_way[(sync["dev"], sync["transaction"])]]
            # Without that time: the sync named the transaction, began before
            # the commit ended and returned no earlier than it. It can still
            # have asked after the end, and then it waited for nothing.
            only_named = [sync for sync in named if not sync["asked"]
                          and sync["begin"] < under_way[(sync["dev"], sync["transaction"])] <= sync["end"]]
            with_asked += bool(asked)
            with_named += bool(only_named) and not asked
            for kind in {of(sync["file"]) for sync in asked}:
                asked_kind[kind] += 1
            if not asked:
                for kind in {of(sync["file"]) for sync in only_named}:
                    named_kind[kind] += 1
        print("\nBy the journal's transactions: %d of the %d waits were for a commit of an ext4 journal by the "
              "frames they blocked in, %d were for room in the journal, which is for no commit, and %d were under "
              "the journal for neither and are linked to nothing. Of the %d, %d began while a commit of the journal "
              "of the written file's file system was under way.%s" % (
                  len(for_a_commit), len(appends), for_room, for_neither, len(for_a_commit), in_a_commit,
                  " Of %d the trace does not have the file system, so they are linked to no commit." % nowhere
                  if nowhere else ""))
        if any(sync["asked"] for sync in trace.syncs):
            print("\nFor %d of them a sync of the process had asked to wait for that commit before it ended. "
                  "By what those syncs were of:\n" % with_asked)
            print("| A sync that asked to wait for the commit that the wait began in, before it ended, of | Waits |"
                  "\n| --- | --- |")
            for kind, waits_for in sorted(asked_kind.items(), key=lambda item: (-item[1], item[0])):
                print("| %s | %d |" % (kind, waits_for))
        else:
            print("\nThis trace does not say when a sync asked to wait for a commit. For %d of them a sync of the "
                  "process named that commit's transaction as the one it waited for, began before the commit ended "
                  "and returned no earlier than it: that the sync asked before the commit ended is likely and is "
                  "not shown. By what those syncs were of:\n" % with_named)
            print("| A sync that named the commit that the wait began in, of | Waits |\n| --- | --- |")
            for kind, waits_for in sorted(named_kind.items(), key=lambda item: (-item[1], item[0])):
                print("| %s | %d |" % (kind, waits_for))

    if len({wait["tid"] for wait in trace.waits}) <= 1:
        return
    by_thread = collections.defaultdict(list)
    for wait in trace.waits:
        by_thread[wait["tid"]].append(wait)
    woke, woke_us = collections.Counter(), collections.Counter()
    for wait in mine:
        asked = what(wait)
        if asked not in (ALLOCATOR, ENGINE, CONDITION):
            continue
        meanwhile = [other for other in by_thread.get(wait["waker_tid"], [])
                     if other["end"] > wait["begin"] and other["begin"] < wait["end"] and other["us"] >= 200]
        overlap = lambda other: min(other["end"], wait["end"]) - max(other["begin"], wait["begin"])
        doing = what(max(meanwhile, key=overlap)) if meanwhile else "no wait of 0.2 ms or more in the trace"
        key = (asked, wait["waker"] or "not recorded", doing if wait["waker"] else "not recorded")
        woke[key] += 1
        woke_us[key] += wait["us"]
    print("\n| The thread waited for | Woken by | What that thread was off a processor for meanwhile | Waits | In all, ms |")
    print("| --- | --- | --- | --- | --- |")
    for key, us in sorted(woke_us.items(), key=lambda item: (item[0][0], -item[1], item[0])):
        print("| %s | %s | %s | %d | %.1f |" % (key[0], key[1], key[2], woke[key], us / 1e3))


main()
