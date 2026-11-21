//! Out-of-line values: the values of Merk nodes that are too long to sit
//! in their cell.
//!
//! - [`InlinePolicy`] says which values stay in their cell.
//! - [`TreeValues`] is what a write scope keeps of the values its cells
//!   hold out of line: it is the scope's
//!   [`Values`](crate::fragment::Values) for fragment maintenance, writes
//!   the values into the commit's log group, and reports every value
//!   pointer a cell stopped holding.
//! - [`read_value`] reads the value of a node of a committed page, and
//!   [`TreeValues::read`] that of a node of a scope's own page, from
//!   [`ValueSource`].
//! - [`TreeOwners`] answers the value log's cleaner for tree cells, and
//!   regroups the values of a page in the order a scan reads them
//!   ([`scan_order`], [`Locality`], [`RegroupPolicy`]). [`Regrouping`] is
//!   how write-back calls it.
//!
//! The bytes are `lignin-format`'s: `docs/lignindb/value-log-format.md` for
//! a value's record, its pointer and the pending form, `tree-page.md` TP13
//! for the cell, `log-format.md` LG20 to LG26 for the log. The files, the
//! counts of live values and the cleaner are `lignin_store::values`. The
//! move rule for carried cells is applied by fragment maintenance
//! ([`fragment`](crate::fragment)); this module is what it reports to. No
//! Merk rule is known here: a value is bytes.
//!
//! ```
//! use lignin_format::PageSize;
//! use lignin_tree::{
//!     fragment::ValueWrite,
//!     value::{InlinePolicy, TreeValues},
//! };
//!
//! # fn main() -> Result<(), lignin_tree::value::ValueError> {
//! // A store keeps values of up to 256 bytes in their cells.
//! let policy = InlinePolicy::DEFAULT;
//! assert!(policy.is_inline(256) && !policy.is_inline(257));
//! assert!(InlinePolicy::new(PageSize::DEFAULT, 512).is_ok());
//!
//! // A scope writes a node with a value of its tree's element. A short
//! // one goes into the cell. A long one is kept by the scope, and the
//! // cell is written with what stands for it until the commit.
//! let mut values = TreeValues::new(policy);
//! let reference = [7; 40];
//! let document = [9; 400];
//! assert_eq!(values.write(&reference)?, ValueWrite::Inline(&reference));
//! let ValueWrite::Pending(pending) = values.write(&document)? else {
//!     unreachable!("400 bytes are above the threshold");
//! };
//! assert_eq!(pending.value_len(), 400);
//! # Ok(())
//! # }
//! ```
//!
//! # Where a value is
//!
//! A cell says which of three it is (`tree-page.md` TP13), and a reader
//! tells them apart by the cell alone
//! ([`NodeRef::value`](crate::node::NodeRef::value)):
//!
//! | The cell holds | The value's bytes are | Reached by |
//! | --- | --- | --- |
//! | the value (`CellValue::Inline`) | in the cell | a slice of the frame |
//! | a value pointer (`CellValue::OutOfLine`): file, offset, length | in the value log, in a record that names the cell's page as its owner | one positional read |
//! | a pending value (`CellValue::Pending`): 8 bytes that name it, and its length | in memory, with the page, and in a log group | the name |
//!
//! A pending value is one that a commit wrote and write-back has not put
//! into the value log yet (`transactions.md` FL7), or one that a scope has
//! written and not committed. The two are told apart by the name:
//!
//! - **A value of a scope that has not committed** has a name the scope
//!   gave it, with the highest bit set, and its bytes are in the scope's
//!   [`TreeValues`], under the page of its cell and the name. No log
//!   address has that bit, short of a log of 2^63 bytes.
//! - **A committed pending value** has the log address of its record as
//!   its name (`log-format.md` LG21), and its bytes are with the frame of
//!   its page version in the buffer pool, attached under that name
//!   (`lignin_store::cache`). The commit puts both there: it gives each
//!   cell the address ([`TreeValues::log_values`]) and attaches the bytes
//!   ([`TreeValues::attach`]).
//!
//! Only the first two forms reach a page image in a segment: write-back
//! places every pending value of a page before it writes the page
//! (`page-delta.md` PD16). Placing and every later rewrite of a pointer
//! overwrite the 12 bytes of the place and move nothing else.
//!
//! # The threshold
//!
//! **A value of up to 256 bytes is stored in its cell, a longer one out of
//! line** ([`InlinePolicy::DEFAULT`]). The number is a parameter of the
//! store ([`InlinePolicy::new`]), not part of the format: a cell keeps the
//! form its value was written in.
//!
//! **The length of the value decides, and nothing else.** A threshold is
//! at most what fits in a cell whose every other part is as long as it can
//! be ([`InlinePolicy::max_threshold`]: 1,376 bytes for pages of 8 KiB,
//! 13,664 for 32 KiB). So a cell with its slot stays under half a page
//! whatever the threshold (`tree-layout.md` C7), and a value never leaves
//! or enters its cell because a key, a feature or a link changed length: a
//! hash update or a link update never writes a value. A threshold is at
//! least 12 bytes, the length of a place: a shorter value gains nothing by
//! leaving. An empty value is never out of line.
//!
//! Why 256:
//!
//! - **Above the size of the documents it costs pages.** The layout was
//!   simulated at 256 bytes (`tree-layout.md`, section 14): 126.1 nodes in
//!   a page of 32 KiB, 74.8% fill, 7.29 pages dirtied per transition. At
//!   512 bytes the 400-byte documents of the reference workload are in
//!   their cells, and a transition dirties 8.71 pages, above the target
//!   of 8.
//! - **Below it nothing is gained on that workload**, whose other values
//!   are short: a transition adds 583 bytes of keys and values, 400 of
//!   them the document (`benchmarks.md`). A lower threshold would send
//!   values out of line that are read with their nodes: each a positional
//!   read more for a point get, a query or a proof that emits it.
//! - **What it costs is one read where Merk needs an old value.** Merk is
//!   handed the old value with a node it fetches for a replacement or a
//!   removal (`cost-model.md`, sections 6.3, 6.6 and 9): the old element
//!   gives the value-defined cost and the storage flags, whose owner and
//!   epochs split the removed bytes. For a value out of line that is one
//!   positional read of the value's records, 18 bytes more than the value,
//!   once for each replaced or removed value above the threshold. A hash
//!   update reads no value, and neither does a write that only passes the
//!   node. A state transition of the reference workload inserts a document
//!   and a reference and replaces nothing: it reads no value.
//!
//! Keeping the element's leading bytes in the cell would not spare that
//! read. An element's encoding begins with its kind, but its flags are its
//! last field: for an item they come behind the value's bytes. The kind
//! alone serves the value-defined costs, whose elements (trees, sum items)
//! are in their cells anyway. To serve the flags a cell would keep the
//! first and the last bytes of a value, which only GroveDB can delimit, in
//! a cell format that has no such form, and Merk would be handed a part of
//! an element where its callbacks are handed the whole one today.
//!
//! # A write
//!
//! A scope has one [`TreeValues`] and hands it to
//! [`Fragments::apply`](crate::fragment::Fragments::apply) with every
//! write.
//!
//! 1. For the value of each node it writes, the scope asks
//!    [`TreeValues::write`], and states what that returns: the value, or a
//!    pending value whose bytes the scope's values keep.
//! 2. Fragment maintenance decides which page each cell is in, and says
//!    what became of the values:
//!
//! | What happened to a cell | Fragment maintenance says | What the commit does |
//! | --- | --- | --- |
//! | it was given a value out of line | `placed`, with the page | logs the value for that page |
//! | its node was deleted, or it was given another value | `released`, with what it held | releases the pointer, if it held one |
//! | it was carried to another page | `carried`, with what it held and both pages | releases the pointer, if it held one, and logs the value again for the new page |
//!
//! A cell that is carried takes its value along (`value-log-format.md` O5):
//! the value's record in the value log names the page of its owner, so the
//! cell in its new page holds a pending value, never the pointer it had.
//! The bytes are not read when the cell is carried. The scope's values
//! remember where they are. The commit reads them when it logs them, and
//! a read of the value by the scope before that reads them as well:
//!
//! | The carried cell held | The bytes are read from |
//! | --- | --- |
//! | a value pointer | the value log, with the page the cell left as the owner |
//! | a pending value of an earlier commit | the frame of the committed page the cell left |
//! | a pending value of the scope's own | nowhere: the scope has them |
//!
//! A cell that one scope carries twice has its value logged once, for the
//! page it ends in, and its pointer released once.
//!
//! A value that was pending in a page when the scope copied it, and whose
//! cell stays in that page, is not the scope's. The log group of an
//! earlier commit holds it for that page, its bytes came with the copy,
//! and write-back places it after the scope's commit.
//!
//! # The commit
//!
//! What a commit does for the values of its tree cells, in the order of
//! `transactions.md` CM3:
//!
//! 1. **The values** ([`TreeValues::log_values`]), before any page: every
//!    value of the scope goes into the group, for the page of its cell,
//!    and the cell gets the pending value that names the value's address
//!    in the group. A carried value is repeated, not named
//!    (`log-format.md` LG23): naming needs holds on value-log files that
//!    nothing keeps yet.
//! 2. **The pages**: the runs that the sinks of the pages collected. The
//!    new name of a cell is among them, as record 21.
//! 3. **The pointers released** ([`TreeValues::released_pointers`]), in
//!    ascending order, merged with those of the flat leaves.
//! 4. **The bytes** ([`TreeValues::attach`]) with the frame of every tree
//!    page the commit installs, and then the page.
//! 5. **The counts**: one [`ValueChange`](lignin_store::values::ValueChange)
//!    for the commit ([`TreeValues::change`], with the flat leaves'
//!    pointers added), applied where the commit installs its pages, under
//!    the counts lock it took before its append (LT7).
//!
//! So **every value pointer a cell stops holding is reported here for the
//! same commit** ([`TreeValues::released_pointers`]). One that a count of
//! live values includes is in the commit's group as a released pointer,
//! and leaves the count of its file with the commit. A value that this
//! type holds is counted by the page step of write-back that places it
//! (PV6, FL7).
//!
//! A bulk load writes its cells with pointers to values that are in the
//! value log and in no count yet, and what its commit does with them is
//! the engine's (`transactions.md` LD5). Such a pointer that a cell of the
//! same scope stopped holding before the commit is reported here like any
//! other, and the engine leaves it out of the group. A cell of a load
//! that a later write carries has its value held here from then on, as
//! that of any carried cell.
//!
//! # Reading
//!
//! [`read_value`] is the read path of a committed node, and allocates
//! nothing for a value in its cell or a pending one. A value in the value
//! log is one positional read into the reader's [`ValueBuf`]
//! (`lignin_store::values`), checked against the length the cell states
//! and against the page of the cell: a pointer that leads to a value of
//! another page is an error, which is what catches a cell that was carried
//! without its value (O10). A pending value is answered from the frame the
//! reader holds, and reads neither the log nor the value log (FL7).
//!
//! [`TreeValues::read`] is the same for a page that a scope has copied or
//! made, with the scope's own values found by their names. It is for no
//! other page: for a value that was pending when the scope copied its
//! page it loads the committed page, whose frame keeps the bytes for as
//! long as the copy keeps page steps off it (PV7). The frame of a page
//! that nobody has copied is replaced when write-back places a value of
//! it, by one that has the pointer and not the bytes.
//! [`TreeValues::read_at`] is that read by the place of a value and the
//! page of its cell, for a caller that holds no node: an evaluation of
//! stale hashes, which is given [`TreeValues::reader`] as its
//! [`ValueReader`](crate::hash::ValueReader).
//!
//! # The cleaner, and regrouping
//!
//! [`TreeOwners`] is
//! [`ValueOwners`](lignin_store::values::ValueOwners) for tree cells: it
//! finds the cell that holds a pointer in the current version of the page a
//! value's record names, and gives it another pointer in a page step
//! (`transactions.md` PV7). The step is refused unless the frame that was
//! looked at is still how the current version of the page is stored, was
//! born by the commit that was looked at, holds the pointer, and no write
//! transaction holds a private copy of the page. The step writes its
//! record to the log ([`StepLog`](crate::flat::StepLog)), puts a frame
//! with the new pointer in the place of the old one, and takes no commit
//! version.
//!
//! **Regrouping** is the same step, made for the page's sake. Values are
//! appended to the value log in the order they are written, so the values
//! of one page lie where their writes left them, and a scan that reads the
//! page's nodes in key order reads the value log at a new place for each.
//! [`TreeOwners::copy_grouped`] appends the values of one page again, back
//! to back, in the order a scan reaches their cells ([`scan_order`]), with
//! the same owner, and [`TreeOwners::install_grouped`] gives each cell the
//! pointer to its copy. Afterwards the page's values are one run
//! ([`Locality`]), and a scan reads them as one sequential read.
//!
//! - **When.** At consolidation: the flusher regroups a page it is about
//!   to write a new base image of, after it has placed the page's pending
//!   values, and writes the image with the new pointers. Between the two
//!   halves it makes the copies durable and has the manifest cover them,
//!   as for any pointer that a page is to hold.
//! - **What it costs.** Every value of the page is written once more, and
//!   the old records are dead space of their files. [`RegroupPolicy`] says
//!   which pages are worth it: by default those where a scan jumps to at
//!   least a quarter of the values.
//! - **What it leaves.** A scan still leaves the run where it leaves the
//!   page. A Merk's nodes are in its pages by fragments, and a node high in
//!   the tree stands between the pages below it: its value is read
//!   between theirs.
//! - **What it does not touch.** What the cells say, the page's version
//!   and every handle. Of a cell only the 12 bytes of its value's place
//!   change, and no cell moves.
//!
//! **How the flusher reaches it.** `lignin-store` knows no tree, so
//! write-back defines what it needs as a trait,
//! [`PageValues`](lignin_store::flush::PageValues), and [`Regrouping`] is
//! this module's implementation of it: [`scan_order`] for the order in
//! which write-back appends the pending values of a page, so that a new
//! page's values are one run from the start, and the two halves of the
//! regrouping, with a [`RegroupPolicy`], for a page that is about to get a
//! new base image. Whoever opens a store hands it to write-back
//! (`Stores::page_values`), with a [`StepLog`](crate::flat::StepLog) that
//! appends through write-back.
//!
//! # What the callers do
//!
//! - **A write scope** hands its [`TreeValues`] to every
//!   [`Fragments::apply`](crate::fragment::Fragments::apply) it makes, and
//!   gives it up with its pages after a call that failed. It keeps a clone
//!   for a savepoint and puts the clone back together with the pages. It
//!   gives up the pending value of a write that was refused
//!   ([`TreeValues::forget`]).
//! - **A scope that applies its writes again**, on pages that it made
//!   again after a rebase or that another scope changed
//!   (`transactions.md` LT4), states them to values that have not seen
//!   them: new ones, or a clone from before the first of them. The values
//!   of the first time hold the pointers it released, and gave away the
//!   pending values it placed.
//! - **It keeps its copy of a committed page it has freed until its
//!   commit**, as it keeps every copy: a cell that left such a page can
//!   name a value that is still with the page, and a page step on the page
//!   would count a value in a cell that the commit then drops unseen.
//! - **It reads** a node of a page it has copied or made with
//!   [`TreeValues::read`], and a node of a committed page from the frame
//!   it holds of that page, at the version its [`ValueSource`] is made
//!   for: a node it has pinned with [`read_value`], and a node it is
//!   shown in a frame through [`TreeValues::read_shown`], which is
//!   [`TreeValues::read`] where it is given no frame.
//! - **A commit** takes the steps above in that order, makes
//!   [`TreeValues::log_values`] again if it writes its group for another
//!   place, and applies one change to the counts.
//! - **Write-back** places the pending values of a page before it
//!   regroups or writes it, and takes a placed value's bytes off the frame
//!   under its name.
//! - **Whoever cleans a value-log file** asks both kinds of owners: a
//!   value's record names a page, which is a tree page or a flat leaf, and
//!   each of [`TreeOwners`] and [`FlatOwners`](crate::flat::FlatOwners)
//!   finds nothing in a page of the other kind.
//! - **A reader** holds an operation guard for as long as it uses what it
//!   read (`transactions.md` PN5), and so does whoever uses
//!   [`TreeOwners`].
//!
//! # Choices made here that the plan left open
//!
//! 1. **The threshold is 256 bytes, from 12 bytes to what fits beside the
//!    largest other parts of a cell**, and the length alone decides. A
//!    threshold above that bound would make a value's place depend on the
//!    length of its cell's links.
//! 2. **A carried value is read at the commit, not at the carry.** A cell
//!    that is carried twice, or carried and then deleted or overwritten,
//!    costs the commit one read or none, and fragment maintenance, which
//!    cannot fail here, reads nothing.
//! 3. **A carried value is repeated in the log, not named** (LG23), until
//!    the store keeps the holds of LG24.
//! 4. **The scope's own names have the highest bit set**, as those of a
//!    flat writer have. A value that a write names and the scope does not
//!    hold is so found before the commit's append, not after it.
//! 5. **A value that an earlier commit left pending is not taken over by
//!    the scope that copies its page.** Its bytes are found with the
//!    committed frame when its cell is carried.
//! 6. **Regrouping is one page step for each value**, the cleaner's step,
//!    with one record each (LG25). A page is not regrouped in one step: a
//!    step that fails after some of its records are in the log would have
//!    to install the part that is.
//! 7. **The order of a page's values is that of its fragments**, each in
//!    key order with the Merks its nodes host, and the fragments by their
//!    first keys. The keys of a page are not sorted as a whole: they can
//!    be of several Merks.
//! 8. **A page is regrouped when a scan jumps to a quarter of its
//!    values.** The number is a guess for PERF-04 to replace: one
//!    overwritten value in a regrouped page makes two jumps.
//! 9. **Tree cells and flat leaves have owners of their own**, and no
//!    type here answers for both.

mod error;
mod order;
mod owners;
mod policy;
mod read;
mod regroup;
mod write;

#[cfg(test)]
mod tests;

pub use error::ValueError;
pub use order::{scan_order, Locality, RegroupPolicy};
pub use owners::{Grouped, Regrouped, TreeOwners};
pub use policy::InlinePolicy;
pub use read::{read_placed, read_value, ValueBuf, ValueSource};
pub use regroup::Regrouping;
pub use write::{ScopeReader, TreeValues};
