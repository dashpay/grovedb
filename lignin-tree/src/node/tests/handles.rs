use std::{collections::HashSet, sync::atomic::AtomicU64, thread};

use lignin_format::tree_page::{HashField, Side};

use super::*;

#[test]
fn a_lease_is_taken_once() {
    let taken: Vec<Lease> = thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| (0..500).map(|_| Lease::take()).collect::<Vec<_>>()))
            .collect();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().expect("the thread ends"))
            .collect()
    });
    let distinct: HashSet<u64> = taken.iter().map(|lease| lease.get()).collect();
    assert_eq!(distinct.len(), 4_000);

    let lease = taken[0];
    assert_eq!(format!("{lease:?}"), format!("lease#{}", lease.get()));
    assert_eq!(lease.to_string(), format!("lease {}", lease.get()));
}

/// The numbers run out before one is given twice.
#[test]
fn a_lease_number_is_not_given_twice_when_they_run_out() {
    let counter = AtomicU64::new(u64::MAX - 2);
    let issued: Vec<_> = (0..5)
        .map(|_| handle::issue(&counter).map(|number| number.get()))
        .collect();
    assert_eq!(
        issued,
        [Some(u64::MAX - 2), Some(u64::MAX - 1), None, None, None]
    );

    let counter = AtomicU64::new(1);
    let taken: Vec<u64> = thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    (0..500)
                        .map(|_| handle::issue(&counter).expect("a number").get())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().expect("the thread ends"))
            .collect()
    });
    let distinct: HashSet<u64> = taken.iter().copied().collect();
    assert_eq!((distinct.len(), distinct.contains(&0)), (4_000, false));
    assert_eq!(distinct.iter().max(), Some(&4_000));
}

#[test]
fn a_handle_is_an_address_a_count_and_a_lease() {
    assert_eq!(size_of::<NodeHandle>(), 24);
    assert_eq!(size_of::<Option<NodeHandle>>(), 24);

    let lease = Lease::take();
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, page(7), lease, ()).expect("a page");
    nodes.insert(&leaf(b"a", &[1], b"one")).expect("fits");
    let second = nodes.insert(&leaf(b"b", &[2], b"two")).expect("fits");
    nodes
        .node_mut(slot(1))
        .expect("a node")
        .remove()
        .expect("it leaves");
    let handle = nodes.view().node(second.slot()).expect("a node").handle();

    assert_eq!(handle.address(), address(7, 2));
    assert_eq!((handle.page(), handle.slot()), (page(7), slot(2)));
    assert_eq!((handle.departures(), handle.lease()), (1, lease));
    assert_ne!(handle, second);
    assert_eq!(
        handle.to_string(),
        format!("{} after 1 departures, under {lease}", address(7, 2))
    );
}

/// What the test knows of a page: the node in each slot, and how many cells
/// have left.
struct Model {
    id: PageId,
    lease: Lease,
    frame: Vec<u8>,
    slots: Vec<Option<u64>>,
    departures: u64,
}

impl Model {
    fn new(id: PageId, lease: Lease) -> Self {
        let mut frame = frame();
        NodePageMut::create(&mut frame, SIZE, id, lease, ()).expect("a page");
        Self {
            id,
            lease,
            frame,
            slots: Vec::new(),
            departures: 0,
        }
    }

    fn view(&self) -> NodePage<'_> {
        NodePage::open(&self.frame, self.lease).expect("a tree page")
    }

    fn open(&mut self) -> NodePageMut<'_> {
        NodePageMut::open(&mut self.frame, SIZE, self.lease, ()).expect("a tree page")
    }

    fn occupied(&self) -> Vec<SlotId> {
        (1..)
            .zip(&self.slots)
            .filter(|(_, node)| node.is_some())
            .map(|(raw, _)| slot(raw))
            .collect()
    }

    fn put(&mut self, at: SlotId, node: u64) {
        if self.slots.len() <= at.index() {
            self.slots.resize(at.index() + 1, None);
        }
        assert_eq!(self.slots[at.index()].replace(node), None, "a free slot");
    }

    fn take(&mut self, at: SlotId) -> u64 {
        self.departures += 1;
        self.slots[at.index()].take().expect("an occupied slot")
    }
}

fn node_id(node: &NodeRef<'_>) -> u64 {
    u64::from_le_bytes(node.feature().try_into().expect("eight bytes"))
}

