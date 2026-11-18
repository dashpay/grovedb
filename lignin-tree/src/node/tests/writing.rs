use lignin_format::{
    delta::{consolidate, Delta, DeltaRun, DeltaRunWriter},
    tree_page::{CellField, HashField, Side, TreeCell, TreePageError},
    value_log::{PendingValue, ValuePointer},
    FileId, FormatError,
};
use lignin_store::load::FRAME_FORMAT;

use super::*;

/// Writes what it is told into the run of one page.
struct Run<'o> {
    page: PageId,
    created: Option<u64>,
    writer: DeltaRunWriter<'o>,
    count: usize,
}

impl<'o> Run<'o> {
    /// A run of `page`, which it creates with a count of `created`.
    fn of(page: PageId, created: Option<u64>, out: &'o mut Vec<u8>) -> Self {
        Self {
            page,
            created,
            writer: DeltaRunWriter::new(page, created, out),
            count: 0,
        }
    }

    /// Ends the run, if anything was told, and returns how many changes
    /// were.
    fn finish(self) -> usize {
        if self.count > 0 {
            self.writer.finish().expect("a run");
        }
        self.count
    }
}

impl ChangeSink for Run<'_> {
    fn created(&mut self, page: PageId, departures: u64) {
        assert_eq!(
            (page, Some(departures), self.count),
            (self.page, self.created, 0)
        );
    }

    fn changed(&mut self, page: PageId, change: &Delta<'_>) {
        assert_eq!(page, self.page);
        self.writer
            .push(change)
            .expect("a record that a page took is one a run takes");
        self.count += 1;
    }
}

/// The image that `base` and the run in `run` give.
fn replayed(id: PageId, base: Option<&[u8]>, run: &[u8]) -> Vec<u8> {
    let run = DeltaRun::decode(run, FRAME_FORMAT).expect("a run");
    let mut out = Vec::new();
    consolidate(id, base, [&run], FRAME_FORMAT, SIZE, &mut out).expect("the run applies");
    out
}

fn pointer(offset: u64, len: u32) -> ValuePointer {
    ValuePointer::new(FileId::new(3).expect("a file"), offset, len).expect("a value pointer")
}

#[test]
fn a_new_node_takes_the_lowest_free_slot() {
    let lease = Lease::take();
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, page(9), lease, ()).expect("a page");
    assert_eq!(nodes.id(), page(9));
    let free = nodes.free_bytes();
    let handles: Vec<_> = [b"a", b"b", b"c"]
        .into_iter()
        .map(|key| nodes.insert(&leaf(key, &[], b"value")).expect("fits"))
        .collect();
    assert_eq!(
        handles.iter().map(NodeHandle::slot).collect::<Vec<_>>(),
        [slot(1), slot(2), slot(3)]
    );
    assert!(handles.iter().all(|handle| handle.departures() == 0));
    assert!(nodes.free_bytes() < free);

    nodes
        .resolve_mut(handles[1])
        .expect("good")
        .remove()
        .expect("it leaves");
    let again = nodes.insert(&leaf(b"d", &[], b"value")).expect("fits");
    assert_eq!((again.slot(), again.departures()), (slot(2), 1));
    let last = nodes.insert(&leaf(b"e", &[], b"value")).expect("fits");
    assert_eq!(last.slot(), slot(4));
}

