//! The move rule of the value log (`tree-layout.md` X1,
//! `value-log-format.md` O5): a cell that is carried takes its value with
//! it.

use super::{grove::Grove, *};

fn grove() -> Grove {
    let mut grove = Grove::new(Layout::proposed(SIZE), 9);
    grove.plain_names = true;
    grove
}

fn key(index: u32) -> [u8; 4] {
    (Rng(u64::from(index)).next() as u32).to_be_bytes()
}

/// A value of 400 bytes, which the grove stores out of line.
fn big(index: u32) -> Vec<u8> {
    (0..400u32).map(|at| (index + at / 7) as u8).collect()
}

fn write_back(grove: &mut Grove) {
    let Grove { log, mem, .. } = grove;
    log.write_back(mem);
    let pages = &mut grove.mem.frames.keys();
    assert!(pages.all(|id| !grove.mem.view(*id).tree_page().has_pending_value()));
}

/// The pointer the cell of `key` holds.
fn pointer(grove: &Grove, key: &[u8]) -> ValuePointer {
    let at = grove.node(0, key).at.expect("the node has a cell");
    let view = grove.mem.view(at.page());
    let cell = view.node(at.slot()).expect("the cell");
    cell.value()
        .pointer()
        .expect("the value is in the value log")
}

/// The acceptance test of TREE-03, second sentence: a node with an
/// out-of-line value that moves to another page has the value appended
/// again with the new page as owner, in the same commit.
#[test]
fn a_carried_cell_has_its_value_appended_again_for_its_new_page() {
    carried_values(false);
}

/// The same when the write states again the pending value a cell holds:
/// the value is carried, and not released and given back.
#[test]
fn a_pending_value_that_a_write_states_again_is_carried_with_its_cell() {
    carried_values(true);
}

fn carried_values(restate: bool) {
    let mut grove = grove();
    grove.restate = restate;
    let (mut placed, mut pending, mut restated) = (0, 0, 0);
    for index in 0..1200 {
        let before = grove.log.carried.len();
        let report = grove.put(0, &key(index), &big(index));

        // Every cell in these pages has its value out of line. Each one
        // that was carried was told to the value log, with the page it left
        // and the page it is in.
        let told = &grove.log.carried[before..];
        assert_eq!(told.len(), report.carried.len());
        assert_eq!(told.len() as u64, report.carries.total().values);
        for (carried, (place, from, to)) in report.carried.iter().zip(told) {
            assert_eq!((carried.from.page(), carried.to.page()), (*from, *to));
            match place {
                ValuePlace::Placed(pointer) => {
                    // The pointer the cell held is dropped: its record names
                    // the page the cell left, and no cell holds it now.
                    let logged = &grove.log.logged[pointer];
                    assert!(!logged.live && logged.owner == *from);
                    let holds = |id: &PageId| {
                        let page = grove.mem.view(*id).tree_page();
                        page.find_value_pointer(pointer).is_some()
                    };
                    assert!(!grove.mem.frames.keys().any(holds));
                    placed += 1;
                }
                ValuePlace::Pending(_) => {
                    pending += 1;
                    restated += usize::from(carried.node.is_some());
                }
            }
            // The cell in the new page is pending, and the value that will
            // be appended for it is owned by that page.
            let view = grove.mem.view(*to);
            let node = view
                .resolve(carried.to)
                .expect("the report's handle is good");
            let waiting = node.value().pending().expect("pending until it is placed");
            assert_eq!(grove.log.pending[&waiting.id()].1, Some(*to));
        }
        if index % 25 == 0 {
            // The commit's write-back appends each value with the page that
            // holds its cell as the owner, and record 21 puts the pointer
            // into the cell. The grove then reads every value through its
            // pointer and its owner.
            write_back(&mut grove);
            grove.check();
        }
    }
    write_back(&mut grove);
    grove.check();
    assert!(
        placed > 300 && pending > 10,
        "{placed} placed, {pending} pending"
    );
    // Some of the pending values that were carried were in cells of nodes
    // the write wrote, which states them again when it is told to.
    assert!(
        restated > 0,
        "{restated} written nodes were carried while pending"
    );
    let carries = grove.fragments.counters().carries;
    assert_eq!(carries.total().values, grove.log.appended_again as u64);
    assert_eq!(carries.total().value_bytes, 400 * carries.total().values);
    assert_eq!(carries.total().cells, carries.total().values);
    assert!(
        grove.log.released.is_empty(),
        "nothing was deleted or overwritten"
    );
    println!(
        "{:.2} values appended again for each value written, {} of them by a split",
        carries.total().values as f64 / 1200.0,
        carries[Cause::Split].values
    );
}

