//! The claim table and the pending marks by themselves (`transactions.md`
//! SC2, SC3, SC9; test T-SC2).

use std::collections::HashMap;

use crate::{
    scope::table::{Lineage, MarkKey, Refused, Scopes, Who},
    txn::{Column, Subtree, Unit},
};

/// A grove by its hosts: each subtree with the one its element is in.
#[derive(Default)]
struct Grove {
    hosts: HashMap<Subtree, Subtree>,
    asked: usize,
}

impl Lineage for Grove {
    type Error = &'static str;

    fn below(&mut self, subtree: &Subtree, above: &Subtree) -> Result<bool, &'static str> {
        self.asked += 1;
        let mut at = *subtree;
        while let Some(host) = self.hosts.get(&at) {
            if host == above {
                return Ok(true);
            }
            at = *host;
        }
        Ok(false)
    }
}

/// A lineage that cannot be read.
struct Unread;

impl Lineage for Unread {
    type Error = &'static str;

    fn below(&mut self, _: &Subtree, _: &Subtree) -> Result<bool, &'static str> {
        Err("unread")
    }
}

fn subtree(name: u8) -> Subtree {
    Subtree::new([name; 32])
}

fn data(name: u8) -> Unit {
    Unit::data(subtree(name))
}

fn aux(name: u8) -> Unit {
    Unit::new(Column::Aux, subtree(name))
}

fn overlap<E>(unit: Unit) -> Result<(), Refused<E>> {
    Err(Refused::Overlap(unit))
}

#[test]
fn a_unit_has_any_number_of_readers_or_one_writer() {
    let mut grove = Grove::default();
    let mut scopes = Scopes::default();
    assert!(!scopes.any_open());
    // With no scope open, a read outside is not looked at.
    assert_eq!(scopes.read(Who::OUTSIDE, data(1), &mut grove), Ok(()));
    let (one, two, three) = (scopes.open(), scopes.open(), scopes.open());
    assert!(scopes.any_open());
    // The first is alone when it opens, the others are beside it.
    assert!(scopes.laid(one) && !scopes.laid(two) && !scopes.laid(three));

    // Read against read: compatible, and taken once.
    for _ in 0..2 {
        assert_eq!(scopes.read(Who::scope(one), data(1), &mut grove), Ok(()));
        assert_eq!(scopes.read(Who::scope(two), data(1), &mut grove), Ok(()));
    }
    // Write against read: refused, and nothing is taken.
    assert_eq!(
        scopes.write(three, &[data(1)], &[], &[], &mut grove),
        overlap(data(1))
    );
    assert_eq!(
        scopes.write(one, &[data(1)], &[], &[], &mut grove),
        overlap(data(1))
    );
    assert!(!scopes.writes(one, data(1)));
    // Write against write, and read against write.
    assert_eq!(
        scopes.write(three, &[data(2)], &[], &[], &mut grove),
        Ok(())
    );
    assert!(scopes.writes(three, data(2)));
    assert_eq!(
        scopes.write(one, &[data(2)], &[], &[], &mut grove),
        overlap(data(2))
    );
    assert_eq!(
        scopes.read(Who::scope(one), data(2), &mut grove),
        overlap(data(2))
    );
    assert_eq!(
        scopes.read(Who::OUTSIDE, data(2), &mut grove),
        overlap(data(2))
    );
    // The writer reads and writes its unit again. Another column of the
    // subtree is another unit.
    assert_eq!(scopes.read(Who::scope(three), data(2), &mut grove), Ok(()));
    assert_eq!(
        scopes.write(three, &[data(2)], &[], &[], &mut grove),
        Ok(())
    );
    assert_eq!(scopes.write(one, &[aux(2)], &[], &[], &mut grove), Ok(()));
    // A reader outside keeps nothing: the unit is written afterwards.
    assert_eq!(scopes.read(Who::OUTSIDE, data(3), &mut grove), Ok(()));
    assert_eq!(scopes.write(two, &[data(3)], &[], &[], &mut grove), Ok(()));
    scopes.check();

    // A scope that ends gives up what it held, and the others keep theirs.
    assert!(scopes.close(two).is_some_and(|closed| !closed.laid));
    assert!(scopes.close(two).is_none());
    assert_eq!(scopes.write(one, &[data(3)], &[], &[], &mut grove), Ok(()));
    // The one reader that is left of a unit writes it.
    assert_eq!(scopes.write(one, &[data(1)], &[], &[], &mut grove), Ok(()));
    assert!(scopes.writes(one, data(1)));
    assert_eq!(
        scopes.read(Who::scope(three), data(1), &mut grove),
        overlap(data(1))
    );
    scopes.check();
    assert!(scopes.close(one).is_some_and(|closed| closed.laid));
    assert_eq!(scopes.read(Who::scope(three), data(1), &mut grove), Ok(()));
    scopes.close(three);
    scopes.check();
    assert!(!scopes.any_open());
    // No lineage was asked for: nothing was dropped.
    assert_eq!(grove.asked, 0);
    // A scope that opens when none is open is alone again.
    let four = scopes.open();
    assert!(scopes.laid(four) && four != one);
}