#[test]
fn every_change_is_made_and_told() {
    let lease = Lease::take();
    let id = page(6);
    let mut frame = frame();
    let mut run = Vec::new();
    let mut sink = Run::of(id, Some(0), &mut run);
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, lease, &mut sink).expect("a page");

    let parent = nodes
        .insert(&leaf(b"parent", &[1], b"value"))
        .expect("fits");
    let child = nodes
        .insert(&CellContent {
            fragment_root: false,
            hosted_roots: &[None, None],
            ..leaf(b"child", &[1], b"first value")
        })
        .expect("fits");
    let (first, second) = ([0x31; 32], [0x32; 32]);

    let mut node = nodes.resolve_mut(parent).expect("good");
    assert_eq!(node.handle(), parent);
    node.set_link(Side::Left, Some(link(child.address(), &first, &[7])))
        .expect("a new link");
    node.set_link(Side::Right, Some(link(address(20, 4), &first, &[7])))
        .expect("a new link");
    node.set_link(Side::Right, Some(link(address(21, 5), &second, &[8])))
        .expect("a link in the room of another");
    node.set_child(Side::Right, address(22, 6))
        .expect("an address");
    node.set_hash(HashField::Value, &second).expect("a hash");
    node.set_hash(HashField::KeyValue, &first).expect("a hash");
    node.set_hash(HashField::Child(Side::Left), &second)
        .expect("a hash");
    node.set_hash(HashField::Child(Side::Right), &first)
        .expect("a hash");
    node.set_feature(&[1, 2, 3, 4, 5, 6, 7, 8, 9])
        .expect("a feature");
    node.set_fragment_root(false).expect("a mark");
    node.set_fragment_root(true).expect("a mark");
    let read = node.node();
    assert_eq!(read.feature(), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(read.link(Side::Left).expect("a link").hash, &second);
    let right = read.link(Side::Right).expect("a link");
    assert_eq!(
        (right.child, right.hash, right.aggregate),
        (address(22, 6), &first, &[8][..])
    );
    assert_eq!((read.value_hash(), read.kv_hash()), (&second, &first));
    assert!(read.is_fragment_root());

    let mut node = nodes.resolve_mut(child).expect("good");
    node.set_value(CellValue::Inline(b"other value"))
        .expect("a value of the same length");
    node.set_value(CellValue::Inline(&[0x77; 400]))
        .expect("a longer value");
    let pending = PendingValue::new([4; 8], 9_000).expect("a pending value");
    node.set_value(CellValue::Pending(pending))
        .expect("a pending value");
    assert_eq!(node.node().value(), CellValue::Pending(pending));
    node.set_value(CellValue::OutOfLine(pointer(4_096, 9_000)))
        .expect("the value's place");
    node.set_hosted_root(1, Some(address(30, 1)))
        .expect("a hosted root");
    node.set_hosted_roots(&[Some(address(31, 2))])
        .expect("fewer hosted roots");
    let read = node.node();
    assert_eq!(read.value(), CellValue::OutOfLine(pointer(4_096, 9_000)));
    assert!(read.hosted_roots().eq([Some(address(31, 2))]));
    let other = leaf(b"child", &[9], b"another cell of the node")
        .encode()
        .expect("a cell");
    node.replace(TreeCell::decode(&other, FRAME_FORMAT).expect("a cell"))
        .expect("a cell of the same key");
    assert_eq!(node.handle(), child);
    assert_eq!(node.node().feature(), [9]);

    let mut node = nodes.resolve_mut(parent).expect("good");
    node.set_link(Side::Left, None).expect("no link");
    assert!(!node.node().has_link(Side::Left));
    nodes
        .resolve_mut(child)
        .expect("good")
        .remove()
        .expect("it leaves");
    let carried = leaf(b"carried", &[], b"from another page")
        .encode()
        .expect("a cell");
    let carried = nodes
        .insert_cell(TreeCell::decode(&carried, FRAME_FORMAT).expect("a cell"))
        .expect("fits");
    assert_eq!((carried.slot(), carried.departures()), (child.slot(), 1));

    assert_eq!(sink.finish(), 23);
    assert_eq!(replayed(id, None, &run), image(&frame));

    let base = image(&frame);
    let mut run = Vec::new();
    let mut sink = Run::of(id, None, &mut run);
    let mut nodes = NodePageMut::open(&mut frame, SIZE, lease, &mut sink).expect("a tree page");
    nodes
        .node_mut(slot(1))
        .expect("a node")
        .set_value(CellValue::Inline(b""))
        .expect("an empty value");
    nodes.insert(&leaf(b"new", &[], b"")).expect("fits");
    assert_eq!(sink.finish(), 2);
    assert_eq!(replayed(id, Some(&base), &run), image(&frame));
}

