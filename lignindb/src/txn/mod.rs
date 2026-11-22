//! The write transaction: private copy-on-write frames over the latest
//! committed state, derived from a write set that is the truth.
//!
//! - [`WritePath`] is what a write transaction is made of: the versions of
//!   a database, its pages, the key sets of its commits ([`Commits`]), the
//!   flat keyspaces of its records ([`Keyspaces`]) and the keyspace of
//!   its subtree directory ([`lignin_tree::directory`]). A transaction is
//!   begun from it.
//! - [`WriteTxn`] is one write transaction, and [`Scope`] the handle a
//!   thread writes it through.
//! - [`Batch`] is the writes of one operation: flat records
//!   ([`RecordWrite`]) and the change sets of Merks ([`MerkWrite`]), whose
//!   nodes are named by their keys ([`NodePut`]).
//! - [`NodeView`] is a node as a transaction shows it, and [`Cursor`] walks
//!   the flat records of one unit, or the nodes of one Merk
//!   ([`MerkCursor`]), in key order. A [`ParkedCursor`] is a cursor of a
//!   scope that is kept apart from the scope between two calls.
//! - [`KeySet`] is the key set of a commit, which decides whether a commit
//!   outside a transaction has met one of its writes.
//!
//! `docs/lignindb/transactions.md` is the specification: RD3 to RD6 and
//! RD10 for what a transaction reads, section 7 for its write set, its
//! frames, the rebase and rollback, PN8 for locations that are kept, PV7
//! and ID1 for what its private copies and page ids hold back. The module
//! writes no file but for a load (WT4, "A load") and takes no version: the
//! commit is [`crate::commit`], and what it is handed is at the end of
//! this page.
//!
//! No example runs here: a [`WritePath`] is made where the database is
//! opened, of the structures of one store, and a caller begins a
//! transaction with [`Db::begin`](crate::Db::begin), whose page has one.
//! The tests of this module make a write path over a store on disk
//! (`tests/store.rs`).
//!
//! # What a transaction reads
//!
//! | Read | Returns |
//! | --- | --- |
//! | [`WriteTxn::get`], [`WriteTxn::node`] | the transaction's newest write that covers the key, and with none the record of the latest committed state at the moment of the call (RD3) |
//! | [`WriteTxn::follow`], [`WriteTxn::child`] | the same record, through a node handle while that is good (PN8) |
//! | [`WriteTxn::subtree`] | what the subtree directory holds of a subtree ([`SubtreeView`]): the transaction's own enters, drops, clears and roots, and for a subtree it has not written the entry of the latest committed state |
//! | [`WriteTxn::committed_get`], [`WriteTxn::committed_node`] | the record of the latest committed state, whatever the transaction wrote there (RD10) |
//! | [`Cursor`], from [`WriteTxn::cursor`] for flat records and from [`WriteTxn::merk_cursor`] for the nodes of a Merk | the committed records of the version the cursor was opened on, merged with the transaction's writes as they are at each step (RD6) |
//!
//! Each call is one operation (RD4): it takes an operation guard, reads the
//! latest version once, and reads every committed record at that version.
//! The flat pages it reads borrow that guard (PN5,
//! [`FlatReader`], [`FlatWriter`]). What outlives the operation is bound to
//! no guard between two of them and reads nothing then: the frames of the
//! transaction, whose base pin keeps their version, and the committed side
//! of a cursor, which the cursor's pin keeps.
//! Between two calls a commit can land, and the next call sees it for every
//! key the transaction has not written. Nobody else reads the
//! transaction's writes: they are in its own memory and in private copies
//! of the buffer pool until a commit installs them (RD8).
//!
//! # The write set, and the two states of a transaction
//!
//! The write set is the ordered list of the transaction's writes (WT2): a
//! put or a delete of a flat record, a put or a delete of a Merk node, a
//! delete of a range. Each has the first-write version of its operation
//! (WT5) and a mark that says whether a commit outside the transaction has
//! met it since (CF2). All bytes of the list are in one arena, and an index
//! beside it gives the newest write that covers a key and the written keys
//! of a unit in order. The index is derived from the list, and holds one
//! boxed key for each key that was written.
//!
//! **The frames are derived from the write set** (WT3). They are private
//! copies of tree pages and of flat pages in the buffer pool, over the
//! committed pages of a base version that the transaction keeps pinned
//! (PN1). Applying the write set's operations, in order, to the committed
//! state of the base gives them, so they can be given up and made again at
//! any time. A transaction is in one of these states:
//!
//! | State | When | Reads | Writes |
//! | --- | --- | --- | --- |
//! | idle | the write set is empty | committed pages at the latest version; no pin, no copy, no page id (PN1) | make the frames |
//! | framed | no write is met, and the frames are over the latest version | the frames: a node is where its handle says, or where its key leads from the root of its Merk | change the frames through the tree layer and the flat keyspaces, then add to the write set |
//! | by key | a write is met (WT7b), or the write set cannot be laid out | each record by its key: from the write set if the transaction wrote the key, and otherwise from the committed tree of its subtree, from the committed root | add to the write set, and nothing else |
//!
//! **An operation brings the transaction to the latest version** (WT7).
//! It takes in the key sets of the commits that were published since the
//! transaction last looked, and marks the writes they meet. Then:
//!
//! - *No write is met* (WT7a). Frames over another version are given up
//!   and made again over the latest one: every operation of the write set
//!   is applied again, with every cell found by its key. The records are
//!   what they were; the pages need not be.
//! - *A write is met* (WT7b). The frames are given up and not made again.
//!   Before the base pin goes, the transaction takes its own copy of every
//!   committed value that a write of it kept (WT3). It then serves its
//!   records exactly as they are, pins nothing, and its commit fails with
//!   a conflict unless a rollback to a mark takes the met writes out.
//!
//! No speed target applies to either (WT7c): the first costs a keyed
//! descent for each node of the write set, and the second makes every read
//! a keyed lookup, which is what the owner ruled for a transaction that a
//! commit has overlapped.
//!
//! **An operation that fails adds nothing** (WT10). A batch is checked
//! before a page is changed, and the tree layer plans a write before it
//! makes it. Where an operation fails after it has changed a page, the
//! frames are made again from the write set, which the batch was never
//! added to.
//!
//! # A batch of Merk nodes
//!
//! A change set names its nodes, their children and its root by key, as
//! Merk hands records to storage (ST6), and the transaction finds what
//! each key names: a node of the same batch, or a cell that is found by a
//! hint that is still good ([`NodePut::at`]) or by its key. All change sets
//! of one batch are one write of the tree layer
//! ([`Fragments::apply`](lignin_tree::fragment::Fragments::apply)), so a
//! host points at the root of the Merk it hosts as the whole batch leaves
//! it (`tree-layout.md` P5). [`Applied`] gives the caller the handles of
//! the nodes it wrote.
//!
//! A change set states one outcome for each key, and the tree is what its
//! root reaches. A written node that the tree does not reach once the
//! batch is laid out is kept as a record of its key, with no cell and in
//! no hash, by the rule of [`MerkWrite`] ("Unlinked records"): a storage
//! batch can leave such a node where a put stood over a later delete of
//! its key inside one operation (GM0). A read by key and a cursor over
//! the Merk show it, and it follows its subtree. A batch that is laid out
//! as it is applied reports it ([`Applied::unlinked`]). A batch that is
//! kept in the write set by key is looked at when it is laid out, and its
//! nodes are read as the write set's records until then.
//!
//! # A load
//!
//! A change set can say that it is the whole Merk ([`MerkWrite::whole`]):
//! a Merk that has no root, handed over in one change set, every node
//! once, a child before its parent, with every value stated, every hash
//! made and the root named. Such a change set that has a node is a
//! **load** (`transactions.md` LD1 to LD8). A state-sync restore, the
//! load hatch and the migrator from RocksDB hand their Merks over so.
//! One that is not what it says is refused, and its batch adds nothing to
//! the write set ([`BatchError::NotWhole`], [`WholeFault`]).
//!
//! **The records are those of the same change set that does not say so.**
//! A load is in the write set as the puts of its nodes, one entry each, so
//! the claims of scopes, the service by key and the rollback to a mark are
//! for it what they are for any write. To the conflict rule it is those
//! puts and a range (below). What differs beside that is where the cells
//! and the values go, where the load is laid out in the frames:
//!
//! - **The layout is the bulk loader's** ([`lignin_tree::bulk`]). The
//!   loads of one batch are laid out before its other change sets are
//!   applied, all through one loader, in pages that the loader starts
//!   ([`Applied::load`] has its counts). The nodes are reported with their
//!   handles, as those of any write ([`Applied::nodes`]).
//! - **A node of a load that hosts a Merk** names the cell of the Merk's
//!   root: of another load of the batch, which is laid out before it
//!   wherever it stands in the batch, or of a Merk the batch does not
//!   write. No Merk is laid out inside the fragment of its host. A node of
//!   a load does not host a Merk that the batch writes node by node, nor
//!   one that another node of the batch states it hosts: the batch is
//!   refused before anything is changed. Nor does it host a Merk whose
//!   entry names another host, or none, once the batch has made what it
//!   states of subtrees, nor one that a move of the batch brings to its
//!   prefix ([`WholeFault::NotItsHost`]): that is refused where the batch
//!   is laid out, before a page of it is started. A node that is written
//!   as any other can host a load.
//! - **The hashes are the ones the change set states.** The loader asks
//!   for each hash of a node when the node arrives, and is answered with
//!   what the change set says: no rule is known here. The hash of the
//!   root, which no node of its Merk states, is in no cell.
//! - **A value out of line goes into the value log when its cell gets its
//!   page**, into the file that the loads of the store append to
//!   ([`LoadFiles`]), and the cell is written with the pointer (LD2). The
//!   values of one fragment are appended in the order a scan reads them,
//!   as write-back would regroup them. The write set keeps where the
//!   value is in the place of its bytes (LD3), and the frames keep
//!   nothing of it. Where the loads have no file that takes the value,
//!   one is created and the manifest is replaced so that it names it, in
//!   the operation that appends the value.
//! - **The commit takes the values over** ([`crate::commit`], "The commit
//!   of a load").
//!
//! **Frames that are made from the write set lay a load out again** (LD4):
//! after a commit of another transaction (WT7a), a rollback to a mark, an
//! operation that failed half way, and when a scope that was open beside
//! another is laid out. The loader lays the change set out anew, in new
//! pages, and each value out of line is read from where the write set has
//! it and appended again, for the page its cell is in now. The records
//! are what they were. The copies that no cell holds are dead space of
//! the value log. A layout that a refusal stops is not made again to
//! tell that refusal: the transaction keeps it, and tells it again to a
//! read or a write that a move decides and to a commit, for as long as
//! the layout would be the same (`transactions.md` LD4). Where it holds
//! a load, the layout is not made again for a write it takes afterwards
//! either, which is then served by key.
//!
//! **A load that is applied where no frames take it has no page then**:
//! where a write of the transaction is met (WT7b), and for a scope that
//! keeps its writes in the write set ([`crate::scope`]). Its values that
//! are too long for a cell go into the value log in that operation all
//! the same, each for no page, and the write set keeps where they are
//! (LD3). A value that fits its cell is in the write set, like that of
//! any write. When frames are made, the load is laid out from there, and
//! each value out of line is appended again for the page of its cell.
//!
//! **To the conflict rule a load is the puts of its nodes and a range
//! over the nodes of its subtree** (LD6). That the Merk is entered and
//! has no root is checked where the load is taken, laid out or by key
//! ([`BatchError::NotEntered`], [`WholeFault::HasRoot`]). When such a
//! refusal is told as a conflict, after the transaction was shown the
//! subtree entered with no root at an earlier version, is in LD6
//! ([`BatchError::MerkTaken`]). By key a load is taken in a batch that
//! drops and clears no subtree ([`BatchError::LoadBeside`]). From then
//! on a
//! commit outside that puts or deletes a node of the Merk meets the load,
//! whatever its key, and the commit of the load ends with the conflict.
//! The commit of a load has the range in its key set, so a transaction
//! that wrote a node of the Merk before it is met in turn. A load keeps
//! no value, so no read is dated for it.
//!
//! **What a transaction holds in memory of a load that is laid out in its
//! frames** (LD7), from the operation until its commit or rollback:
//!
//! - its pages as private copies, every cell of the Merk, and with each
//!   page the run of records that made it, which is what the commit logs:
//!   the bytes of the cells once more
//! - one entry of the write set for each node, with the key, the feature,
//!   the two hashes, the links and the value if that is in its cell, and
//!   the node's key once more in the index
//! - for a value out of line the 12 bytes of its place, in the cell and in
//!   the run, and 20 bytes in the write set, whatever its length
//!
//! While the operation lays the load out, the loader keeps the cell of
//! each node that is in no page yet, in whatever order the nodes are
//! stated. Of a value out of line the transaction keeps only where it is
//! until the cell gets its page: in the batch, or in the value log where
//! the frames are made again. There a value is read when its node is
//! handed to the loader and again when it is appended, each time into a
//! buffer that is used again for the next value: what the buffers hold is
//! a few times the bytes of the longest such value, whatever the number
//! of nodes that wait. The batch itself is its caller's ([`TxnStats`] has
//! the pages and the bytes of the write set).
//!
//! # Subtrees and the directory
//!
//! Where the root of a Merk is, the subtree directory says
//! ([`lignin_tree::directory`]): the transaction opens every Merk through
//! it, and tells it what its writes do.
//!
//! - **A batch says which subtrees it enters, drops, clears and moves**
//!   ([`SubtreeWrite`]), as the caller that writes the hosting elements
//!   knows them. A change set and a flat record, in any column, are of a
//!   subtree that is entered ([`BatchError::NotEntered`]).
//! - **A subtree is moved to another prefix with the subtrees below it**
//!   ([`SubtreeWrite::Move`], `transactions.md` section 7.5). The
//!   directory's records and the flat records are written under the new
//!   prefixes and no node is copied. The write set keeps the statement:
//!   it says under which prefix the committed state has what the
//!   transaction reads under each prefix afterwards, and frames that are
//!   made again make the move again. The move is dated at the read of
//!   the node that hosted the subtree, and is a write of every key under
//!   every prefix it names (CF10).
//! - **A subtree is entered as one that is new, or if it is absent**
//!   ([`SubtreeWrite::Enter`], [`SubtreeWrite::EnterIfAbsent`]). The first
//!   is refused where the subtree is entered. The second is judged
//!   wherever the write is laid out: it enters a subtree that is not
//!   entered there, leaves one that is entered with the tree type and the
//!   host it states as it is, and is refused where the subtree is entered
//!   otherwise ([`BatchError::EnteredOtherwise`]). Frames that are made
//!   again apply it again, so it finds a subtree that a commit beside the
//!   transaction entered since, where the first kind is refused and the
//!   transaction's commit with it. Neither is a key of the conflict rule.
//! - **A batch states a subtree anew** ([`SubtreeWrite::Convert`]): the
//!   tree type of its entry, and its place among the subtrees of its
//!   host, as a conversion in place leaves them
//!   ([`Directory::restate`](lignin_tree::directory::Directory::restate)).
//!   The subtree keeps its nodes, its root and what is below it. A batch
//!   that moves a subtree states none anew that its moves name
//!   ([`BatchError::ConvertMoved`]).
//! - **No record of a subtree is read without its entry.** In every
//!   column the committed records under a prefix belong to the entry the
//!   directory has under that prefix, so where the version that is read
//!   has no live entry, nothing is read: not a node, not a flat record,
//!   and not under a cursor. An entry that was found is kept for its
//!   version, and what the transaction itself dropped since is asked of
//!   its write set at every read.
//! - **Every root that one write changed is told in one statement**
//!   ([`Directory::set_roots`](lignin_tree::directory::Directory::set_roots)),
//!   from the report of the write: the Merks of the batch whose change set
//!   sets a root by their name, and every root that was carried by the
//!   address it had, also of a Merk the batch did not open
//!   (`tree-layout.md` S3). The cell one root left can be the cell another
//!   is in afterwards, so they are resolved together, against the entries
//!   as they were before the write.
//! - **A handle on the directory is of one version, and none is loaded
//!   here.** The engine keeps the handle of every version that is read
//!   ([`Directories`](crate::snapshot::Directories)), and a transaction
//!   takes copies (`WritePath::directory_at`): the frames one of their
//!   base when they are made, so again after a commit of another landed
//!   and after every rollback; the committed records beside them one of
//!   the committed version of that read; and a cursor one of its own
//!   version. Two of these ask for a version long after the transaction
//!   pinned it: a cursor at its first step, and a read that follows a
//!   commit of another, which takes the values the transaction kept from
//!   the version of its base. A handle that was loaded late, after a later commit landed
//!   and what a drop left was reclaimed, would take a subtree that was
//!   below the dropped one for a subtree in use.
//! - **A drop is one write** (`transactions.md` CV9, CF5): one entry of
//!   the write set, a delete of every key of every column under the
//!   subtree, which is in the key set of the commit as the subtree. A
//!   clear is one entry as well, a delete of every key of the data column;
//!   what else a clear takes is GROVE-05's to state. The Merks below go
//!   with either, and neither names them:
//!   - what is committed below is not read from the drop on, because no
//!     open returns it: the frames' directory has no live entry for it,
//!     and what is read beside the frames is not read under a Merk the
//!     transaction dropped or cleared;
//!   - what the transaction itself had written below is covered by the
//!     entry of the drop, which names those subtrees, so that it is gone
//!     also where the same transaction enters the subtree again.
//! - **A drop ends every handle that was read before it** (PN8): the
//!   cells of the Merk stay in their pages, and a handle into them would
//!   still be followed.
//! - **A drop, a clear and an enter are the tree layer's**
//!   ([`lignin_tree::drop`]). A batch that drops or clears is one
//!   [`drop::unlink`](lignin_tree::drop::unlink): the write to the tree
//!   pages with the root of each Merk named as dropped, the Merk's records
//!   in the directory, and the range of each dropped Merk in every flat
//!   column, by the lazy range delete. No node of the Merk is read, and no
//!   flat page between the two ends of a range: those pages are named in a
//!   record of a keyspace of their own, which the write path names as it
//!   names the directory's, and the reclaimer frees them. A subtree is
//!   entered through [`drop::enter`](lignin_tree::drop::enter), which
//!   takes out what an entry that a drop left under the prefix has, so a
//!   subtree starts with no flat record whatever was under its prefix
//!   before. A clear keeps the cleared Merk's own aux, roots and meta
//!   records, and takes its data.
//! - **A drop is given every column** ([`Keyspaces::columns`]) and the
//!   version the frames are built over. A record of unlinked pages says
//!   which version its writer read at, so frames that are made again over
//!   another base make the drop and the enter again with that base: they
//!   are made from the write set, by the same calls.
//! - **What was left is handed to the commit** (`Parts::dropped`).
//!   Reclaiming it is background work, which the engine starts (TXN-07).
//!
//! Three things a batch may not say, beside what it names that is not
//! there:
//!
//! - **Nodes and flat records in one data column.** The data column of a
//!   subtree holds the nodes of a Merk or flat records (ST1). A flat write
//!   where the batch, the write set or the state the transaction reads
//!   has nodes is refused ([`BatchError::NotFlat`]), and so is a change
//!   set where the write set has flat records ([`BatchError::NotMerk`]).
//! - **A root with no node.** A change set that sets its root writes or
//!   deletes a node ([`BatchError::RootAlone`]).
//! - **A host that keeps its pointers beside the Merk it hosts.** The
//!   node that hosts a Merk of the same batch names it ([`Hosted::Set`],
//!   [`BatchError::HostKeeps`]).
//!
//! A value is kept ([`ValuePut::Keep`]) or stated ([`ValuePut::Value`]).
//! Whether a stated value is stored in its cell or out of line is the
//! store's rule, by the value's length alone
//! ([`InlinePolicy`](lignin_tree::value::InlinePolicy),
//! [`WritePath::inline_policy`]). The bytes of a value out of line stay
//! with the transaction's frames until its commit carries them in the log
//! group and attaches them to the frame of their page (WT4, FL7). Every
//! rebuild of the frames states the write set to values that have not
//! seen it. The values of a load that is laid out are the exception ("A
//! load").
//!
//! **Stale hashes** (`lignin_tree::hash`). A node is written with the
//! hashes its writer made, and says which it did not make
//! ([`NodePut::stale`]). The frames keep those marks: they follow every
//! write of the tree pages, a later write of the node with the hash made
//! takes its mark off, and [`NodeView::stale`] shows them to a reader. The
//! marks are of the frames' pages, so they are made again from the write
//! set whenever the frames are, which is also what a rollback to a mark
//! does. The commit is handed them and evaluates them all (CM3 step 5).
//! Where every hash is made by its writer, as on existing GroveVersions,
//! nothing is marked, a write does nothing for them, and no rule runs.
//!
//! # Locations that are kept
//!
//! A [`NodeHandle`](lignin_tree::node::NodeHandle) is good under the lease
//! it was read under, while no cell has left its page. The transaction
//! takes a new lease whenever a page id could name another page, or a frame
//! could be another frame: when the latest version moves while it has no
//! frames, when its frames are made again (a rebase, a rollback to a mark,
//! a failed operation), and when it is rolled back. A handle of another
//! lease is not followed, and the node is found by its key. No address is
//! handed out that is not inside a handle.
//!
//! # A kept value and the read it is of
//!
//! A node that is written with its value kept ([`ValuePut::Keep`]) states
//! hashes that its writer made for the value it read. From the write on
//! the node is a write of its key, and a commit that writes the key
//! afterwards meets it (CF2). This section is the rule for the time before
//! the write (CF2a; D-097, D-098 item 7, D-100): **a kept value of the
//! committed state is dated at its read.** If a commit outside the
//! transaction wrote the node's key after the writer read the node, the
//! write is met where it is made, and the transaction's commit is a
//! conflict, as it is when the commit lands after the write. So no node is
//! published under hashes that were made for a value another commit has
//! replaced.
//!
//! - **The write names the read** by the node's handle ([`NodePut::at`]):
//!   the one the node was shown with when the writer read the value it
//!   made its hashes for ([`NodeView::handle`]). The handle a write
//!   reports for a node it wrote ([`Applied::nodes`]) can be named too,
//!   by a later keep of a writer that holds the node as that write left
//!   it: the node is the transaction's own from that write on, and the
//!   handle is dated at it. The handle of a cell a
//!   write moved without writing its key ([`Applied::carried`]) is a
//!   place and no read. Neither is a later read of the node the read the
//!   hashes were made for: a caller that keeps the newest place of a node
//!   keeps it apart from the handle of the read it made its hashes from.
//! - **The transaction dates its leases.** A handle carries the lease it
//!   was read under and no version. So the transaction remembers, for
//!   every lease it has shown a node under, the version the node was shown
//!   at, and keeps the key sets of the commits it takes in afterwards
//!   (`reads`).
//! - **A keep by a handle of the lease of its own operation** costs that
//!   one comparison: no commit was taken in since the read.
//! - **A keep by a handle of a lease that has ended** is compared, at the
//!   write, with the key sets of the commits after the lease's version, up
//!   to the write's ([`KeySet::meets_key`]): a key, a range that holds it,
//!   a subtree that was dropped or cleared, every key (CF5). If one of them
//!   wrote the key, the entry of the write is marked as met. No stored
//!   value is read for it.
//! - **A read the transaction cannot date is taken to be met**: a
//!   conflict that may be false, and never a wrong node. That is so for:
//!   - a handle of a lease it has shown no node under since it last ended:
//!     another transaction's handle, a snapshot's, one from before its own
//!     last commit or rollback;
//!   - a read that is more commits before the write than the transaction
//!     keeps key sets of (64, `reads::KEPT_COMMITS`);
//!   - a handle of the frames on which a write was made that was itself
//!     met in this way. The handles that write reported are of the
//!     write's version and name bytes the writer never read. They would
//!     pass for reads once a rollback has taken the write out. So the
//!     lease of those frames is forgotten, and with it the reads that
//!     were made through them.
//! - **A keep that names no read** of a node the transaction has not
//!   written, whose value is the committed state's, is refused
//!   ([`BatchError::NoRead`]). No page and no entry of the write set is
//!   changed by it. What the scope of the write claimed for the batch
//!   stays claimed, as for any batch that is refused.
//! - **A value the transaction stated itself is its own to keep.** No read
//!   has to be named for it, a handle that is named decides nothing, and CF2
//!   decides alone, from the transaction's first write of the key.
//! - **A node the transaction wrote before with its value kept has the
//!   committed value still**, however often it wrote it. A later keep by a
//!   handle is dated at that handle's read, as the first was: the read
//!   that is named may be an earlier one. A later keep that names no
//!   handle is of the transaction's own record of the node, which has
//!   none: that record is there only since the write that CF2 looks from.
//! - **A node that is not there has no value to keep**, whatever read the
//!   write names, and the batch is refused: no keep with nothing to keep
//!   enters the write set ([`BatchError::NoValue`]). Where its subtree
//!   was dropped, a write to the frames says [`BatchError::NotEntered`],
//!   and one that is taken by key says `NoValue`. Where a node is under
//!   the key again, a commit wrote the key, and the keep is met. A node
//!   that the transaction wrote and left out of its tree is there for as
//!   long as the unlinked record it left of it is ([`MerkWrite`],
//!   "Unlinked records").
//! - **Where a commit outside took the node away after the read, the
//!   refusal says so** ([`BatchError::ValueTaken`]), and is a conflict to
//!   the caller ([`TxnError::is_conflict`]). A commit took a node away
//!   where the keep names a read the transaction can date, the key set of
//!   a commit since that read holds the node's key (a delete of the node,
//!   a drop or a clear of its Merk), and the committed version the
//!   operation reads has no node under it. The refusal says so where that
//!   holds for every node of the Merk that the batch has no value to keep
//!   of. Only a refusal asks, so no write that is taken pays for it.
//!   Everywhere else the refusal is the plain one: for a keep that names
//!   no read, for a read the transaction cannot date, for a node the
//!   transaction deleted itself, for a Merk it dropped or cleared itself
//!   or that is below one it did, and for a drop of a Merk above the
//!   node's by a commit outside, whose key set does not name the Merk
//!   below.
//!
//! The mark is the entry's own. It stays while the entry is in the write
//! set: when the frames are made again, through a rollback to a savepoint
//! that was set after the write, when another scope is rolled back, and
//! when the scope that made the write is laid out later. It goes with the
//! entry (CF4). A write that is met has the transaction serve its records
//! by key (WT7b). It shows the node with the hashes its writer stated and
//! with its copy of the node's value. The copy is the one an earlier
//! write of the node took, where there is one, and otherwise the value
//! that was committed when this write was made. Either can be the bytes
//! that another commit gave the node after the writer's read: the hashes
//! may then not be theirs, until the transaction writes the node again
//! or its commit fails.
//!
//! The rule is about the commits of others. What the transaction did
//! itself between a read and the keep is its writer's to know: a rollback
//! to a savepoint that takes its own write of the node out leaves the
//! committed value there, and a keep by a handle from before that rollback
//! keeps it.
//!
//! **On one thread no key set is compared.** A writer that reads a node
//! and writes it with no commit of another taken in between writes at the
//! version it read at. Its keep is one of the lease of its operation,
//! which costs the one comparison, or of a lease of the same version,
//! which costs one lookup of the lease: the node was read before the
//! transaction's first write made its frames. No commit can land in
//! between where one thread reads and writes a Merk within one operation,
//! so no keep is met or taken to be met there.
//!
//! # Scopes and threads
//!
//! A transaction is `Send` and `Sync`. Its writes go through a [`Scope`],
//! and any number of scopes may be open, each on a thread of its own:
//! [`crate::scope`] says what a scope claims, how it merges or is rolled
//! back, and what the transaction keeps for its scopes. This module is
//! what a scope that is alone writes through, and what every scope reads
//! through. Savepoints are marks kept in a stack ([`crate::savepoint`]):
//! [`Mark`] and the rollback to one are what they build on.
//!
//! Every call holds the transaction's own lock (LT7 level 2) for its
//! length, and the function a node is shown to runs under it: it must not
//! call the transaction.
//!
//! # What the commit takes
//!
//! `WriteTxn::seal` hands the transaction to its commit
//! ([`crate::commit`]), which holds the commit lock (CM3 steps 1 to 4):
//! the commits published since are taken
//! in, a met write is the conflict (CF2), and otherwise the frames are over
//! the latest version and are handed over with the key set of the commit.
//! The commit installs the tree copies, each with the run of delta records
//! that its changes made, and the flat writer's copies, publishes the page
//! ids, frees the freed pages, records the key set ([`Commits::record`])
//! and then publishes the version. The key set is recorded first, so that
//! a transaction that reads a version finds the key set of every commit up
//! to it.
//!
//! # What a transaction holds
//!
//! - **A version pin** on its base while it has frames, and none while it
//!   is idle or serves its records by key. Each cursor has a pin of its
//!   own, and a cursor over a Merk a lease of its own beside it.
//! - **Private copies** of the pages it changed, made or freed. The pool
//!   cannot evict them, and no page step changes a page under one (PV7).
//!   A copy of a tree page is asked for with
//!   [`make_private`](lignin_store::cache::BufferPool::make_private), and
//!   the flat keyspaces' writer asks for its own with
//!   [`make_private_once`](lignin_store::cache::BufferPool::make_private_once):
//!   one form for each kind of page.
//!   Their number is reported ([`TxnStats`]) and not bounded: nothing is
//!   spilled to a file (WT4 allows it, and does not ask for it).
//! - **Page ids** of the pages it made, until its commit publishes them or
//!   its frames are given up (ID1).
//! - **A watch on the key sets of commits** ([`Commits`]), from its first
//!   write, from the first read that shows it a node, or from the first
//!   cursor it opens over a Merk, whichever comes first. A cursor watches
//!   from when it is opened, and its lease is dated then, before it has
//!   shown a node. The watch stands before the version of the read is
//!   taken, so the key set of every commit after a read reaches the
//!   transaction. A read that shows no node (a key with no node, a handle
//!   that is no longer good, a read that the claim of another scope
//!   refuses) dates nothing, and gives the watch up again unless the
//!   transaction has a write or remembers a lease. A read that fails
//!   after it was claimed, on the store's files, leaves the watch as it
//!   is. The watch is given up when the transaction ends, by a rollback or
//!   by a commit, also one that commits nothing, and before that when a
//!   rollback takes its last write out while it remembers no lease. A
//!   cursor that is open does not hold the watch by itself: its lease is
//!   forgotten like any other, at the transaction's end or when the key
//!   sets no longer reach back to it. It dates its lease again, at the
//!   version it reads, whenever it shows a node of that version, and the
//!   watch starts anew then. A keep by a handle of that lease is answered
//!   for where the key sets the transaction holds reach back to that
//!   version, and is taken to be met otherwise. A read of the cursor that
//!   shows no node gives the watch up as any other does. A transaction
//!   that reads flat records only never watches. While a transaction
//!   with a write runs no operation, [`Commits`] keeps the key set of
//!   every commit that is published, which is what the conflict rule
//!   compares its writes with (CF3). One that watches with no write holds
//!   back the newest 64 and no more: it dates its reads with them, and
//!   keeps no more of them itself. It holds all from before the version
//!   its first write is dated with. Once its last write is taken out
//!   again, it holds the newest where it remembers a read still, and
//!   gives the watch up where it remembers none.
//! - **The version of each lease it has shown a node under, and the key
//!   sets of the newest 64 commits it took in** while it remembers such a
//!   lease (`reads`; "A kept value and the read it is of"). The leases are
//!   its own for each version it read at, one for each version it read a
//!   committed node at ([`WriteTxn::committed_node`]), that of each making
//!   of its frames and of each write of theirs that drops or clears a
//!   Merk, and that of each cursor it opened over a Merk. A lease is one
//!   small record. It is forgotten when the transaction ends, and when
//!   the key sets no longer reach back to its version, which is at the
//!   65th commit after it: with no commit taken in, the records of a
//!   transaction that opens cursor after cursor stay until it ends, also
//!   those of cursors that are gone. A key set is shared with every
//!   transaction that keeps it.
//! - **A hold on the value-log files its loads appended to**, from the
//!   first value out of line of a load until its write set is empty again
//!   or its commit takes the hold with its frames
//!   ([`crate::commit`], "The commit of a load"). While it is held, no
//!   such file is deleted.
//! - **A place among the open write transactions of the store's counts**
//!   ([`WritePath::stats`]), from its first write until its write set is
//!   empty again: by its commit, a rollback, a rollback to a savepoint
//!   that takes every write out, or its drop. That is the time in which it
//!   can hold private copies, which is what holds the log
//!   (`lignin_store::stats`, "What an operator watches"). A transaction
//!   that only reads is not counted.
//!
//! # Choices made here that the plan left open
//!
//! 1. **A transaction that is overlapped has no frames.** It serves every
//!    record by key from its write set, as RocksDB serves a transaction's
//!    batch, and is laid out again only if a rollback empties the overlap
//!    set.
//! 2. **The key sets of commits are kept until every transaction with
//!    writes has taken them** ([`Commits`]). The other way, marking the
//!    met writes at the commit, makes the committer take each
//!    transaction's lock. The commit keeps CF3 this way
//!    ([`crate::commit`], "The conflict rule"). A transaction with no
//!    write that has been shown a node holds back the newest 64 only
//!    (D-155).
//! 3. **The subtree directory is written through the transaction's own
//!    flat pages**, in the operation that changes it, so its records
//!    commit, roll back and are made again with the frames. A batch is
//!    checked against it before a page is changed, so that a batch which
//!    names what is not entered is refused with every page as it was; what
//!    the directory itself refuses afterwards costs the frames, which are
//!    made again.
//! 4. **The flat records are in one tree for each column** ([`Keyspaces`]),
//!    under the subtree's 32 bytes and the key. Where the roots of those
//!    trees are kept is TXN-07's.
//! 5. **A Merk's nodes are written by key, with handles as hints.** A node
//!    of a batch is found by descending from the root of its Merk unless
//!    the caller passes a handle that is still good. A node that keeps a
//!    committed value names its handle whether it is still good or not:
//!    it says when the value was read ("A kept value and the read it is
//!    of").
//! 6. **A node is shown to a function**, not returned: the view borrows a
//!    latched frame or the write set, and no latch is held when a call
//!    returns (LT3).
//! 7. **The cursor stands on a key.** A write under an open cursor is
//!    defined: the cursor reads what is under its key now, and steps on
//!    from the key. This tightens RD6, which left it unspecified. A scope
//!    takes a write through a shared reference, so that its own cursor
//!    can be open across it, and is one thread's at a time.
//! 8. **A transaction that gave a root to a Merk that had none is not
//!    laid out over a root that a commit outside made since**
//!    ([`BatchError::RootTaken`]). No writer of an existing GroveVersion
//!    gets there, because both would have written the element that hosts
//!    the Merk (GM3). The rule for iteration two is GROVE-11's.
//! 9. **A node's value is read where it is, by the tree layer's read
//!    path** ([`NodeView::value`], `lignin_tree::value`): in its cell, in
//!    the value log, with the frame of its page while it is pending, or
//!    with the transaction that wrote it. The frames keep the values of
//!    their cells in a [`TreeValues`](lignin_tree::value::TreeValues),
//!    which is what the commit logs, attaches and counts from.
//! 10. **The transaction finds which nodes of a batch are in the tree**:
//!     from the root the change set states, through the links it states.
//!     The writer does not say which of its nodes the tree does not
//!     reach. Such a node is kept as an unlinked record, in a form of the
//!     transaction's own, in the flat keyspace of the data column. Under
//!     a root that is kept, a written node that has a cell stays in its
//!     tree ([`MerkWrite`], "Unlinked records").
//!
//! 10. **A load is stated by its change set**, and the engine does not
//!     take a change set for one by its shape. The commit of a load can
//!     sync a value-log file and replace the manifest, which no other
//!     commit does, so a caller asks for it.
//! 11. **The loads of a store append to one value-log file at a time**
//!     ([`LoadFiles`]), not each transaction to a file of its own: a
//!     session that loads in many transactions would otherwise leave a
//!     file for each.
//! 12. **Frames that are made again append the values of a load again.**
//!     The record of a value names the page of its cell, and pages that
//!     are made again have other ids. Keeping the pages of a load through
//!     a rebuild would spare it, and needs pages that outlive the frames
//!     they are of.
//! 13. **A Merk that a node of a load hosts is laid out beside its host,
//!     not inside the host's fragment**, which the loader can also do
//!     ([`HostedLoad::Loaded`](lignin_tree::bulk::HostedLoad::Loaded)).
//!     The cells of one write of the loader are then of one Merk, which
//!     is how the handles of a load's nodes are told apart by their keys.
//! 14. **The loads of one batch share the loader's pages, and those of
//!     two batches do not.** A loader fills a page of each level until it
//!     is done, and nothing else may write those pages meanwhile. So a
//!     caller that loads many small Merks hands them over in one batch.
//!
//! # What is not here
//!
//! - **A stale hash in a load**: a load states every hash
//!   ([`WholeFault::StaleHash`]). Where iteration two hands a Merk over
//!   with hashes left to finalize, it marks them in a write after the
//!   load (GROVE-11).
//! - **A Merk in two loads**: a load is the whole of a Merk that had no
//!   root. A Merk too large for one change set is the caller's to cut
//!   another way (`DECISIONS.md` D-087 item 5).
//! - **The record Merk's callers expect from a cursor**, which Merk's
//!   store builds (`merk::store::lignin`). A cursor over a Merk shows the
//!   node ([`MerkCursor::node`]): the transaction's own, or the one that
//!   the ordered cursor of the tree layer ([`lignin_tree::cursor`]) stands
//!   on at the cursor's version. A record names each child by its key,
//!   which a cell does not hold, so the cursor also reads the keys of the
//!   node's children with it ([`MerkCursor::node_with_children`],
//!   [`ChildKeys`]): for a node of its committed side from the children's
//!   cells at its own version, which no read of the scope reaches.
//! - **Starting a reclaimer** (CL6): what a drop left is reclaimed by
//!   `lignin_tree::drop::Reclaimer`, which the engine runs on a thread of
//!   its own, with the log of its page steps ([`crate::db`], "Background
//!   work").
//! - **While the transaction serves its records by key**, a batch is not
//!   checked against the directory, but for a load: its subtree is
//!   entered and its Merk has no root, as the write set says it over the
//!   committed version, and its batch drops and clears no subtree (LD6).
//!   What else a batch says of subtrees is checked when the frames are
//!   made, and its commit is refused then.
//! - **A flat write among the nodes of a Merk** is refused
//!   ([`BatchError::NotFlat`]).
//! - **Scopes, claims and pending marks**, and the conflict rule for what
//!   a drop covers below the subtree it names (CF5): [`crate::scope`].
//!   [`MerkWrite::anchored`] is passed on to the tree layer, and the frames
//!   answer it which hosts are behind from the marks.