#[test]
fn the_claims_of_one_operation_are_granted_together_or_not_at_all() {
    let mut grove = Grove::default();
    let mut scopes = Scopes::default();
    let (one, two) = (scopes.open(), scopes.open());
    assert_eq!(scopes.read(Who::scope(two), data(2), &mut grove), Ok(()));
    let refused = scopes.write(one, &[data(1), data(2), data(3)], &[], &[], &mut grove);
    assert_eq!(refused, overlap(data(2)));
    for unit in [data(1), data(3)] {
        assert!(!scopes.writes(one, unit));
        assert_eq!(scopes.write(two, &[unit], &[], &[], &mut grove), Ok(()));
    }
    scopes.check();
}

/// What an operation asks for before it takes its claims is answered as
/// the claims would be, and takes nothing.
#[test]
fn the_claims_of_one_operation_are_asked_for_and_not_taken() {
    // 5 hosts 6.
    let mut grove = Grove::default();
    grove.hosts.insert(subtree(6), subtree(5));
    let mut scopes = Scopes::default();
    let one = scopes.open();
    // A scope that is alone is refused nothing, and no lineage is read.
    let alone = scopes.write_refused(one, &[data(1)], &[data(2)], &[subtree(5)], &mut Unread);
    assert_eq!(alone, Ok(None));
    let two = scopes.open();
    for unit in [data(1), data(2)] {
        assert_eq!(scopes.read(Who::scope(one), unit, &mut grove), Ok(()));
    }
    assert_eq!(scopes.read(Who::scope(two), data(2), &mut grove), Ok(()));

    // The one reader of a unit would write it, and holds it as it did:
    // another scope reads it afterwards, and then the write is refused.
    let asked = scopes.write_refused(one, &[data(1)], &[], &[], &mut grove);
    assert_eq!(asked, Ok(None));
    assert!(!scopes.writes(one, data(1)));
    assert_eq!(scopes.read(Who::scope(two), data(1), &mut grove), Ok(()));
    let asked = scopes.write_refused(one, &[data(1)], &[], &[], &mut grove);
    assert_eq!(asked, Ok(Some(data(1))));

    // Each kind of claim is refused with the unit that the claim itself
    // is refused with.
    assert_eq!(scopes.write(two, &[data(4)], &[], &[], &mut grove), Ok(()));
    assert_eq!(scopes.write(two, &[aux(6)], &[], &[], &mut grove), Ok(()));
    let cases = [
        (vec![data(3), data(2)], vec![], vec![], data(2)),
        (vec![data(3)], vec![data(4)], vec![], data(4)),
        (vec![data(3)], vec![], vec![subtree(5)], aux(6)),
    ];
    for (units, reads, whole, held) in &cases {
        let held = *held;
        let asked = scopes.write_refused(one, units, reads, whole, &mut grove);
        assert_eq!(asked, Ok(Some(held)));
        assert!(!scopes.writes(one, data(3)));
        let refused = scopes.write(one, units, reads, whole, &mut grove);
        assert_eq!(refused, overlap(held));
    }
    // A subtree that another scope dropped holds what is below it, and a
    // lineage that is not read is the error of the one that asks.
    scopes.close(two);
    let two = scopes.open();
    let dropped = scopes.write(two, &[data(5)], &[], &[subtree(5)], &mut grove);
    assert_eq!(dropped, Ok(()));
    let asked = scopes.write_refused(one, &[data(6)], &[], &[], &mut grove);
    assert_eq!(asked, Ok(Some(data(5))));
    let asked = scopes.write_refused(one, &[data(6)], &[], &[], &mut Unread);
    assert_eq!(asked, Err("unread"));
    assert!(!scopes.writes(one, data(6)));
    scopes.check();
}

