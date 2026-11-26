# How LigninDB works

This chapter is the map of LigninDB for the person who runs a node on
it: the parts, what each must not know about, and what happens on the
way from a batch to the disk, on a read, at a checkpoint and when a
database is opened again. It stays one level above the design documents
and links into them. It is part of task OPS-05 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).

## Where it sits

GroveDB keeps a grove of Merk trees. Below Merk is a storage engine,
which GroveDB reaches through one trait, `Engine`
([engine.md](../engine.md)). Two engines implement it: the RocksDB
engine, which GroveDB has always had, and LigninDB. A node chooses one
when it opens its database. Merk's tree shape, its hashes and its costs
stay in Merk, above the seam, so that on every existing GroveVersion a
grove has the same root hash, the same proofs and the same
`OperationCost` on either engine, as the golden corpus holds. The plan
holds this for what a resync of mainnet or testnet can meet, and states
the differences it leaves in states those histories do not reach:
[engine.md](../engine.md#a-subtree-leaves-with-its-element) names them,
the first after a tree is removed by a batch's plain delete while it
still holds elements.

```text
the application          a node: blocks, state transitions, checkpoints, state sync
grovedb                  operations, batches, proofs, references, state sync   GroveDb<E>
-------------------------------- Engine -------------------------------------------------
grovedb-merk             AVL shape, hash rules, cost rules, proofs, chunks
append-only trees        MMR, bulk-append, dense, commitment, private-document
------------- NodeStore (Merk nodes) / FlatStore (ordered key-value records) ------------
RocksDB engine           grovedb-storage
LigninDB engine          lignindb, lignin-tree, lignin-store, lignin-format, lignin-io
```

Merk reaches its nodes through `NodeStore`
([node-store.md](../node-store.md)), and the append-only trees reach
their records through `FlatStore` ([flat-store.md](../flat-store.md)).
On LigninDB both are kept in pages that the engine owns: a Merk node is a
cell of a tree page, which stores its value hash, its key-value hash and
its children's hashes beside it, so that **every hash is read as it was
stored, and a changed hash is logged as a small record of its own,
without the node's value** ([tree-layout.md](../tree-layout.md),
[page-delta.md](../page-delta.md)).

## The crates, and what each must not know about

The layers are crates, and a crate depends only on those below it. What a
crate does not know about, it cannot get wrong.

| Crate | Owns | Must not know about | Read |
| --- | --- | --- | --- |
| `lignin-format` | the bytes of every file: page images, delta records, log records, segments, the manifest, the value log. Encode and decode, as pure functions. | files, threads, Merk | [format.md](../format.md), [tree-page.md](../tree-page.md), [flat-page.md](../flat-page.md), [page-delta.md](../page-delta.md), [log-format.md](../log-format.md), [segment-manifest.md](../segment-manifest.md), [value-log-format.md](../value-log-format.md) |
| `lignin-io` | the file layer: the `Vfs` trait, the system's files, a file system that injects faults for tests, and direct I/O on Linux | any format above raw bytes | [sync-handles.md](../sync-handles.md) |
| `lignin-store` | the buffer pool, the write-ahead log, segments, the page table, write-back, recovery, the cleaner, the value log, the counts | trees, keys, hashes | [page-table.md](../page-table.md), [page-alloc.md](../page-alloc.md), [crash-tests.md](../crash-tests.md) |
| `lignin-tree` | the Merkle host: Merk nodes and flat records in pages, the subtree directory, lazy hashing, cursors, the drop of a subtree, the bulk loader | Merk's hash and cost rules, which it is given | [tree-layout.md](../tree-layout.md) |
| `lignindb` | the engine as a caller sees it: options, transactions, snapshots, savepoints, write scopes, checkpoints, the physical check | GroveDB's elements | [transactions.md](../transactions.md), [background-and-readers.md](../background-and-readers.md), [disk-full-and-backup.md](../disk-full-and-backup.md) |
| `lignin-tools` | the `lignin` command: inspect, verify, compact, dump and load, migrate from RocksDB | | [the runbook](migration.md) |
| `grovedb-merk` | gains `NodeStore`, with a store for each engine, and the cost model that names no engine | which engine is underneath, outside its stores | [node-store.md](../node-store.md), [cost-model.md](../cost-model.md) |
| `grovedb` | gains `Engine`, `GroveDb<E>`, and the engine chosen at the open | engine types, outside its module `engine` | [engine.md](../engine.md) |

`lignin-testkit` holds the workloads, the golden corpus, the
differential fuzzer, the crash campaigns and the benchmarks; it is not
published.

## The files of a database

A LigninDB database is one directory:

