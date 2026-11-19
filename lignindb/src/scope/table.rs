//! The scopes of one write transaction: which are open, what each has
//! claimed, and the pending marks.
//!
//! Nothing here reads a page or knows the write set. The transaction calls
//! it under its own lock, and the model of `tests/model.rs` calls it under
//! a lock of its own.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::txn::{Column, Subtree, Unit};

/// The number of an open scope. It is never zero, and no two scopes of one
/// transaction have had one number.
pub(crate) type ScopeId = u32;

/// Who makes an operation of a transaction: one of its scopes, or a caller
/// outside any scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Who(u32);

impl Who {
    /// A caller outside any scope. It only reads (`transactions.md` SC1).
    pub(crate) const OUTSIDE: Self = Self(0);

    /// The scope `id`.
    pub(crate) const fn scope(id: ScopeId) -> Self {
        Self(id)
    }

    /// The scope, if it is one.
    pub(crate) const fn id(self) -> Option<ScopeId> {
        match self.0 {
            0 => None,
            id => Some(id),
        }
    }
}

/// The element a pending mark is on: the subtree it is in, and its key
/// (`transactions.md` SC9). The pair is opaque here.
pub(crate) type MarkKey = (Subtree, Box<[u8]>);

/// One change of a pending mark: whether it was set, and where.
pub(crate) type MarkChange = (bool, MarkKey);

/// What a scope left when it ended.
#[derive(Debug)]
pub(crate) struct Closed {
    /// Whether its writes were laid out on the transaction's pages.
    pub(crate) laid: bool,
    /// The changes it made to pending marks, oldest first.
    pub(crate) marks: Vec<MarkChange>,
}

#[derive(Debug)]
struct Open {
    /// Whether its writes are laid out on the transaction's pages as it
    /// makes them. The writes of a scope that was opened beside another
    /// are kept in the write set until they are laid out, at its merge or
    /// before.
    laid: bool,
    /// The units it holds a claim on.
    units: Vec<Unit>,
    marks: Vec<MarkChange>,
}

/// Who holds a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Claim {
    /// One scope writes it.
    Write(ScopeId),
    /// These scopes have read it.
    Read(Vec<ScopeId>),
}

impl Claim {
    /// Whether a scope other than `who` holds any of it.
    fn held_beside(&self, who: Who) -> bool {
        match self {
            Self::Write(holder) => Some(*holder) != who.id(),
            Self::Read(readers) => readers.iter().any(|reader| Some(*reader) != who.id()),
        }
    }
}

/// Says which subtree is below which, as the caller's subtree directory
/// has it. A claim on a subtree that is dropped or cleared covers every
/// subtree below it (`transactions.md` CV9).
pub(crate) trait Lineage {
    /// What a lookup can fail with.
    type Error;

    /// Whether `subtree` is below `above`: hosted by an element of it, or
    /// of a subtree that is below it. A subtree is not below itself.
    fn below(&mut self, subtree: &Subtree, above: &Subtree) -> Result<bool, Self::Error>;
}

/// Why a claim was not granted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused<E> {
    /// Another open scope holds the unit, or a subtree above it
    /// (`transactions.md` SC3).
    Overlap(Unit),
    /// The lineage could not be read.
    Lineage(E),
}

impl<E> From<E> for Refused<E> {
    fn from(error: E) -> Self {
        Self::Lineage(error)
    }
}

/// The scopes of one transaction (`transactions.md` section 9.1).
///
/// - **Claims** (SC2, SC3). The unit of exclusion is a column of a
///   subtree. A scope holds a unit for reading with any number of others,
///   or for writing alone, from its first read or write of the unit until
///   it ends. A claim is granted or refused at once: nothing here waits.
///   A scope that drops or clears a subtree holds every unit of every
///   subtree below it as well, also one that is entered later.
/// - **Pending marks** (SC9). Each scope has the changes it made, and the
///   transaction the marks that merged scopes left, with every change of
///   those in a log so that a rollback to a mark of the write set can undo
///   them (WT8).
#[derive(Debug, Default)]
pub(crate) struct Scopes {
    last: ScopeId,
    open: BTreeMap<ScopeId, Open>,
    claims: HashMap<Unit, Claim>,
    /// The subtrees that open scopes dropped or cleared, each with its
    /// scope: everything below them is held.
    whole: Vec<(ScopeId, Subtree)>,
    pending: BTreeSet<MarkKey>,
    /// Every change of `pending` that had an effect, oldest first.
    log: Vec<MarkChange>,
}

