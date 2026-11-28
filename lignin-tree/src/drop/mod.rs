//! Subtree drop: unlinking a Merk without reading it, and reclaiming what
//! it left.
//!
//! - [`unlink`] is the drop: the write of the transaction that drops or
//!   clears Merks ([`Leaving`]). It writes the page of each host, the
//!   Merk's records in the directory and the two ends of its flat records
//!   in each column, whatever the Merk holds.
//! - [`enter`] enters a Merk, so that it starts with no flat record.
//! - [`rekey`] moves Merks to other prefixes, with their flat records.
//!   [`Flats`] is the flat side of both: the columns ([`Column`]) and the
//!   keyspace of the unlinked pages.
//! - [`Reclaimer`] takes out what a drop left, in page steps on the pages
//!   of a store, and [`Reclaim`] is one dropped Merk while that goes on.
//! - [`Wait`] says when it may begin.
//! - [`FlatDrops`] is the lazy range delete of a flat keyspace: it unlinks
//!   the pages of a range without reading them and records them
//!   ([`FlatDrop`]), and [`Reclaimer::reclaim_flat`] frees them.
//!
//! The rules are `docs/lignindb/tree-layout.md` D1 to D4 and
//! `transactions.md` CV9, CL6 and PV7. The records in the log are
//! `lignin-format`'s (`log-format.md` LG27), and what a Merk leaves in the
//! directory is the directory's ([`directory`](crate::directory)). No Merk
//! rule is known here: a cell is a key, links and a value's place.
//!
//! # The drop
//!
//! A drop is a write of its transaction (CV9). [`unlink`] makes it of two
//! parts that exist already: the write to the tree pages, in which the
//! root of each Merk is named as dropped
//! ([`Write::dropped`](crate::fragment::Write::dropped)), and
//! [`Directory::unlink`](crate::directory::Directory::unlink) or
//! [`Directory::clear`](crate::directory::Directory::clear). It reads the
//! entry of each Merk and nothing below it. So what a drop writes is the
//! page of the host and at most four records of the directory (D1: the
//! entry, the record of the root and that of the host where the Merk has
//! them, and the record of the drop), and **what a Merk holds does not
//! enter into it**: a million nodes cost what one costs.
//!
//! **The flat records of a dropped Merk go with the drop** (the exception
//! in D1): [`unlink`] takes the Merk's range out of every column by the
//! lazy range delete. That reads one path of each column and writes the
//! leaf at each end of the range, the branches above them and one record
//! of the pages between, whatever the range holds; a column with no record
//! of the Merk is read and not written. A cleared Merk keeps its flat
//! records. Nothing of the Merks below either is read: their flat records
//! are the reclaimer's, as their entries are.
//!
//! One thing can add to that. **A host inside a dropped Merk that is
//! behind is written first**, with its roots, by a write of its own
//! ([`Pages::host_is_behind`](crate::fragment::Pages::host_is_behind),
//! `tree-layout.md` T3): the walk below follows every pointer it finds,
//! and a pointer that is behind would stay so for good. That is at least
//! one page for each such host, and it is bounded by what the transaction
//! wrote, not by the size of the subtree. On existing GroveVersions no
//! host is behind.
//!
//! The address of the root is kept from the drop on: it is in the record
//! that the directory leaves under the Merk's id.
//!
//! # Reclaiming
//!
//! What a drop leaves is storage and no state (CV9): the cells of the
//! Merk, the cells of every Merk nested in it, and the directory records
//! of those. It is reclaimed when no reader can reach it ([`Wait`], PN6),
//! by background work that takes no commit version (CL6).
//!
//! Every change is **one page step on the current version of one page**
//! (PV7): it is refused while a write transaction holds a private copy of
//! the page ([`Busy::Copied`]), it appends one record to the log under the
//! step's lock (LG27), and it changes the counts of the value pointers it
//! released in the same step (PV6, `value-log-format.md` O6). So no commit
//! brings a removed cell back, and none installs a version of a freed
//! page. A step that is refused changes nothing and is tried again.
//!
//! The steps of one dropped Merk come in this order:
//!
//! 1. **What the Merks below it left outside the tree pages**, the
//!    innermost first. For each: its flat records in every column, a leaf
//!    at a time, and then the record of its root, its entry, and the
//!    record of its host
//!    ([`Directory::left_by`](crate::directory::Directory::left_by)). The
//!    nested subtrees are found through the directory and through nothing
//!    else, and [`Reclaimed::merks`] names each. The flat records go
//!    before the entry because the entry is what says whose they are: a
//!    reclaiming that begins again finds them by it, and a step is made
//!    only while the entry under the prefix is that Merk's (see below).
//! 2. **The Merk's own record.**
//! 3. **The cells**, from the root down. A cell goes after the cell that
//!    names it, and with every cell of its page that hangs below it there:
//!    one step takes whole fragments out of one page. The cells of the
//!    nested Merks are among them, reached through the pointers of their
//!    hosts. A page that keeps no cell is freed in the step that takes its
//!    last cells, with the latest version (`page-alloc.md` A4): the page
//!    table gets the free link, then the pool is told, then the id is
//!    retired. Nothing else frees such a page, so it is freed once.
//!
//! **Why the cells go last, and from the top.** A slot that a removal
//! frees is given to the next cell that arrives in the page
//! (`tree-layout.md` S4), and a page id that is freed to the next page.
//! A cell does not say which Merk it is of, and two cells of different
//! Merks can be equal byte for byte. So an address that names a freed
//! slot cannot be told from one that names its cell, by anything a page
//! holds. Two things follow.
//!
//! - *No address of a cell that is gone is left in a cell that stays.*
//!   Going down, the cell that held the address is gone before the cell
//!   it named. Every page is as a write leaves it after every step: its
//!   fragment-root marks are right, and every address in it names its
//!   cell.
//! - *The walk begins once.* While the Merk's record is in the directory,
//!   no cell of the subtree is gone, so the record leads to the root and
//!   every pointer below it is exact. That is what makes it right to begin
//!   after a restart or a crash that came during step 1 or before it. Once
//!   a cell is gone, the record would lead a second walk to a slot that
//!   may hold another cell, so the record goes first.
//!
//! **What this costs: a crash during step 3 leaves cells that nothing
//! leads to.** From step 2 on, the cells that are left are known to the
//! [`Reclaim`] in memory and to nothing on disk. If the process ends
//! before the walk does, they stay in their pages, with the value
//! pointers they hold counted, and their pages are never freed. They are
//! in no state and no read returns them. The pages they are in pass every
//! check of a page, and what tells them from cells in use is that nothing
//! names them: a walk of every page finds them, and the invariant checker
//! makes that walk. A background step can only take things out, one page
//! at a time, so it cannot keep on disk where a walk stands; the choices
//! at the end say what would.
//!
//! The directory's part is safe at every point: a record that is gone
//! leaves no trace a later record could be taken for, because no id is
//! given twice, and
//! [`Directory::left_by`](crate::directory::Directory::left_by) answers
//! for what is still there.
//!
//! # What reclaiming leaves in the pages
//!
//! Reclaiming does not merge (D3). A page from which cells were taken
//! keeps its other cells where they are, and a page that is left nearly
//! empty stays so until a write touches it. A flat leaf from which a
//! record of the directory was taken keeps its place and its prefix, also
//! with no entry left: a page step changes one leaf and nothing else.
//! The same holds for the leaves of a column from which the flat records
//! of a nested Merk were taken: a leaf that held nothing else stays in the
//! tree with no entry, and a bucket of long keys stays with an empty tree,
//! until a write merges the leaf or a range delete covers it.
//! [`Reclaimed`] counts the cells and their bytes, the pages that were
//! written again and the pages that were freed, and the flat records and
//! the leaves they were taken out of, for whoever measures the space
//! (PERF-08).
//!
//! # The flat records of subtrees
//!
//! A subtree has records outside its Merk: in the aux, meta and roots
//! columns, and its data where a tree keeps that flat. A record of the
//! subtree under `prefix` is in its column under `lead ++ prefix ++ key`
//! ([`Column`]). One rule says whose a record is (`tree-layout.md` D6):
//!
//! > **In every column, at every version, the records under a prefix are
//! > those of the entry that the directory stores under that prefix at
//! > that version**, an entry in use or a left-over one. Where the
//! > directory stores no entry, no column has a record.
//!
//! So **a reader finds flat records through the directory**: it opens the
//! prefix at the version it reads
//! ([`Directory::open`](crate::directory::Directory::open)), and where
//! that returns nothing there is no record for it in any column. A handle
//! that is kept across writes is asked again
//! ([`Directory::is_live`](crate::directory::Directory::is_live)). The
//! records of a left-over entry are in the pages and are shown to nobody,
//! exactly as the entry is (CV9, T-CL1).
//!
//! Three writers keep the rule, and they are all in this module:
//!
//! 1. **The drop** takes the dropped Merk's own range out of every column
//!    ([`unlink`]): its entry goes, so its records go with it.
//! 2. **The enter** takes out what a left-over entry under the prefix has,
//!    in the operation that puts the new entry there ([`enter`]): the
//!    records under the prefix are the new entry's from then on, and it
//!    starts with none.
//! 3. **The reclaimer** takes the flat records of a Merk that was below a
//!    dropped one out before the Merk's entry, and only while the entry
//!    that the directory stores under the prefix is that Merk's
//!    ([`Reclaimer::reclaim`]).
//!
//! **A write transaction that enters every Merk through [`enter`] and
//! drops and clears through [`unlink`] keeps the rule, with nothing else
//! to remember.** What the two need of it is [`Flats`], the same for both.
//!
//! The rule asks two things of code that is not here:
//!
//! - **A move takes the flat records along.** An operation that gives a
//!   Merk another prefix
//!   ([`Directory::rekey`](crate::directory::Directory::rekey)) writes its
//!   flat records under the new prefix and removes them under the old one
//!   in the same operation (GROVE-03).
//! - **A clear keeps the cleared Merk's own flat records**, and drops
//!   those of the Merks below it. [`unlink`] does that for
//!   [`Leaving::Cleared`]; an operation that means to empty the columns of
//!   the cleared Merk too removes them itself (GROVE-05).
//!
//! **Why a step of the reclaimer cannot take a record of another Merk.**
//! A Merk can be entered again under the prefix at any time, with records
//! under the keys the old one had. The reclaimer reads the entry under the
//! prefix and a leaf at one version, goes on only if the entry is the
//! nested Merk's, and makes its step only if that version of the leaf is
//! still the current one. A page version that a reader of some version
//! sees holds no write of a later commit, so the records under the prefix
//! in it are those of the entry of that version. The enter that puts
//! another entry there rewrites the leaf at each end of the range, which
//! are other versions from its commit on, and unlinks the leaves between
//! whole. A step can still be made on one of those: it takes left-over
//! records out of a page that waits to be freed, which no reader of the
//! new Merk reaches, and the freeing releases what is left in it.
//!
//! # Flat keyspaces
//!
//! [`FlatTree::delete_range`](crate::flat::FlatTree::delete_range) reads
//! every leaf it frees. [`FlatDrops::unlink_range`] is the lazy form: the
//! leaf at each end of the range loses its entries as before, the pages
//! between are taken out of their branches unread
//! ([`FlatTree::unlink_range`](crate::flat::FlatTree::unlink_range)), and
//! one record here names them. The write is of the leaves at the two ends
//! and the branches above them, whatever the range holds.
//!
//! [`Reclaimer::reclaim_flat`] frees those pages later: a page after every
//! page below it, each leaf read as it goes, for the value pointers of its
//! entries and for the trees of its buckets of long keys, and the record
//! last. **This walk can begin again anywhere.** The pages a range delete
//! left hold nothing in use, so no commit writes them and nothing but the
//! reclaimer frees them. Their versions therefore tell what a slot of a
//! shared tree page cannot: a page that was born after the range delete
//! committed is another page under the same id. The record keeps the
//! version its writer read at for that: a commit takes the version after
//! its base (`transactions.md` CM3).
//!
//! # What the callers do
//!
//! - **A write transaction** drops with [`unlink`], in the operation that
//!   deletes or rewrites the host, and states the roots that the write
//!   carried to the directory afterwards, as after any write. It writes
//!   the hosts that are behind first. It enters every Merk with [`enter`].
//!   **It gives the three calls of an operation, [`unlink`], [`rekey`] and
//!   [`enter`], one set of pending records of the directory**
//!   ([`Pending`](crate::directory::Pending)), states its roots among
//!   them, and settles them once
//!   ([`Directory::settle`](crate::directory::Directory::settle)): the
//!   calls put the directory's records there and write the tree pages and
//!   the columns themselves. Each reads the directory as the records of
//!   the calls before it leave it.
//!   It gives both every column ([`Flats`]), and the version it reads at:
//!   a transaction that makes its pages again over a newer base
//!   (`transactions.md` WT7) makes these calls again with that base.
//! - **A write transaction copies a tree page before a write is planned on
//!   it.** Fragment maintenance reads the pages of a write, plans, and
//!   then changes them, and asks that nothing changes a page between the
//!   two ([`fragment`](crate::fragment), "One writer"). A page step can,
//!   on any page that has no private copy, and a step of this module takes
//!   cells out: the plan would have counted cells and bytes that are gone,
//!   and would keep a page that it leaves with no cell. So what a write
//!   reads for its plan is its own copy, as a flat keyspace takes a leaf
//!   before it concludes anything from it. The steps here are refused for
//!   as long as the copy exists.
//! - **Whoever runs background work** reads the records of the drops at
//!   the latest version, under an operation guard, and makes a [`Wait`]
//!   for each with that version. It asks the wait between operations,
//!   holding no guard. When the wait is over it makes a [`Reclaim`] and
//!   calls [`Reclaimer::reclaim`] with the directory of the latest
//!   version and every column until it is done: at once, and again after
//!   [`Progress::Blocked`]. It does the same for the records of the lazy
//!   range deletes ([`Reclaimer::flat_drops`],
//!   [`Reclaimer::reclaim_flat`]), which drops and enters write as well. It keeps a [`Reclaim`] that
//!   [holds cells](Reclaim::holds_cells) until it is done, also through a
//!   shutdown that can wait.
//! - **It loads its directory again** when a reclaiming is done: the
//!   handle still has the id of the dropped Merk, which does no harm.
//! - **A commit** is not told of any of this. Its own steps are page steps
//!   (`lignin_store::cache`), so it meets a page as a step left it.
//! - **Write-back** owes a segment the page versions that a step changed,
//!   and nothing of a page that a step freed.
//! - **One reclaimer works on one dropped Merk.** Two may work on
//!   different ones at once, also where their cells share a page: each
//!   step is refused or made as a whole.
//!
//! # Choices made here that the plan left open
//!
//! 1. **The cells go from the top down, and after the Merk's record.** The
//!    other order, cells first and from the leaves, keeps the record until
//!    all is gone and could begin again after a crash, if a walk could
//!    tell a cell from another one in its slot. It cannot, and taking one
//!    for the other removes cells in use. So a crash while the cells of a
//!    Merk go leaves the rest of them unreachable. What would close that
//!    is one of two things the store does not have: a background step
//!    that changes two pages as one, or a place where background work
//!    keeps where a walk stands.
//! 2. **A page is freed with the latest version**, not with the drop's,
//!    which nothing keeps. The pool takes a free only with a later version
//!    than the page version was born with, so a page that the latest
//!    commit wrote waits for the next one ([`Busy::Newborn`]).
//! 3. **The free link is pushed for every freed page**, also one that no
//!    segment holds: nothing older of it can be linked afterwards
//!    (`page-table.md` T18). One page has none to be given: a page that
//!    was made under an id which named another page before, and that
//!    write-back has not written yet. The newest link of its id is still
//!    the free link of the earlier page, and the table takes no second
//!    one. The step goes on as for any page it frees, as a commit does
//!    (`lignindb::commit`, "A page that is freed again before it was
//!    written back"): no segment ever held the page, the pool takes its
//!    versions off the lists of what is to be written back, and the id is
//!    retired once. Write-back links no block of a page once the pool was
//!    told that it is freed, and a pass that looked before that can have
//!    linked the page behind the refused push: so the step pushes the free
//!    link once more after the pool's word, and it then lands above that
//!    link. [`Reclaimed::pages_never_linked`] and
//!    [`FlatReclaimed::pages_never_linked`] count the pages that were
//!    given no free link by either push. The record
//!    of the step is in the log before the page is freed, which is what a
//!    checkpoint of the page table needs of it (`page-table.md` K14).
//! 4. **Every step holds the lock on the counts**, also one that releases
//!    no pointer, from its record to its end: a sample of the counts then
//!    finds every step whose record is before it made, as LG28 needs of
//!    all background changes.
//! 5. **The wait counts from the version the record was read at**, since
//!    the record does not say when it was written.
//! 6. **The lazy range delete keeps its pages in a keyspace of its own**,
//!    one record for each range delete, with the version its writer read
//!    at. A range delete that takes no page out whole writes no record.
//! 7. **The trees of the buckets in a leaf at an end of the range are left
//!    unread too**, each as one page of the record.
//! 8. **A record of the directory is taken out by its key alone**, one
//!    step for each: the three records of a Merk are in three places of
//!    the keyspace.
//! 9. **The walk checks what it removes.** A cell that another page names
//!    is marked as the root of its fragment, a cell that its own page
//!    names is not, none is named twice, and what a step leaves passes the
//!    page's own check. A page in which that fails is not changed, and the
//!    other pages go all the same. The Merk's own record goes only once
//!    the step on its root's page was made ready in full.
//! 10. **A record of the directory is taken out only as it was read.** A
//!     Merk can be entered again under the prefix of a left-over entry
//!     between the reclaimer's look and its step, and that entry is not
//!     the reclaimer's. The step compares the entry with the one it saw.
//!     An entry whose value is out of line is known by the place of the
//!     value, which write-back and the cleaner change, so an entry that
//!     is not as it was seen is looked at again, and never passed over.
//! 11. **A step that frees a page checks, and then frees.** The pool has
//!     no page step that frees: its free takes the step's lock itself. So
//!     the check that no private copy exists and the free are two holds of
//!     that lock, with the lock on the counts held across both, which
//!     keeps every other background step out. What it rests on is that
//!     **no write copies a page that holds no cell in use**: a write reads
//!     and changes the pages of the cells it holds and the pages beside
//!     them, each of which holds a cell in use. If a write did copy such a
//!     page between the two and commit it, its commit or this step would
//!     be refused by the pool, after the free link is in the page table:
//!     the step ends with [`DropError::Broken`]. A free inside the pool's
//!     page step would make this one step, and the rule unneeded.
//! 12. **A step that failed before its record changed nothing**, and is
//!     made again: the log can refuse a record, and the walk goes on from
//!     where it was. A count of live values that does not take the
//!     pointers of a step is noted ([`Reclaim::miscount`]) and does not
//!     stop the walk: the step is made, and the value log keeps the file.
//! 13. **The flat records of the dropped Merk go with the drop, and those
//!     of the Merks below it with the reclaimer.** The record that a drop
//!     leaves in the directory has the Merk's id and its root, and no
//!     prefix, so nothing could find the Merk's own flat records later;
//!     and the prefix can be entered again in the next operation. The
//!     Merks below keep their entries, which have their prefixes, until
//!     the reclaimer takes them. Unlinking theirs in the drop as well
//!     would make the drop walk the directory below the Merk, which D1
//!     rules out.
//! 14. **The reclaimer takes flat records out a leaf at a time, and never
//!     a page.** A page step changes one leaf, so it cannot take a leaf
//!     out of its branch. The lazy range delete could, and is a write of a
//!     transaction: background work has none. What is left is leaves with
//!     no entry, which [`Reclaimed::flat_leaves`] bounds.
//! 15. **A call makes at most the page steps it was given**, and a look at
//!     a record that was not as it was read counts as one, so that a call
//!     ends.

mod column;
mod error;
mod flat;
mod merk;
mod rekey;
mod step;
mod unlink;
mod wait;

#[cfg(test)]
mod tests;

pub use column::{enter, Column, Flats};
pub use error::DropError;
pub use flat::{FlatDrop, FlatDrops, FlatReclaimed};
pub use merk::{Progress, Reclaim, Reclaimed};
pub use rekey::rekey;
pub use step::{Busy, Reclaimer};
pub use unlink::{unlink, Leaving, Unlink};
pub use wait::Wait;