```text
<db>/
  LOCK                 held by the one process that has the database open
  MANIFEST             the format, the page size, the live files and their valid lengths, the table checkpoint in force
  wal/00000004.log     the write-ahead log: one group for each commit
  seg/00000001.seg     segments: page images and page deltas, written once and never changed
  val/00000001.val     the value log: values too large to be kept in their page
  table/00000003.pt    page-table checkpoints: where each page's blocks are
  RESERVE              room kept for a full device, where the options ask for it
```

The manifest and each file of the log, the segments, the value log and
the page table have a header with the format version, and every block a
checksum; `LOCK` and `RESERVE` hold no data. The manifest is replaced whole (a new file given the name
`MANIFEST`), and it names each file with its valid length. A file that
the manifest calls closed or sealed is the database's up to that length;
the log file that the database was writing when its session ended is
read past it, up to the first item that is not whole.
`lignin manifest` prints it, and `lignin segments` the segments it names.

## A write, from the batch to the disk

In iteration one, which is what a node runs today, one thread writes and
every commit hashes up to the root of the grove, as on RocksDB.

1. **GroveDB applies a batch** in a write transaction of the engine. For
   each subtree it opens a handle of the node store, and Merk runs its
   AVL insert, delete and rebalance on the nodes it reads. What the
   operation is charged comes from the cost model over logical sizes
   ([cost-model.md](../cost-model.md)), so it is what RocksDB is charged;
   the physical work is free to be smaller.
2. **The transaction writes into private copies** of the pages it
   changes, in the buffer pool: the new and changed values, and the
   hashes on the path to the root, each in its cell. The transaction
   reads its own writes; nobody else sees them
   ([transactions.md](../transactions.md), section 7).