impl Scopes {
    /// Opens a scope. Its writes are laid out as it makes them if no
    /// other scope is open.
    pub(crate) fn open(&mut self) -> ScopeId {
        // An id is never given twice, and none is that of the merged
        // writes.
        self.last = self.last.checked_add(1).expect("fewer scopes than ids");
        let scope = Open {
            laid: self.open.is_empty(),
            units: Vec::new(),
            marks: Vec::new(),
        };
        self.open.insert(self.last, scope);
        self.last
    }

    /// Whether a scope is open.
    pub(crate) fn any_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// Whether the writes of the open scope `id` are laid out on the
    /// transaction's pages.
    pub(crate) fn laid(&self, id: ScopeId) -> bool {
        self.open.get(&id).is_some_and(|scope| scope.laid)
    }

    /// The writes of the scope `id` are laid out from here on.
    pub(crate) fn set_laid(&mut self, id: ScopeId) {
        self.scope(id).laid = true;
    }

    /// The changes the open scope `id` made to pending marks, oldest
    /// first.
    pub(crate) fn marks_of(&self, id: ScopeId) -> &[MarkChange] {
        self.open.get(&id).map_or(&[], |scope| &scope.marks)
    }

    /// Ends the scope `id`: its claims go, and what it left is returned.
    pub(crate) fn close(&mut self, id: ScopeId) -> Option<Closed> {
        let scope = self.open.remove(&id)?;
        for unit in scope.units {
            let gone = match self.claims.get_mut(&unit) {
                Some(Claim::Read(readers)) => {
                    readers.retain(|reader| *reader != id);
                    readers.is_empty()
                }
                Some(Claim::Write(_)) => true,
                None => false,
            };
            if gone {
                self.claims.remove(&unit);
            }
        }
        self.whole.retain(|(holder, _)| *holder != id);
        Some(Closed {
            laid: scope.laid,
            marks: scope.marks,
        })
    }

    /// Whether the scope `id` holds `unit` for writing.
    pub(crate) fn writes(&self, id: ScopeId, unit: Unit) -> bool {
        self.claims.get(&unit) == Some(&Claim::Write(id))
    }

    /// The subtree that another scope than `who` dropped or cleared and
    /// that `unit` is below, if there is one.
    fn held_above<L: Lineage>(
        &self,
        who: Who,
        unit: Unit,
        lineage: &mut L,
    ) -> Result<Option<Subtree>, L::Error> {
        for (holder, above) in &self.whole {
            let other = Some(*holder) != who.id();
            if other && unit.subtree != *above && lineage.below(&unit.subtree, above)? {
                return Ok(Some(*above));
            }
        }
        Ok(None)
    }

    /// Takes a read claim on `unit` for `who` (SC3). It is refused if
    /// another open scope writes the unit, or has dropped or cleared a
    /// subtree it is below.
    ///
    /// A caller outside any scope is a scope that ends with the call: the
    /// claim is checked and not kept.
    pub(crate) fn read<L: Lineage>(
        &mut self,
        who: Who,
        unit: Unit,
        lineage: &mut L,
    ) -> Result<(), Refused<L::Error>> {
        if self.open.is_empty() {
            return Ok(());
        }
        if let Some(held) = self.read_refused(who, unit, lineage)? {
            return Err(Refused::Overlap(held));
        }
        if let Some(id) = who.id() {
            self.note_read(id, unit);
        }
        Ok(())
    }

