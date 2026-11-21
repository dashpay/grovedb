//! LigninDB storage engine: transactions, snapshots, savepoints, checkpoints and verify
//!
//! LigninDB is the storage engine under GroveDB's trees
//! ([the plan](https://github.com/dashpay/grovedb/issues/1052)). This crate
//! is the engine as a caller sees it: one type, [`Db`], and the handles it
//! gives out. The crates below it are its parts: `lignin-tree` lays Merk
//! nodes and flat records out in pages, `lignin-store` keeps the pages, the
//! log and the files, `lignin-format` is their bytes and `lignin-io` the
//! file layer. The rules every call keeps are
//! `docs/lignindb/transactions.md`.
//!
//! # The contract
//!
//! **Everything a caller does to a database goes through [`Db`].** This
//! page is the contract of that surface: what a caller calls, in which
//! order, and what it may not reach around.
//!
//! ```
//! use lignindb::{
//!     commit::{Committed, NoStaleHashes},
//!     txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite, Unit},
//!     Db, Options,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! // Open, which makes the database if the directory has none.
//! let db = Db::open(dir.path(), &Options::default())?;
//!
//! // A write transaction, written through a scope and committed through
//! // the database.
//! let subtree = Subtree::new([1; 32]);
//! let txn = db.begin();
//! let scope = txn.scope()?;
//! scope.apply(&Batch {
//!     subtrees: &[SubtreeWrite::Enter {
//!         subtree,
//!         tree_type: b"tree",
//!         host: None,
//!     }],
//!     records: &[RecordWrite::put(Column::Meta, subtree, b"height", b"1")],
//!     ..Batch::default()
//! })?;
//! scope.merge();
//! // It reads its own writes, and nobody else does.
//! let mut value = Vec::new();
//! assert!(txn.get(Unit::new(Column::Meta, subtree), b"height", &mut value)?);
//! assert!(db.snapshot().operation().open(subtree)?.is_none());
//! match db.commit(&txn, &mut NoStaleHashes) {
//!     Ok(Committed::Published(published)) => assert_eq!(published.version, db.version()),
//!     Ok(Committed::Empty) => unreachable!("the transaction wrote"),
//!     // A commit outside the transaction wrote one of its keys first:
//!     // the caller makes its writes again.
//!     Err(error) if error.is_conflict() => unreachable!("nobody else writes"),
//!     // The session takes no write any more: close, and open again.
//!     Err(error) if error.reopen().is_some() => return Err(error.into()),
//!     Err(error) => return Err(error.into()),
//! }
//!
//! // A snapshot for a read with no transaction, durability on demand,
//! // the counts, and the end of the session.
//! let snapshot = db.snapshot();
//! assert!(snapshot.operation().open(subtree)?.is_some());
//! drop(snapshot);
//! db.flush()?;
//! assert_eq!(db.stats().commits.published, 1);
//! drop(txn);
//! db.close()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## What a caller calls
//!
//! | To | A caller calls | And is given |
//! | --- | --- | --- |
//! | open a database, or make one | [`Db::open`] with [`Options`] | the one handle of that directory ([`db::OpenError`] says why not) |
//! | end the session | [`Db::close`], or drops the handle | the lock released and every file closed |
//! | write | [`Db::begin`], then [`WriteTxn::scope`](txn::WriteTxn::scope) and [`Scope::apply`](txn::Scope::apply) with a [`Batch`](txn::Batch); or [`batch`] to stage the writes of one operation across subtrees | a write transaction that reads its own writes |
//! | undo a part | [`WriteTxn::set_savepoint`](txn::WriteTxn::set_savepoint) and [`rollback_to_savepoint`](txn::WriteTxn::rollback_to_savepoint) ([`savepoint`]); a scope that is rolled back ([`scope`]) | the transaction as it was at the mark |
//! | commit | [`Db::commit`] with the transaction and the hash rules of its GroveVersion ([`NoStaleHashes`](commit::NoStaleHashes) on the existing ones) | [`Committed`](commit::Committed), or a [`db::WriteError`] |
//! | give a transaction up | [`Db::rollback`] | the transaction empty, to be written again |
//! | write with no transaction | [`Db::write`] | one commit of one batch |
//! | read at one version | [`Db::snapshot`], then [`Snapshot::operation`](snapshot::Snapshot::operation) and [`Operation::open`](snapshot::Operation::open) | the subtree as that version has it, or `None` |
//! | read the latest version, pinning nothing | [`Db::read`] | one [`Operation`](snapshot::Operation) |
//! | make commits durable | [`Db::flush`] | the version every later crash recovers at least ([`db::Flushed`]) |
//! | empty the database | [`Db::wipe`] | one commit that leaves no record |
//! | make a checkpoint | [`Db::checkpoint`] with a new directory and [`CheckpointOptions`](checkpoint::CheckpointOptions) | the database of one version, durable, in a directory that [`Db::open`] opens ([`checkpoint::Created`]) |
//! | look at the engine | [`Db::stats`], [`Db::keyspaces`], [`Db::reopen`] | every count, the flat keyspaces, why writes are refused |
//!
//! ## What a caller does not reach around
//!
//! The modules below [`db`] are public because their types are what the
//! calls above take and return, and because their documentation is where
//! each rule is stated. Three of their types are views of one open store
//! that only [`Db`] makes, and a caller holds none of them:
//!
//! - **[`Committer`](commit::Committer) and
//!   [`CommitState`](commit::CommitState).** A commit, a rollback and a
//!   write with no transaction are [`Db::commit`], [`Db::rollback`] and
//!   [`Db::write`]. The facade does what a commit owes the rest of the
//!   engine and the commit path does not know of: it refuses writes in a
//!   session that has to be opened again, hands the reclaimer what the
//!   commit dropped, counts the hashes the commit evaluated, and wakes
//!   write-back. A commit made past it leaves dropped subtrees
//!   unreclaimed until the next open, and goes on writing to a log that
//!   makes nothing durable any more.
//! - **[`WritePath`](txn::WritePath).** A write transaction is begun with
//!   [`Db::begin`], and the facade hands out no write path: a transaction
//!   does not show the one it was begun from. What the layer above needs
//!   of it is on the transaction: its own counts
//!   ([`WriteTxn::stats`](txn::WriteTxn::stats)) and the store's, for the
//!   hashes that layer computes itself
//!   ([`WriteTxn::counts`](txn::WriteTxn::counts)).
//!   [`WriteTxn::rollback`](txn::WriteTxn::rollback) is the transaction's
//!   own half of [`Db::rollback`], which also lets write-back place what
//!   waited for the transaction's copies.
//! - **[`ReadPath`](snapshot::ReadPath),
//!   [`Versions`](snapshot::Versions) and
//!   [`Directories`](snapshot::Directories).** A snapshot is
//!   [`Db::snapshot`] and an operation [`Db::read`]. Nothing outside this
//!   crate can pair a read path with the pages of a store, and no call of
//!   the facade gives out the versions of a database: a version of one is
//!   published by a commit alone.
//!
//! One way around is left: the feature `testing` (`txn::testing`), which
//! only tests switch on, gives the store of this crate's own tests with
//! its write path and its parts. The replay of the corpus in
//! `lignin-testkit` uses it, because it reads what the facade gives
//! nobody: the pages of a version through the loader, the roots of the
//! directory, and the segments after a write-back.
//!
//! What a caller does hold, for as long as it likes, borrows the
//! database: a [`WriteTxn`](txn::WriteTxn) with its scopes and cursors, a
//! [`Snapshot`](snapshot::Snapshot) with its operations, and what they
//! read. The handle cannot be closed or dropped while one of them lives,
//! and each of them is of the database it was taken from: a transaction
//! of one database is refused by the commit of another.
//!
//! ## What holds across calls
//!
//! - **One handle for a directory, at a time** (P-04), and it is `Send`
//!   and `Sync`: threads share it by reference or in an `Arc`.
//! - **Any number of write transactions are open, and one commits at a
//!   time.** A commit fails with a conflict when a commit outside it
//!   wrote a key after the transaction first wrote it (CF2). Where the
//!   transaction wrote a Merk node and kept the committed value of the
//!   node, and not a value it had stated itself, the commit also fails
//!   when a commit outside wrote the node after the transaction read it,
//!   or when the engine cannot tell ([`txn`], "A kept value and the read
//!   it is of").
//!   [`WriteError::is_conflict`](db::WriteError::is_conflict) is true for
//!   these. A write of such a node is refused where no node is left to
//!   keep the value of, and that refusal is a conflict too where the
//!   transaction can tell that commits outside are its cause
//!   ([`BatchError::ValueTaken`](txn::BatchError::ValueTaken)): the scope
//!   that writes is told ([`TxnError::is_conflict`](txn::TxnError::is_conflict)),
//!   and the transaction stays open. The two answer true for nothing
//!   else.
//! - **A commit ends its transaction** unless the error says otherwise
//!   ([`WriteError::leaves_transaction_open`](db::WriteError::leaves_transaction_open)).
//! - **Published is not durable** (DU1). With the default sync policy a
//!   commit is durable once [`Db::flush`] returned, or the session was
//!   closed. A crash lands on one committed version (DU2).
//! - **A reader waits for no commit and for no write transaction**, and
//!   holds back only space: a snapshot that is kept for long keeps what
//!   was dropped since from being reclaimed. Where a reader and a writer
//!   meet at a lock, it is held for a few instructions and over no read
//!   of a file.
//! - **No record of a subtree is read or written without its entry.** A
//!   batch enters a subtree before it writes into it, as one that is new
//!   ([`SubtreeWrite::Enter`](txn::SubtreeWrite::Enter)) or if no writer
//!   has entered it
//!   ([`SubtreeWrite::EnterIfAbsent`](txn::SubtreeWrite::EnterIfAbsent)),
//!   in any column,
//!   and a subtree that was dropped, with every subtree below it, is gone
//!   for every reader from the drop's version on.
//! - **After [`WriteError::reopen`](db::WriteError::reopen) says so, no
//!   write is taken**: the caller closes the database and opens it again
//!   ([`db`], "A session that takes no write any more").
//!
//! # Modules
//!
//! - [`db`] and [`options`]: the facade. The directory, the lock, what an
//!   open refuses, the background work, wipe.
//! - [`txn`]: the write transaction, its batches, its reads and cursors.
//! - [`scope`]: several threads writing different Merks in one
//!   transaction. [`savepoint`]: marks to roll back to. [`batch`]: the
//!   writes of one operation staged across subtrees.
//! - [`commit`]: how a transaction becomes the next version, and every
//!   way a commit ends.
//! - [`snapshot`]: versions, pins and guards, and what a reader reads.
//! - [`verify`]: the check of every physical rule of a store, which the
//!   tests run on every store they leave.

#![forbid(unsafe_code)]

pub mod backup;
pub mod batch;
pub mod checkpoint;
pub mod commit;
pub mod db;
#[cfg(test)]
mod models;
pub mod options;
pub mod savepoint;
pub mod scope;
pub mod snapshot;
pub mod space;
pub mod txn;
pub mod verify;

pub use db::Db;
pub use options::Options;
