# LigninDB on a full disk, and backup

This is the specification for task OPS-08 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
says:

- what the engine does when the device it writes to has **no room left**:
  what each call answers, what stays readable, when a session goes on and
  when the database has to be opened again (sections 2 to 4)
- how the engine **gets room back**, in which order, and what a reserve is
  for (section 5)
- how a **consistent backup** is made from a checkpoint, and restored
  (section 6)
- what an **operator** does in each case (section 7)

It builds on [transactions.md](transactions.md) (TXN-01: CM5 and CM9 for a
commit that fails, DU1 to DU3 for durability, CK1 to CK10 for checkpoints),
on the file layer's contract (`lignin_io::vfs`) and on
[platform-requirements.md](platform-requirements.md) (P-03, P-14 to P-19),
and uses their rule numbers. The code is `lignindb::space`,
`lignindb::backup` and, for the reserve of a session, `lignindb::db`, and
the tests are those of section 9. Section 8 lists the choices made here
that the plan left open.

## 1. Scope

A device is **full** when the file layer answers an operation with "no
space left": the device has no free block, or a quota on it is used up
(`lignin_io::vfs`, "Errors"). The operations that can be answered so are
those that can need room: a write, an append, a sync, the making of a file
or of a directory, a rename and a hard link. A read, a removal and a lock
that makes no file are never refused for room.

This document does not say how much room a database takes (PERF-08), how
the cleaner chooses what it moves (STORE-08, PERF-04), or how a node is
told that its disk fills (OPS-03). It names the counts an operator watches
(`lignin_store::stats`, STORE-11) and does not define them.

## 2. What never happens

**DF1. A full device never changes what is committed.** Whatever operation
is refused for room, at whatever moment, and however long the device stays
full:

- every reader reads a committed version, whole: the latest one, or the one
  it pinned (CM4, RD1);
- the store keeps every physical rule that `lignindb::verify` checks, on
  the pages as they are and on the pages as the next open has them;
- the next open lands on one committed version (DU2). After a power cut,
  or a process that was killed, it is at least the newest version that
  was made durable: by a `flush()`, a checkpoint or a close that returned
  `Ok`, by a commit that returned `Ok` under a policy that syncs at the
  commit, or by the open of the session. And it is at most the latest
  version that was published. With no power cut since, it is the latest
  version that was published after a close that returned `Ok`, and also
  after a close that answered an error, unless the operating system
  failed to write to the device something that the session wrote to its
  files. A sync that the device refused is such a failure, and what the
  system keeps in memory of the bytes it could not write is not promised
  (`sync-handles.md`, section 6): the open can then land on an earlier
  version with no power cut, and never on one before the newest that was
  made durable. In each case one version more is possible: a commit that
  was answered as unsettled (DF2).

**DF2. A commit that a full device had answered as not made is never
recovered** (CM9 (b)): `CommitError::Failed`, `CommitError::NotReady`,
and a commit that was refused because the session had ended
(`WriteError::Reopen`). Two answers of a full device are errors and say
something else themselves. A commit that was answered
`CommitError::NotDurable` is published, and the sync of the log that it
asked for was refused. The next open has it if the system still has the
log whole up to it, which it does not promise after a sync that was
refused (DF1), and if no power cut loses it. A commit that was answered
`CommitError::Unsettled` is the last commit of what the next open finds,
or is not there.

DF1 and DF2 are about what a device that refuses for room causes. The
answers of other failures are `transactions.md`'s (CM9): among them
`CommitError::Halted`, whose commit is in the log and is recovered.

**DF3. Nothing is refused for room and then half done.** Each call of
section 3 either has its whole effect or leaves what it found. What a
refused operation left in a file (a part of an append, a file that was
made and not named) is cut off or removed before anything depends on it,
by the session if it can and by the next open otherwise.

## 3. What each call answers

A caller learns that the device is full from the error of the call that
met it. `lignindb::space::out_of_space` says so of any error of the engine
whose cause was the file layer's "no space left".

**DF4. The answers.**

| Call | On a full device | The session afterwards |
| --- | --- | --- |
| `Db::open` | `OpenError`, with the file layer's error as its source. Nothing of an existing database was changed that a later open does not expect: a recovery that fails is followed by one that lands on the same version (`lignin_store::recovery`, "Twice"). A database that was being made is made by the next open. | there is none |
| `Db::commit`, `Db::write`, `Db::wipe`: the group does not reach the log | `CommitError::Failed`: the commit has not happened and never will (CM5). The transaction is ended. | goes on. The next commit is refused with `CommitError::NotReady`, its transaction whole, until the failed bytes are cut off the log, which needs a little room (section 5). |
| the same, where it was an append of the background work beside the commit that was refused | `CommitError::Failed` with `LogError::NoActiveFile`: the commit has not happened. Where the commit had not taken its transaction yet, `CommitError::NotReady` with the same, and the transaction whole. Neither error says that the device is full, because the commit's own operations were not refused. | goes on, as above |
| the same, and the engine cannot make durable that the group is out of the log | `CommitError::Unsettled` | takes no write any more (`Reopen::Stopped`): the next open settles it |
| the same, with a sync policy that syncs at the commit, and the sync is refused | `CommitError::NotDurable`: the commit is published and is not known to be durable | takes no write any more (`Reopen::NotDurable`) |
| a commit that finds a replacement of the manifest in doubt and cannot have it settled yet (DF8a) | `CommitError::NotReady`, with the transaction whole: the commit can be made again | goes on |
| a commit that comes while the session takes no write | `WriteError::Reopen`, the transaction whole | as before |
| `Db::flush` | `DbError::Flush` | takes no write any more (`Reopen::NotDurable`): after a sync that failed the log makes nothing durable in this session |
| `Db::snapshot`, `Db::read`, every read through a transaction | what it reads at any other time | as before |
| `Db::rollback` | `Ok`: a rollback writes to no file | as before |
| `Db::checkpoint`, which is `Checkpointer::create` on an open database | `CheckpointError`, with nothing put at the target, or the whole target (section 6.4). A part of a checkpoint that could not be taken away is beside the target, under its name with `.creating` added. What a creation leaves of the live database is said in `lignindb::checkpoint` ("Durability, and a creation that fails"). A creation has a manifest in doubt settled before it makes a directory: where the settling it asks for after the catch-up answers an error, it fails with `CheckpointError::Settle`, and where the manifest is in doubt again when the creation reads it, with `CheckpointError::Manifest`. Either can be made again (DF8a). | goes on, unless a sync of the log or of a segment was refused, the log's own (`CheckpointError::Sync`) or one of write-back's catch-up: then it has ended (DF8) |
| `Backup::copy` | `BackupError`, with nothing left of the target, or a target without a manifest, or the whole target (section 6.4). The source is as it was. | no session is involved |
| `Db::close` | `DbError`. The handle is closed all the same, and the next open replays what the close did not cover. | has ended |
| the background work: write-back, the reclaiming of what drops left, the cleaner | nothing is answered to anyone. The turn that was refused is counted, its error is kept as text (`DbStats::background`), and it is made again at the next turn. A session that keeps a reserve releases it (DF13). | goes on, unless what was refused was a sync of the log or of a segment: then it has ended (DF8) |

**DF5. Reads go on throughout.** A reader takes no lock that a refused
write holds, and reads pages from the buffer pool or from files that are
only read. A full device refuses no read.

**DF6. A session that says it takes no write any more is ended by its
caller**: `Db::reopen` says why, the caller closes the handle and opens the
database again when there is room. Until then every read is answered.

## 4. When a session goes on, and when it is opened again

A device that was full has one of two outcomes for a session, and **the
session says which** (D-151, D-157): `Db::reopen` is asked, or the next
commit is made. A session ends for what stays failed in the store, and
for nothing else.

**DF7. A refusal that involved no sync of the log or of a segment leaves
the session as it was.** `Db::reopen` says nothing, and once the device
has room the next commit is taken, with nothing for the caller to do:

- The cut that a failed append left to make (CM9 item 4) is made by
  write-back's next turn, or by the thread of the next commit before it
  takes the commit lock. From then on commits are taken.
- A turn of write-back that was refused is made again: the pages that were
  not written are in the buffer pool and in the log, the values that were
  not placed are placed by a later pass, and a table checkpoint that was
  not installed leaves the one before it in force.
- A step of the reclaimer and a pass of the cleaner that were refused are
  made again.
- A replacement of the manifest that did not finish is settled (DF8a).

**DF8. A refused sync of the log or of a segment ends the session,
whichever step met it.** Two things stay failed in a store until it is
opened again, whatever the device does later:

| What was refused | What stays failed | `Db::reopen` |
| --- | --- | --- |
| a sync of the log | A sync that fails cannot be made good (`lignin_io::vfs`): what was not durable is unknown from then on, so the log makes nothing durable any more, and the cut that a failed append waits for is never made (`lignin_store::wal`). | `Reopen::NotDurable`, or `Reopen::Stopped` where a commit was left unsettled by it |
| a sync of a segment | The blocks that were not durable cannot be made durable, and chains point to them. No table checkpoint is installed any more, so the log keeps every commit from then on (`lignin_store::flush`, "When something fails"). | `Reopen::SegmentsNotDurable` |
| a sync of a value-log file | Nothing. A cell is given a pointer to a value only once a sync has made the value durable and a manifest covers it, so no cell points to a value that the refused sync was for. The file is closed at the length that was durable before and is not synced again, and a later pass appends each value behind that length that a cell still needs to another file (DF11, D-246). | nothing: the session goes on |

The step that meets one need not be a commit or a `flush()`. A turn of
write-back does, a table checkpoint, the creation of a checkpoint of the
database, a pass of the cleaner. A session in one of these states would
take commits that can never become durable, keep a log that only grows, or
refuse every commit for a failure that is long over. So the engine looks at
the two before every commit, after every commit, and whenever `Db::reopen`
or `Db::stats` is asked. From the first look that finds one and records
it:

- `Db::reopen` says why, and goes on saying the same. The session gives
  one reason, the first that was seen where it is answered for, also
  where a commit stopped the session for a reason of its own in the same
  moment: `Db::reopen`, `DbStats::reopen` and every commit that is
  refused for it give that one;
- every commit that begins is refused with `WriteError::Reopen`, its
  transaction whole (DF4). This holds under every sync policy, also under
  one that asks no sync of a commit;
- every read is answered as before (DF5);
- a `flush()` still makes durable what was published, unless it is the
  log's sync that failed;
- the close finishes what it can and answers an error (DF4);
- the next open recovers the database at a version that DF1 allows. A
  refused sync is a failure of the kind that DF1 names: also with no
  power cut, the open can land before the latest version that was
  published.

**A session can end in the last turn that its close waits for** (D-225).
A close ends the three threads first, and each makes the turn it is in to
its end. Where such a turn is refused a sync of a segment, the session
has ended and nobody could ask it: the close takes the handle. Where the
close comes to close the files of the session, and write-back's part of
it (`WriteBack::finish`) answers that segments are not durable, the
close answers with one of the two errors below.

| The close answers | What that says |
| --- | --- |
| `DbError::Clean` with `CleanerError::SegmentNotDurable` | The cleaner's segment could not be made durable, at the close or in a pass of the cleaner's thread before it. The error names the segment, and has what the device answered as its cause where the cleaner's own call met the refusal: `lignindb::space::out_of_space` then says whether the device was full. |
| `DbError::Flush` with `FlushError::SegmentsNotDurable` | A sync of a segment failed in the session, and the close installed no table checkpoint. The error has no cause. What the device answered was given to the call that met the refusal, or was counted with its text among the errors of the background work (`DbStats::background`), which goes with the handle. |

Both are the state that `Db::reopen` calls `Reopen::SegmentsNotDurable`,
by their type, and the close's error carries no `Reopen` beside them.
The second is what such a close answers where the cleaner lost no
segment: so where a turn of write-back's own thread was refused the sync
of write-back's segment, and that is all that was lost. The cause of
that refusal is not kept for the close.

The look is two reads of what the store holds already: no operation on a
file, and no lock that a commit waits for.

What the look finds is recorded under the commit lock, as a commit and a
`flush()` record a failed sync of their own, so no commit is published
behind the record of a log that makes nothing durable. Before a commit the
lock is tried and not waited for: a commit that is being made records what
it meets itself, and a look that comes meanwhile records nothing. After a
commit the thread that made it looks again and waits for the lock. So a
store that failed beside commits ends the session after at most one more
commit of each thread that was writing. A commit that was waiting for the
lock when a failed sync of a segment was recorded is still taken: it is in
the log like any other commit that was published and not made durable,
and the next open has it where DF1 says that it has such a commit.

**DF8a. A replacement of the manifest that did not finish is settled, and
ends no session** (D-157). Where the rename of a new manifest is refused,
or the sync of the directory after it, it is not known which of two
manifests a crash would leave (`lignin_store::segment`). That does not
last. Four have the manifest opened again, which syncs the directory and
finds out which one there is: write-back where it next needs the manifest,
a commit that finds it in doubt before it begins, a close once the
background work has ended, and the creation of a checkpoint before it
makes a directory. Then the session goes on as in DF7.
`Db::reopen` and `Db::stats` are looks: they say that the session has not
ended, make no operation on a file, and settle nothing.

- Where the manifest cannot be opened again yet, because the device still
  refuses, the commit that found it in doubt is refused with that error
  and its transaction whole (`CommitError::NotReady`), a close answers the
  error and finishes nothing, a creation of a checkpoint fails with it and
  makes no directory (`CheckpointError::Settle`), and the next one to come
  by tries again.
- A commit looks at the manifest without waiting for it. While write-back
  holds it to replace it, or another thread holds it to have it settled,
  a commit that comes sees nothing of the doubt and is made, as it is
  beside any turn of write-back: the commit itself writes the log, and no
  manifest. So `CommitError::NotReady` is the answer to the commit that
  itself found the manifest in doubt and could not have it settled, and
  not to every commit of that moment. A commit that found the doubt
  waits for the manifest's lock to have it settled, behind whoever holds
  it then.
- A table checkpoint that turns the log replaces the manifest for the
  log's files before it replaces it to install itself. Where such an
  earlier replacement is refused once, in its rename or in the sync of
  the directory after it, write-back needs the manifest again in the same
  call and has it settled there: the checkpoint is installed, and returns
  with the manifest in no doubt. Where the replacement that installs is
  refused, the checkpoint fails and the manifest is in doubt when it
  returns, which is the state the other points here are about.
- The creation of a checkpoint reads the manifest itself, and asks for it
  to be settled before it makes a directory. A manifest that is in doubt
  again when the creation reads it, by a replacement after that, fails the
  creation with `CheckpointError::Manifest`: it leaves nothing at its
  target, and can be made again.
- A manifest that cannot be opened again for another reason than room
  refuses every commit that finds it so, and the session does not end for
  it: the directory was removed under the handle, or cannot be written, or
  its sync fails each time. A commit that stays refused with
  `CommitError::NotReady` on a device with room says so in its error, and
  the handle is closed (section 7.1).
- Settling opens the manifest and then tells each of the four stores of
  it. Where the manifest was opened and a store answers an error (it could
  not delete a file that the manifest names no longer), every store was
  told all the same and the manifest is in doubt no more. The commit that
  had it settled is answered that error once (`CommitError::NotReady`, its
  transaction whole), the next commit finds nothing in doubt, and the
  session does not end for it. A creation of a checkpoint that had it
  settled after its catch-up is answered that error once as well
  (`CheckpointError::Settle`, with no directory made).
- A close has the lock file alone to go by before it opens the manifest
  again, and takes the manifest it finds, as write-back does. A directory
  whose files were written over in place while the handle was open and its
  manifest in doubt is not told apart from the database's own (P-18 holds
  for a directory that was removed or put in another's place).

**DF9. An open needs room.** A recovery appends to no file of an earlier
session (M16): it names their files closed in a new manifest and begins a
log file of its own, and it writes the unsynced end of the last log file
again. So a database is not opened on a device with no room at all: the
open fails with the file layer's error, changes nothing that the next open
does not expect, and succeeds once there is room. How much it needs is in
section 5.3.

## 5. Getting room back

### 5.1 What the engine holds that is dead

| What | Goes when | Bounded by |
| --- | --- | --- |
| The log before the place the table checkpoint in force keeps it from | a later table checkpoint is installed | `FlushConfig::max_log_held` (256 MiB), by backpressure on committers. Not bounded while a write transaction stays open (FL5) or once a page cannot be written (section 7.2). |
| Blocks of segments that no chain points to | the cleaner has moved what is live out of their segment, and a table checkpoint that no longer points into it is in force | `CleanerConfig::target_dead_percent` of all blocks (14), while the cleaner keeps up |
| Values that no cell points to | the cleaner has copied what is live out of their file | `CleanerConfig::value_dead_percent` |
| Files that a checkpoint shares | the checkpoint is deleted too | everything the live database had when the checkpoint was made |
| What a version pin keeps | the pin is dropped | nothing: a snapshot that is kept for long keeps what was freed since |
| Cells of a dropped Merk whose reclaiming a crash cut short | never: they are space and no state (`tree-layout.md` D2). `lignindb::verify` counts them (`Unreached`). | the Merk that was being reclaimed |

### 5.2 The order

**DF10. On a device with no free byte the engine gives no room back by
itself.** Every way in which one of the rows above goes begins with a
write: a table checkpoint and the manifest that names it, the copies of
live blocks, the record that ends a log file. `lignindb::space` says why
in full. The room for those writes is what a reserve is kept for (DF13).

**DF11. The order in which room comes back** once there is a little:

1. The writer has failed (DF4), and readers go on.
2. The reserve is released, if one is kept (`lignindb::space::Reserve`):
   by the session that keeps it (DF13), or by whoever keeps one beside a
   session that keeps none. Or an operator makes room (section 7.1).
3. Write-back takes its turn: the cut of the log, the pages that wait,
   and, where one is due, a table checkpoint. The log files that the
   checkpoint covers go.
4. The cleaner takes its passes: segments and value-log files that are all
   dead go, and live blocks are moved out of the others while the dead
   space is above its target. It asks for the table checkpoint that each
   deletion waits for, and the log before that checkpoint goes with it.
5. Commits are taken again from the cut on, which is in step 3: by the
   session that was refused (DF7), or, where it has ended, by the next one
   (DF8).
6. The reserve is filled again once commits are taken, a table
   checkpoint was installed and the cleaner is at rest (DF13).

The engine does steps 3 to 5 by itself, and steps 2 and 6 in a session
that keeps a reserve (`Options::reserve`, DF13). In a session that keeps
none, steps 2 and 6 are whoever opens the database's.

**A file that write-back gave up is one the cleaner can take in the same
session** (D-209). Where the device filled while write-back was appending to the
value-log file it fills, write-back gives the file up and the next value
goes to another file (`FlushStats::value_files_given_up`). The file's
writer syncs what the file holds, through the handle that appended it,
and is dropped after that sync has returned. The file is then closed
where it ends. The cleaner chooses among closed files, by rules that
hold for every closed file, so this one is among them with no close and
no open of the database in between.

- If that sync is refused as well, the file is closed at the length that
  the last sync that succeeded had made durable, and in this session it
  is neither appended to nor synced again: a sync that succeeded later
  would say nothing of the bytes the refused one was for. The same holds
  where the sync that covers the values of a pass is refused, and where
  the sync that was to close a file that is full is. The values behind
  that length are given up, and a later pass appends each of them that a
  cell still needs to another file. No manifest covers them, and a cell
  is given its pointer only behind a manifest that covers the value, so
  no cell points behind that length. What the device holds of the file
  behind that length is no part of the file as the engine reads it, and
  is not counted as its dead space: it goes when the file is dropped.
  The session goes on (D-246; the table of DF8).
- The cleaner gives up the value-log file for its own copies in the same
  way when a cleaning into it failed, whether a copy had been written by
  then or not (`CleanerStats::value_files_given_up`). It keeps the file
  after the one failure that says nothing against the file: a value of
  the file that is being cleaned whose owner was not found, of which no
  copy is made.
- Of a segment that write-back or the cleaner gives up, after a refused
  append or a refused sealing, a sync is asked at once. Where the
  segment holds blocks that no sync has covered, and no sync of it has
  failed, that sync is made through the handle that appended its blocks
  (`sync-handles.md`, S2). Where the request is answered `Ok`, after
  that sync or with nothing left to sync, the writer is dropped, and a
  segment with no writer is one the cleaner chooses from. Where it is
  answered an error, as it is with no call
  of the file layer for a segment whose sealing failed in its sync, the
  writer is kept, so that the blocks behind what is durable stay
  readable, and the session ends (DF8).

**DF12. A background loop waits after a turn that failed, and a refused
creation uses up no log file id** (D-204). Three threads work behind a
session: write-back's loop, the reclaimer's and the cleaner's. Each
counts what failed and goes on (`DbStats::background`). In this rule a
turn is one turn of write-back's loop, one round of the reclaimer's
thread, or one turn of the cleaner's loop, which makes two passes. A
turn has failed if it returned an error to its loop: a turn of
write-back (`WriteBack::tick`), a pass of the cleaner's turn, a call of
the reclaimer in its round. A page step of write-back whose record the
log refused is such an error of its pass, though the step is only put
off. A step that the private copy of a write transaction holds back is
none.

- **After a turn that failed, the loop waits before its next turn,
  whoever asks for one.** The wait is the loop's idle time after the
  first turn of a row of failed turns and twice as long after each
  further one, up to `FlushConfig::max_failed_wait`, and never shorter
  than the idle time. An idle time of less than a millisecond counts as
  a millisecond for these waits. As a session is configured by default,
  the idle time is 50 ms for write-back and 100 ms for the two others,
  and the bound is one second for all three.
- **Write-back's row is of turns that failed and moved nothing on.** A
  failed turn has moved something on if its pass did what no later turn
  has to do again: it wrote a page version to a segment, gave a value
  its place, or kept a superseded version for its readers and took it
  off what is owed. A log that was rolled, a file that was made, and a
  value that was appended and has no place yet do not count: a turn can
  do each of them in vain. After a failed turn that moved something on
  the wait is the idle time, and the row begins anew. So a session with
  one page that cannot be written (section 7.2), which fails every turn,
  goes on writing its other pages. The reclaimer's row and the cleaner's
  are of turns that failed, whatever else they did.
- **The end of the session ends that wait, and the release of the
  session's reserve does (DF13). Nothing else does.** After a turn that
  did not fail a loop is woken as before this rule, and a round of the
  reclaimer that left work at its bound of calls is followed by the next
  at once. After a turn that failed, whoever asks for a turn is given
  the one at the end of the wait. `FlushStats::turns_put_off` counts the
  waits of write-back's loop that lasted their time and in which a turn
  was asked for.
- **A session that releases its reserve has made room, and each of the
  three loops takes its next turn at once** (D-288). The thread that
  released the reserve tells the three loops before it does anything
  else. For write-back's loop and for the cleaner's the release ends one
  sleep: the one the loop is in, or the first one that it comes to, also
  where that is the wait after a turn that failed. For the reclaimer's
  thread it ends one wait in the same way, and the wake that comes with
  it can end one idle wait of that thread besides, as any wake does. A
  wait that begins later lasts as this rule says, and the
  loop's row of failed turns is as it was: if the turn after the release
  fails as well, the wait after it is the one that the row gives. With
  room the writer is taken at once (DF7), and the room of a reserve is
  given back for the turns of write-back and of the cleaner: so they do
  not wait for it.
- **The first turn that does not fail ends the row.** The loop sleeps
  and is woken as before it.
- **With room again nothing is to be done.** The wait that is running
  ends by itself, and the turn after it is made on a device with room.
  The cut of the log does not wait for that turn: the thread of the next
  commit makes the cut itself before it takes the commit lock (DF7).

The rule paces turns that fail. A turn that does not fail is as it was:
on a full device the rounds of a reclaimer that has nothing to reclaim
write nothing, do not fail, and go on at its idle time.

**What a failed turn of write-back leaves**, in the state in which its
loop did not wait before this rule: a value waits for its page step, and
the log takes no record until it is rolled. In the first three rows the
device takes no new log file. In the fourth it takes the file and keeps
what it refuses, and in the last it takes the file and not the record.

| The device | Log file ids | Names that no sync of a directory has covered |
| --- | --- | --- |
| refuses the creation of the file | None. An id is used up by a file that entered the log, and by a file that a creation found under its name. A file has entered the log once it is durable with its name: its sync, the sync of `wal/` and the sync of the temporary file of the manifest that is to name it are made as one set, and the file has entered when the set has been made, also where the replacement of the manifest fails after it. A creation that was refused takes no id, and neither does a file that was made and given up before it entered the log: the next creation asks for a file under the same id. | none |
| makes the file, refuses its header and takes the file away again: a device with no free byte | none, for the same reason | one creation and one removal of that one name for each failed turn |
| makes the file, refuses its header and does not take the file away | One for every two creations: the next creation finds the file under the id, which is then that file's, and the one after it asks under the next id. | one file for each of those ids. The next open removes them, and fails where the device refuses that as well. |
| takes the bytes of each record of the log and answers that it has no room | The log finds the start record of its next file whole in that file and turns to it, to a file that takes no item and is one of the log's. A roll makes six turns at most, and a roll whose last turn ends so has failed. So a failed turn of write-back uses six ids at most. | none |
| takes the log's files with their start records, and refuses the record of each page step | A failed turn that rolls the log uses one where the roll finds a file made ready for the log to turn to, and two where the roll has to make that file first, if no other thread attempts an append to the log during the turn: the roll gives the log a file, the page step is refused its record in that file, and the turn has failed by it. A record of another thread that is refused in that file during the roll can make the roll use a further file. | none |

**The rates.** In a row of failed turns that move nothing on, the wait
grows to its bound, and from then on write-back's loop makes one turn in
each such wait at most: with the default bound, one a second. That is
at most one creation and one removal of a name a second in the second
row, 1,800 ids an hour in the third, 21,600 in the fourth, and 3,600 in
the fifth where each failed turn uses one id. A shorter bound gives as
many more. These are the rates of
write-back's loop. A pass of the cleaner that asks for a table
checkpoint rolls the log as well, and its loop is paced by the same
rule. Nothing else that rolls the log is paced by it: the thread of a
commit rolls the log before the commit where an append had failed
(DF7), and so does a table checkpoint that a caller asks for, before
its mark.

