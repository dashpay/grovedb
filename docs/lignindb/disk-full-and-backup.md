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
and uses their rule numbers. The code is `lignindb::space` and
`lignindb::backup`, and the tests are those of section 9. Section 8 lists
the choices made here that the plan left open.

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
`CommitError::NotDurable` is published: the next open has it unless a
power cut loses it. A commit that was answered `CommitError::Unsettled`
is the last commit of what the next open finds, or is not there.

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
| `Checkpointer::create` | `CheckpointError`, with nothing left of the target, or a target without a manifest, or the whole target (section 6.4). The live database is as it was. A creation that meets a manifest in doubt fails with `CheckpointError::Manifest` and is made again (DF8a). | goes on, unless the sync of the log was refused (`CheckpointError::Sync`): then it has ended (DF8) |
| `Backup::copy` | `BackupError`, with nothing left of the target, or a target without a manifest, or the whole target (section 6.4). The source is as it was. | no session is involved |
| `Db::close` | `DbError`. The handle is closed all the same, and the next open replays what the close did not cover. | has ended |
| the background work: write-back, the reclaiming of what drops left, the cleaner | nothing is answered to anyone. The turn that was refused is counted, its error is kept as text (`DbStats::background`), and it is made again at the next turn. | goes on, unless what was refused was a sync of the log or of a segment: then it has ended (DF8) |

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
the log like any other, and the next open has it.

**DF8a. A replacement of the manifest that did not finish is settled, and
ends no session** (D-157). Where the rename of a new manifest is refused,
or the sync of the directory after it, it is not known which of two
manifests a crash would leave (`lignin_store::segment`). That does not
last. Three have the manifest opened again, which syncs the directory and
finds out which one there is: write-back where it next needs the manifest,
a commit that finds it in doubt before it begins, and a close once the
background work has ended. Then the session goes on as in DF7.
`Db::reopen` and `Db::stats` are looks: they say that the session has not
ended, make no operation on a file, and settle nothing.

- Where the manifest cannot be opened again yet, because the device still
  refuses, the commit that found it in doubt is refused with that error
  and its transaction whole (`CommitError::NotReady`), a close answers the
  error and finishes nothing, and the next one to come by tries again.
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
- Until a commit, a close or write-back has come by, the creation of a
  checkpoint of the database fails with `CheckpointError::Manifest`: it
  reads the manifest itself. It leaves nothing, and is made again. (A
  creation that asks for the settled manifest itself is left to
  `TXN-06-F1`.)
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
  session does not end for it.
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
in full.

**DF11. The order in which room comes back** once there is a little:

1. The writer has failed (DF4), and readers go on.
2. The reserve is released, if one is kept (`lignindb::space::Reserve`),
   or an operator makes room (section 7.1).
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
6. The reserve is filled again once a table checkpoint was installed and
   the dead space is within its target.

The engine does steps 3 to 5 by itself. Steps 2 and 6 are whoever opens
the database's until the reserve is wired into `Db` (section 8, item 3).

**A known defect in step 4 (D-209).** Where the device filled while
write-back was appending to the value-log file it fills, the dead bytes of
that file do not come back in the same session. Write-back gives the file
up, the file stays open with no writer, and the cleaner cleans no file
that is open. The session takes commits again (step 5), and the room that
came back can be too little to fill the reserve again (step 6). A close
and an open give the file to the cleaner. `STORE-06-F3` removes this; the
two tests of `db::tests::room` are ignored until it does, and the
measurement of section 5.3 is from before it was found.

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
in bytes, at head `a0f90fa3` plus this task): a database with pages of
4 KiB, log files of 96 KiB and segments of 256 KiB is written until a
commit is refused, with a cleaner that is behind. It is full after 6
commits with 573 KiB in its files: 462 KiB of values of which 109 KiB are
live, 32 KiB of segments, 77 KiB of log. What is left free is taken away,
so that the device has no free byte.

| Reserve that is released | What happens |
| --- | --- |
| none | nothing comes back in three rounds of write-back, cleaner and commit; every commit is refused; the session goes on and reads |
| 1, 4, 16 KiB | the same: write-back's pages and the cleaner's new files take the room before anything is dropped |
| 32, 64, 128, 256, 384, 512 KiB and 1 MiB | 443 KiB come back; the session takes the next commit; the reserve is filled again |

