//! The flat keyspace: a B+tree of flat pages for ordered data that is not
//! part of a Merk tree.
//!
//! - [`FlatTree`] is one keyspace: get, put, delete, range delete, a
//!   write of many keys and a [`Cursor`], over keys and values of any
//!   length.
//! - [`FlatPages`] and [`FlatPagesMut`] are what the tree asks of whoever
//!   holds its pages: a reader at one version, and the one writer of a
//!   write transaction.
//! - [`Committed`] reads the committed pages of a store, and [`Private`]
//!   is a writer's pages as private copies in the buffer pool.
//! - [`log`] makes what a commit writes to the log for a flat page.
//! - [`FlatOwners`] answers the value log's cleaner for flat leaves.
//!
//! The bytes of a page are `lignin-format`'s (`docs/lignindb/flat-page.md`),
//! and this module adds no format to them. What it adds above them is
//! stated here, and listed at the end for the tasks that build on it: the
//! subtree directory (TREE-05), the write transaction (TXN-03, TXN-05),
//! write-back (STORE-06) and the adapters that put GroveDB's `FlatStore`
//! and Merk's aux and meta records on it (GROVE-04, MERK-05). The module
//! knows no cost rule: `FlatStore` reports costs, and the adapter adds
//! them (`docs/lignindb/flat-store.md`).
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{manifest::Manifest, CommitVersion, PageSize};
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::{
//!     alloc::PageAllocator,
//!     cache::{BufferPool, OwnerId, PoolConfig},
//!     load::PageLoader,
//!     segment::{ManifestStore, SegmentStore},
//!     table::PageTable,
//!     values::ValueStore,
//! };
//! use lignin_tree::{
//!     flat::{Committed, FlatConfig, FlatTree, Private},
//!     OperationGuard,
//! };
//!
//! // What keeps the pages an operation read: the snapshot layer's guard.
//! struct Guard;
//! impl OperationGuard for Guard {}
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let size = PageSize::DEFAULT;
//! let empty = Manifest::new(size, Default::default(), None, Vec::new())?;
//! let manifest = ManifestStore::create(vfs, dir.path(), empty)?;
//! let segments = SegmentStore::open(&manifest)?;
//! let values = ValueStore::open(&manifest, &[])?;
//! let (table, ids) = (PageTable::new(), PageAllocator::new());
//! let pool = BufferPool::new(PoolConfig {
//!     page_size: size,
//!     capacity: 8 << 20,
//! })?;
//! let loader = PageLoader::new(&pool, &table, &segments, size)?;
//!
//! // The flat pages of a write transaction: private copies, over the
//! // committed pages of the version it reads.
//! let guard = Guard;
//! let base = Committed::new(loader, &values, &guard, CommitVersion::EMPTY);
//! let mut pages = Private::new(base, &pool, OwnerId::new(1), &ids);
//! let tree = FlatTree::create(&mut pages, FlatConfig::new(size))?;
//!
//! tree.put(&mut pages, b"aux/state", b"a value")?;
//! // A value that is too long for a page goes out of line, and a key that
//! // is too long for one goes into the tree of a bucket.
//! tree.put(&mut pages, b"meta/root", &[7; 100_000])?;
//! tree.put(&mut pages, &[b'k'; 100_000], b"under a long key")?;
//!
//! let mut value = Vec::new();
//! assert!(tree.get(&pages, &[b'k'; 100_000], &mut value)?);
//! assert_eq!(value, b"under a long key");
//!
//! // The keys of one prefix are a keyspace of their own.
//! let mut cursor = tree.cursor_within(b"meta/");
//! cursor.seek_to_first(&pages)?;
//! assert_eq!(cursor.key(), Some(&b"root"[..]));
//! cursor.next(&pages)?;
//! assert!(!cursor.valid());
//!
//! assert!(tree.delete(&mut pages, b"aux/state")?);
//! tree.delete_prefix(&mut pages, b"meta/")?;
//! assert!(!tree.get(&pages, b"meta/root", &mut value)?);
//! # Ok(())
//! # }
//! ```
//!
//! # Keys
//!
//! A key is a byte string, and keys are in the order of their bytes, a key
//! before every longer key that begins with it (`flat-page.md` P1). The
//! empty key is a key. A value is a byte string of up to 2^32 − 1 bytes,
//! and may be empty.
//!
//! One tree can hold many keyspaces, each the keys of one prefix, as the
//! keyspaces of GroveDB's subtrees are (`flat-store.md`):
//! [`FlatTree::cursor_within`] walks one of them and shows its keys without
//! the prefix, and [`FlatTree::delete_prefix`] removes one. How many trees
//! an engine keeps, and what it puts before a key, is the engine's.
//!
//! **No key is too long.** A page stores a key whole, so the tree stores no
//! key longer than [`FlatConfig::key_limit`], an eighth of a page. A longer
//! key is kept in the **bucket** of its first `key_limit` bytes: one entry
//! of the tree, whose key is those bytes followed by one zero byte, and
//! whose value is the root page of a tree of its own. That tree holds the
//! keys that begin with those bytes, each under the rest of the key, and a
//! rest that is still too long has a bucket in that tree in turn. An entry
//! whose key is longer than the limit is therefore a bucket, and it stands
//! where its keys belong: behind the key that is exactly its first bytes,
//! and before every other key that is greater. Get, put, delete, range
//! delete and the cursor take a long key as any other, and a cursor shows
//! it whole.
//!
//! The tree of a bucket is a tree like any other: it grows and shrinks by
//! pages, there is no limit to the keys that share their first bytes or to
//! their values, and a write to one of them changes the pages on its way
//! and no others. A write never leaves it empty: the last key that a
//! delete takes out of it takes the bucket along. A key is as many trees
//! deep as it has eighths of a page, and a write of it reads one leaf for
//! each: keys that long are for what RocksDB accepts and nobody writes,
//! not for a workload.
//!
//! # The tree
//!
//! A keyspace is named by its **root page**, which is the same page for as
//! long as the keyspace exists. A leaf holds entries, a branch holds
//! separators between its children (`flat-page.md` P6), and every leaf is
//! as many levels below the root as every other.
//!
//! **The prefix of a page is what its keys share**: the longest byte
//! string that the first and the last key of the page begin with, and the
//! whole key in a page of one key. So the bytes of a page follow from what
//! it holds and from nothing else (`flat-page.md` P4, `format.md` B9), and
//! the invariant checker can ask for exactly that prefix.
//!
//! **A page that has outgrown its image is cut.** The first piece stays in
//! the page and each other piece goes to a new one. Each piece is as long
//! as it can be under the prefix its own keys share, and where one cut is
//! enough it is put where the two pieces are nearest in length. Keys that
//! arrive in order are the exception: where a new entry goes on a run of
//! keys, which the key after it does not belong to, the cut keeps the run
//! together, so that the keys which follow find room at the end of their
//! page and the pages behind them stay full. The separator of two leaves
//! is the shortest byte string that divides them. A branch that is cut
//! gives the separator at each cut to the branch above it. The root is cut
//! like any page, except that every piece goes to a new page and the root
//! becomes the branch above them.
//!
//! Any two entries fit one page: a stored key is at most `key_limit + 1`
//! bytes and a value in a page at most [`FlatConfig::max_inline`], a
//! quarter of a page or less. So a page that is too long can always be
//! cut, and no piece is empty.
//!
//! **A page that has lost entries is looked at.** A leaf with no entry is
//! freed and taken out of its branch, with the separator to its left. A
//! page whose image is under a quarter of a page is merged with the
//! neighbour to its right below the same branch, or to its left if it is
//! the last child, if the two make a page of at most three quarters. The
//! left page of the two takes the entries of both and the right one is
//! freed. A branch that has lost a child is looked at in turn. A root with
//! one child takes what that child holds, and the child is freed.
//!
//! **A range delete** goes down from the root. A page whose key range lies
//! inside the range is freed with every page below it, and every leaf among
//! them is read first, for the values its entries hold out of line and for
//! the trees of its buckets. A leaf at an end of the range loses the
//! entries inside it. Afterwards the leaf at each end of the range is
//! looked at as above, if it lost an entry or pages beside it left, and no
//! other page is. A range delete that finds nothing to remove copies no
//! page and writes none, in the tree of a bucket as in any other. Unlinking a range without reading it is the
//! subtree drop's (TREE-09).
//!
//! **A write of many keys hands a leaf one image** ([`FlatTree::write`]).
//! It is given changes in the order of their keys ([`Changes`]): for each
//! key the value it gets, or none where its record goes. It goes through
//! the keys that a page stores as they are from the last to the first: it
//! goes down to the leaf of the last key that is left, takes that leaf
//! once, makes in it every change whose key lies in the leaf's key range,
//! and writes what the leaf then holds, once; then it goes down again for
//! the next key. What the leaf holds after all its changes is cut and
//! looked at by the rules above, before it is written: a leaf that has
//! outgrown its image is written as the pieces it needs; one that is left
//! with no entry is freed and not written; one that lost an entry and is
//! under a quarter of a page is merged with its neighbour in the one
//! image that is written, or into the root where the two are the root's
//! only children. So the write hands its pages at most one image for each
//! leaf it changes and one more for each further piece of a cut, and no
//! page but the root is handed two images of a leaf: the root is where a
//! leaf that is left with no entry was one of its two children, and the
//! root takes what the other holds while the write has keys of that one
//! still to come to. A leaf is for that not merged with a neighbour to
//! its left that the write has still to come to, and stays under a
//! quarter of a page until a later write looks at it. A leaf in which no change changes anything (a delete of a key that
//! is not there, a value that the key has already in its page) is neither
//! taken nor written. The keyspace holds afterwards what a put and a
//! delete for each change leave it holding. Its pages can be other pages,
//! because a leaf is cut and merged once for all it takes: where the new
//! entries of a leaf stand beside each other and end a run of keys, the
//! cut keeps the run together as it does for one key. A key that is kept
//! in a bucket is written by itself, after the others, and what is said
//! here of one image is not said of it. [`Written`] says how many leaves
//! took changes and how many images of leaves were written for them. The
//! subtree directory writes the records of one batch this way (TXN-03),
//! and none of its keys is kept in a bucket.
//!
//! # Values
//!
//! A value of up to [`FlatConfig::max_inline`] bytes is in its entry. A
//! longer one is **out of line**: the entry holds the 16 bytes of the
//! value's place (`flat-page.md` P5), a pointer into the value log or, for
//! a value that is not there yet, a pending value (P15).
//!
//! - **A new value is pending.** A put hands the value to its writer
//!   ([`FlatPagesMut::hold_value`]), for the page of its entry. The value
//!   travels in the commit's log group and reaches the value log at
//!   write-back (`transactions.md` FL7).
//! - **A value that an entry stops holding is dropped**
//!   ([`FlatPagesMut::drop_value`]): the entry was deleted, was given
//!   another value, or was in a range that was deleted. For a pointer, the
//!   writer's commit releases it (`value-log-format.md` O6).
//! - **An entry that goes to another page takes its value along**
//!   ([`FlatPagesMut::carry_value`]). This is the move rule of the value
//!   log (O5): a value's record names the page of its owner, so an entry
//!   that a split, a merge or a root that takes its child carries to
//!   another page gets the value again, as a pending value of the new page,
//!   and gives up the place it held, in the same write and so in the same
//!   commit. An entry that stays in its page keeps its place, whatever
//!   happens to the entries around it (O3).
//!
//! # The pages of a reader and of a writer
//!
//! The tree holds no page. A read is given [`FlatPages`] and a write
//! [`FlatPagesMut`], and the transaction layer decides what stands behind
//! them. Two implementations on the page store are here.
//!
//! [`Committed`] is a reader at one version: a page comes from the buffer
//! pool or is loaded into it (`lignin_store::load`), and a value in the
//! value log is read through its pointer with the page as its owner. It
//! borrows the guard of its operation
//! ([`OperationGuard`](crate::OperationGuard), `transactions.md` PN5), and
//! a page it hands out borrows the guard through it, as a page of the tree
//! pages' reader does ([`NodeReader`](crate::node::NodeReader)).
//!
//! **A value is read for the page that is held.** A page step can give a
//! page another frame at any time: write-back puts a pointer where a
//! pending value was, the cleaner puts another pointer, and a reclaim
//! takes out the entries that a dropped subtree left (`transactions.md`
//! PV7). So [`FlatPages::read_value`] is given the page that the entry was
//! read from, still held, and a pending value is read from that frame and
//! not from the page as it is by then.
//!
//! [`Private`] is the flat side of a write transaction. It reads its own
//! copies, and for every other page what [`Committed`] reads.
//!
//! - **A page is copied before it is changed, and before anything is
//!   concluded from it** ([`FlatPagesMut::take`]). From the copy on, no
//!   page step changes the page: a pointer that the writer reads from its
//!   copy and releases is the pointer the page holds at the commit, and
//!   the entries that a write removes or keeps are those of the copy, not
//!   of the page as it was when the write went down to it. A committed
//!   page that the writer frees keeps its copy until the commit, for the
//!   same reason. A page step changes one leaf and never a branch, so the
//!   way down to a leaf is as it was read.
//! - **A copy is the writer's or nobody's.** The writer asks the pool for
//!   a copy that it alone is given
//!   ([`BufferPool::make_private_once`](lignin_store::cache::BufferPool::make_private_once)):
//!   a copy that the pool holds under the writer's owner id is refused in
//!   the step that would make one, so two writers under one owner id never
//!   change one copy, whichever asks first. A copy that cannot be made
//!   whole is discarded at once.
//! - **The writer's copies outlive its operation.** The writer borrows an
//!   operation's guard for the committed pages it reads, and
//!   [`Private::under`] takes it to the next operation of its transaction
//!   with all it holds.
//! - **A writer ends as a whole.** [`Private::into_parts`] is its commit,
//!   and [`Private::roll_back`] gives it up. After a call that failed the
//!   writer's pages are in no state to go on from, and giving the writer
//!   up is what is left. What the pool would not let go of stays with the
//!   writer, and giving it up again finishes that. The id of a page that the writer made and freed
//!   again stays the writer's until one of the two, so that nobody is
//!   given it while a rollback could bring the page back (ID1). An id
//!   goes back only with its copy: one whose copy the pool would not
//!   discard stays reserved.
//! - **A flat page is written whole.** An image is immutable and exactly
//!   as long as its content (`flat-page.md` P11), so every change builds
//!   the image again and puts it at the start of the copy's frame.
//! - **The bytes of a pending value are attached to the frame of its
//!   page, under the 8 bytes that name the value** ([`Committed`] reads
//!   them there), as the bytes of a tree cell's pending value are. The
//!   writer keeps them by that name and attaches them again whenever it
//!   writes the page. A value that is given its log address has another
//!   name from then on, and its bytes go with it
//!   ([`Private::log_values`]). Placing a value takes its bytes off.
//!
//! # What a commit writes
//!
//! A commit makes its log group of three things (`log-format.md` LG14,
//! LG19 to LG23), which [`Private`] gives it:
//!
//! 1. **The values** ([`Private::log_values`]). Every value that an entry
//!    of the writer holds and no log has is written into the group for the
//!    page of its entry, and the entry gets the pending value that names
//!    the value's log address ([`log::stamp_pending`]). A carried value is
//!    repeated, not named (LG23): its bytes go into the group again. A
//!    group is written for one place (LG22), so a commit that writes its
//!    group again for another place takes this step and the next again,
//!    and the values of the writer are written and named anew each time.
//!    A value that was pending when the writer copied its page is in the
//!    group of an earlier commit, and stays as it is.
//! 2. **The pages**, in ascending order ([`Private::log_page`],
//!    [`Private::freed`]). The run of a page is the records that take it
//!    from what it was to what it is ([`log::push_changes`]): removes and
//!    ranges of removes, then inserts and replaces, then the prefix, or an
//!    image where there was no page, where no record says the change or
//!    where the image is shorter. They are what the changes come to,
//!    however often the page was written.
//! 3. **The pointers released** ([`Private::released`]).
//!
//! Then it installs the copies ([`Private::into_parts`]). A copy that is as
//! the page was has no run, and is discarded instead. The ids of the pages
//! that the writer made and freed again go back with the commit.
//!
//! # The value log's cleaner
//!
//! [`FlatOwners`] is `ValueOwners` for flat leaves: it finds the entry
//! that holds a pointer in the current version of the page a value's
//! record names, and gives it another pointer in a page step, which builds
//! the leaf's image again in a frame of its own and is refused under the
//! three checks of PV7. The step is dated by the latest version at the
//! moment it is installed.
//!
//! # The cursor
//!
//! [`Cursor`] is seek, seek-for-prev, first, last, next and prev, with the
//! positions `flat-store.md` asks for (rule 6). It holds no page between
//! calls and no leaf links exist, so it moves from leaf to leaf through
//! the branches above them, which it remembers. Under a writer whose pages
//! have changed since the cursor moved, a step goes on from the cursor's
//! key: every write under a cursor is defined, the delete of the record it
//! stands on among them (rule 7). The pages say that they have changed
//! with a [`Revision`] that no two states of any reader or writer share.
//!
//! Under a reader the cursor knows where it stands by the key of the entry
//! there, not by the entry's position alone: a page step can take an entry
//! out of a leaf under it. It then reads its record by the key and steps
//! on from the key, and a record that is gone has no value. Under a long
//! key the cursor is in the tree of each bucket on the way to it, and goes
//! from the last key of one to what follows the bucket.
//!
//! **A walk holds its leaf.** [`Cursor::walk`] goes from the record the
//! cursor stands on to the first record that its caller stops at, and
//! shows the caller each record on the way, that one first: its key, and
//! its value where the caller asks for it ([`Look`]). Over pages that do
//! not change meanwhile that is [`Cursor::key`], [`Cursor::value`] and a
//! step, one after another. It is for a caller that passes many records
//! and reads little of each, as the subtree directory does past the
//! entries of Merks with no node.
//!
//! - **Records that a leaf shows.** While the leaf of the cursor's record
//!   shows that record (the pages are of the cursor's revision, and the
//!   entry is where the cursor left it), the walk holds that leaf, and no
//!   other page, across the records it shows of it, and calls its caller
//!   with the leaf held. A value that the leaf holds itself is shown as
//!   the leaf's bytes, and a value out of line is read for the leaf that
//!   is held, as every value is, where the caller asks for it. What is
//!   shown is what the leaf held when the walk read it: an entry that a
//!   page step takes out of the leaf while it is held is still shown, as
//!   it is to a cursor that read it just before the step.
//! - **A record that its leaf does not show.** Where the pages are of
//!   another revision than the cursor's, or the entry is not where the
//!   cursor left it, the record the cursor stands on is shown as
//!   [`Cursor::key`] and [`Cursor::value`] show it, with no page held: its
//!   key whether or not the record is still there, and its value, read by
//!   the key into a copy, where the caller asks for it and the record has
//!   one. The walk goes on from the key, as a step does.
//! - **From leaf to leaf** the walk moves as a step of the cursor does.
//! - **The position.** The walk leaves the cursor on the record its
//!   caller stopped at, or not positioned where the caller stopped at
//!   none. The cursor is not moved while the caller runs.
//!   A walk that fails leaves no position.
//!
//! # Damage
//!
//! Pages that are not a tree end a call with [`FlatError::Damaged`], or
//! with the error of the page that could not be read, and not without an
//! end. What a failed write has changed by then is the writer's to give
//! up. Three things are checked, each from the pages on the call's own
//! way, and they are what bounds every call:
//!
//! - a page is the page that was asked for, and one level below the
//!   branch that names it, so no way down is longer than the root's level;
//! - the tree of a bucket begins at none of the pages on the way to the
//!   bucket: not at the root of a tree the call is in, and not at a
//!   branch or the leaf it passed. So a call goes into a tree once, and no
//!   long key stands for a record of a tree on its way;
//! - a page that a range delete freed is not read again.
//!
//! What is not on a call's way is not checked by it. A branch that names
//! a page of another tree at the right level, or a bucket whose tree
//! begins at such a page, reads as what that page holds. Finding it takes
//! a walk of every page of the keyspace, with each page reached once and
//! every key inside the bounds its branches give: that walk is the
//! invariant checker's, and the tests here make it after every step.
//!
//! # What the callers do
//!
//! - **A reader** holds an operation guard for as long as it uses pages it
//!   read, which [`Committed`] sees to, and reads at a version it has
//!   pinned or at the latest version it read under the guard
//!   (`transactions.md` PN5).
//! - **A write transaction** has one writer of a keyspace at a time. Its
//!   write set is the truth (WT3): after an error it gives the writer up
//!   ([`Private::roll_back`]) and makes its pages again from its write
//!   set, as it does for a rebase (WT7, WT10).
//! - **A commit** takes the three parts above in that order, releases the
//!   pointers under the counts lock, and frees a page only after it has
//!   discarded the writer's copy of it under that lock.
//! - **Write-back** places each pending value of a leaf in a page step,
//!   takes its bytes off the frame under the 8 bytes its entry held, and
//!   writes a leaf to a segment only when no value of it is pending
//!   (`flat-page.md` P15).
//! - **The drain of the pending prefix drops** (`flat-store.md`) lists its
//!   records with a cursor, in ascending order of prefix, and makes each
//!   step, a [`FlatTree::delete_prefix`] or a delete, a commit of its own.
//!   The steps are durable in the order they were made because their
//!   groups are in the log in that order.
//!
//! # Choices made here that the plan left open
//!
//! 1. **The prefix of a page is what its keys share**, not what the bounds
//!    of its key range share. With the bounds, the last page of a run of
//!    keys has no prefix, because its range is open, and the pages cut
//!    from it are as full as their keys were without one: 28% under keys
//!    that arrive in order, against 91%. A put can now change the prefix
//!    of its page, which costs nothing while a page is built again for
//!    every change, and one record of the prefix in the log.
//! 2. **Long keys are kept in the trees of buckets**, above the page
//!    format, which is unchanged. The limit is an eighth of a page. A
//!    bucket's value is the four bytes of its tree's root page, in the
//!    entry: the invariant checker and the subtree drop follow it as the
//!    tree does, and tell a bucket by the length of its key.
//! 3. **The root keeps its page id.** Whoever names a keyspace stores one
//!    id for good. A root that is cut carries every entry it holds, with
//!    its values, to new pages.
//! 4. **A value goes out of line above an eighth of a page**, and
//!    [`FlatConfig::with_max_inline`] sets another limit, from 16 bytes to
//!    a quarter of a page. The threshold of tree cells is TREE-08's.
//! 5. **Merging**: under a quarter, into at most three quarters, with a
//!    neighbour below the same branch, and no entry is moved between two
//!    pages that both stay. An empty leaf always leaves.
//! 6. **A cut keeps a run of ordered keys together.** Without it, keys
//!    that count up under several prefixes in turn leave half-full pages.
//! 7. **The run of a page is made at the commit, from the page before and
//!    after**, not gathered write by write.
//! 8. **A pending value's bytes are attached under its name**, the 8
//!    bytes its entry holds, as for a tree cell. The position of
//!    the entry, which [`FlatPages::read_value`] is still given, names
//!    nothing in the pool.
//! 9. **A range delete reads every leaf it frees.** The lazy form is
//!    TREE-09's.
//! 10. **The pages are behind two traits.** [`Private`] is one way to hold
//!     a writer's pages, and TXN-03 may hold them another way. [`Committed`]
//!     borrows the reader's operation guard, as the node reader of the
//!     tree pages does (TREE-02).
//! 11. **A failed call ends its writer.** The tree does not undo half a
//!     split. TXN-03 makes the pages again from the write set.
//! 12. **Page steps change leaves only**, and take entries out or change
//!     the place of a value, without moving an entry to another page. The
//!     tree relies on it between its way down and its copy of the leaf. A
//!     step that changed a branch would need the tree to copy every page
//!     on its way. **No page step takes a bucket out**: a write reads the
//!     bucket of a long key before it copies anything. A step that takes
//!     the last key out of a bucket's tree leaves the bucket with an empty
//!     tree, which reads as no key, is passed by a cursor, takes the next
//!     put, and leaves with a range delete that covers it, or with the
//!     last key that a later delete takes out of it. The reclaim of a
//!     dropped subtree's entries (TREE-09) keeps to both.

mod cursor;
mod error;
pub mod log;
mod long_key;
mod node;
mod owners;
mod pages;
mod store;
mod tree;

#[cfg(test)]
pub(crate) mod tests;

pub use cursor::{Cursor, Look};
pub use error::FlatError;
#[cfg(any(test, feature = "held_reads"))]
pub use node::written::{leaf_images_written, leaf_pieces_cut};
pub use owners::{FlatOwners, StepLog};
pub use pages::{FlatConfig, FlatPages, FlatPagesMut, Revision};
pub use store::{Committed, PageIds, Parts, Private, PrivatePage};
pub use tree::{Changes, FlatTree, Unlinked, Written};