    /// The unit that refuses `who` a read of `unit`, if one does: `unit`
    /// where another open scope writes it, and otherwise the data column
    /// of a subtree above it that another open scope dropped or cleared.
    fn read_refused<L: Lineage>(
        &self,
        who: Who,
        unit: Unit,
        lineage: &mut L,
    ) -> Result<Option<Unit>, L::Error> {
        match self.claims.get(&unit) {
            Some(Claim::Write(holder)) if Some(*holder) != who.id() => return Ok(Some(unit)),
            Some(Claim::Write(_)) => return Ok(None),
            Some(Claim::Read(readers)) if who.id().is_some_and(|id| readers.contains(&id)) => {
                return Ok(None);
            }
            _ => {}
        }
        let above = self.held_above(who, unit, lineage)?;
        Ok(above.map(|above| Unit::new(Column::Data, above)))
    }

    /// Keeps the read claim of the scope `id` on `unit`, which no other
    /// scope writes. A scope that holds the unit already holds it as it
    /// did.
    fn note_read(&mut self, id: ScopeId, unit: Unit) {
        match self.claims.entry(unit).or_insert(Claim::Read(Vec::new())) {
            Claim::Read(readers) if !readers.contains(&id) => readers.push(id),
            _ => return,
        }
        self.scope(id).units.push(unit);
    }

    /// Takes the claims of one operation of the scope `id` (SC3): a write
    /// claim on each of `units`, a read claim on each of `reads`, which the
    /// operation reads and does not write, and a claim on every unit below
    /// each subtree of `whole`, which the operation drops or clears. All
    /// are granted, or none is.
    ///
    /// A write claim is refused if another open scope has read or written
    /// the unit, or has dropped or cleared a subtree it is below. A read
    /// claim is refused as [`Self::read`] refuses it. A claim on
    /// everything below a subtree is refused if another open scope holds a
    /// unit there.
    pub(crate) fn write<L: Lineage>(
        &mut self,
        id: ScopeId,
        units: &[Unit],
        reads: &[Unit],
        whole: &[Subtree],
        lineage: &mut L,
    ) -> Result<(), Refused<L::Error>> {
        let who = Who::scope(id);
        if self.open.len() > 1 {
            for unit in reads {
                if let Some(held) = self.read_refused(who, *unit, lineage)? {
                    return Err(Refused::Overlap(held));
                }
            }
            for unit in units {
                if (self.claims.get(unit)).is_some_and(|claim| claim.held_beside(who)) {
                    return Err(Refused::Overlap(*unit));
                }
                if let Some(above) = self.held_above(who, *unit, lineage)? {
                    return Err(Refused::Overlap(Unit::new(Column::Data, above)));
                }
            }
            for above in whole {
                for (unit, claim) in &self.claims {
                    let below = claim.held_beside(who)
                        && unit.subtree != *above
                        && lineage.below(&unit.subtree, above)?;
                    if below {
                        return Err(Refused::Overlap(*unit));
                    }
                }
            }
        }
        for unit in units {
            // The unit is the scope's to write from here on. What the map
            // had for it was nothing, or a claim of this scope alone: it
            // wrote the unit before, or had read it and nobody else had.
            let held = self.claims.insert(*unit, Claim::Write(id)).is_some();
            if !held {
                self.scope(id).units.push(*unit);
            }
        }
        for unit in reads {
            self.note_read(id, *unit);
        }
        for above in whole {
            if !self.whole.contains(&(id, *above)) {
                self.whole.push((id, *above));
            }
        }
        Ok(())
    }

    /// Checks what the table holds against itself: every claim is of an
    /// open scope, a unit that is written has one holder, and each scope's
    /// list is the units it holds.
    #[cfg(test)]
    pub(crate) fn check(&self) {
        for (unit, claim) in &self.claims {
            let holders = match claim {
                Claim::Write(holder) => vec![*holder],
                Claim::Read(readers) => readers.clone(),
            };
            assert!(!holders.is_empty(), "a claim of nobody on {unit}");
            for holder in holders {
                let scope = self.open.get(&holder).expect("a claim of an open scope");
                assert!(
                    scope.units.contains(unit),
                    "{unit} is not in its scope's list"
                );
            }
        }
        for (id, scope) in &self.open {
            for unit in &scope.units {
                let held = match self.claims.get(unit) {
                    Some(Claim::Write(holder)) => holder == id,
                    Some(Claim::Read(readers)) => readers.contains(id),
                    None => false,
                };
                assert!(held, "the scope {id} lists {unit}, which it does not hold");
            }
        }
        for (holder, _) in &self.whole {
            assert!(self.open.contains_key(holder), "a drop of a closed scope");
        }
    }