/// Whatever is done to a page, the changes that were told bring the page it
/// was to the page it is.
#[test]
fn the_changes_that_are_told_rebuild_the_page() {
    let lease = Lease::take();
    let id = page(11);
    let mut rng = Rng(0xD317A);
    let mut frame = frame();
    NodePageMut::create(&mut frame, SIZE, id, lease, ()).expect("a page");
    let (mut told, mut refused) = (0, 0);

    for round in 0..300 {
        let base = image(&frame);
        let before = frame.clone();
        let mut run = Vec::new();
        let mut sink = Run::of(id, None, &mut run);
        let mut nodes = NodePageMut::open(&mut frame, SIZE, lease, &mut sink).expect("a tree page");
        for _ in 0..rng.below(12) {
            let occupied: Vec<_> = nodes.view().nodes().map(|node| node.handle()).collect();
            let picked = (!occupied.is_empty()).then(|| occupied[rng.below(occupied.len())]);
            let hash = [rng.next() as u8; 32];
            let bytes = vec![rng.next() as u8; rng.below(700)];
            let side = [Side::Left, Side::Right][rng.below(2)];
            let result = match (rng.below(10), picked) {
                (0..=2, _) | (_, None) => {
                    let key = [b'k', rng.below(4) as u8];
                    nodes
                        .insert(&leaf(&key, &bytes[..bytes.len() % 9], &bytes))
                        .map(drop)
                }
                (roll, Some(handle)) => {
                    let mut node = nodes.resolve_mut(handle).expect("no cell has left");
                    match roll {
                        3 => node.remove(),
                        4 => node.set_value(CellValue::Inline(&bytes)),
                        5 => node.set_link(
                            side,
                            Some(link(address(50, 1), &hash, &bytes[..bytes.len() % 26])),
                        ),
                        6 => node.set_link(side, None),
                        7 => node.set_hash(HashField::Child(side), &hash),
                        8 => node.set_feature(&bytes[..bytes.len() % 26]),
                        _ => node.set_value(CellValue::OutOfLine(pointer(
                            4_096 + rng.next() % 1_000,
                            1 + rng.below(100_000) as u32,
                        ))),
                    }
                }
            };
            match result {
                Ok(()) => {}
                Err(NodeError::Page(FormatError::TreePage(
                    TreePageError::NoSuchLink { .. } | TreePageError::PageFull { .. },
                ))) => refused += 1,
                Err(other) => panic!("round {round}: {other}"),
            }
        }
        let count = sink.finish();
        told += count;
        if count == 0 {
            assert!(frame == before, "round {round}");
        } else {
            assert!(
                replayed(id, Some(&base), &run) == image(&frame),
                "round {round}"
            );
        }
    }
    assert!(
        told > 1_000 && refused > 50,
        "{told} told, {refused} refused"
    );
}

#[test]
fn a_change_that_is_refused_changes_nothing_and_is_not_told() {
    let lease = Lease::take();
    let id = page(4);
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, lease, ()).expect("a page");
    let handle = nodes
        .insert(&CellContent {
            hosted_roots: &[None],
            ..leaf(b"key", &[1], b"value")
        })
        .expect("fits");
    for len in [900, 100, 0] {
        while nodes
            .insert(&leaf(b"filler", &[], &[0xF1; 900][..len]))
            .is_ok()
        {}
    }
    let before = frame.clone();

    let mut run = Vec::new();
    let mut sink = Run::of(id, None, &mut run);
    let mut nodes = NodePageMut::open(&mut frame, SIZE, lease, &mut sink).expect("a tree page");
    let refused = |result: Result<(), NodeError>| match result {
        Err(NodeError::Page(FormatError::TreePage(error))) => error,
        other => panic!("{other:?}"),
    };

    let full = nodes.insert(&leaf(b"filler", &[], &[0xF1; 900])).map(drop);
    assert!(matches!(refused(full), TreePageError::PageFull { .. }));
    let large = vec![0; SIZE.max_cell_len()];
    let large = nodes.insert(&leaf(b"large", &[], &large)).map(drop);
    assert!(matches!(refused(large), TreePageError::CellTooLarge { .. }));
    let other = leaf(b"other", &[], b"").encode().expect("a cell");
    let other = TreeCell::decode(&other, FRAME_FORMAT).expect("a cell");

    let mut node = nodes.resolve_mut(handle).expect("good");
    assert_eq!(
        refused(node.set_child(Side::Left, address(2, 2))),
        TreePageError::NoSuchLink {
            slot: handle.slot(),
            side: Side::Left
        }
    );
    assert_eq!(
        refused(node.set_hash(HashField::Child(Side::Right), &[0; 32])),
        TreePageError::NoSuchLink {
            slot: handle.slot(),
            side: Side::Right
        }
    );
    assert_eq!(
        refused(node.set_hosted_root(1, None)),
        TreePageError::NoSuchHostedRoot {
            slot: handle.slot(),
            index: 1
        }
    );
    assert_eq!(
        refused(node.set_feature(&[0; 256])),
        TreePageError::FieldTooLong {
            field: CellField::Feature,
            len: 256,
            max: 255
        }
    );
    assert_eq!(
        refused(node.replace(other)),
        TreePageError::KeyChanged {
            slot: handle.slot()
        }
    );
    assert!(matches!(
        refused(node.set_value(CellValue::Inline(&[1; 2_000]))),
        TreePageError::PageFull { .. }
    ));
    let wide = link(address(2, 2), &[0; 32], &[0; 255]);
    assert!(matches!(
        refused(node.set_link(Side::Left, Some(wide))),
        TreePageError::PageFull { .. }
    ));

    assert_eq!(sink.count, 0);
    assert!(frame == before);
    assert_eq!(
        NodePage::open(&frame, lease)
            .expect("a tree page")
            .resolve(handle)
            .map(|node| node.key()),
        Some(&b"key"[..])
    );
}

