use std::error::Error;

use lignin_format::{
    flat_page::{FlatLeafBuilder, FlatValue},
    tree_page::{HashField, Side, TreePageError},
    value_log::{PendingValue, ValuePointer},
    FileId, FormatError,
};

use super::*;

#[test]
fn a_view_gives_every_part_of_its_node() {
    let lease = Lease::take();
    let pointer = ValuePointer::new(FileId::new(12).expect("a file"), 4_096, 70_000)
        .expect("a value pointer");
    let pending = PendingValue::new([9; 8], 513).expect("a pending value");
    let hosted = [Some(address(40, 2)), None, Some(address(41, 1))];
    let (left_hash, right_hash) = ([0xA1; 32], [0xA2; 32]);
    let inner = CellContent {
        key: b"inner key",
        feature: &[4, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0],
        value_hash: &[0x51; 32],
        kv_hash: &[0x52; 32],
        left: Some(link(address(7, 2), &left_hash, &[4, 9, 9])),
        right: Some(link(address(30, 17), &right_hash, &[4, 8])),
        hosted_roots: &hosted,
        value: CellValue::OutOfLine(pointer),
        fragment_root: true,
    };
    let left = CellContent {
        fragment_root: false,
        value: CellValue::Pending(pending),
        ..leaf(b"left", &[0], b"")
    };
    let empty = CellContent {
        fragment_root: false,
        ..leaf(b"", &[], b"")
    };

    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, page(7), lease, ()).expect("a page");
    let handles = [&inner, &left, &empty].map(|node| nodes.insert(node).expect("fits"));
    let nodes = NodePage::open(&frame, lease).expect("a tree page");
    assert_eq!((nodes.id(), nodes.departures()), (page(7), 0));
    assert_eq!((nodes.len(), nodes.is_empty()), (3, false));

    let node = nodes.node(slot(1)).expect("a node");
    assert_eq!(node.handle(), handles[0]);
    assert_eq!(node.address(), address(7, 1));
    assert_eq!(node.key(), b"inner key");
    assert_eq!(node.feature(), inner.feature);
    assert_eq!(node.value(), CellValue::OutOfLine(pointer));
    assert_eq!(node.value_len(), 70_000);
    assert_eq!(
        (node.value_hash(), node.kv_hash()),
        (&[0x51; 32], &[0x52; 32])
    );
    assert_eq!(node.hash(HashField::Value), Some(&[0x51; 32]));
    assert_eq!(node.hash(HashField::KeyValue), Some(&[0x52; 32]));
    assert_eq!(node.hash(HashField::Child(Side::Left)), Some(&left_hash));
    assert_eq!(node.hash(HashField::Child(Side::Right)), Some(&right_hash));
    assert!(node.has_link(Side::Left) && node.has_link(Side::Right));
    assert_eq!(node.link(Side::Left), inner.left);
    assert_eq!(node.link(Side::Right), inner.right);
    let roots = node.hosted_roots();
    assert_eq!(roots.len(), 3);
    assert!(roots.eq(hosted));
    assert!(node.is_fragment_root());
    assert_eq!(node.cell().as_bytes(), inner.encode().expect("a cell"));

    let child = node.link(Side::Left).expect("a link").child;
    let node = nodes.node(child.slot()).expect("the child is in the page");
    assert_eq!(node.key(), b"left");
    assert_eq!(node.value(), CellValue::Pending(pending));
    assert_eq!(node.value_len(), 513);
    assert!(!node.has_link(Side::Left) && !node.has_link(Side::Right));
    assert_eq!(node.link(Side::Right), None);
    assert_eq!(node.hash(HashField::Child(Side::Left)), None);
    assert_eq!(node.hosted_roots().len(), 0);
    assert!(!node.is_fragment_root());

    let node = nodes.node(slot(3)).expect("a node");
    assert_eq!((node.key(), node.feature()), (&[][..], &[][..]));
    assert_eq!(
        (node.value(), node.value_len()),
        (CellValue::Inline(&[]), 0)
    );
    assert_eq!(nodes.node(slot(4)), None);
}

#[test]
fn the_nodes_of_a_page_come_in_slot_order() {
    let lease = Lease::take();
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, page(2), lease, ()).expect("a page");
    assert!(nodes.view().is_empty());
    assert_eq!(nodes.view().nodes().next(), None);
    for key in [b"d", b"a", b"c", b"b"] {
        nodes.insert(&leaf(key, &[], b"value")).expect("fits");
    }
    nodes
        .node_mut(slot(2))
        .expect("a node")
        .remove()
        .expect("it leaves");

    let nodes = NodePage::open(&frame, lease).expect("a tree page");
    let all = nodes.nodes();
    assert_eq!(all.size_hint(), (0, Some(4)));
    let keys: Vec<_> = all.clone().map(|node| node.key()).collect();
    assert_eq!(keys, [b"d", b"c", b"b"]);
    let slots: Vec<_> = all
        .clone()
        .rev()
        .map(|node| node.address().slot())
        .collect();
    assert_eq!(slots, [slot(4), slot(3), slot(1)]);
    assert!(all
        .clone()
        .all(|node| nodes.resolve(node.handle()) == Some(node)));

    let mut fused = all;
    assert_eq!(fused.by_ref().count(), 3);
    assert_eq!((fused.next(), fused.next_back()), (None, None));

    let image = nodes.tree_page().to_image();
    assert_eq!(nodes.tree_page().used_bytes(), image.len());
}

#[test]
fn a_frame_that_holds_no_tree_page_is_refused() {
    let lease = Lease::take();
    let refused = |frame: &[u8]| match NodePage::open(frame, lease) {
        Err(NodeError::Page(error)) => error,
        other => panic!("{other:?}"),
    };

    let zeros = frame();
    assert_eq!(refused(&zeros), FormatError::UnknownRecordKind { code: 0 });
    assert_eq!(
        refused(&zeros[..10]),
        FormatError::Truncated {
            needed: 22,
            available: 10
        }
    );

    let mut leaf = FlatLeafBuilder::new(page(3), b"", SIZE.image_capacity()).expect("a leaf");
    leaf.push(b"key", FlatValue::Inline(b"value"))
        .expect("fits");
    let flat = leaf.finish();
    assert!(matches!(
        refused(&flat),
        FormatError::UnknownRecordKind { .. }
    ));

    let mut frame = zeros;
    let error = NodePageMut::open(&mut frame, SIZE, lease, ()).expect_err("no page");
    assert!(matches!(
        error,
        NodeError::Page(FormatError::UnknownRecordKind { code: 0 })
    ));
    let error = NodePageMut::create(&mut frame[..100], SIZE, page(1), lease, ())
        .expect_err("a short frame");
    let short = FormatError::from(TreePageError::BufferTooSmall {
        len: 100,
        needed: SIZE.image_capacity(),
    });
    assert_eq!(error.to_string(), format!("tree page: {short}"));
    assert!(error.source().is_some());
}

#[test]
fn an_error_says_what_was_refused() {
    let missing = NodeError::NoNode {
        address: address(7, 3),
    };
    assert_eq!(missing.to_string(), format!("no node at {}", address(7, 3)));
    assert!(missing.source().is_none());

    let page: NodeError = FormatError::MissingId.into();
    assert_eq!(
        page.to_string(),
        format!("tree page: {}", FormatError::MissingId)
    );
    assert!(page.source().is_some());
}