#[test]
fn a_drop_holds_everything_below_it() {
    // 1 hosts 2, which hosts 3. 4 is beside them.
    let mut grove = Grove::default();
    grove.hosts.insert(subtree(2), subtree(1));
    grove.hosts.insert(subtree(3), subtree(2));
    let mut scopes = Scopes::default();
    let (one, two) = (scopes.open(), scopes.open());

    // A scope holds a unit below: the drop above it is refused, whether
    // the unit is read or written, and takes nothing.
    assert_eq!(scopes.read(Who::scope(two), aux(3), &mut grove), Ok(()));
    let columns = Column::ALL.map(|column| Unit::new(column, subtree(1)));
    let drop = scopes.write(one, &columns, &[], &[subtree(1)], &mut grove);
    assert_eq!(drop, overlap(aux(3)));
    assert!(!scopes.writes(one, data(1)));
    scopes.close(two);

    // The drop is granted, and from then on a unit below it is the
    // dropping scope's: for a read, for a write and for a reader outside,
    // at any depth, in any column, also a subtree that is entered later.
    let two = scopes.open();
    assert_eq!(
        scopes.write(one, &columns, &[], &[subtree(1)], &mut grove),
        Ok(())
    );
    let held = overlap(data(1));
    assert_eq!(scopes.read(Who::scope(two), aux(3), &mut grove), held);
    assert_eq!(scopes.write(two, &[data(2)], &[], &[], &mut grove), held);
    assert_eq!(scopes.read(Who::OUTSIDE, data(3), &mut grove), held);
    grove.hosts.insert(subtree(5), subtree(3));
    assert_eq!(scopes.write(two, &[data(5)], &[], &[], &mut grove), held);
    // The dropped subtree itself is held by its units, and what is beside
    // it is free. The dropping scope reaches below its own drop.
    assert_eq!(
        scopes.read(Who::scope(two), aux(1), &mut grove),
        overlap(aux(1))
    );
    assert_eq!(scopes.write(two, &[data(4)], &[], &[], &mut grove), Ok(()));
    assert_eq!(scopes.write(one, &[data(3)], &[], &[], &mut grove), Ok(()));
    assert_eq!(
        scopes.write(one, &columns, &[], &[subtree(1)], &mut grove),
        Ok(())
    );
    // A drop below another scope's drop is refused as any write there is,
    // and a drop above another scope's unit as well.
    assert_eq!(
        scopes.write(two, &[data(2)], &[], &[subtree(2)], &mut grove),
        held
    );
    grove.hosts.insert(subtree(1), subtree(6));
    grove.hosts.insert(subtree(4), subtree(6));
    let above = scopes.write(two, &[data(6)], &[], &[subtree(6)], &mut grove);
    assert!(matches!(above, Err(Refused::Overlap(unit)) if unit.subtree != subtree(6)));
    scopes.check();

    // A clear holds the data column of its subtree, and everything below.
    let three = scopes.open();
    grove.hosts.insert(subtree(8), subtree(7));
    assert_eq!(
        scopes.write(three, &[data(7)], &[], &[subtree(7)], &mut grove),
        Ok(())
    );
    assert_eq!(scopes.read(Who::scope(two), aux(7), &mut grove), Ok(()));
    assert_eq!(
        scopes.read(Who::scope(two), aux(8), &mut grove),
        overlap(data(7))
    );

    // With the scope, its drop goes.
    scopes.close(one);
    assert_eq!(scopes.read(Who::scope(two), aux(3), &mut grove), Ok(()));
    scopes.check();

    // A lineage that cannot be read refuses the claim with its error.
    assert_eq!(
        scopes.read(Who::scope(two), aux(9), &mut Unread),
        Err(Refused::Lineage("unread"))
    );
    let unread = scopes.write(two, &[aux(9)], &[], &[], &mut Unread);
    assert_eq!(unread, Err(Refused::Lineage("unread")));
}