Segments, value-log files and table checkpoints take an id for each
creation that was refused, as before this rule.

**The log's ids do not begin again.** After the last id (2³² − 2) the
log is given no further file (`LogError::FileIdsExhausted`), and the
session ends with a reason of its own (`Reopen::LogFileIdsUsedUp`):
reads go on and every commit is refused. Where a manifest has recorded
the last id as used, the next open fails for the same reason, since
every open begins a log file (DF9). A log that is given a file a second
comes there in 136 years.

**DF13. A session that keeps a reserve releases it when the device
refuses it something for room, and fills it again once room is back.**
`Options::reserve` is the length in bytes of the reserve that a session
keeps in its directory. With 0, which is the default, the session keeps
none: it asks the device for nothing on the reserve's two names
(`RESERVE`, `RESERVE.new`), and a file of either name that somebody else
made there stays as it is.

- **The open fills the reserve**, as its last step before it starts the
  background work: where the directory has no reserve of that length, it
  is written and made durable (`Reserve::fill`), and one that is there is
  kept. An open that cannot fill it, for room or for another failure of
  the file layer, is made all the same, and the session then fills the
  reserve by the rule below.
- **The session releases the reserve when it is given an error that a
  full device caused** (`lignindb::space::out_of_space`), by the thread
  that was given it and where the session is told of it: before the call
  returns, before the loop begins its wait (DF12), and, for the page
  steps that a commit's thread makes for write-back after its commit
  (`WriteBack::place_ready`), before that thread waits for write-back:

  | What is refused | Where |
  | --- | --- |
  | a commit, a write with no transaction, a wipe | the call returns the error (`Db::commit`, `Db::write`, `Db::wipe`, `Db::wipe_entering`) |
  | a flush, the creation of a checkpoint | `Db::flush`, `Db::checkpoint` |
  | a close, also the close that a drop of the handle makes | `Db::close`. A close can be given two errors, the cleaner's and write-back's, and answers with one: either of them releases the reserve. |
  | a turn of write-back | its loop; the page steps and the turns that a commit's thread makes for write-back after its commit |
  | a pass of the cleaner | its loop |
  | a call of the reclaimer | its thread, and `Db::reclaim` |

  The release removes the two files of a reserve and syncs nothing
  (`Reserve::take_away`): the reserve, and what a fill that was cut
  short left under the name `RESERVE.new`. Where it took a file off the
  device it is counted as a release, with the bytes of what it took
  (`DbStats::reserve`), and the thread that made it tells the three
  loops at once, so that each takes its next turn (DF12).
  An error that has no refusal of the file layer for room among its
  causes releases nothing: such is the answer to a commit whose own
  operations were not refused (DF4, third row). The turn of the
  background work that was refused its append releases the reserve
  there, when its error reaches its loop.
  What is no error of anything releases nothing either. After its
  commit a committer makes page steps for write-back
  (`WriteBack::place_ready`). That call says for which refusals of the
  log it puts a step off and returns no error: the session is told of
  none then, and the reserve is released where a later turn or call is
  refused for room.
  An operation of a write transaction that the device refuses is
  answered to its caller by the transaction, and the session is not
  given its error: so the append of a value of a load, which is made
  before the commit (`transactions.md`, LD2). The reserve is released
  where the commit of that transaction, or another call or turn, is
  refused for room.
  A file that the file layer does not let the release remove stays, and
  the other one goes all the same. `DbStats::reserve` keeps what the
  last such removal said, and the next refusal for room tries again.
- **After a release in which no removal failed, and until its next fill
  begins, the session asks the device for nothing on the reserve's names
  when it is refused.** So on a device that stays full, where no
  removal fails, the reserve is released once, at the first refusal for
  room that the session is told of, and the turns that fail after it
  (DF12) make no operation for it.
- **The session fills the reserve again from the cleaner's thread**, at
  the end of a turn of the cleaner's loop in which the cleaner was at
  rest, once both of these marks have moved on since the session was
  last told of a refusal for room, and since its last fill that failed:
  1. The latest version that the session has published: a commit was
     taken (step 5 of DF11 comes before step 6).
  2. The session's count of the table checkpoints that it installed
     (`FlushStats::checkpoints`): write-back has caught up to one.
     Write-back counts a table checkpoint once its manifest is in force,
     so a checkpoint whose manifest came into force just before the
     session was told, and that is counted after it, moves the mark.

  The two marks are read when the session is told of the refusal, under
  the lock of the reserve, and again after a fill that failed. A session
  whose open could not fill the reserve counts from that fill.

  A table checkpoint is installed when write-back finds one due
  (`WriteBack::checkpoint_due`; by default once 64 MiB were logged
  since the last one) or the cleaner asks for one. So a session that
  takes few writes can be without its reserve for as long as it lasts.
  The next open tries the fill at its end, as every open does.

  The cleaner was at rest in a turn if no pass of the turn failed, and
  none of the cleaner's counts moved in it but those of its passes, of
  its reads of the page table and of the links it trimmed
  (`CleanerStats`). It copied no block and no value, sealed and closed
  no file, retired and dropped none, and asked for no table checkpoint.
  A reserve that is taken back while the cleaner still works takes the
  room that the cleaner's copies need.
- **A fill that fails is counted, and takes away what it wrote**, as far
  as the file layer lets it (`Reserve::fill`). The session does not hold
  its reserve then. What the fill could not take away stays on the
  device, and so does a reserve that the fill found at its length and
  could only not make durable by its name: the next refusal for room
  takes that off the device as it takes a reserve that is held, and
  counts a release where it found a file. The next fill is tried by the
  same rule, with the two marks read after the fill that failed.
- **One call of the reserve is made at a time.** A release waits for a
  fill that is being written, and so does the call or the turn that was
  refused. A close waits for it as well: it waits for the turn of the
  cleaner's loop that makes the fill.
- **A fill takes the room it fills, and the session does not know how
  much room there is.** While a fill writes, its bytes take room, up to
  the length of the reserve. A commit or a turn beside it can be refused
  for that room. Its refusal waits for the fill, releases what the fill
  made, and puts the next fill off. So on a device whose free room is
  about the length of the reserve, a fill can cost calls and turns that
  are refused beside it, with what such a refusal brings (DF4 to DF8).
  The next fill is tried after a further commit, a further table
  checkpoint and a turn of the cleaner at rest.
- **An open that is refused for room releases no reserve**: no session
  is there yet. Whoever opens the database removes the file `RESERVE`
  (section 7.1), and the open that follows fills it again at its end, if
  there is room for it then.
- **The session goes by what it did, and not by the file.** A reserve
  that somebody removes while the session holds it is found gone at the
  next refusal for room, which then counts no release, and the session
  fills it again by the rule.

`DbStats::reserve` says what the session keeps and holds, how often it
was given an error that a full device caused, how often it released and
filled its reserve, how many fills failed, and what the last failure
said.

### 5.3 How much room is a little

**What has to be written before the first byte comes back:**

| Step | Needs room for | Then gives back |
| --- | --- | --- |
| The cut of a failed append | the header of the next log file, and a manifest that names it | nothing: commits are taken again |
| A table checkpoint | the blocks of every page that waits for write-back (committers are made to wait at `FlushConfig::max_unwritten` page versions), the checkpoint itself, and a manifest | the log files before it |
| A pass of the cleaner over a segment or a value-log file | the copies of what is still live in that file | the whole file, once the table checkpoint it asks for is in force |
| An open (DF9) | a manifest, the header of a new log file, and the end of the last log file that was not synced, written again | nothing by itself |

So a database in which much is dead needs little, and one that is nearly
all live needs about as much as its largest file to empty one.

**Measured** (`db::tests::room`, on the simulated device with a capacity
in bytes, at head `e4e8bbf6` with the reserve of DF13): a database with
pages of 4 KiB, log files of 96 KiB and segments of 256 KiB, in a session
that keeps a reserve and has none of its threads. It is written, with
write-back's turns after every commit and a cleaner that makes no turn,
until the device first refuses the session something for room. That is a
turn of write-back after 7 commits, with 610 KiB in the files of the
database: 501 KiB of values of which 109 KiB are live, 33 KiB of
segments, 75 KiB of log. The session has released its reserve there
(DF13). What else is free is taken away, so that the bytes of the reserve
are all the room there is. Then the turns of the session's loops are made
in the order of DF11: write-back's turns, twelve turns of the cleaner
with write-back's after each, a commit, and twelve more of both.

| Reserve that the session keeps | What happens |
| --- | --- |
| none | nothing comes back in three rounds of write-back, cleaner and commit; every commit is refused; the session goes on and reads |
| 1, 4, 16, 32 KiB | nothing comes back: write-back's values and pages and the cleaner's new files take the room before anything is dropped. The commit is refused, and the reserve is not filled again. |
| 64, 128, 256, 384, 512 KiB and 1 MiB | 483 KiB come back; the session takes the commit, and fills its reserve again |

