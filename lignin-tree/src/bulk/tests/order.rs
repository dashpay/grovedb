//! A Merk in the order of its keys.

use std::collections::BTreeMap;

use super::*;

/// A node of a stream in key order, with a child of the given heights on
/// each side that has one.
fn ordered<'a>(key: &'a [u8], left: Option<(u8, u8)>, right: Option<(u8, u8)>) -> NodeLoad<'a, ()> {
    let link = |heights| LinkLoad {
        child: (),
        heights,
        aggregate: &[][..],
    };
    NodeLoad {
        key,
        feature: &[0],
        value: b"a value",
        value_hash: None,
        left: left.map(link),
        right: right.map(link),
        hosted: &[],
    }
}

fn push(bench: &mut Bench, merk: &mut InOrder, node: &NodeLoad<'_, ()>) -> Result<(), LoadError> {
    let (mem, log, rules) = (&mut bench.mem, &mut bench.log, &mut bench.rules);
    merk.push(&mut bench.loader, mem, log, rules, node)
}

fn shape<T: std::fmt::Debug>(result: Result<T, LoadError>) -> ShapeError {
    match result {
        Err(LoadError::Shape(shape)) => shape,
        other => panic!("{other:?}"),
    }
}

/// Loads a Merk of the model of `seed`, in key order or a child before its
/// parent, and returns the pages and where the root went.
fn loaded(seed: u64, hosts: usize, in_order: bool) -> (BTreeMap<PageId, Vec<u8>>, Root) {
    let (mut bench, mut rng) = (Bench::proposed(), Rng(seed));
    let mut model = Model::new(2, hosts, 25, 40);
    let count = 1 + rng.below(900);
    let top = model.merk(&mut rng, count, 0).expect("a Merk");
    let subtree = match in_order {
        true => bench.merk_in_order(&model, top),
        false => bench.subtree(&model, top),
    };
    let root = bench.root(subtree).expect("a root");
    let report = bench.finish();
    let nodes = model.nodes.len();
    assert_eq!(bench.compare(&model, top, root.address, &report), nodes);
    bench.mem.check();
    bench.mem.check_fragment_sizes(&bench.layout);
    (bench.mem.frames, root)
}

#[test]
fn a_merk_in_key_order_gives_the_pages_of_one_given_bottom_up() {
    for seed in 0..40 {
        assert!(
            loaded(seed, 0, true) == loaded(seed, 0, false),
            "seed {seed}"
        );
    }
}

#[test]
fn a_grove_in_key_order_is_loaded_as_it_was_given() {
    for seed in 0..40 {
        loaded(seed, 8, true);
    }
}

#[test]
fn a_merk_with_no_node_has_no_root() {
    let mut bench = Bench::proposed();
    let root = InOrder::new().finish(&mut bench.loader);
    assert_eq!(root.expect("a Merk"), None);
    assert_eq!(bench.finish(), LoadReport::default());
}

#[test]
fn two_shapes_of_the_same_keys_are_told_apart_by_their_heights() {
    // a, b, c with b at the top; and a at the top, c below it, b below c.
    type Sides = (Option<(u8, u8)>, Option<(u8, u8)>);
    let shapes: [[Sides; 3]; 2] = [
        [(None, None), (Some((0, 0)), Some((0, 0))), (None, None)],
        [(None, Some((1, 0))), (None, None), (Some((0, 0)), None)],
    ];
    let tops = shapes.map(|shape| {
        let (mut bench, mut merk) = (Bench::proposed(), InOrder::new());
        for (key, (left, right)) in [b"a", b"b", b"c"].into_iter().zip(shape) {
            push(&mut bench, &mut merk, &ordered(key, left, right)).expect("a node");
        }
        let root = merk.finish(&mut bench.loader).expect("a Merk");
        let root = bench.root(root.expect("a root")).expect("a place");
        bench.mem.check();
        let top = bench.mem.node(root.address);
        let child = |side| {
            top.link(side)
                .map(|link| bench.mem.node(link.child).key().to_vec())
        };
        (top.key().to_vec(), child(Side::Left), child(Side::Right))
    });
    let key = |key: &[u8]| Some(key.to_vec());
    assert_eq!(tops[0], (b"b".to_vec(), key(b"a"), key(b"c")));
    assert_eq!(tops[1], (b"a".to_vec(), None, key(b"c")));
}

#[test]
fn a_left_child_that_was_not_given_is_refused() {
    let (mut bench, mut merk) = (Bench::proposed(), InOrder::new());
    let refused = push(&mut bench, &mut merk, &ordered(b"a", Some((0, 0)), None));
    assert_eq!(shape(refused), ShapeError::NoLeft);
    assert!(matches!(bench.leaf(b"b"), Err(LoadError::Failed)));
    let again = push(&mut bench, &mut merk, &ordered(b"a", None, None));
    assert!(matches!(again, Err(LoadError::Failed)));
    assert!(matches!(
        merk.finish(&mut bench.loader),
        Err(LoadError::Failed)
    ));
}

#[test]
fn a_left_child_of_other_heights_is_refused() {
    let (mut bench, mut merk) = (Bench::proposed(), InOrder::new());
    push(&mut bench, &mut merk, &ordered(b"a", None, None)).expect("a leaf");
    let refused = push(&mut bench, &mut merk, &ordered(b"b", Some((1, 0)), None));
    let heights = ShapeError::LeftHeights {
        stated: (1, 0),
        found: (0, 0),
    };
    assert_eq!(shape(refused), heights);
}

#[test]
fn a_subtree_with_no_parent_is_refused() {
    let (mut bench, mut merk) = (Bench::proposed(), InOrder::new());
    push(&mut bench, &mut merk, &ordered(b"a", None, None)).expect("a leaf");
    let refused = push(&mut bench, &mut merk, &ordered(b"b", None, None));
    assert_eq!(shape(refused), ShapeError::NoParent);
}

#[test]
fn a_right_child_that_was_not_given_is_refused() {
    let (mut bench, mut merk) = (Bench::proposed(), InOrder::new());
    push(&mut bench, &mut merk, &ordered(b"a", None, Some((0, 1)))).expect("a node");
    push(&mut bench, &mut merk, &ordered(b"b", None, Some((0, 0)))).expect("a node");
    let refused = merk.finish(&mut bench.loader);
    assert_eq!(shape(refused), ShapeError::NoRight { waiting: 2 });
    assert!(matches!(bench.leaf(b"c"), Err(LoadError::Failed)));
}

#[test]
fn a_node_that_is_refused_ends_the_merk() {
    let (mut bench, mut merk) = (Bench::proposed(), InOrder::new());
    bench.rules.refuse = Some((Rule::Kv, b"a".to_vec()));
    push(&mut bench, &mut merk, &ordered(b"a", None, Some((0, 0)))).expect("it waits");
    let refused = push(&mut bench, &mut merk, &ordered(b"b", None, None));
    assert!(
        matches!(refused, Err(LoadError::Rule(_))),
        "its parent is refused"
    );
    assert!(matches!(bench.leaf(b"c"), Err(LoadError::Failed)));
}