#[test]
fn a_value_is_released_when_its_cell_is_deleted_or_given_another_value() {
    let mut grove = grove();
    for index in 0..15 {
        grove.put(0, &key(index), &big(index));
    }
    write_back(&mut grove);
    let page = grove.page_of(0, &key(3));

    // Another value out of line: the pointer is released, and the new value
    // is owned by the cell's page.
    let old = pointer(&grove, &key(3));
    grove.put(0, &key(3), &big(999));
    assert_eq!(
        grove.log.released.last(),
        Some(&(ValuePlace::Placed(old), page))
    );
    let view = grove.mem.view(page);
    let waiting = view.pending_in(grove.node(0, &key(3)).at);
    assert_eq!(grove.log.pending[&waiting.id()].1, Some(page));

    // A value that was not placed yet is released as what it is.
    grove.put(0, &key(3), &big(1000));
    assert_eq!(
        grove.log.released.last(),
        Some(&(ValuePlace::Pending(waiting), page))
    );
    let waiting = grove.mem.view(page).pending_in(grove.node(0, &key(3)).at);
    grove.put(0, &key(3), b"small");
    assert_eq!(
        grove.log.released.last(),
        Some(&(ValuePlace::Pending(waiting), page))
    );

    let old = pointer(&grove, &key(5));
    grove.delete(0, &key(5));
    assert_eq!(
        grove.log.released.last(),
        Some(&(ValuePlace::Placed(old), page))
    );
    assert_eq!(grove.log.released.len(), 4);

    // A hash, a link, a rotation and a new node beside it leave a value
    // where it is (X2).
    for index in 100..120 {
        grove.put(0, &key(index), b"small");
    }
    grove.touch(0, &key(7));
    assert_eq!(grove.log.released.len(), 4);
    assert!(grove.log.carried.is_empty());
    assert_eq!(grove.mem.frames.len(), 1);
    grove.check();
}

#[test]
fn a_cell_that_is_carried_and_given_a_value_in_one_write_costs_the_new_value_only() {
    let mut grove = grove();
    for index in 0..500 {
        grove.put(0, &key(index), &big(index));
    }
    write_back(&mut grove);
    let mut seen = 0;
    for round in 0..30 {
        let page_of = |node: usize| grove.nodes[node].at.map(|at| at.page());
        let pair = grove.nodes.iter().enumerate().find_map(|(id, node)| {
            let mut successor = node.right.filter(|_| node.alive && node.left.is_some())?;
            while let Some(left) = grove.nodes[successor].left {
                successor = left;
            }
            (page_of(successor) != page_of(id)).then_some((id, successor))
        });
        let Some((gone, successor)) = pair else { break };
        let (gone, taker) = (
            grove.nodes[gone].key.clone(),
            grove.nodes[successor].key.clone(),
        );
        let from = grove.nodes[successor].at.expect("placed");
        let held = pointer(&grove, &taker);
        let (carried, released) = (grove.log.carried.len(), grove.log.released.len());
        let report = grove.delete_and_put(0, &gone, &taker, &big(7000 + round));
        write_back(&mut grove);
        let moved = report.carried.iter().find(|carried| carried.from == from);
        let Some(moved) = moved else { continue };

        // The value it held is released, not carried, and the new one is
        // owned by the page the cell ended in, which the grove checks.
        let place = ValuePlace::Placed(held);
        assert!(grove.log.released[released..].contains(&(place, from.page())));
        assert!(!grove.log.carried[carried..]
            .iter()
            .any(|told| told.0 == place));
        assert_eq!(grove.page_of(0, &taker), moved.to.page());
        seen += 1;
    }
    grove.check();
    assert!(
        seen > 10,
        "{seen} cells were carried and written in one write"
    );
}

trait PendingIn {
    fn pending_in(&self, at: Option<CellAddress>) -> PendingValue;
}

impl PendingIn for NodePage<'_> {
    fn pending_in(&self, at: Option<CellAddress>) -> PendingValue {
        let cell = self.node(at.expect("the node has a cell").slot());
        let value = cell.expect("the cell").value();
        value.pending().expect("the value is pending")
    }
}