Of the 483 KiB, 39 came back in write-back's turns and 444 in the
cleaner's after them. The log went from 75 KiB to 1 KiB, the value log
from 501 KiB to 109 KiB, and the segments from 33 KiB to 15 KiB. The
value-log file that write-back was filling when the device filled was
given up there and closed (DF11), and the cleaner emptied it.

**What follows for the size of a reserve.** Of the sizes that were tried,
32 KiB was not enough and 64 KiB was; no size between them was tried.
64 KiB is 16 pages of that database and 10% of its files. It is a number
of that database and of that order of turns, and not a rule.

**The writer takes the room of a reserve as write-back and the cleaner
do.** Nothing holds a commit back while the reserve is out. With room,
the thread of the next commit makes the cut of the log itself and commits
(DF7), and the three loops take their next turn at once (DF12): the
writer and the loops come to the room at the same moment.
`db::tests::reserve` has two runs with a writer, each at every size
from 1 KiB to 1 MiB of the table above. In both the writer commits when
the session needs a commit, since a fill waits for a commit and for a
table checkpoint that follow the last refusal for room (DF13): once
after the release; again where its commit was refused; where the
session was given an error that a full device caused, or a fill of its
failed, after the writer's last commit began; and, in the second run,
where that commit made no table checkpoint due and write-back has
nothing to write. Whenever 40 commits in a row were refused, somebody
makes room. In both runs the session holds its reserve when its threads
have ended.

**In the first**, the session of the measurement is full as above: it
is at rest, and it has been told of every refusal. From that moment its
three loops run as its threads run them, beside that writer, whose
commit is a group of 72 KiB.

- With a reserve of 384 KiB or more the session gets by with its
  reserve and with the writer's one commit. Nothing is refused after
  the release, so what is written before the room is back is that
  commit, what write-back writes for it, and what write-back and the
  cleaner had to write when the device filled, in whatever order the
  threads take their turns. The run fails otherwise, and it failed in
  no run that was made.
- With a smaller reserve the schedule decides. Where the writer's
  commit is taken before write-back's turn, it takes 72 KiB of the
  reserve, and with 128 KiB write-back then has 56 KiB for 72 KiB of
  values and the cleaner has nothing: the device stays full, with the
  reserve gone, until somebody makes room.

**In the second**, a database is opened with its three threads and fills
its device, while they run, with values that are live: its writer waits
for write-back between two commits. At the first refusal the session
releases its reserve. The writer's next commit takes those values out
again, and its commits after that rewrite others. With the values out
the cleaner gives their room back, and the session fills its reserve
again, with the room that it had or once somebody had made room. Which
of the two is the schedule's to say at every size: the threads were in
their turns when the reserve was released, so what the session is told
after it decides how often the writer commits.

So a reserve covers what the table of this section names, and what the
writer commits before write-back and the cleaner have made their turns.

For a node nothing was measured. What a reserve has to cover there: the
pages that can wait for write-back, one table checkpoint, the live part
of the file the cleaner empties first, and the commits that are taken
meanwhile. No configured file length bounds these: a table checkpoint
grows with the number of pages, `FlushConfig::max_unwritten` is where
committers are made to wait for the pages that wait, and no bound on
their bytes, and the bytes of a commit are its caller's. As a first
estimate, and no more than that, this document suggests one log file plus
one segment at their configured lengths (64 MiB each by default), until a
measurement on a database of a node's size replaces it (PERF-08). For the
database of the measurement that estimate is 384 KiB, with the table
checkpoint and the manifest beside the two files.

**The default is no reserve**: `Options::reserve` is 0.

- A reserve is written and made durable once, by the open of each
  directory that keeps one. A process that opens many databases, as a
  node opens every checkpoint it makes (P-15), would write one into each.
- No length is right for every database. The estimate above is for a
  database that takes writes, and it is not measured.
- A session that keeps none makes no operation on a file that it did not
  make before the reserve was wired: the counts of section 9 are those of
  sessions that keep none.

Whoever opens the database that takes the writes of a node sets the
length.

## 6. Backup

### 6.1 What a backup is

**BK1. A backup is a database directory that shares no file with the
database it is of**, made from a directory that nobody writes while it is
copied. Opening it is the normal open (CK7), and it opens at one committed
version.

**BK2. A consistent backup of a database that is in use is made from a
checkpoint.** A checkpoint is one committed version (CK1), durable when
its creation returns (CK4), and a directory that nobody writes until it is
opened. It is made in the time of a few hard links and holds back no
commit (CK5). By itself it is no backup: it is the live database's own
files under a second name, on the same device.

**BK3. A database that is closed is backed up by copying its directory.**
No checkpoint is needed for a node that is stopped.

**BK4. Never the directory of a database that is open.** Its manifest and
its files change under whoever reads them, and a copy of that is no state
of it.

### 6.2 The procedure

1. **Make a checkpoint for the backup** in a directory of its own with
   `Db::checkpoint`. The GroveDB LigninEngine adapter does not yet
   implement `create_checkpoint`. Do not open it. A node that opens every
   checkpoint it makes (P-15) makes one more for the backup.
2. **Copy the checkpoint's directory to where the backup is kept**, with
   `lignindb::backup::Backup::copy`, or with any tool that copies a
   directory with its `LOCK` file (`cp -a`, `rsync -a`, `tar`).
3. **Delete the checkpoint** (`lignindb::checkpoint::delete`, or by
   removing its directory, P-18). Until it is deleted it keeps the bytes
   of every file the live database has let go of since (section 5.1).

**BK5. What the engine's copy does that a tool's does not.** It takes the
lock of the source for as long as it runs, and refuses a source that a
handle has open. It copies each file as far as the manifest says the file
is the database's, and none of what the live database has appended behind
that since. It makes each file, each name and the target's own name
durable before it writes the manifest, last, so a copy that is cut short
is no database. It reads the copied log to its end and says which version
the backup holds. And it takes away what it made when it fails.

**BK6. A tool's copy is consistent under the same condition**: nobody has
the directory open while it is copied, and nobody opens it before the copy
is complete. It holds bytes behind the valid lengths of the files the
checkpoint shares with a database that goes on writing. They are no part
of the backup, and a recovery reads none of them (`segment-manifest.md`
M15). **It is durable only once it was synced**, by the tool where it has
an option for that or by a `sync` after it: a tool that returns has
written the copy, and a power cut right after can lose it or leave a part
of it.

**BK7. Nothing of the source is synced.** The log files of a checkpoint
are the live database's own, and a sync of one that fails through another
handle or another name is invisible to the log that writes it
(`lignin_store::wal`). Whoever copies a checkpoint reads it, and asks the
live database for durability where it needs it (`Db::flush`). So the
target of a copy is outside its source: the engine's copy refuses a
target whose path is inside the source's (`BackupError::TargetInSource`).
It compares the two paths as they are given, whatever the file layers
they are of, and does not see a target that is inside the source by
another name for it.

### 6.3 Restoring

1. Copy the backup into a directory that is not there, with the same call
   or the same tools.
2. Open it. It is a database like any other, at the version the backup
   holds. (A backup of version 0 is of a directory in which a database
   was being made and never was. Its open makes the database, as the next
   open of that directory would have.)

**BK8. A database directory has its `LOCK` file, and an open makes none
in a directory that holds a database** (`OpenError::NoLockFile`, D-139).
The engine's copy makes the file. A copy by a tool that left it out is
given an empty file named `LOCK`, by someone who knows that no handle has
the directory open.

**BK9. What a backup does not hold**: the files of the source that its
manifest does not name, a reserve (section 5), and what somebody else
keeps in the directory (P-03), unless whoever makes the backup names it
(`Backup::carrying`), as a node names the state it keeps beside a
checkpoint (P-17). The copy refuses a name that is no plain name of a
file in the source, the manifest's and the lock's
(`BackupError::CarriedName`). A file that it takes is carried as it is:
one whose name has an open refuse a directory as another engine's
(`CURRENT`, `IDENTITY`, `MANIFEST-…`, `OPTIONS-…`, an `.sst` file) has it
refuse the backup as well.

**BK10. A backup that is to be relied on is checked once.** The copy
reads the log, and not the pages. The physical check (`lignindb::verify`)
reads every page and every value: it is run on the checkpoint before it is
copied, or on a restore of the backup. Either opens the directory it
checks, which writes to it as every open does, and leaves it a database of
the same version.

### 6.4 A target that was left without a manifest

**BK11. A directory with no `MANIFEST` is no database, no checkpoint and
no backup.** A copy writes the manifest last. One that is cut short by
the power can leave no target, a target without a manifest (empty, with
its `LOCK` file alone, or with some of its files), or the whole copy. One
that fails and cannot take away what it made can leave a target without a
manifest as well.