/// A handle is followed to the node it was issued for, or not at all: never
/// to another node. It is followed exactly as long as no cell has left its
/// page, by the test's own count of the cells that left. Each handle is
/// checked after every change until 200 to 300 later handles have been
/// issued.
#[test]
fn a_handle_names_its_node_until_a_cell_leaves_its_page() {
    let lease = Lease::take();
    let mut rng = Rng(0x7EE2);
    let mut pages = [Model::new(page(3), lease), Model::new(page(4), lease)];
    let mut issued: Vec<(NodeHandle, u64, u64)> = Vec::new();
    let mut next_node = 0u64;
    let (mut followed, mut refused, mut carried) = (0u32, 0u32, 0u32);

    for _ in 0..6_000 {
        let which = rng.below(2);
        let roll = rng.below(100);
        let occupied = pages[which].occupied();
        let picked = (!occupied.is_empty()).then(|| occupied[rng.below(occupied.len())]);
        let has_room = |model: &Model| model.view().tree_page().free_bytes(SIZE) >= 1_000;
        let room = has_room(&pages[which]);
        let room_beside = has_room(&pages[1 - which]);

        match (roll, picked) {
            (_, None) | (0..=29, _) if room => {
                // Few keys, so that a slot is given to a cell with a key
                // that a cell of that slot had before.
                let key = [b'k', rng.below(3) as u8];
                let value = vec![0xEE; rng.below(300)];
                let model = &mut pages[which];
                let mut nodes = model.open();
                let handle = nodes
                    .insert(&leaf(&key, &next_node.to_le_bytes(), &value))
                    .expect("the page has room");
                model.put(handle.slot(), next_node);
                issued.push((handle, next_node, model.departures));
                next_node += 1;
            }
            (30..=34, Some(at)) if room_beside => {
                let [first, second] = &mut pages;
                let (from, to) = if which == 0 {
                    (first, second)
                } else {
                    (second, first)
                };
                let cell = from.view().node(at).expect("a node").cell();
                let mut nodes = to.open();
                let handle = nodes.insert_cell(cell).expect("the page has room");
                let mut nodes = from.open();
                nodes
                    .node_mut(at)
                    .expect("a node")
                    .remove()
                    .expect("it leaves");
                let node = from.take(at);
                to.put(handle.slot(), node);
                issued.push((handle, node, to.departures));
                carried += 1;
            }
            (35..=94, Some(at)) if room => {
                let model = &mut pages[which];
                let mut nodes = model.open();
                let mut node = nodes.node_mut(at).expect("a node");
                match roll % 4 {
                    0 => node.set_value(CellValue::Inline(&vec![0xAB; rng.below(300)])),
                    1 => node.set_hash(HashField::Value, &[roll as u8; 32]),
                    2 => node.set_link(
                        Side::Left,
                        Some(link(address(90, 1), &[roll as u8; 32], &[])),
                    ),
                    _ => node.set_link(Side::Left, None),
                }
                .expect("the change is made");
                let handle = node.handle();
                let node = model.slots[at.index()].expect("a node");
                issued.push((handle, node, model.departures));
            }
            (_, Some(at)) => {
                let model = &mut pages[which];
                let mut nodes = model.open();
                nodes
                    .node_mut(at)
                    .expect("a node")
                    .remove()
                    .expect("it leaves");
                model.take(at);
            }
            (_, None) => continue,
        }

        if issued.len() > 300 {
            issued.drain(..100);
        }
        for &(handle, node, issued_at) in &issued {
            let model = pages
                .iter_mut()
                .find(|model| model.id == handle.page())
                .expect("a page of the test");
            assert_eq!(handle.departures(), issued_at, "{handle}");
            let good = model.departures == issued_at;
            let found = model.view().resolve(handle).map(|found| node_id(&found));
            assert_eq!(found, good.then_some(node), "{handle}");
            if good {
                assert_eq!(model.slots[handle.slot().index()], Some(node));
                followed += 1;
            } else {
                refused += 1;
            }
            let mut nodes = model.open();
            let found = nodes
                .resolve_mut(handle)
                .map(|found| node_id(&found.node()));
            assert_eq!(found, good.then_some(node), "{handle}");
        }
    }

    assert!(
        followed > 20_000 && refused > 100_000 && carried > 100,
        "{followed} followed, {refused} refused, {carried} carried"
    );
}

#[test]
fn the_key_of_the_cell_that_is_found_proves_nothing() {
    let lease = Lease::take();
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, page(5), lease, ()).expect("a page");
    let first = nodes
        .insert(&leaf(b"same", &[1], b"of one Merk"))
        .expect("fits");
    nodes
        .resolve_mut(first)
        .expect("good")
        .remove()
        .expect("it leaves");
    let second = nodes
        .insert(&leaf(b"same", &[2], b"of another"))
        .expect("fits");

    let nodes = nodes.view();
    assert_eq!(second.address(), first.address());
    let found = nodes.node(first.slot()).expect("a node");
    assert_eq!((found.key(), found.feature()), (&b"same"[..], &[2][..]));
    assert_eq!(nodes.resolve(first), None);
    assert_eq!(nodes.resolve(second), Some(found));
}