    fn scope(&mut self, id: ScopeId) -> &mut Open {
        self.open.get_mut(&id).expect("the scope is open")
    }

    /// The scope `id` sets the pending mark on `key`, or with `set` false
    /// takes it off (SC9). A mark is not a claim.
    pub(crate) fn mark(&mut self, id: ScopeId, set: bool, key: MarkKey) {
        self.scope(id).marks.push((set, key));
    }

    /// Whether `key` is marked pending as `who` is shown it: by the marks
    /// the asking scope has set and taken off, and otherwise by those the
    /// merged scopes left. A mark of another scope that is still open is
    /// not shown, as its writes are not.
    pub(crate) fn is_pending(&self, who: Who, key: &MarkKey) -> bool {
        let own = who.id().and_then(|id| self.open.get(&id));
        let changed = own.and_then(|scope| scope.marks.iter().rev().find(|(_, at)| at == key));
        match changed {
            Some((set, _)) => *set,
            None => self.pending.contains(key),
        }
    }

    /// Makes the changes of a scope that merged the transaction's (SC6).
    pub(crate) fn merge_marks(&mut self, marks: Vec<MarkChange>) {
        for (set, key) in marks {
            let changed = match set {
                true => self.pending.insert(key.clone()),
                false => self.pending.remove(&key),
            };
            if changed {
                self.log.push((set, key));
            }
        }
    }

    /// How many changes the transaction's marks have taken: what a mark of
    /// the write set remembers.
    pub(crate) fn marks_len(&self) -> usize {
        self.log.len()
    }

    /// Undoes every change of the transaction's marks after the first
    /// `len`, newest first (WT8), and says whether there was one.
    pub(crate) fn rewind_marks(&mut self, len: usize) -> bool {
        let changed = self.log.len() > len;
        while self.log.len() > len {
            match self.log.pop().expect("more changes than the mark") {
                (true, key) => self.pending.remove(&key),
                (false, key) => self.pending.insert(key),
            };
        }
        changed
    }

    /// Takes every mark of the transaction off (WT9).
    pub(crate) fn clear_marks(&mut self) {
        self.pending.clear();
        self.log.clear();
    }

    /// Whether the transaction has no pending mark: a commit needs that
    /// (CM1).
    pub(crate) fn finalized(&self) -> bool {
        self.pending.is_empty()
    }

    /// The changes of the open scopes whose writes are laid out.
    fn laid_changes(&self) -> impl Iterator<Item = &MarkChange> {
        let laid = self.open.values().filter(|scope| scope.laid);
        laid.flat_map(|scope| &scope.marks)
    }

    /// The marks that stand for the pages the transaction's frames show:
    /// those of the merged scopes, with the changes of the open scopes
    /// whose writes the frames show as well.
    pub(crate) fn laid_marks(&self) -> BTreeSet<MarkKey> {
        let mut marks = self.pending.clone();
        for (set, key) in self.laid_changes() {
            match set {
                true => marks.insert(key.clone()),
                false => marks.remove(key),
            };
        }
        marks
    }

    /// Whether `key` may be marked in the pages the transaction's frames
    /// show: a merged scope left the mark, or an open scope whose writes
    /// the frames show has set it.
    pub(crate) fn marked_on_pages(&self, key: &MarkKey) -> bool {
        self.pending.contains(key) || self.laid_changes().any(|(set, at)| *set && at == key)
    }

    /// Every mark that stood at some point of the writes the transaction's
    /// frames show: those that stand, and those that were taken off again.
    pub(crate) fn ever_marked(&self) -> BTreeSet<MarkKey> {
        let mut marks = self.pending.clone();
        marks.extend(self.log.iter().map(|(_, key)| key.clone()));
        marks.extend(self.laid_changes().map(|(_, key)| key.clone()));
        marks
    }
}
