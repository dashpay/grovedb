//! The records of background changes: what they say of a page, and how a
//! replay tells a page that has the change from one that lacks it.

use super::*;
use crate::{
    tree_page::{TreePage, TreePageMut},
    PageSize,
};

fn leaf_of(raw_page: u32, entries: &[(&[u8], FlatValue<'_>)]) -> Vec<u8> {
    let mut leaf = FlatLeafBuilder::new(page(raw_page), b"", CAPACITY).unwrap();
    for (key, value) in entries {
        leaf.push(key, *value).unwrap();
    }
    leaf.finish()
}

fn leaf_with(entries: &[(&[u8], FlatValue<'_>)]) -> Vec<u8> {
    leaf_of(7, entries)
}

#[test]
fn the_place_of_a_value_is_given_to_the_cell_that_still_holds_the_old_one() {
    let size = PageSize::MIN;
    let mut frame = vec![0; size.get()];
    let waiting = ValuePlace::Pending(pending(45, 300));
    let placed = ValuePlace::Placed(pointer(1, 20, 300));
    let mut tree = TreePageMut::init(&mut frame, size, page(7), 0).unwrap();
    for (raw_slot, key, place) in [(1, b"a", waiting), (2, b"b", placed)] {
        let cell = cell_bytes(key, cell_value(place));
        tree.insert_encoded(slot(raw_slot), TreeCell::decode(&cell, V1).unwrap())
            .unwrap();
    }

    let to = pointer(13, 364, 300);
    let place = ValueMove::in_tree(page(7), slot(1), waiting, to).unwrap();
    assert_eq!(
        (place.page(), place.slot(), place.from(), place.to()),
        (page(7), Some(slot(1)), waiting, to)
    );
    assert_eq!((place.released(), place.acquired()), (None, to));
    let elsewhere = pointer(2, 20, 300);
    let moved = ValueMove::in_tree(page(7), slot(2), placed, elsewhere).unwrap();
    assert_eq!(moved.released(), placed.pointer());

    // A step that is not for this page, this cell or this place is not due.
    let not_due = [
        ValueMove::in_tree(page(8), slot(1), waiting, to).unwrap(),
        ValueMove::in_tree(page(7), slot(2), waiting, to).unwrap(),
        ValueMove::in_tree(page(7), slot(3), waiting, to).unwrap(),
        ValueMove::in_tree(page(7), slot(1), placed, to).unwrap(),
        ValueMove::in_tree(page(7), slot(1), ValuePlace::Pending(pending(46, 300)), to).unwrap(),
        ValueMove::in_flat_leaf(page(7), waiting, to).unwrap(),
    ];
    for step in not_due {
        assert!(!step.is_due_in_tree(&tree.page()), "{step:?}");
    }

    for (step, raw_slot) in [(place, 1), (moved, 2)] {
        assert!(step.is_due_in_tree(&tree.page()));
        let before = tree.page().to_image();
        step.delta().expect("a delta").apply(&mut tree).unwrap();
        // Made once, the step is not due again, and it changed the place
        // and nothing else.
        assert!(!step.is_due_in_tree(&tree.page()));
        let cell = tree.page().cell(slot(raw_slot)).unwrap();
        assert_eq!(cell.value_pointer(), Some(step.to()));
        let after = tree.page().to_image();
        let changed = before
            .iter()
            .zip(&after)
            .filter(|(old, new)| old != new)
            .count();
        assert!(changed <= 12, "{changed} bytes");
        assert_eq!(before.len(), after.len());
    }
    assert!(ValueMove::in_flat_leaf(page(7), waiting, to)
        .unwrap()
        .delta()
        .is_none());
    let view = TreePage::from_validated(&frame, V1).unwrap();
    assert!(!place.is_due_in_tree(&view));
}

#[test]
fn the_place_of_a_value_in_a_flat_leaf_is_found_by_the_place_it_holds() {
    let from = ValuePlace::Placed(pointer(1, 20, 300));
    let to = pointer(13, 364, 300);
    let other = ValuePlace::Pending(pending(77, 300)).encode();
    let leaf_holding = |held: &[u8; 16]| {
        leaf_with(&[
            (b"a", FlatValue::Inline(b"inline")),
            (b"b", FlatValue::OutOfLine(held)),
            (b"c", FlatValue::Inline(b"")),
        ])
    };
    let before = leaf_holding(&from.encode());
    let after = leaf_holding(&to.encode());
    let unrelated = leaf_holding(&other);

    let step = ValueMove::in_flat_leaf(page(7), from, to).unwrap();
    assert_eq!(step.slot(), None);
    assert_eq!(step.is_due_in_flat_leaf(&before, V1), Ok(true));
    let mut out = b"kept".to_vec();
    step.apply_to_flat_leaf(&before, V1, CAPACITY, &mut out)
        .unwrap();
    assert_eq!(out[4..], after);
    assert_eq!(step.is_due_in_flat_leaf(&after, V1), Ok(false));
    assert_eq!(step.is_due_in_flat_leaf(&unrelated, V1), Ok(false));

    // Made a second time, or to a page that does not hold the place, the
    // step is refused and writes nothing.
    let mut out = b"kept".to_vec();
    for image in [&after, &unrelated] {
        assert_eq!(
            step.apply_to_flat_leaf(image, V1, CAPACITY, &mut out),
            Err(wal_error(WalError::NoSuchPlace))
        );
    }
    let branch = FlatBranchBuilder::new(page(7), b"", CAPACITY, 1, page(3))
        .unwrap()
        .finish();
    assert_eq!(step.is_due_in_flat_leaf(&branch, V1), Ok(false));
    assert_eq!(
        step.apply_to_flat_leaf(&branch, V1, CAPACITY, &mut out),
        Err(wal_error(WalError::WrongPageKind))
    );
    assert!(step.is_due_in_flat_leaf(&before[..9], V1).is_err());
    assert!(step
        .apply_to_flat_leaf(&[0; 40], V1, CAPACITY, &mut out)
        .is_err());
    // Another page that holds the place is not the page of the step.
    let another = leaf_of(8, &[(b"b", FlatValue::OutOfLine(&from.encode()))]);
    assert_eq!(step.is_due_in_flat_leaf(&another, V1), Ok(false));
    assert_eq!(
        step.apply_to_flat_leaf(&another, V1, CAPACITY, &mut out),
        Err(wal_error(WalError::WrongPage {
            found: page(8),
            expected: page(7),
        }))
    );
    // An inline value with the same 16 bytes is not the place.
    let inline = leaf_with(&[(b"b", FlatValue::Inline(&from.encode()))]);
    assert_eq!(step.is_due_in_flat_leaf(&inline, V1), Ok(false));
    // The page is as long as it was: a place takes the room of a place.
    assert_eq!(
        step.apply_to_flat_leaf(&before, V1, before.len(), &mut out),
        Ok(())
    );
    assert_eq!(out[4..], after);

    // A step for a tree cell is not one for a leaf.
    let in_tree = ValueMove::in_tree(page(7), slot(1), from, to).unwrap();
    assert_eq!(in_tree.is_due_in_flat_leaf(&before, V1), Ok(false));
    assert_eq!(
        in_tree.apply_to_flat_leaf(&before, V1, CAPACITY, &mut out),
        Err(wal_error(WalError::WrongPageKind))
    );
    assert_eq!(out[..4], *b"kept");
}

#[test]
fn a_value_moves_to_another_place_and_keeps_its_length() {
    let invalid = Err(wal_error(WalError::InvalidChange));
    let to = pointer(13, 364, 300);
    assert_eq!(
        ValueMove::in_tree(page(7), slot(1), ValuePlace::Placed(to), to),
        invalid
    );
    assert_eq!(
        ValueMove::in_flat_leaf(page(7), ValuePlace::Placed(to), to),
        invalid
    );
    for from in [
        ValuePlace::Placed(pointer(13, 364, 299)),
        ValuePlace::Pending(pending(45, 301)),
    ] {
        assert_eq!(ValueMove::in_tree(page(7), slot(1), from, to), invalid);
        assert_eq!(ValueMove::in_flat_leaf(page(7), from, to), invalid);
    }
    assert!(ValueMove::in_tree(
        page(7),
        slot(1),
        ValuePlace::Placed(pointer(13, 365, 300)),
        to
    )
    .is_ok());
}

#[test]
fn a_reclaiming_step_removes_or_frees_and_names_what_it_released() {
    let mut rng = SplitMix64::new(0x4EC1);
    let tree = random_tree_run(&mut rng, page(7), true, usize::MAX);
    let flat = random_flat_run(&mut rng, page(7), true, usize::MAX);
    let tree_run = Removed::Tree(DeltaRun::decode(&tree, V1).unwrap());
    let flat_run = Removed::Flat(FlatRun::decode(&flat, V1).unwrap());
    assert_eq!((tree_run.page(), flat_run.page()), (page(7), page(7)));
    let (low, high) = (pointer(3, 20, 4), pointer(3, 21, 4));

    let written = |removed, freed: Option<u64>, released: &[ValuePointer]| {
        let mut out = b"kept".to_vec();
        let result = write_reclaim(
            page(7),
            removed,
            freed.map(|raw| CommitVersion::new(raw).unwrap()),
            released.iter().copied(),
            &mut out,
        );
        if result.is_err() {
            assert_eq!(out, b"kept");
        }
        result.map(|()| out[4..].to_vec())
    };
    for (removed, freed) in [
        (Some(tree_run), None),
        (Some(flat_run), None),
        (Some(tree_run), Some(3)),
        (Some(flat_run), Some(3)),
        (None, Some(3)),
    ] {
        let record = written(removed, freed, &[low, high]).unwrap();
        let (Record::Change(change), len) = decode(&record).unwrap() else {
            panic!("a change");
        };
        assert_eq!(len, record.len());
        assert_eq!(change.page(), page(7));
        let Change::Reclaim(reclaim) = change else {
            panic!("a reclaim");
        };
        assert_eq!(reclaim.removed(), removed);
        assert_eq!(reclaim.freed(), freed.map(version));
        assert_eq!(reclaim.released().collect::<Vec<_>>(), [low, high]);
        let mut again = Vec::new();
        change.write(&mut again).unwrap();
        assert_eq!(again, record);
    }

    let invalid = Err(wal_error(WalError::InvalidChange));
    assert_eq!(written(None, None, &[low]), invalid);
    assert_eq!(
        written(None, Some(0), &[]),
        Err(wal_error(WalError::NotACommit))
    );
    for released in [[high, low], [low, low]] {
        assert_eq!(
            written(None, Some(3), &released),
            Err(wal_error(WalError::EntriesOutOfOrder))
        );
    }
    // The pointers are read once, and the record holds what was read.
    let mut left = [low, high].into_iter();
    let mut once = Vec::new();
    write_reclaim(
        page(7),
        None,
        Some(version(3)),
        std::iter::from_fn(|| left.next()),
        &mut once,
    )
    .unwrap();
    assert_eq!(Ok(once), written(None, Some(3), &[low, high]));
    let many: Vec<_> = (0..65_536).map(|at| pointer(3, 20 + at, 4)).collect();
    assert_eq!(written(None, Some(3), &many), invalid);
    assert!(written(None, Some(3), &many[..65_535]).is_ok());

    // The run is of the page, and takes out and nothing else.
    let elsewhere = random_tree_run(&mut rng, page(8), true, usize::MAX);
    assert_eq!(
        written(
            Some(Removed::Tree(DeltaRun::decode(&elsewhere, V1).unwrap())),
            None,
            &[]
        ),
        Err(wal_error(WalError::WrongPage {
            found: page(8),
            expected: page(7),
        }))
    );
    let mut creates = Vec::new();
    DeltaRunWriter::new(page(7), Some(0), &mut creates)
        .finish()
        .unwrap();
    let mut inserts = Vec::new();
    let mut writer = FlatRunWriter::new(page(7), &mut inserts);
    writer
        .push(&FlatChange::Insert {
            key: b"k",
            value: FlatValue::Inline(b""),
        })
        .unwrap();
    writer.finish().unwrap();
    assert_eq!(
        written(
            Some(Removed::Tree(DeltaRun::decode(&creates, V1).unwrap())),
            None,
            &[]
        ),
        invalid
    );
    assert_eq!(
        written(
            Some(Removed::Flat(FlatRun::decode(&inserts, V1).unwrap())),
            None,
            &[]
        ),
        invalid
    );

    // The record of a value's place is written again as it was read.
    let step =
        ValueMove::in_tree(page(7), slot(1), ValuePlace::Pending(pending(45, 4)), low).unwrap();
    let change = Change::Value(step);
    assert_eq!(change.page(), page(7));
    let (mut first, mut second) = (Vec::new(), Vec::new());
    change.write(&mut first).unwrap();
    step.write(&mut second).unwrap();
    assert_eq!(first, second);
    assert_eq!(decode(&first), Ok((Record::Change(change), 48)));
}