#[test]
fn a_handle_is_followed_in_its_own_page_and_under_its_own_lease() {
    let lease = Lease::take();
    let other_lease = Lease::take();
    let (mut first, mut second) = (frame(), frame());
    let mut nodes = NodePageMut::create(&mut first, SIZE, page(5), lease, ()).expect("a page");
    let handle = nodes.insert(&leaf(b"key", &[1], b"here")).expect("fits");
    let mut nodes = NodePageMut::create(&mut second, SIZE, page(6), lease, ()).expect("a page");
    let twin = nodes.insert(&leaf(b"key", &[2], b"there")).expect("fits");
    assert_eq!(
        (twin.slot(), twin.departures()),
        (handle.slot(), handle.departures())
    );

    let here = NodePage::open(&first, lease).expect("a tree page");
    let there = NodePage::open(&second, lease).expect("a tree page");
    assert_eq!(
        here.resolve(handle).map(|node| node.feature()),
        Some(&[1][..])
    );
    assert_eq!(there.resolve(handle), None);
    assert_eq!(here.resolve(twin), None);

    let elsewhere = NodePage::open(&first, other_lease).expect("a tree page");
    assert_eq!(elsewhere.lease(), other_lease);
    assert_eq!(elsewhere.resolve(handle), None);
    let mut nodes = NodePageMut::open(&mut first, SIZE, other_lease, ()).expect("a tree page");
    assert!(nodes.resolve_mut(handle).is_none());
    assert!(nodes.node_mut(handle.slot()).is_some());
}

/// A frame that is put back to an earlier state counts its departures on
/// from that state, so a count can come again with another cell in the
/// slot. The count cannot tell, and the lease has to: whoever puts a frame
/// back takes a new one.
#[test]
fn a_frame_that_goes_back_needs_a_new_lease() {
    let lease = Lease::take();
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, page(8), lease, ()).expect("a page");
    let kept = nodes.insert(&leaf(b"kept", &[1], b"stays")).expect("fits");
    let other = nodes
        .insert(&leaf(b"other", &[2], b"stays too"))
        .expect("fits");
    let savepoint = frame.clone();

    let mut nodes = NodePageMut::open(&mut frame, SIZE, lease, ()).expect("a tree page");
    nodes
        .resolve_mut(kept)
        .expect("good")
        .remove()
        .expect("it leaves");
    let late = nodes
        .insert(&leaf(b"late", &[3], b"rolled back"))
        .expect("fits");
    assert_eq!((late.slot(), late.departures()), (kept.slot(), 1));

    frame.copy_from_slice(&savepoint);
    let mut nodes = NodePageMut::open(&mut frame, SIZE, lease, ()).expect("a tree page");
    nodes
        .resolve_mut(other)
        .expect("good")
        .remove()
        .expect("it leaves");

    let under_the_old_lease = NodePage::open(&frame, lease).expect("a tree page");
    let wrong = under_the_old_lease
        .resolve(late)
        .expect("the count came again");
    assert_eq!(wrong.key(), b"kept");

    let under_a_new_lease = NodePage::open(&frame, Lease::take()).expect("a tree page");
    assert_eq!(under_a_new_lease.resolve(late), None);
    assert_eq!(under_a_new_lease.resolve(kept), None);
    let again = under_a_new_lease.node(kept.slot()).expect("a node");
    assert_eq!(under_a_new_lease.resolve(again.handle()), Some(again));
}

/// A cell that arrives does not move the count. Two copies of one page that
/// are changed side by side can each put a cell into the same free slot, and
/// then one handle fits both. Each copy has a lease of its own.
#[test]
fn two_copies_that_go_on_side_by_side_need_a_lease_each() {
    let shared = Lease::take();
    let mut committed = frame();
    let mut nodes = NodePageMut::create(&mut committed, SIZE, page(8), shared, ()).expect("a page");
    let before = nodes.insert(&leaf(b"before", &[1], b"")).expect("fits");
    let (mut first, mut second) = (committed.clone(), committed.clone());

    let put = |frame: &mut [u8], lease, feature: &[u8]| {
        let mut nodes = NodePageMut::open(frame, SIZE, lease, ()).expect("a tree page");
        nodes.insert(&leaf(b"same", feature, b"")).expect("fits")
    };
    let in_first = put(&mut first, shared, &[2]);
    let in_second = put(&mut second, shared, &[3]);
    assert_eq!(in_first, in_second);
    let wrong = NodePage::open(&second, shared)
        .expect("a tree page")
        .resolve(in_first)
        .expect("one handle fits both copies");
    assert_eq!(wrong.feature(), [3]);

    let (one, other) = (Lease::take(), Lease::take());
    let in_first = put(&mut first, one, &[4]);
    let in_second = put(&mut second, other, &[5]);
    assert_eq!(in_first.address(), in_second.address());
    let first = NodePage::open(&first, one).expect("a tree page");
    let second = NodePage::open(&second, other).expect("a tree page");
    assert_eq!(
        first.resolve(in_first).map(|node| node.feature()),
        Some(&[4][..])
    );
    assert_eq!(
        second.resolve(in_second).map(|node| node.feature()),
        Some(&[5][..])
    );
    assert_eq!(
        (first.resolve(in_second), second.resolve(in_first)),
        (None, None)
    );

    // The page the copies were made from is before both of them: a handle
    // into it is good in a copy, and a handle into a copy finds nothing in
    // it.
    let ancestor = NodePage::open(&committed, shared).expect("a tree page");
    let copy = NodePage::open(first.tree_page().as_bytes(), shared).expect("a tree page");
    assert_eq!(
        copy.resolve(before).map(|node| node.key()),
        Some(&b"before"[..])
    );
    assert_eq!(
        ancestor.resolve(copy.node(slot(2)).expect("a node").handle()),
        None
    );
}