3. **The commit** fails with a conflict where a commit outside the
   transaction wrote a key after the transaction first wrote it, the rule
   of RocksDB's optimistic transactions, and where it kept the committed
   value of a Merk node that a commit outside wrote after the
   transaction read it ([transactions.md](../transactions.md#82-the-conflict-rule)).
   Otherwise it appends one group to the log: the new values, the changes to the
   pages, and one record for each hash that changed. Then the new
   version is published at once to every later reader. Under the default
   sync policy the log is not synced at the commit; it is made durable by
   `flush()`, a checkpoint or a close ([tuning](tuning.md#durability)).
   One commit is made at a time; any number of transactions are open.
4. **Write-back**, a thread of the engine, writes what commits changed
   into the current segment: as deltas behind a page's last image, and as
   a whole new image once a page's chain is long. It places large values
   in the value log, and when enough log has gone by it takes a table
   checkpoint, which records where every page's blocks are. The log
   before that checkpoint is then let go
   ([page-table.md](../page-table.md), [page-delta.md](../page-delta.md)).
5. **The cleaner**, another thread, moves what is still live out of
   segments and value-log files that are mostly dead, and gives the files
   back. **The reclaimer**, a third, frees the pages of subtrees that
   were dropped ([background-and-readers.md](../background-and-readers.md)).

A commit with writes therefore makes one append to the log, whatever
the number of changes in it (one with none appends nothing and publishes
no version), and the pages reach the segments later in the background.
If the background falls behind, committers are made to wait
([tuning](tuning.md#write-back)).

## A read

1. **A read takes a version**: a snapshot holds the one it was taken
   at for as long as it lives, and a cursor the one it opened at, with
   the later writes of its own transaction; another read takes the
   latest for that call, so a later call can see a commit made in
   between. A read of a snapshot takes no lock of a transaction; a few
   of its lookups hold, for their call, a lock that other readers of the
   same subtree take too. A read
   through a write transaction holds that transaction's own lock for
   its call, so another call on the same transaction waits for it, and
   opening a cursor of a transaction takes that lock for a moment.
2. **The subtree directory** gives the page and slot of the subtree's
   root, from the path's 32-byte prefix, as RocksDB's prefixes are.
3. **Descent follows page ids and slots** from cell to cell: a node's
   children are reached by their page and slot, and not looked up by key
   in an index, while Merk compares keys at each node as it does on
   RocksDB. A page that is not in the buffer pool is
   read from the blocks of its segments that hold its last image and
   the deltas after it, each checked against its checksum, and the
   deltas are applied to the image.
4. **Hashes are used as stored.** A proof is assembled from the cells:
   the hashes of nodes that it does not show are read from their pages,
   and a value that is kept out of its page is read from the value log
   only when it is asked for.

## A checkpoint

A checkpoint (`GroveDb::create_checkpoint(path)`, `Db::checkpoint`) is
the database at one committed version, in a directory of its own
([transactions.md](../transactions.md#11-checkpoints)):

1. In one step under the commit lock, the creation records the version,
   the live files and the length of each. The files are synced up to
   those lengths, so the version is durable in the live database too.
2. The checkpoint is built in `path.creating`: a hard link to each of
   those files, or a copy of one that the file system does not link
   (another file system, or one with no hard links), and a manifest of
   its own that names them with the recorded lengths. Files are only ever appended to, so neither database
   changes a byte the other has; what the live database appends behind a
   recorded length is no part of the checkpoint.
3. When everything in it is durable, the directory is given the name
   `path`. A creation that is cut short leaves nothing at `path`, or the
   whole checkpoint.

Commits go on while it is built. The recording step holds the commit
lock for that step. A commit waits for the checkpoint's sync where its
own sync policy syncs at the commit, and a commit that finds a
replacement of the manifest in doubt waits for whoever holds the
manifest, the creation among them ([lignindb::checkpoint](../../../lignindb/src/checkpoint/mod.rs)
says each). A creation that finds
more than 1 MiB of log held for the checkpoint's first open to replay
has write-back try to catch up first ([engine.md](../engine.md#checkpoints)).
A checkpoint is a database like any other: opened by the normal open, which recovers it to
its version, and able to take writes of its own that its source does not
see. It keeps the files it shares with the live database, so their disk
space comes back when it is deleted
([backup and full disk](backup-and-full-disk.md#back-up-a-running-node)).

## Recovery: every open

Every open recovers, whatever ended the session before: a close, a
killed process, a power cut, or nothing at all for a new database or a
checkpoint ([transactions.md](../transactions.md#12-durability-and-recovery),
section 12).

1. **The look, then the lock.** The open looks at the directory first,
   and then takes `LOCK`, so one process at a time has the database
   open, and looks again: it refuses one of another
   engine, of a newer format, without its manifest, and the other cases
   that [the messages](messages.md#at-an-open) list first; those are
   found before anything is written.
2. **The manifest** names the table checkpoint in force and every live
   file with its valid length.
3. **The table checkpoint** is loaded: where each page's blocks are, as
   of the version it covers.
4. **The log is replayed** from the first item of the oldest log file
   the manifest keeps, since commits that the checkpoint does not cover
   can lie before the place it was taken at, up to the first item that
   is not whole, behind which nothing is read: the last whole commit
   group before it gives the version the open lands on, or, where the
   log holds none, the version its start record names, and the whole
   items of the background up to it are read too. What the checkpoint covers already is
   skipped, and a group that was cut by the crash does not count. The pages that the log changes are rebuilt in
   the buffer pool.
5. **The session's files**: the files of earlier sessions are named
   closed in a new manifest, and the log goes on in a new file of its
   own. An open that fails after this has written to the directory.
6. **The keyspaces are found**: the open reads the record of where the
   flat keyspaces are and the subtree directory, whatever replay changed
   ([messages](messages.md#at-an-open) has what each refusal says).
7. **What replay brought back is written**: on the opening thread, a
   few turns of write-back, then the fill of the reserve where the
   options keep one.
8. **The session begins**: the three background threads start, and go
   on with what is still owed.

So the database opens at one committed version, never a part of one, and
never one older than the newest that was made durable. A file that the
open reads (the manifest, the table checkpoint, the log, the blocks of
the pages that replay changes, the record of the keyspaces and the
subtree directory) and that does not read, or does not
hold what the manifest says, stops the open with an error that names it
([messages](messages.md#in-a-recovery)). The open does not read every
page: a block that nothing reads at the open is checked when it is read,
and `lignin verify` reads them all.

## When a session must be opened again

A few failures cannot be made good while the database is open: a sync
of the log or of a segment that the device refused, a commit whose fate
is unknown, a log that has used its last file id. From the first of
them, the session takes no write and answers every read, and every
commit is refused with the same reason, until the database is closed and
opened again, which settles what the log holds; a log with no file id
left is the exception, since an open can fail on it too
([disk-full-and-backup.md](../disk-full-and-backup.md), DF8;
[messages](messages.md#when-a-session-ends)).

## How it is checked

- **The golden corpus** is what RocksDB answered at a pinned commit:
  root hashes, proofs, costs and results of a fixed set of traces. It is
  replayed on LigninDB in CI ([ci.md](../ci.md#the-corpus-gate)).
- **The physical invariant checker** (`lignin verify`) reads every
  page and value of the store as a close left it, holds every rule of
  the store, and makes every stored hash of the Merks in use again by
  Merk's rules but those the rules take as stored (the value hash of a
  reference); the cells of a dropped Merk that waits to be reclaimed
  are walked, and their hashes not made again. `Db::verify`, the check
  beside a running session, leaves out a few comparisons that need the
  store at rest (the page ids, the counts of live values, the page table
  against the segments), and makes no hash again unless it is given
  Merk's rules.
- **Crash campaigns** cut the engine at millions of points and recover
  each ([crash-tests.md](../crash-tests.md)); the full device is swept
  at every file operation ([disk-full-and-backup.md](../disk-full-and-backup.md),
  section 9).
- **The differential fuzzer** applies random operations to RocksDB and
  to LigninDB, and compares the cost, the result, the root hash and the
  proof bytes after every step ([ci.md](../ci.md#nightly)).