mod arena;
mod check;
mod commits;
mod cursor;
mod error;
mod frames;
mod key;
pub(crate) mod load;
mod opened;
mod pages;
mod path;
pub(crate) mod reads;
#[allow(clippy::module_inception)]
mod txn;
pub(crate) mod unlinked;
mod view;
mod write;
pub(crate) mod write_set;

/// A page store on disk with its log and its write-back, as the open of the
/// engine will make them: the store of this crate's own tests.
///
/// It is for tests outside this crate that read the parts of a store,
/// which the engine ([`crate::Db`]) gives nobody; a test of what a user of
/// the engine does opens a [`crate::Db`]. [`testing::Db::new`] makes a
/// store, [`testing::Db::path`] its write path, and
/// [`testing::Db::commit`] commits a transaction through the commit of
/// this crate ([`crate::commit`]). It runs no write-back loop.
#[cfg(all(feature = "testing", not(loom)))]
#[path = "tests/store.rs"]
#[expect(
    clippy::should_implement_trait,
    reason = "the generator of the tests has a `next` and is no iterator"
)]
pub mod testing;

#[cfg(all(test, not(loom)))]
pub(crate) mod tests;

pub use commits::Commits;
pub use cursor::{CommittedSide, Cursor, FlatSide, MerkCursor, MerkSide, ParkedCursor, Resumed};
pub use error::{BatchError, Differs, HostName, TxnError, WholeFault};
pub use key::{Column, KeySet, Subtree, Unit};
pub use load::LoadFiles;
pub(crate) use path::Store;
pub use path::{FlatReader, FlatWriter, Keyspaces, WritePath};
pub(crate) use txn::Held;
pub use txn::{Scope, TxnStats, WriteTxn};
pub use view::{ChildKeys, LinkView, NodeView, SubtreeView};
pub use write::{
    Applied, Batch, Entering, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut,
    SubtreeWrite, ValuePut,
};
pub use write_set::Mark;
pub(crate) use {
    frames::FlatFrames,
    pages::{TreeCopy, TreePages},
    txn::{Parts, Sealed},
};

/// The write half of a snapshot being read-only (`transactions.md` RD1,
/// T-RD5): the entry point that writes through a transaction answers
/// [`SnapshotError::ReadOnly`](crate::snapshot::SnapshotError::ReadOnly)
/// for a snapshot, and changes nothing.
impl crate::snapshot::Snapshot<'_> {
    /// Refused: a snapshot is read-only.
    pub fn apply(&self, batch: &Batch<'_>) -> Result<Applied, crate::snapshot::SnapshotError> {
        let _ = batch;
        Err(crate::snapshot::SnapshotError::ReadOnly)
    }
}