/// An operation reads units it does not write: the Merks that its nodes
/// name as hosted. Those claims are granted with its write claims or not
/// at all, and kept as every read claim is.
#[test]
fn an_operation_reads_units_beside_those_it_writes() {
    let mut grove = Grove::default();
    let mut scopes = Scopes::default();
    let (one, two) = (scopes.open(), scopes.open());
    assert_eq!(scopes.write(two, &[data(2)], &[], &[], &mut grove), Ok(()));
    // A unit that another scope writes is not read, and the operation is
    // granted nothing.
    let refused = scopes.write(one, &[data(1)], &[data(4), data(2)], &[], &mut grove);
    assert_eq!(refused, overlap(data(2)));
    assert!(!scopes.writes(one, data(1)));
    assert_eq!(scopes.write(two, &[data(4)], &[], &[], &mut grove), Ok(()));
    // A unit that is read is written by nobody else from here on, and read
    // by anybody.
    let granted = scopes.write(one, &[data(1)], &[data(5), data(5)], &[], &mut grove);
    assert_eq!(granted, Ok(()));
    assert_eq!(scopes.read(Who::scope(two), data(5), &mut grove), Ok(()));
    assert_eq!(
        scopes.write(two, &[data(5)], &[], &[], &mut grove),
        overlap(data(5))
    );
    assert_eq!(
        scopes.write(one, &[data(5)], &[], &[], &mut grove),
        overlap(data(5))
    );
    // A unit the scope writes is not read less for being named: it stays
    // written.
    let granted = scopes.write(one, &[data(8)], &[data(1)], &[], &mut grove);
    assert_eq!(granted, Ok(()));
    assert!(scopes.writes(one, data(1)));
    // Nothing is read below a subtree that another scope dropped.
    grove.hosts.insert(subtree(7), subtree(6));
    let columns = Column::ALL.map(|column| Unit::new(column, subtree(6)));
    let dropped = scopes.write(two, &columns, &[], &[subtree(6)], &mut grove);
    assert_eq!(dropped, Ok(()));
    let below = scopes.write(one, &[data(9)], &[data(7)], &[], &mut grove);
    assert_eq!(below, overlap(data(6)));
    assert!(!scopes.writes(one, data(9)));
    let unread = scopes.write(one, &[], &[data(7)], &[], &mut Unread);
    assert_eq!(unread, Err(Refused::Lineage("unread")));
    scopes.check();

    // A scope that is alone is asked nothing, and keeps what it read for
    // the scope that opens beside it.
    let mut scopes = Scopes::default();
    let alone = scopes.open();
    let granted = scopes.write(alone, &[data(1)], &[data(2)], &[], &mut Unread);
    assert_eq!(granted, Ok(()));
    let beside = scopes.open();
    assert_eq!(
        scopes.write(beside, &[data(2)], &[], &[], &mut grove),
        overlap(data(2))
    );
    assert_eq!(scopes.read(Who::scope(beside), data(2), &mut grove), Ok(()));
    scopes.close(alone);
    // The one reader that is left writes the unit, and holds it alone.
    assert_eq!(
        scopes.write(beside, &[data(2)], &[], &[], &mut grove),
        Ok(())
    );
    assert!(scopes.writes(beside, data(2)));
    let third = scopes.open();
    assert_eq!(
        scopes.read(Who::scope(third), data(2), &mut grove),
        overlap(data(2))
    );
    scopes.check();
}