Here the room came back in the cleaner's passes and not in write-back's
turns before them: no table checkpoint was due, the cleaner asked for
one, and the log went with it (77 KiB to 1 KiB) together with the
value-log file (462 KiB to 109 KiB).

**What follows for the size of a reserve.** Of the sizes that were tried,
16 KiB was not enough and 32 KiB was; no size between them was tried.
32 KiB is 8 pages of that database and 6% of its files. It is a number of
that database and not a rule.

For a node nothing was measured. The table above names what a reserve has
to cover: the pages that can wait for write-back, one table checkpoint,
and the live part of the file the cleaner empties first. No configured
file length bounds these: a table checkpoint grows with the number of
pages, and `FlushConfig::max_unwritten` is where committers are made to
wait for the pages that wait, and no bound on their bytes. As a first
estimate, and no more than that,
this document suggests one log file plus one segment at their configured
lengths (64 MiB each by default), until a measurement on a database of a
node's size replaces it (PERF-08).

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

1. **Make a checkpoint for the backup**, in a directory of its own
   (`Checkpointer::create`; through GroveDB, `create_checkpoint`). Do not
   open it. A node that opens every checkpoint it makes (P-15) makes one
   more for the backup.
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
no backup.** A creation of a checkpoint and a copy write the manifest
last. One that is cut short by the power, or that fails and cannot take
away what it made, leaves its target without one: empty, with its `LOCK`
file alone, or with some of its files.

One failure leaves the whole target instead. The manifest was written,
and then its name could not be made durable, and the manifest could not
be taken away again, or the power was cut before its removal was durable.
Every file that the manifest names was durable before the manifest was
written, so that target is the whole checkpoint or the whole copy, and
never a manifest with a file missing. The call answered an error all the
same: the target is removed like the others, and the call is made again.

- Nothing opens such a directory as the database it was to be. An open
  that may create a database makes a new and empty one in a directory that
  holds nothing of a database yet: so no open that may create is pointed
  at the target of a creation or of a copy before that call has returned
  `Ok` (`lignindb::checkpoint`, "What other tasks take from here").
- **It is removed by whoever asked for it**, with everything in it, and
  the creation or the copy is made again: both refuse a target that is
  there. Nothing is lost with it. Its files are the live database's under
  a second name, or copies.
- `lignindb::checkpoint::delete` removes an empty directory and one that
  holds its lock file alone. It refuses one that holds files and no
  manifest, because it cannot tell it from a database that lost its
  manifest: that one an operator looks at before it goes.

A directory that holds `MANIFEST.deleting` is one whose deletion was cut
short. `delete` finishes it.

## 7. For an operator

### 7.1 The device is full

**What is seen.** Commits fail, and their error says that the device is
full (`lignindb::space::out_of_space`): the node stops at a block. Reads
are answered. `DbStats::background` counts turns of write-back that failed
and keeps the last error as text.

**What is true then.** Nothing that was made durable is lost, nothing
that was committed is damaged (DF1), and nothing needs a repair. No file
of the database directory is to be removed by hand: each one is needed,
or is taken away by the engine when it is not. The one exception is the
reserve, the file `RESERVE`, which is there to be removed, with
`RESERVE.new`, which is what a fill of it left that was cut short.

**What to do**, in this order:

1. **Make room.** Release the reserve (`Reserve::release`, or remove the
   file `RESERVE` from the database directory). If none is kept, free
   something else on the device:
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
     not finish"** is made again after the next commit: a commit that
     finds the manifest in doubt has it settled before it begins (DF8a).
     Asking `Db::reopen` settles nothing. The session has not ended for
     it.
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
4. **Fill the reserve again** (`Reserve::fill`) once the dead space is
   within its target, and not before (DF11 step 6).
5. **If the device is full of data that is live**, steps 1 to 3 give back
   less than the writer needs. The database needs a larger device: stop
   the node, back the database up or copy it (section 6), and restore it
   where there is room.