#[test]
fn a_node_is_opened_for_changes_by_its_slot_or_by_its_handle() {
    let lease = Lease::take();
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, page(4), lease, ()).expect("a page");
    let first = nodes.insert(&leaf(b"first", &[], b"1")).expect("fits");
    let second = nodes.insert(&leaf(b"second", &[], b"2")).expect("fits");

    assert!(nodes.node_mut(slot(3)).is_none());
    let node = nodes.node_mut(second.slot()).expect("a node");
    assert_eq!((node.handle(), node.node().key()), (second, &b"second"[..]));
    node.remove().expect("it leaves");

    assert!(nodes.resolve_mut(second).is_none());
    assert!(nodes.resolve_mut(first).is_none());
    assert!(nodes.node_mut(second.slot()).is_none());
    let node = nodes
        .node_mut(first.slot())
        .expect("the node stays in its slot");
    let renewed = node.handle();
    assert_eq!(
        (renewed.address(), renewed.departures()),
        (first.address(), 1)
    );
    assert!(nodes.resolve_mut(renewed).is_some());
}

/// A cell is carried by putting its bytes into the page it goes to, taking
/// it out of the page it leaves, and rewriting the one address of it.
#[test]
fn a_cell_is_carried_to_another_page_as_its_bytes() {
    let lease = Lease::take();
    let (mut above, mut below, mut beside) = (frame(), frame(), frame());
    let hash = [0x44; 32];
    let mut nodes = NodePageMut::create(&mut below, SIZE, page(2), lease, ()).expect("a page");
    let child = nodes
        .insert(&CellContent {
            value: CellValue::OutOfLine(pointer(8_192, 5_000)),
            ..leaf(b"child", &[1], b"")
        })
        .expect("fits");
    let mut nodes = NodePageMut::create(&mut above, SIZE, page(1), lease, ()).expect("a page");
    let parent = nodes
        .insert(&CellContent {
            left: Some(link(child.address(), &hash, &[])),
            ..leaf(b"parent", &[1], b"value")
        })
        .expect("fits");

    let from = NodePage::open(&below, lease).expect("a tree page");
    let cell = from.resolve(child).expect("good").cell();
    let mut to = NodePageMut::create(&mut beside, SIZE, page(3), lease, ()).expect("a page");
    let carried = to.insert_cell(cell).expect("fits");
    let mut from = NodePageMut::open(&mut below, SIZE, lease, ()).expect("a tree page");
    from.resolve_mut(child)
        .expect("good")
        .remove()
        .expect("it leaves");
    let mut nodes = NodePageMut::open(&mut above, SIZE, lease, ()).expect("a tree page");
    nodes
        .resolve_mut(parent)
        .expect("no cell has left the parent's page")
        .set_child(Side::Left, carried.address())
        .expect("an address");

    let parent = NodePage::open(&above, lease)
        .expect("a tree page")
        .resolve(parent)
        .expect("good");
    let stored = parent.link(Side::Left).expect("a link").child;
    assert_eq!(stored, address(3, 1));
    let beside = NodePage::open(&beside, lease).expect("a tree page");
    let node = beside.node(stored.slot()).expect("the address is exact");
    assert_eq!(node.key(), b"child");
    assert_eq!(node.value(), CellValue::OutOfLine(pointer(8_192, 5_000)));
    let below = NodePage::open(&below, lease).expect("a tree page");
    assert!(below.is_empty() && below.resolve(child).is_none());
    assert_eq!(below.departures(), 1);
}