fn mark(name: u8, key: &[u8]) -> MarkKey {
    (subtree(name), key.into())
}

#[test]
fn marks_are_a_scopes_until_it_merges() {
    let mut scopes = Scopes::default();
    assert!(scopes.finalized());
    let (one, two) = (scopes.open(), scopes.open());
    let (host, other) = (mark(1, b"host"), mark(1, b"other"));

    // A mark is shown to the scope that set it, and to nobody else.
    scopes.mark(one, true, host.clone());
    scopes.mark(two, true, other.clone());
    assert!(scopes.is_pending(Who::scope(one), &host));
    assert!(!scopes.is_pending(Who::scope(two), &host));
    assert!(!scopes.is_pending(Who::OUTSIDE, &host));
    assert!(
        scopes.finalized(),
        "a mark of an open scope holds no commit back"
    );
    assert_eq!(scopes.marks_of(one), &[(true, host.clone())]);
    assert!(scopes.marks_of(99).is_empty());
    // The frames show the writes of the scope that is alone, and its marks
    // with them.
    let laid = |scopes: &Scopes| scopes.laid_marks().into_iter().collect::<Vec<_>>();
    assert_eq!(laid(&scopes), std::slice::from_ref(&host));
    assert!(scopes.marked_on_pages(&host) && !scopes.marked_on_pages(&other));

    // Merged, it is the transaction's, and shown to everybody.
    let closed = scopes.close(one).expect("open");
    scopes.merge_marks(closed.marks);
    assert!(scopes.marked_on_pages(&host));
    assert!(scopes.is_pending(Who::scope(two), &host));
    assert!(scopes.is_pending(Who::OUTSIDE, &host));
    assert!(!scopes.finalized());
    let merged = scopes.marks_len();

    // A scope takes a merged mark off: for itself at once, for the others
    // when it merges. A scope that is rolled back leaves it.
    scopes.mark(two, false, host.clone());
    assert!(!scopes.is_pending(Who::scope(two), &host));
    assert!(scopes.is_pending(Who::OUTSIDE, &host));
    let three = scopes.open();
    scopes.mark(three, false, host.clone());
    scopes.close(three);
    assert!(scopes.is_pending(Who::OUTSIDE, &host));
    scopes.set_laid(two);
    assert_eq!(laid(&scopes), std::slice::from_ref(&other));
    let ever: Vec<MarkKey> = scopes.ever_marked().into_iter().collect();
    assert_eq!(ever, [host.clone(), other.clone()]);
    let closed = scopes.close(two).expect("open");
    scopes.merge_marks(closed.marks);
    assert!(!scopes.is_pending(Who::OUTSIDE, &host));
    assert!(scopes.is_pending(Who::OUTSIDE, &other));
    // A change that changes nothing is none: setting a mark twice.
    let four = scopes.open();
    scopes.mark(four, true, other.clone());
    let closed = scopes.close(four).expect("open");
    let before = scopes.marks_len();
    scopes.merge_marks(closed.marks);
    assert_eq!(scopes.marks_len(), before);

    // A rollback to a mark of the write set undoes what came after it.
    assert!(scopes.rewind_marks(merged));
    assert!(!scopes.rewind_marks(merged), "nothing came after it");
    assert!(scopes.is_pending(Who::OUTSIDE, &host));
    assert!(!scopes.is_pending(Who::OUTSIDE, &other));
    assert!(scopes.rewind_marks(0));
    assert!(scopes.finalized());
    let five = scopes.open();
    scopes.mark(five, true, host.clone());
    let closed = scopes.close(five).expect("open");
    scopes.merge_marks(closed.marks);
    scopes.clear_marks();
    assert!(scopes.finalized() && scopes.marks_len() == 0);
}