**A database that does not open on a full device** (DF9) needs the same
step 1, and opens then. An open that was refused has changed nothing that
the next one does not expect.

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
- **A target that was left without a manifest** by a checkpoint or a
  backup that was cut short is removed by whoever asked for it
  (section 6.4).

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
3. **The engine does not release or fill a reserve by itself yet.** The
   cleaner is not a part of `Db` at this head (`TXN-07-F1` wires it), and
   releasing a reserve is only worth it where the cleaner then runs. The
   order of DF11 is tested with the cleaner as the store has it, over the
   parts of an open database (section 9). What remains for the wired
   form: `Db` releases the reserve when a commit or a turn of the
   background work is refused for room, and fills it again when a table
   checkpoint was installed and the cleaner is within its target.
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
| A manifest in doubt that is settled (DF8a) | `Shared::settle_manifest` in `lignindb/src/db/store.rs`, `Db::refused`, `Db::finish` |
| Whether a sync of the log failed | `LogStore::sync_failed`, `lignin-store/src/wal/store.rs` |
| The settling of the manifest, for whoever answers for the session | `WriteBack::settle_manifest`, `lignin-store/src/flush/write_back.rs` |
| The capacity of the simulated device | `lignin-io/src/sim/budget.rs` |

**The acceptance test** is the sweep of `lignindb/src/db/tests/full.rs`.
One session through the facade is scripted: a database is made, written
with and without a transaction, written back, flushed, checkpointed,
reclaimed after a drop, wiped, closed and opened again: 522 file
operations, 79 of them syncs, and 537 with 94 syncs under a policy that
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
`48c69be1` plus this task: a change of what the store does to its files
at an open, a close or a turn of write-back changes them.

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
| `db::tests::ended` | each of the two states of DF8, met by a step that is no commit and no flush, ends the session; a `flush()` still makes durable what was published; the look waits for no commit, makes no operation on a file and settles nothing; a session gives the first reason it had, also where the commit path stops it while the store's reason is being kept; a manifest in doubt is settled by a commit and by a close, and ends no session; a table checkpoint is installed though the replacement for its turn of the log was refused; a close whose table checkpoint is refused the start record of the log's next file once does all a close does, whether the device kept the record or not; a commit that cannot have it settled is refused and can be made again; a commit beside a holder of the manifest is made (DF8a) |
| `db::tests::clean` (two of its tests) | a sync of the cleaner's segment that was refused in a pass ends the session, which says so before a close or write-back has met the segment (DF8); a close that is the first to be refused the sync of the cleaner's segment answers with the cleaner's error, which has the device's refusal |
| `db::tests::room` (ignored until `STORE-06-F3`, D-209) | DF10 and DF11 on a device with a capacity in bytes: nothing comes back while nothing is released, and with the reserve released write-back and the cleaner give back what is dead, the session takes the next commit, and the reserve is filled again; the least reserve that is enough (section 5.3) |
| `db::tests::filling` | random fills under a database whose background threads run, with the same model: 12 seeds of 40 rounds, in which the device fills in each of the four ways and has room again |
| `db::tests::backup` | a backup from a checkpoint of a database that is written meanwhile is the database at the checkpoint's version (BK2): the copy waits, with every file measured and none copied, until 5 commits were made beside it; a restore of it takes writes |
| `backup::tests::copy` | BK1 to BK11: closed and killed sources, refusals (a target inside the source among them), the valid lengths, carried files, a target that cannot be listed, a database that was being made, cut at each operation of its creation, a full device and a power cut under the target at every operation |
| `space::tests` | `out_of_space` through the errors of the crate; a reserve that is filled, released, cut short and filled on a full device; a fill that fails or is cut at each operation, and the fill after it, which makes the reserve whole and durable; no block that two reserves share |
| `lignin-io`: `sim::tests::budget` | the capacity: what is counted, what is refused, what a refused operation leaves, a hole, a file that only a lock holds |
| `lignin-store`: `wal::tests::faults::the_log_says_that_a_sync_failed_and_makes_none_to_say_so` | the accessor says what is and makes no operation |
| `lignin-store`: `flush::tests::faults::a_manifest_in_doubt_is_settled_when_write_back_is_asked` | the settling, where it works and where the device still refuses |