A creation of a checkpoint leaves no such target. It builds the checkpoint
beside the target, in a directory of the target's name with `.creating`
added, and gives that directory the name of the target only once
everything in it is durable (`lignindb::checkpoint`, "Durability, and a
creation that fails"). So a creation that the power cuts leaves nothing at
the target, or the whole checkpoint. A part of a checkpoint that a creation leaves, when
it is cut short or when it fails and cannot take away what it made, is
under that other name. A directory of that name is not to be opened as a
database: the engine's open refuses a path whose last name ends in
`.creating`, in any case of its ASCII letters (`OpenError::Creating`). It
is for a later creation at the same target to take over, or for
`lignindb::checkpoint::delete` to remove: that module says when each does.

One failure leaves the whole target instead. For a copy, the manifest was
written, and then its name could not be made durable, and the manifest
could not be taken away again, or the power was cut before its removal was
durable. For a creation, the directory was given the name of the target,
and that could not be made durable; then the name could not be taken back,
or the power was cut before that was durable. Every file that the manifest
names was durable before, so that target is the whole checkpoint or the
whole copy, and never a manifest with a file missing. A call that failed
answered an error all the same, and a call that the power cut answered
nothing: either way the target is removed like the others, and the call
is made again.

- Nothing opens such a directory as the database it was to be. An open
  that may create a database makes a new and empty one in a directory that
  holds nothing of a database yet: so no open that may create is pointed
  at the target of a creation or of a copy before that call has returned
  `Ok` (`lignindb::checkpoint`, "What other tasks take from here").
- **A target that was so left is removed by whoever asked for it**, with
  everything in it, and the creation or the copy is made again: both
  refuse a target that is there. Nothing is lost with it. Its files are
  the live database's under a second name, or copies.
- `lignindb::checkpoint::delete` removes an empty directory and one that
  holds its lock file alone. Of a directory whose name does not end in
  `.creating`, it refuses one that holds files and no manifest, because it
  cannot tell it from a database that lost its manifest: that one an
  operator looks at before it goes.

A directory that holds `MANIFEST.deleting` is one whose deletion was cut
short. `delete` finishes it.

## 7. For an operator

### 7.1 The device is full

**What is seen.** Commits fail, and their error says that the device is
full (`lignindb::space::out_of_space`): the node stops at a block. Reads
are answered. `DbStats::background` counts turns of write-back that failed
and keeps the last error as text. While turns of the background work
fail, each loop waits between them (DF12). In a session that keeps a
reserve, `DbStats::reserve` says whether it was released (DF13).

**What is true then.** Nothing that was made durable is lost, nothing
that was committed is damaged (DF1), and nothing needs a repair. No file
of the database directory is to be removed by hand: each one is needed,
or is taken away by the engine when it is not. The one exception is the
reserve, the file `RESERVE`, which is there to be removed, with
`RESERVE.new`, which is what a fill of it left that was cut short.

**What to do**, in this order:

1. **Make room.** A session that keeps a reserve releases it at the
   first refusal for room that it is told of (DF13), and that room can
   be used up by now: `DbStats::reserve` says whether the session holds
   one, how often it released one, and what the last fill or removal
   that failed said.
   A reserve that nobody released, of a database that is not open or
   whose session keeps none, or one whose release failed, is released by
   removing the file `RESERVE` from the database directory, and
   `RESERVE.new` with it, which holds what a fill wrote that was cut
   short (`Reserve::release` removes both). If there is none, or its
   room was not enough, free something else on the device:
   - delete checkpoints that are no longer needed
     (`lignindb::checkpoint::delete`, or remove their directories): a
     checkpoint keeps every file the database has let go of since it was
     made (section 5.1);
   - end what holds old versions: a snapshot that was kept for long, a
     write transaction that was left open;
   - move away what is not the database's.
2. **Ask the session** (`Db::reopen`). An application does this by
   itself: it is what a commit answers.
   - It says nothing: the session goes on. The next commit is taken, and
     the background work gives back what is dead (step 3).
   - It says that the session has ended (DF8): close the database and
     open it again. A sync of the log or of a segment was refused while
     the device was full, and that stays failed for the session. Nothing
     that was made durable is lost by it. A commit that was published
     after the newest durable version is in the next open if the system
     still has the log whole up to it, which it does not promise after a
     sync that was refused (DF1): the open can land on an earlier commit
     than the last one with no power cut, and `Db::version` says where
     it landed. After a power cut it has what DF1 says.
   - **A checkpoint that fails with "a replacement of the manifest ... did
     not finish"** can be made again: a creation has the manifest settled
     before it makes a directory (DF8a). One that fails with "the manifest
     was in doubt, and settling it answered an error" can be made again as
     well. Asking `Db::reopen` settles nothing. The session has not ended
     for either.
   - **A commit that stays refused with "the session takes no commit
     yet" although the device has room** names what keeps the log from
     being cut or the manifest from being opened again: a directory that
     was removed or cannot be written. Close the handle; nothing that was
     published before is lost by that.
3. **Let the engine give back what is dead**: write-back takes a table
   checkpoint and drops the log files it covers, and the cleaner drops the
   segments and value-log files that are dead and moves what is live out
   of the others. The counts to watch are the log that is held and the
   dead space of the segments and of the value log
   (`lignin_store::stats`).
4. **The reserve is filled again.** A session that keeps one fills it
   by itself (DF13). Whoever keeps one beside a session that keeps none
   fills it (`Reserve::fill`) once the dead space is within its target,
   and not before (DF11 step 6).
5. **If the device is full of data that is live**, steps 1 to 3 give back
   less than the writer needs. The database needs a larger device: stop
   the node, back the database up or copy it (section 6), and restore it
   where there is room.

**A database that does not open on a full device** (DF9) needs the same
step 1, and opens then. Its reserve is released by removing the file: an
open that is refused releases none (DF13). An open that was refused has
changed nothing that the next one does not expect.

### 7.2 The log grows and does not shrink

The log is kept from the place of the last table checkpoint on, and
write-back bounds it (`FlushConfig::max_log_held`). Three things take the
bound away, and the first two are seen in the counts of
`lignin_store::stats` (the log that is held, the pages that could not be
written, the oldest open write transaction):

- **An open write transaction holds the log** (D-091 item 1). A page that
  a write transaction has copied cannot be written back, a table
  checkpoint waits for it, and so the log is kept from the commit before
  that copy for as long as the transaction lives. Platform's write
  transactions last one block. A transaction that an application forgot
  is committed or rolled back, and the log goes at the next checkpoint.
- **A page that cannot be written makes the log grow until the database
  is opened again** (D-091 item 7). Write-back reports the page at every
  turn (`DbStats::background`), writes every other page, and takes no
  table checkpoint that would cover it. Every commit is in the log and is
  recovered from it. Close the database and open it again: the recovery
  builds the page from the log, and write-back writes it in the new
  session. If the same page fails there again, the error names it: that is
  a defect to report with what `lignindb::verify` says, and the database
  is restored from a backup (section 6.3).
- **A session that has ended** takes no write, so its log grows no more:
  the engine ends a session whose table checkpoints cannot be installed
  any more (DF8) instead of letting its log grow.

### 7.3 Space that does not come back

- **A checkpoint that is kept** keeps the files it shares with the live
  database (section 5.1). Delete it when its backup is made (section 6.2).
- **A crash while the cells of one dropped Merk were being reclaimed
  leaves the rest of that Merk's cells in their pages**: space, and no
  state (D-093 item 3). No reader reaches them, and nothing is wrong with
  the database. `lignindb::verify` counts them as cells that no root
  reaches. The engine does not give them back in this iteration: that
  needs a durable place for where the reclaiming stood. A backup carries
  them, and a database that is built again from its data has none. It is
  at most the rest of one Merk's cells for each crash at that moment.
- **A target that was left without a manifest** by a backup that was cut
  short, and a directory that a creation of a checkpoint left beside its
  target, are dealt with as section 6.4 says.

### 7.4 Backup and restore

| To | Do |
| --- | --- |
| back up a database that is in use | make a checkpoint, copy its directory, delete the checkpoint (section 6.2) |
| back up a database that is stopped | copy its directory, with its `LOCK` file (BK3) |
| know that a backup is good | run `lignindb::verify` on the checkpoint before the copy, or on a restore (BK10) |
| restore | copy the backup into a directory that is not there, and open it (section 6.3) |
| deal with a directory that has no `MANIFEST` | remove it and make the checkpoint or the copy again (section 6.4) |
| deal with a restored directory that answers `NoLockFile` | the copy left `LOCK` out: put an empty file of that name there, with no handle open (BK8) |

Never copy the directory of a database that is open (BK4), and never sync
a log file of a checkpoint through a tool: the live database's log does
not see a sync that fails through another name (BK7). `cp`, `rsync` and
`tar` only read their source. They do not make the copy durable by
themselves: sync the copy before it is relied on (BK6). `Backup::copy`
returns when its copy is durable (BK5).

## 8. Choices made here that the plan left open

1. **A reserve is a file of written bytes in the database directory**
   (`RESERVE`, written as `RESERVE.new` until its bytes are durable;
   `lignindb::space::Reserve`), and not a number the engine keeps free.
   A file's bytes are there when they are given back, whatever else
   filled the device, and the file layer is asked nothing about free
   space, which it cannot answer for every device. The price is that the
   bytes are written once when the reserve is filled. Its size is whoever
   opens the database's to choose (section 5.3).
2. **The session says whether it goes on** (D-151, D-157). A refusal that
   involved no sync of the log or of a segment leaves it usable, and such
   a sync ends it, whichever step met it, under every sync policy. The
   engine derives that from two states the store keeps, at the one place
   that answers for the session (`Db::reopen`, and every commit before it
   begins), so no step that is added later has to remember to say it. For
   this the log says whether a sync of it failed
   (`LogStore::sync_failed`, D-153), and `Reopen` has one more reason
   (`SegmentsNotDurable`). The session gives one reason, the first that
   was seen there. A manifest in doubt is no such state: a commit before
   it begins and a close have it settled, by the call write-back makes
   for itself (`WriteBack::settle_manifest`), and a look at the session
   settles nothing.
3. **A session keeps a reserve where it is asked to, and none by
   default** (`Options::reserve`, DF13; the reasons for the default are
   in section 5.3). It releases the reserve at the first error that a
   full device caused, wherever the session is given one, and fills it
   again from the cleaner's thread. Two things were chosen in that rule.
   - "The cleaner is within its target" (DF11 step 6) is read as "the
     cleaner was at rest in a turn of its loop". The share of dead bytes
     of a store stays above the cleaner's target where what is dead is in
     a file that the cleaner does not take: one that is being filled and
     holds less than `CleanerConfig::min_segment_len` is not closed for
     it. A reserve that waited for that share would not be filled again
     in such a store.
   - Nothing holds the writer back while the reserve is out. DF11 has
     commits taken from the cut on, and the session keeps to that. What
     it costs is in section 5.3.
4. **A backup is a copy of a directory that nobody has open**, made by
   `Backup::copy` under the source's lock, and a backup of a database in
   use is a copy of a checkpoint. There is no backup that reads a live
   database's files beside its writer: a checkpoint is made in the time of
   a few hard links, and what a copy of it reads is fixed.
5. **The copy is cut at the manifest's lengths, and the log is read to
   say the version.** A file that the manifest calls closed or sealed is
   copied up to the length the manifest vouches for, so nothing that the
   live database appended to a shared log file since the checkpoint is
   carried. The file the checkpoint's log ends in is copied whole, and
   the copy's log is read to its end, which says the version and fails
   the copy if the log does not read.
6. **A copy that fails takes away what it made, manifest first.** Where
   it cannot, the target is left without a manifest, or, if a manifest
   was written and could not be taken away, with every file the manifest
   names. It is never a database with a file missing.
7. **The simulated device has a capacity in bytes**
   (`SimVfs::set_capacity`, in `lignin-io/src/sim/budget.rs`): an append
   or a write that would take the files beyond it is refused as a full
   device refuses it. It is what makes "full" a state of the device in a
   test, where a fault rule makes it a property of one operation.

## 9. Code and tests

| What | Where |
| --- | --- |
| `out_of_space`, `Reserve` | `lignindb/src/space.rs` |
| `Backup::copy`, `BackupError` | `lignindb/src/backup/` |
| The session's end as the store has it (DF8): `Reopen::SegmentsNotDurable`, the look before and after every commit | `lignindb/src/db/error.rs`, `store.rs` (`Store::refusal`), `mod.rs` (`Db::refused`, `Db::settle`); `CommitState::try_lock` |
| A manifest in doubt that is settled (DF8a) | `Shared::settle_manifest` in `lignindb/src/db/store.rs`, `Db::refused`, `Db::finish`; `Checkpointer::create` in `lignindb/src/checkpoint/create.rs` |
| Whether a sync of the log failed | `LogStore::sync_failed`, `lignin-store/src/wal/store.rs` |
| The settling of the manifest, for whoever answers for the session | `WriteBack::settle_manifest`, `lignin-store/src/flush/write_back.rs` |
| The capacity of the simulated device | `lignin-io/src/sim/budget.rs` |
| The wait after a failed turn (DF12) | `FlushConfig::wait_after_failure` and `WriteBack::run` in `lignin-store/src/flush/`, `Cleaner::run` in `lignin-store/src/cleaner/run.rs`, `reclaim_loop` in `lignindb/src/db/reclaim.rs` |
| The release that ends that wait (DF12) | `WriteBack::room_made` in `lignin-store/src/flush/run.rs`, `Cleaner::room_made` in `lignin-store/src/cleaner/run.rs`, `Signal::room_made`, `Shared::room_made` and `Troubles::refused`, which calls it, in `lignindb/src/db/store.rs` |
| The log's file ids (DF12) | `LogStore::create` and `LogStore::file_ids_used_up` in `lignin-store/src/wal/store.rs`; `Reopen::LogFileIdsUsedUp` |
| A value-log file that is given up (DF11) | `WriteBack::give_up_value_file` in `lignin-store/src/flush/write_back.rs`; the cleaner's in `lignin-store/src/cleaner/values.rs` |
| The reserve of a session (DF13) | `lignindb/src/db/reserve.rs`; `Options::reserve`; `Troubles` in `lignindb/src/db/store.rs`, which each failed turn of the background work is told to; `Db::answered` and `end_turn` in `lignindb/src/db/mod.rs`; `Reserve::take_away` in `lignindb/src/space.rs` |
| What a close answers after a pass of the cleaner's thread lost its segment (DF8) | `Troubles::cleaner_loop_failed` in `lignindb/src/db/store.rs`, `Db::finish` |

**The acceptance test** is the sweep of `lignindb/src/db/tests/full.rs`.
One session through the facade is scripted: a database is made, written
with and without a transaction, written back, flushed, checkpointed,
reclaimed after a drop, wiped, closed and opened again: 528 file
operations, 79 of them syncs, and 543 with 94 syncs under a policy that
syncs every commit. It is run with no fault, and then once for each file
operation that a device can refuse for room, with the device full at that
operation.

A database of the sweep is opened with none of its background threads,
and the script makes every turn of write-back and of the reclaimer; the
cleaner makes none. Nothing but the script and the device then decides what a run
does: each run makes the file operations of the run with no fault up to
the one that is refused, so the operation that is refused is the one at
its place in that run. A run that came to its refusal by other operations
fails the sweep, which names the first operation that differed, and so do
two runs with no fault that differ. No run is made again.

A thread that `Db::open` starts can make its first turn before a test
ends it, and either thread's turn shows in what the session does next:

- The reclaimer's first round frees the page ids that the open took as
  retired, which an earlier session had freed. The first commit of the
  session can then give a new page one of them. In the script that is the
  root of a Merk: the subtree directory keeps a record under the root's
  address, which then sorts before the record of another Merk and not
  after it, and the block of the directory's page is one byte longer.
- Write-back's first turn makes the file operations that the script's
  first turn makes otherwise, and meets what the device refuses of them
  before the session is looked at.

Nine sweeps are made. No background thread and no clock decides anything
in them, and the counts were the same in every run that was made of them,
on Linux x86-64 and on macOS aarch64. They are the counts at head
`48c69be1` plus this task, and the same were counted again at head
`b37f322d` with `STORE-06-F3` (DF12) and at head `e4e8bbf6` with the
reserve of DF13, in sessions that keep none: a change of what the store
does to its files at an open, a close or a turn of write-back changes
them.

| The device | Sync policy | Operations refused |
| --- | --- | --- |
| refuses one operation | no sync at a commit | 367 |
| refuses one operation | a sync at every commit | 382 |
| is full from the operation on, syncs included | no sync at a commit | 367 |
| is full from the operation on, syncs included | a sync at every commit | 382 |
| is full from the operation on, and syncs work | no sync at a commit | 288 |
| refuses one operation, and loses what a refused sync had not made durable | no sync at a commit | 367 |
| is full from the operation on, and loses the same | a sync at every commit | 382 |
| refuses one operation and loses the same, and write-back makes its next turn before the session's next commit | no sync at a commit | 367 |
| refuses one operation and keeps the same, and write-back makes its next turn first | no sync at a commit | 367 |

At each of the 3,269 operations:

- the session that met the refusal is looked at before anything ends it,
  and the session that is open when the script has ended is looked at
  again: every reader reads the latest published version, whole (DF1,
  DF5); `lignindb::verify` finds the store sound as it is, with the device
  still full (DF1); and whatever a power cut at that moment leaves opens
  at a version that the answers allow, with that version's records, and
  is sound (DF1, DF2);
- after such a power cut an open on the device that is still full is
  refused for room or made, and the open after it lands where a second
  recovery of the same cut opens with nothing refused (DF4);
- a checkpoint that was made is the database of its version, by a backup
  that is copied from it and opened, and what a refused creation left is
  no database, or the whole checkpoint (BK2, BK11);
- every call that failed was refused for room, or the session had said
  that it had ended; and no call is answered with what stays failed while
  `Db::reopen` says nothing (DF8);
- with room again the session that met the refusal takes a commit, or it
  has said that it has ended (DF7, DF8). It is asked at once where one
  operation was refused, and, where the device stays full, with room for
  that one commit before the script closes it. The session that is open
  when the script has ended is asked again, and where there is none, or
  it has ended, the session after it takes the next commit. A session
  that was opened after the refusal is not counted as one that resumed.
  284 sessions of each sweep that refuses one operation resumed where
  they were, 160 where the device stays full and syncs work, and 3 and 7
  where syncs are refused as well, under the two policies. A sweep in
  which none did fails;
- no session has ended for a replacement of the manifest that was refused
  once (DF8a), whether the session's next commit or write-back came first:
  36 runs of each sweep that refuses one operation;
- nothing of the background work failed while the device had refused
  nothing;
- write-back, a flush and a close work again, and the database closes and
  opens once more as what it was.

| Test | What it shows |
| --- | --- |
| `db::tests::full` (6 tests, 9 sweeps) | the above, at every operation |
| `db::tests::ended` | each of the two states of DF8, met by a step that is no commit and no flush, ends the session; a `flush()` still makes durable what was published; the look waits for no commit, makes no operation on a file and settles nothing; a session gives the first reason it had, also where the commit path stops it while the store's reason is being kept; a manifest in doubt is settled by a commit, by a close and by the creation of a checkpoint, and ends no session; a table checkpoint is installed though the replacement for its turn of the log was refused; a close whose table checkpoint is refused the start record of the log's next file once does all a close does, whether the device kept the record or not; a commit or a creation that cannot have it settled is refused and can be made again; a commit beside a holder of the manifest is made (DF8a) |
| `db::tests::clean` (four of its tests) | a sync of the cleaner's segment that was refused in a pass ends the session, which says so before a close or write-back has met the segment (DF8); a close that is the first to be refused the sync of the cleaner's segment answers with the cleaner's error, which has the device's refusal, and so does a close after a pass of the cleaner's loop that was refused it; a close whose cleaner was refused for room and whose write-back failed for something else answers with write-back's error and has released the reserve (DF13) |
| `db::tests::room` | DF10, DF11 and DF13 on a device with a capacity in bytes, in a session at rest: a session that keeps a reserve has released it at the first refusal for room; in one that keeps none nothing comes back; with the room of the reserve write-back and the cleaner give back what is dead, the session takes the next commit and fills its reserve again; the least reserve that is enough in that order of turns (section 5.3) |
| `db::tests::reserve` | DF13. An open fills the reserve where the directory has none of its length, writes none again that is there, and is made where the fill is refused. Each call of the facade that a full device refuses releases the reserve (a commit, a write, a wipe, a flush, the creation of a checkpoint, a close, a drop), and so do a turn of write-back, a pass of the cleaner and a call of the reclaimer, told as their loops tell them, and each of the three loops running alone. An error that is not for room releases nothing, and after a release in which no removal failed the session asks the device for nothing on the reserve's names. A removal that the file layer refuses is tried again at the next refusal for room; a reserve that a fill left under its name without making the name durable goes there as well, and so does what a fill that was cut short left under the other name; after a fill that left nothing no release is counted. The fill waits for a commit and a table checkpoint that followed the last refusal that the session was told of, in either order, and for a turn in which the cleaner was at rest, which can be the first turn after them, also where a pass of the cleaner over the value log met the refusal; a fill that fails is tried after a further commit and a further checkpoint. A release ends the wait after a failed turn of each loop, which is an hour in these tests: write-back's loop and the cleaner's come to a second turn where either released the reserve, the reclaimer's thread comes to a second round, and the cleaner's loop takes its next turn where a commit that was refused released it, where a turn of write-back that was refused is told to the session, and where the turn that a commit's thread makes for write-back is refused (DF12). A call that room was made before the reclaimer's thread can be woken ends its first idle wait. On a device that stays full, under a database with its three threads and at every size of the ladder of section 5.3, the reserve is released once, no fill is tried, and write-back's failed turns are those its waits allow (DF12). Each loop that released the reserve takes its own next turn at once. A turn at rest is one that moved no count of the cleaner but its passes, its reads of the page table and its trims. The acceptance, at every size of that ladder: a database that is opened with its three threads fills its device, the session releases its reserve at the first refusal, the cleaner gives room back, commits are taken, the session fills its reserve again, and it holds its reserve when its threads have ended, with the room that it had or once somebody had made room. The same from a device that a session at rest had filled with a cleaner that was behind, with the three loops running from then on beside a writer: there, from 384 KiB up, with the room of the reserve alone, one commit of the writer and no refusal after the release. |
| `db::tests::filling` | random fills under a database whose background threads run, with the same model: 12 seeds of 40 rounds, in which the device fills in each of the four ways and has room again |
| `db::tests::backup` | a backup from a checkpoint of a database that is written meanwhile is the database at the checkpoint's version (BK2): the copy waits, with every file measured and none copied, until 5 commits were made beside it; a restore of it takes writes |
| `backup::tests::copy` | BK1 to BK11: closed and killed sources, refusals (a target inside the source among them), the valid lengths, carried files, a target that cannot be listed, a database that was being made, cut at each operation of its creation, a full device and a power cut under the target at every operation |
| `space::tests` | `out_of_space` through the errors of the crate; a reserve that is filled, released, cut short and filled on a full device; a fill that fails or is cut at each operation, and the fill after it, which makes the reserve whole and durable; no block that two reserves share |
| `lignin-io`: `sim::tests::budget` | the capacity: what is counted, what is refused, what a refused operation leaves, a hole, a file that only a lock holds |
| `lignin-store`: `wal::tests::faults::the_log_says_that_a_sync_failed_and_makes_none_to_say_so` | the accessor says what is and makes no operation |
| `lignin-store`: `flush::tests::faults::a_manifest_in_doubt_is_settled_when_write_back_is_asked` | the settling, where it works and where the device still refuses |
| `db::tests::pacing` | DF12 on a session whose three threads run: after a commit that was refused its append, on a device that goes on refusing writes and names, write-back's failed turns in 400 ms are those its waits allow, the log's next file is asked for once in each of them and once by each checkpoint the cleaner asks for, no log file id is used, the session has not ended, and with room it takes the next commit; in the same state no wake reaches the reclaimer's thread, no round of it fails, and after every round but the one at which it is told to end it takes its idle wait; the reclaimer's wait after a failed round, which a wake does not end; a log whose ids are used up ends the session and says why |
| `lignin-store`: `flush::tests::pacing` | the waits of write-back's loop turn by turn (the idle time, doubling to the bound; from a millisecond where the idle time is nothing), one file operation for each failed turn, no id used and no name changed where names are refused, two changes of one name for each failed turn on a device with no free byte; on a device that keeps the records it refuses, a roll whose turns end in a file that takes no item is a failed turn with the device's answer, with its wait and six log files at most; on a device that takes the log's files and refuses the record of each page step, the step is put off and the turn has failed by it, with its wait and one log file for each turn that rolls, also under the loop with nobody else who asks for a turn; a failed turn that wrote a page ends the row of turns in vain; the wait grows to a bound of any size; a call for a turn ends no wait after a failed turn; a call that room was made ends the wait that is running or that begins next, and no later one, and the loop takes its next turn at once each time it is told |
| `lignin-store`: `cleaner::tests::errors` (three of its tests) | the same wait for the cleaner's loop; told that room was made, the loop takes its next turn at once, and the call ends one sleep |
| `lignin-store`: `wal::tests::faults` (three of its tests) | a creation that is refused again and again uses up no log file id; an id is used up by a file that is found under it; the ids end at the largest one and do not begin again |
| `lignin-store`: `flush::tests::values`, `cleaner::tests::values` (seven tests) | a value-log file that is given up after a refused append is synced through the handle that appended and closed in the session, with the values before the refused one placed in it; the cleaner cleans and drops it with no open in between; with the sync of the giving up refused too, the file is closed at the length an earlier sync had covered, which the manifest in force names, and is neither appended to nor synced again, the values behind that length are in no cell and go to another file, a table checkpoint is installed, and the cleaner drops the file; where the refused sync is of a second file of the pass, the value in the first keeps its place under the manifest that covers it; a refused sync at the turn to the next file gives the full file up and is counted; the cleaner's own file is given up in the same way |
| `lignin-store`: `values::tests::disk_faults::an_append_that_a_file_admitted_is_answered_before_a_sync_of_the_file_that_fails` | an append to a value-log file and a sync of that file are in one order: a sync that another thread asks for waits for an append that was admitted, and no append is handed to the file layer for the file after the sync that failed |
| `lignin-store`: `flush::tests::checkpoint::a_checkpoint_that_rolls_its_log_in_vain_fails_with_what_the_device_answered` | a table checkpoint that cannot be given a log file for its mark fails with the device's refusal by name |
