//! What a write puts again so that it holds the element of a Merk
//! (D-335).
//!
//! A batch that puts nodes of a Merk and not the node of the element
//! that hosts it hands the Merk over anchored, and the tree pages
//! refuse an anchored root that grows in a page with no room for it and
//! with nothing below the root. Where batches that were settled apart
//! are applied together, each such batch puts again the node of the
//! element and the node above it in its Merk, as the transaction holds
//! them, and names the Merk in the element's node (`held_elements` in
//! `access.rs`). The write that names a Merk in its element after the
//! Merk was handed over by itself does the same
//! (`LigninNodeStore::name_hosted`).
//!
//! The tests of a batch are shown the batch as the transaction would be
//! handed it (`access::shown_in_a_group`): a parent Merk of elements,
//! two of which host Merks, and batches that write those Merks.

use std::sync::atomic::{AtomicU32, Ordering};

use lignin_format::tree_page::Side;
use lignin_tree::{hash::Stale, node::NodeHandle};
use lignindb::txn::{
    Batch, Hosted, NodeView, RootPut, Scope, SubtreeView, TxnError, Unit, ValuePut,
};

use super::{
    super::{
        access::{shown_in_a_group, Reader, Reads, Walk},
        feature::encode_feature,
        LigninMove,
    },
    *,
};

/// The Merk that holds the elements.
const PARENT: LigninSubtree = LigninSubtree::alone([1; 32]);

/// The Merks that elements of the parent host.
const FIRST: Prefix = [221; 32];
const SECOND: Prefix = [222; 32];
const THIRD: Prefix = [223; 32];

/// The key of the element that hosts the first Merk and the third, and
/// the key of the element that hosts the second, whose node is the one
/// above the first element's.
const HOST: u64 = 43;
const OTHER: u64 = 44;

fn element(key: u64, hosted: &[Prefix]) -> LigninHost {
    LigninHost {
        subtree: PARENT.prefix,
        key: be(key),
        hosted: hosted.to_vec(),
    }
}

/// The subtree `prefix` as a handle names it: under the element at
/// `key`, which hosts `first` first.
fn under(prefix: Prefix, key: u64, first: Prefix) -> LigninSubtree {
    LigninSubtree {
        prefix,
        host: Some(element(key, &[first])),
    }
}

/// The nine nodes that each hosted Merk is made with.
fn nodes() -> Vec<(Vec<u8>, Op)> {
    (0..9).map(|n| basic(n, b"below")).collect()
}

/// Forty nodes more, after which a Merk of those nine has another root.
fn more() -> Vec<(Vec<u8>, Op)> {
    (100..140).map(|n| basic(n, b"below")).collect()
}

/// The elements the parent is made with.
fn elements() -> Vec<(Vec<u8>, Op)> {
    (40..=60).map(|n| basic(n, b"element")).collect()
}

/// The parent with its elements, and nine nodes in each of the three
/// hosted Merks: the first and the third under the element [`HOST`],
/// the second under [`OTHER`]. The roots of the parent, the first, the
/// second and the third.
fn made(transaction: &WriteTxn<'_>) -> [Option<Vec<u8>>; 4] {
    let staged = LigninBatch::new();
    let upper = LigninNodeStore::deferred(transaction, PARENT, Some(&staged));
    let mut upper = open_layered(upper, None);
    apply(&mut upper, &elements());
    let mut roots = vec![upper.root_key()];
    drop(upper);
    for (prefix, key) in [(FIRST, HOST), (SECOND, OTHER), (THIRD, HOST)] {
        let first = if key == HOST { FIRST } else { SECOND };
        let lower =
            LigninNodeStore::deferred(transaction, under(prefix, key, first), Some(&staged));
        let mut lower = open_layered(lower, None);
        apply(&mut lower, &nodes());
        roots.push(lower.root_key());
    }
    let mut settled = staged
        .settle(transaction)
        .unwrap()
        .expect("a settled batch");
    settled.host(&element(HOST, &[FIRST, THIRD]));
    let enter = |prefix: Prefix, host: LigninHost| {
        let host = Some(host);
        (LigninSubtree { prefix, host }, b"tree".to_vec())
    };
    let enters = [
        enter(FIRST, element(HOST, &[FIRST, THIRD])),
        enter(THIRD, element(HOST, &[FIRST, THIRD])),
        enter(SECOND, element(OTHER, &[SECOND])),
    ];
    (LigninSettled::apply(vec![settled], transaction, &enters).unwrap()).expect("the grove");
    roots.try_into().expect("four roots")
}

/// A batch that puts forty nodes more in each of `merks`, each a hosted
/// Merk with its element's key and its root, and that applies
/// `elements` to the parent under `parent_root`, if there are any.
fn part(
    transaction: &WriteTxn<'_>,
    merks: &[(Prefix, u64, &Option<Vec<u8>>)],
    parent_root: &Option<Vec<u8>>,
    elements: &[(Vec<u8>, Op)],
) -> LigninSettled {
    let staged = LigninBatch::new();
    for (prefix, key, root) in merks {
        let first = if *key == HOST { FIRST } else { SECOND };
        let lower =
            LigninNodeStore::deferred(transaction, under(*prefix, *key, first), Some(&staged));
        apply(&mut open_layered(lower, (*root).clone()), &more());
    }
    if !elements.is_empty() {
        let upper = LigninNodeStore::deferred(transaction, PARENT, Some(&staged));
        apply(&mut open_layered(upper, parent_root.clone()), elements);
    }
    staged
        .settle(transaction)
        .unwrap()
        .expect("a settled batch")
}

/// A batch that writes a record of the parent and no node.
fn record(transaction: &WriteTxn<'_>) -> LigninSettled {
    let staged = LigninBatch::new();
    let handle = LigninNodeStore::deferred(transaction, PARENT, Some(&staged));
    (handle.put_meta_record(b"k", b"v").unwrap()).expect("a staged record");
    staged
        .settle(transaction)
        .unwrap()
        .expect("a settled batch")
}

/// A node as a batch states it: its key, its two hashes, its feature,
/// and the key and the hash of each link.
type Stated = (
    Vec<u8>,
    CryptoHash,
    CryptoHash,
    Vec<u8>,
    [Option<(Vec<u8>, CryptoHash)>; 2],
);

/// What a transaction is handed of one Merk of a batch.
#[derive(Debug, PartialEq)]
struct Handed {
    nodes: Vec<Stated>,
    /// The keys of the nodes that keep their value by a read and state
    /// no stale hash.
    kept: Vec<Vec<u8>>,
    /// The nodes that name the Merks they host, with those Merks.
    names: Vec<(Vec<u8>, Vec<Prefix>)>,
    anchored: bool,
    /// Whether the batch keeps the root, removes no node, and is no
    /// load.
    root_kept: bool,
}

fn handed(batch: &Batch<'_>, of: Prefix) -> Option<Handed> {
    let merk = (batch.merks.iter()).find(|merk| merk.subtree == Subtree::new(of))?;
    let link =
        |link: Option<lignindb::txn::LinkPut<'_>>| link.map(|link| (link.key.to_vec(), *link.hash));
    let stated = |node: &lignindb::txn::NodePut<'_>| {
        let links = [link(node.left), link(node.right)];
        let feature = node.feature.to_vec();
        (
            node.key.to_vec(),
            *node.kv_hash,
            *node.value_hash,
            feature,
            links,
        )
    };
    let kept = |node: &&lignindb::txn::NodePut<'_>| {
        node.value == ValuePut::Keep && node.at.is_some() && node.stale == Stale::NONE
    };
    let names = |node: &lignindb::txn::NodePut<'_>| match node.hosted {
        Hosted::Set(hosted) => {
            let hosted = hosted.iter().map(|hosted| *hosted.as_bytes()).collect();
            Some((node.key.to_vec(), hosted))
        }
        Hosted::Keep => None,
    };
    Some(Handed {
        nodes: merk.nodes.iter().map(stated).collect(),
        kept: (merk.nodes.iter().filter(kept))
            .map(|node| node.key.to_vec())
            .collect(),
        names: merk.nodes.iter().filter_map(names).collect(),
        anchored: merk.anchored,
        root_kept: merk.root == RootPut::Keep && merk.removed.is_empty() && !merk.whole,
    })
}

/// What `part` hands a transaction of the Merk `of`, as one batch of a
/// group of several where `group` says so, and as a batch that is
/// applied alone otherwise.
fn shown(
    transaction: &WriteTxn<'_>,
    part: &LigninSettled,
    group: bool,
    of: Prefix,
) -> Option<Handed> {
    let mut seen = None;
    match group {
        true => {
            let run = &mut |batch: &Batch<'_>| {
                seen = Some(handed(batch, of));
                Written::default()
            };
            shown_in_a_group(transaction, part, run).expect("the batch is shown");
        }
        false => {
            let run = &mut |batch: &Batch<'_>| {
                seen = Some(handed(batch, of));
                Ok(Written::default())
            };
            BatchPart::with_batch(part, run).expect("the batch is shown");
        }
    }
    seen.expect("the batch was shown")
}

/// How many places `part` is reported for each of its Merks where every
/// node of the batch it hands over is given one: in a group where
/// `group` says so, and alone otherwise.
fn reported(transaction: &WriteTxn<'_>, part: &LigninSettled, group: bool) -> Vec<usize> {
    let placed = |batch: &Batch<'_>| Written {
        locations: (batch.merks.iter())
            .map(|merk| vec![None; merk.nodes.len()])
            .collect(),
        ..Written::default()
    };
    let written = match group {
        true => shown_in_a_group(transaction, part, &mut |batch: &Batch<'_>| placed(batch)),
        false => BatchPart::with_batch(part, &mut |batch: &Batch<'_>| Ok(placed(batch))),
    };
    let written = written.expect("the batch is shown");
    written.locations.iter().map(Vec::len).collect()
}

/// The nodes on the way from `root` of the parent to the element at
/// `to`, as the transaction holds them, each as a write that puts it
/// again as it is states it.
fn way(transaction: &WriteTxn<'_>, root: &Option<Vec<u8>>, to: u64) -> Vec<Stated> {
    let reader = LigninNodeStore::deferred(transaction, PARENT, None);
    let mut way = Vec::new();
    let mut at = root.clone().expect("a root");
    loop {
        let node = reader.get_node(&at, NO_VALUE_DEFINED_COST, version());
        let node = node.unwrap().expect("a read").expect("a node");
        let link = |left: bool| {
            let link = node.link(left);
            link.map(|link| (link.key().to_vec(), *link.hash()))
        };
        let feature = encode_feature(&node.feature_type()).as_slice().to_vec();
        let links = [link(true), link(false)];
        way.push((
            at.clone(),
            *node.kv_hash(),
            *node.value_hash(),
            feature,
            links.clone(),
        ));
        let [left, right] = links;
        let next = match be(to).cmp(&at) {
            std::cmp::Ordering::Equal => return way,
            std::cmp::Ordering::Less => left,
            std::cmp::Ordering::Greater => right,
        };
        at = next.expect("a link towards the element").0;
    }
}

/// The node above the element at `to` and the node of the element, as
/// [`way`] gives them.
fn held_for(transaction: &WriteTxn<'_>, root: &Option<Vec<u8>>, to: u64) -> Vec<Stated> {
    let way = way(transaction, root, to);
    assert!(way.len() > 2, "the element is two links below the root");
    way[way.len() - 2..].to_vec()
}

fn keys(nodes: &[Stated]) -> Vec<Vec<u8>> {
    nodes.iter().map(|node| node.0.clone()).collect()
}

/// A batch that writes one of two Merks of an element, and nothing of
/// the parent: alone it is handed over as it is, with the Merk
/// anchored. In a group it puts again the node of the element and the
/// node above it, each as the transaction holds it, with its value kept
/// by a read, and the element's node names the Merks the batch says it
/// hosts. What is reported to the batch is of its own nodes. The group
/// is applied with a batch that writes neither node, and committed: the
/// store is sound at its close, which reads the pointer of the element
/// against the root of its Merk.
#[test]
fn a_batch_of_a_group_holds_the_element_of_a_merk_it_writes() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, ..] = made(&transaction);
    let expected = held_for(&transaction, &parent_root, HOST);
    assert_eq!(keys(&expected), [be(OTHER), be(HOST)]);

    let mut writing = part(&transaction, &[(FIRST, HOST, &first_root)], &None, &[]);
    writing.host(&element(HOST, &[FIRST, THIRD]));
    let alone = shown(&transaction, &writing, false, FIRST).expect("the Merk");
    assert!(alone.anchored);
    assert_eq!(shown(&transaction, &writing, false, PARENT.prefix), None);

    let held = shown(&transaction, &writing, true, PARENT.prefix).expect("the parent");
    assert_eq!(held.nodes, expected);
    assert_eq!(held.kept, keys(&expected));
    assert_eq!(held.names, [(be(HOST), vec![FIRST, THIRD])]);
    assert!(!held.anchored && held.root_kept, "{held:?}");
    let own = reported(&transaction, &writing, false);
    assert_eq!(own.len(), 1);
    assert_eq!(reported(&transaction, &writing, true), own);
    let named = shown(&transaction, &writing, true, FIRST).expect("the Merk");
    assert_eq!(
        named,
        Handed {
            anchored: false,
            ..alone
        }
    );

    let second = record(&transaction);
    let applied = LigninSettled::apply(vec![writing, second], &transaction, &[]);
    applied.unwrap().expect("the group is applied");

    let reference = MemNodeStore::new();
    let mut grown = open_layered(reference.clone(), None);
    apply(&mut grown, &nodes());
    apply(&mut grown, &more());
    let root = grown.root_key();
    assert_ne!(root, first_root, "the Merk has another root");
    let reader = LigninNodeStore::deferred(&transaction, under(FIRST, HOST, FIRST), None);
    let expected = records(&reference, root.clone());
    assert_same_records(&records(&reader, root), &expected, "the hosted Merk");
    let upper = LigninNodeStore::deferred(&transaction, PARENT, None);
    let host = upper.get_node(&be(HOST), NO_VALUE_DEFINED_COST, version());
    let host = host.unwrap().expect("a read").expect("the element");
    assert_eq!(host.value_as_slice(), b"element");
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}

/// The Merks of two elements, one of whose nodes is the node above the
/// other's, written by one batch of a group: that node is put again
/// once, as the node above the one element and as the node of the
/// other, each element names its Merks, and none of the three Merks is
/// anchored.
#[test]
fn a_batch_of_a_group_puts_a_node_of_two_elements_again_once() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, second_root, third_root] = made(&transaction);
    let merks = [
        (FIRST, HOST, &first_root),
        (SECOND, OTHER, &second_root),
        (THIRD, HOST, &third_root),
    ];
    let mut writing = part(&transaction, &merks, &None, &[]);
    writing.host(&element(HOST, &[FIRST, THIRD]));

    let mut expected = held_for(&transaction, &parent_root, HOST);
    for node in held_for(&transaction, &parent_root, OTHER) {
        if !expected.contains(&node) {
            expected.push(node);
        }
    }
    assert_eq!(expected.len(), 3);
    let held = shown(&transaction, &writing, true, PARENT.prefix).expect("the parent");
    let sorted = |mut nodes: Vec<Stated>| {
        nodes.sort();
        nodes
    };
    assert_eq!(sorted(held.nodes.clone()), sorted(expected));
    assert_eq!(held.kept, keys(&held.nodes));
    let mut names = held.names.clone();
    names.sort();
    assert_eq!(
        names,
        [(be(HOST), vec![FIRST, THIRD]), (be(OTHER), vec![SECOND])]
    );
    for prefix in [FIRST, SECOND, THIRD] {
        let merk = shown(&transaction, &writing, true, prefix).expect("the Merk");
        assert!(!merk.anchored, "{prefix:?}");
    }

    let second = record(&transaction);
    let applied = LigninSettled::apply(vec![writing, second], &transaction, &[]);
    applied.unwrap().expect("the group is applied");
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}

/// A batch that also writes the parent: the node above the element is
/// the one the batch leaves there, and where the batch puts it, it is
/// not put again. A batch that puts the node above the element puts the
/// element's node alone again; the group is applied and committed. So
/// does a batch that removes the node above the element and puts the
/// node above that one with the element on its side, as no commit of
/// Merk does without putting the element: the node that the
/// transaction still holds above the element is none of the write.
#[test]
fn a_batch_of_a_group_puts_again_what_it_does_not_put_above_the_element_as_it_leaves_it() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, ..] = made(&transaction);
    let merk = [(FIRST, HOST, &first_root)];
    // The keys of the nodes the batch puts in the parent itself, with
    // the element's node alone put again after them.
    let host_alone = |writing: &LigninSettled, what: &str| {
        let own = shown(&transaction, writing, false, PARENT.prefix).expect("the parent");
        assert!(own.names.is_empty(), "{what}: {own:?}");
        let held = shown(&transaction, writing, true, PARENT.prefix).expect("the parent");
        assert_eq!(held.nodes[..own.nodes.len()], own.nodes[..], "{what}");
        assert_eq!(keys(&held.nodes[own.nodes.len()..]), [be(HOST)], "{what}");
        assert!(held.kept.contains(&be(HOST)), "{what}");
        assert_eq!(held.names, [(be(HOST), vec![FIRST, THIRD])], "{what}");
        let places = reported(&transaction, writing, false);
        assert_eq!(places.len(), 2, "{what}");
        assert_eq!(reported(&transaction, writing, true), places, "{what}");
        assert!(!shown(&transaction, writing, true, FIRST).unwrap().anchored);
        keys(&own.nodes)
    };

    // The node two above the element, with the element in the place of
    // the node above it, which is removed.
    let staged = LigninBatch::new();
    let lower = LigninNodeStore::deferred(&transaction, under(FIRST, HOST, FIRST), Some(&staged));
    apply(&mut open_layered(lower, first_root.clone()), &more());
    let upper = LigninNodeStore::deferred(&transaction, PARENT, Some(&staged));
    let way = keys(&way(&transaction, &parent_root, HOST));
    let two_above = u64::from_be_bytes(way[way.len() - 3].clone().try_into().expect("a key"));
    let mut turned = node_of(&upper, two_above);
    let above = node_of(&upper, OTHER);
    let to_host = |link: &Option<Link>| link.as_ref().is_some_and(|link| link.key() == be(HOST));
    assert!(to_host(&above.inner.left) != to_host(&above.inner.right));
    let link = match to_host(&above.inner.left) {
        true => above.inner.left.clone(),
        false => above.inner.right.clone(),
    };
    match turned.inner.left.as_ref().map(Link::key) == Some(&be(OTHER)[..]) {
        true => turned.inner.left = link,
        false => turned.inner.right = link,
    }
    let mut change_set = upper.change_set();
    let prepared = change_set.prepare_node(&turned, NodeChange::StructureChanged);
    change_set.put_node(turned.key(), prepared, None).unwrap();
    change_set.delete_node(&be(OTHER), None);
    (upper.commit(change_set).unwrap()).expect("the change set is staged");
    drop(upper);
    let mut turning = staged
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    turning.host(&element(HOST, &[FIRST, THIRD]));
    assert_eq!(
        host_alone(&turning, "the node above is removed"),
        [be(two_above)]
    );

    let puts = [basic(OTHER, b"again")];
    let mut putting = part(&transaction, &merk, &parent_root, &puts);
    putting.host(&element(HOST, &[FIRST, THIRD]));
    let put = host_alone(&putting, "the node above is put");
    assert!(put.contains(&be(OTHER)), "{put:?}");
    let second = record(&transaction);
    let applied = LigninSettled::apply(vec![putting, second], &transaction, &[]);
    applied.unwrap().expect("the group is applied");
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}

/// Where an element is not held, the batch is handed over as it is
/// alone, with the Merk anchored: where it removes the node of the
/// element that it is said to hang under, where whoever applies it
/// drops the parent with it, where the
/// element it is said to hang under is not the one the Merk is entered
/// under, and where the element that its handle named is said to host
/// other Merks and not this one. A batch that removes the element's
/// node and is told nothing of it, and one that puts that node, hand
/// the Merk over as one that is not anchored, and put nothing again.
#[test]
fn a_batch_of_a_group_is_handed_over_as_it_is_where_it_holds_no_element() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, ..] = made(&transaction);
    let as_alone = |writing: &LigninSettled, what: &str| {
        for of in [PARENT.prefix, FIRST] {
            let alone = shown(&transaction, writing, false, of);
            assert_eq!(shown(&transaction, writing, true, of), alone, "{what}");
        }
        let merk = shown(&transaction, writing, true, FIRST).expect("the Merk");
        merk.anchored
    };
    let merk = [(FIRST, HOST, &first_root)];

    let removes = [(be(HOST), Op::Delete)];
    let mut removing = part(&transaction, &merk, &parent_root, &removes);
    assert!(!as_alone(&removing, "the element's node is removed"));
    removing.host(&element(HOST, &[FIRST, THIRD]));
    assert!(as_alone(
        &removing,
        "the node of the named element is removed"
    ));

    let mut dropping = part(&transaction, &merk, &None, &[]);
    dropping.drop_subtree(&PARENT.prefix);
    assert!(as_alone(&dropping, "the parent is dropped"));

    let mut elsewhere = part(&transaction, &merk, &None, &[]);
    elsewhere.host(&element(OTHER, &[FIRST]));
    assert!(as_alone(&elsewhere, "another element is named"));

    let mut said = part(&transaction, &merk, &None, &[]);
    said.host(&element(HOST, &[THIRD]));
    assert!(as_alone(&said, "the element is said to host another Merk"));

    let puts = [basic(HOST, b"again")];
    let putting = part(&transaction, &merk, &parent_root, &puts);
    assert!(!as_alone(&putting, "the element's node is put"));
}

/// A batch that removes every node of a Merk and puts none leaves the
/// Merk with no root. In a group the element of that Merk is held as
/// the element of a Merk that the batch puts nodes of: its node is put
/// again and names the Merk. So a batch that puts nodes of the first
/// Merk and empties the second, whose element is the node above the
/// first Merk's, puts that node again once, as an element. A node that
/// is put again only as the node above keeps the roots its cell names,
/// and the tree pages refuse a cell that keeps a root which the same
/// write removes. The group is applied and committed, and the second
/// Merk is entered with no root.
#[test]
fn a_batch_of_a_group_holds_the_element_of_a_merk_it_empties() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, second_root, _] = made(&transaction);
    let removed: Vec<_> = (0..9).map(|n| (be(n), Op::Delete)).collect();
    let emptying = |with_the_first: bool| {
        let staged = LigninBatch::new();
        if with_the_first {
            let first = under(FIRST, HOST, FIRST);
            let lower = LigninNodeStore::deferred(&transaction, first, Some(&staged));
            apply(&mut open_layered(lower, first_root.clone()), &more());
        }
        let second = under(SECOND, OTHER, SECOND);
        let lower = LigninNodeStore::deferred(&transaction, second, Some(&staged));
        apply(&mut open_layered(lower, second_root.clone()), &removed);
        let mut settled = staged
            .settle(&transaction)
            .unwrap()
            .expect("a settled batch");
        settled.host(&element(HOST, &[FIRST, THIRD]));
        settled
    };

    let alone = emptying(false);
    let merk = shown(&transaction, &alone, false, SECOND).expect("the Merk");
    assert!(merk.anchored && merk.nodes.is_empty(), "{merk:?}");
    assert!(!merk.root_kept, "{merk:?}");
    assert_eq!(shown(&transaction, &alone, false, PARENT.prefix), None);
    let held = shown(&transaction, &alone, true, PARENT.prefix).expect("the parent");
    assert_eq!(held.nodes, held_for(&transaction, &parent_root, OTHER));
    assert_eq!(held.kept, keys(&held.nodes));
    assert_eq!(held.names, [(be(OTHER), vec![SECOND])]);
    let named = shown(&transaction, &alone, true, SECOND).expect("the Merk");
    assert_eq!(
        named,
        Handed {
            anchored: false,
            ..merk
        }
    );

    let both = emptying(true);
    let mut expected = held_for(&transaction, &parent_root, HOST);
    for node in held_for(&transaction, &parent_root, OTHER) {
        if !expected.contains(&node) {
            expected.push(node);
        }
    }
    assert_eq!(expected.len(), 3);
    let held = shown(&transaction, &both, true, PARENT.prefix).expect("the parent");
    let sorted = |mut nodes: Vec<Stated>| {
        nodes.sort();
        nodes
    };
    assert_eq!(sorted(held.nodes.clone()), sorted(expected));
    let mut names = held.names.clone();
    names.sort();
    assert_eq!(
        names,
        [(be(HOST), vec![FIRST, THIRD]), (be(OTHER), vec![SECOND])]
    );
    for prefix in [FIRST, SECOND] {
        let merk = shown(&transaction, &both, true, prefix).expect("the Merk");
        assert!(!merk.anchored, "{prefix:?}");
    }

    let second = record(&transaction);
    let applied = LigninSettled::apply(vec![both, second], &transaction, &[]);
    applied.unwrap().expect("the group is applied");
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    assert_eq!(lignin.root_in_directory(SECOND), None);
    assert!(lignin.root_in_directory(FIRST).is_some());
}

/// A node that is put again only as the node above an element keeps the
/// Merks it hosts as its cell has them. Where a batch touches a subtree
/// that this node hosts, and the node is not held as an element, the
/// node is left out. An element below it whose cell names what it is
/// said to host already is held alone, and its Merk is anchored no
/// more. The node above the first Merk's element is the
/// element of the second Merk. The batch that writes the first Merk is
/// shown, as a batch of a group, with each way a subtree is touched: it
/// is dropped the second Merk with, it writes the second Merk while it
/// is told that the element of that Merk hosts nothing, it enters a
/// Merk under that element, and it is told a move that takes the second
/// Merk away, one that brings a Merk to that element, and one that takes
/// the second Merk along. An element that is said to host a Merk which
/// its cell may not name is held with the node above it, and not held
/// where that node is left out: a Merk that is entered with the batch,
/// one that was entered with no node, and one that is entered under
/// another element. These batches are shown and not applied.
#[test]
fn a_batch_of_a_group_leaves_out_the_node_above_that_hosts_what_it_touches() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, second_root, _] = made(&transaction);
    let writing = |touch: &dyn Fn(&LigninBatch)| {
        let staged = LigninBatch::new();
        let first = under(FIRST, HOST, FIRST);
        let lower = LigninNodeStore::deferred(&transaction, first, Some(&staged));
        apply(&mut open_layered(lower, first_root.clone()), &more());
        touch(&staged);
        let mut settled = staged
            .settle(&transaction)
            .unwrap()
            .expect("a settled batch");
        settled.host(&element(HOST, &[FIRST, THIRD]));
        settled
    };
    let untouched = writing(&|_| ());
    let with_above = shown(&transaction, &untouched, true, PARENT.prefix).expect("the parent");
    assert_eq!(with_above.nodes, held_for(&transaction, &parent_root, HOST));
    assert_eq!(keys(&with_above.nodes), [be(OTHER), be(HOST)]);
    let alone = |settled: &LigninSettled, what: &str| {
        let held = shown(&transaction, settled, true, PARENT.prefix).expect("the parent");
        assert_eq!(held.nodes, with_above.nodes[1..], "{what}");
        assert_eq!(held.kept, [be(HOST)], "{what}");
        assert_eq!(held.names, [(be(HOST), vec![FIRST, THIRD])], "{what}");
        assert!(!held.anchored && held.root_kept, "{what}: {held:?}");
        let merk = shown(&transaction, settled, true, FIRST).expect("the Merk");
        assert!(!merk.anchored, "{what}");
    };

    let mut dropped = writing(&|_| ());
    dropped.drop_subtree(&SECOND);
    alone(&dropped, "the Merk of the node above is dropped");

    let mut written = writing(&|staged| {
        let second = under(SECOND, OTHER, SECOND);
        let lower = LigninNodeStore::deferred(&transaction, second, Some(staged));
        apply(&mut open_layered(lower, second_root.clone()), &more());
    });
    written.host(&element(OTHER, &[]));
    alone(&written, "the Merk of the node above is written");
    let merk = shown(&transaction, &written, true, SECOND).expect("the Merk");
    assert!(merk.anchored, "{merk:?}");

    let entered = writing(&|staged| {
        let new = LigninSubtree {
            prefix: [230; 32],
            host: Some(element(OTHER, &[SECOND, [230; 32]])),
        };
        staged.enter(&new, b"tree").expect("the Merk is entered");
    });
    alone(&entered, "a Merk is entered under the node above");

    // A move from one prefix to another, of a subtree that the element
    // at one key hosts before and the element at another afterwards,
    // with the subtrees it takes along.
    type Along = Vec<(Prefix, Prefix)>;
    let moved = |from: Prefix, to: Prefix, keys: (u64, u64), below: Along| {
        let mut settled = writing(&|staged| {
            staged.move_subtree(&from, &to);
            for (from, to) in &below {
                staged.move_subtree(from, to);
            }
        });
        settled.move_as(&LigninMove {
            from,
            to,
            above: PARENT.prefix,
            old_key: be(keys.0),
            new_key: be(keys.1),
            position: 0,
            below: below.clone(),
        });
        settled
    };
    let away = moved(SECOND, [231; 32], (OTHER, 61), Vec::new());
    alone(&away, "the Merk of the node above is moved away");
    let brought = moved([232; 32], [233; 32], (61, OTHER), Vec::new());
    alone(&brought, "a Merk is moved to the node above");
    let along = moved([232; 32], [233; 32], (61, 62), vec![(SECOND, [234; 32])]);
    alone(&along, "the Merk of the node above is taken along");

    // What the batch of the first Merk hands over where its element is
    // said to host `list`: with the node above it, and nothing of the
    // parent where that node is left out.
    let said_to_host = |list: &[Prefix], touch: &dyn Fn(&LigninBatch), what: &str| {
        let mut naming = writing(touch);
        naming.host(&element(HOST, list));
        let held = shown(&transaction, &naming, true, PARENT.prefix).expect("the parent");
        assert_eq!(held.nodes, with_above.nodes, "{what}");
        assert_eq!(held.names, [(be(HOST), list.to_vec())], "{what}");
        naming.drop_subtree(&SECOND);
        for of in [PARENT.prefix, FIRST] {
            let as_alone = shown(&transaction, &naming, false, of);
            assert_eq!(shown(&transaction, &naming, true, of), as_alone, "{what}");
        }
        assert_eq!(
            shown(&transaction, &naming, true, PARENT.prefix),
            None,
            "{what}"
        );
        let merk = shown(&transaction, &naming, true, FIRST).expect("the Merk");
        assert!(merk.anchored, "{what}: {merk:?}");
    };
    let new: Prefix = [235; 32];
    let with_the_new = [FIRST, THIRD, new];
    let enters = |staged: &LigninBatch| {
        let new = LigninSubtree {
            prefix: new,
            host: Some(element(HOST, &with_the_new)),
        };
        staged.enter(&new, b"tree").expect("the Merk is entered");
    };
    said_to_host(&with_the_new, &enters, "a Merk is entered with the batch");
    said_to_host(
        &[FIRST, SECOND],
        &|_| (),
        "a Merk is entered under another element",
    );
    // A Merk that is entered under the element, at its place, with no
    // node: the node of the element was not written with it.
    let empty: Prefix = [236; 32];
    let with_the_empty = [FIRST, THIRD, empty];
    let staged = LigninBatch::new();
    let entered = LigninSubtree {
        prefix: empty,
        host: Some(element(HOST, &with_the_empty)),
    };
    staged
        .enter(&entered, b"tree")
        .expect("the Merk is entered");
    (staged.apply(&transaction).unwrap()).expect("the batch is applied");
    said_to_host(&with_the_empty, &|_| (), "a Merk was entered with no node");
}

/// What a batch of a group touched counts for the batches after it in
/// the group. Whoever applies the group drops the second Merk, whose
/// element is the node above the first Merk's, with the batch that
/// writes the first Merk, or with a batch before that one: the
/// transaction is handed the same number of writes both ways, as the
/// node above is left out both ways. The last batch writes the element
/// of the dropped Merk as one that hosts nothing, and the group is
/// applied and committed.
#[test]
fn a_batch_of_a_group_leaves_out_the_node_above_that_hosts_what_a_batch_before_it_touched() {
    let grown = |with_an_earlier_batch: bool| {
        let lignin = Lignin::new();
        let transaction = lignin.path().begin();
        let [parent_root, first_root, ..] = made(&transaction);
        let mut writing = part(&transaction, &[(FIRST, HOST, &first_root)], &None, &[]);
        writing.host(&element(HOST, &[FIRST, THIRD]));
        let mut earlier = record(&transaction);
        match with_an_earlier_batch {
            true => earlier.drop_subtree(&SECOND),
            false => writing.drop_subtree(&SECOND),
        }
        let puts = [basic(OTHER, b"an element of no tree")];
        let mut last = part(&transaction, &[], &parent_root, &puts);
        last.host(&element(OTHER, &[]));
        let before = transaction.stats().writes;
        let group = match with_an_earlier_batch {
            true => vec![earlier, writing, last],
            false => vec![writing, earlier, last],
        };
        let applied = LigninSettled::apply(group, &transaction, &[]);
        applied.unwrap().expect("the group is applied");
        let writes = transaction.stats().writes - before;
        assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
        assert_eq!(lignin.root_in_directory(SECOND), None);
        writes
    };
    assert_eq!(grown(true), grown(false));
}

/// The reads of a transaction, of which the first `stale` reads of a
/// child by the location of its parent are answered as reads by a
/// location that a commit made stale, and every such read after those,
/// where `missing` says so, as the read of a link that leads to no
/// node.
struct Unsteady<'a, 'd> {
    transaction: &'a WriteTxn<'d>,
    stale: AtomicU32,
    missing: bool,
}

impl Reads for Unsteady<'_, '_> {
    fn node<R>(
        &self,
        subtree: Subtree,
        key: &[u8],
        hint: Option<NodeHandle>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        Reads::node(self.transaction, subtree, key, hint, read)
    }

    fn committed_node<R>(
        &self,
        subtree: Subtree,
        key: &[u8],
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        Reads::committed_node(self.transaction, subtree, key, read)
    }

    fn child<R>(
        &self,
        subtree: Subtree,
        parent: NodeHandle,
        side: Side,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<Option<R>, TxnError> {
        let stale = self.stale.load(Ordering::Relaxed);
        if stale > 0 {
            self.stale.store(stale - 1, Ordering::Relaxed);
            return Ok(None);
        }
        if self.missing {
            return Ok(Some(read(None)));
        }
        Reads::child(self.transaction, subtree, parent, side, read)
    }

    fn get(
        &self,
        unit: Unit,
        key: &[u8],
        committed: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        Reads::get(self.transaction, unit, key, committed, out)
    }

    fn subtree<R>(
        &self,
        subtree: Subtree,
        read: impl FnOnce(Option<SubtreeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        Reads::subtree(self.transaction, subtree, read)
    }
}

impl Reader for Unsteady<'_, '_> {
    type Reads = Self;

    fn reading<T>(&self, read: impl FnOnce(&Self::Reads) -> T) -> T {
        read(self)
    }

    fn writing<T>(
        &self,
        parts: usize,
        write: impl FnOnce(&Scope<'_, '_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        Reader::writing(self.transaction, parts, write)
    }

    fn walk<'a>(&'a self, subtree: Subtree) -> Result<Box<dyn Walk + 'a>, TxnError> {
        Reader::walk(self.transaction, subtree)
    }
}

/// A node and its children are read in operations of their own. Where
/// the transaction says that the location a child is read by is good no
/// more, as it does where a commit landed between the two reads, the
/// node is read again, and what the batch holds is what it holds with
/// no such commit. A node that changes under every read of it, and a
/// link that leads to no node, are errors: no element is taken as one
/// that is not held on their account.
#[test]
fn a_node_whose_location_went_stale_is_read_again_for_the_element() {
    let lignin = Lignin::new();
    let made_by = lignin.path().begin();
    let [parent_root, first_root, ..] = made(&made_by);
    assert!(matches!(lignin.commit(&made_by), Outcome::Committed(_)));
    drop(made_by);
    let transaction = lignin.path().begin();
    let mut writing = part(&transaction, &[(FIRST, HOST, &first_root)], &None, &[]);
    writing.host(&element(HOST, &[FIRST, THIRD]));
    let steady = shown(&transaction, &writing, true, PARENT.prefix).expect("the parent");
    assert_eq!(steady.nodes, held_for(&transaction, &parent_root, HOST));

    let shown_by = |stale: u32, missing: bool| {
        let reader = Unsteady {
            transaction: &transaction,
            stale: AtomicU32::new(stale),
            missing,
        };
        let mut seen = None;
        let run = &mut |batch: &Batch<'_>| {
            seen = Some(handed(batch, PARENT.prefix));
            Written::default()
        };
        let shown = shown_in_a_group(&reader, &writing, run).map(|_| ());
        (shown, seen.flatten(), reader.stale.load(Ordering::Relaxed))
    };
    for stale in [1, 2, 7] {
        let (shown, held, left) = shown_by(stale, false);
        shown.expect("the batch is shown");
        assert_eq!(left, 0, "{stale}: no child was read by a location");
        assert_eq!(held.as_ref(), Some(&steady), "{stale}");
    }
    let (refused, held, _) = shown_by(u32::MAX, false);
    let refused = refused.expect_err("every read is stale").to_string();
    assert!(refused.contains("changed under every read"), "{refused}");
    assert_eq!(held, None);
    let (refused, held, _) = shown_by(0, true);
    let refused = refused.expect_err("a link to no node").to_string();
    assert!(refused.contains("a link leads to no node"), "{refused}");
    assert_eq!(held, None);
}

/// A batch before which `before` runs when it is handed over: what
/// happens between the batch before it in its group and itself.
struct After<'a>(&'a LigninSettled, &'a dyn Fn());

impl BatchPart for After<'_> {
    fn with_batch(
        &self,
        run: &mut dyn FnMut(&Batch<'_>) -> Result<Written, Error>,
    ) -> Result<Written, Error> {
        (self.1)();
        BatchPart::with_batch(self.0, run)
    }

    fn host_of(&self, merk: Subtree) -> Option<LigninHost> {
        BatchPart::host_of(self.0, merk)
    }
}

/// A batch of a group that holds an element writes what it puts again
/// where it is applied, here the node of the element and the node above
/// it: the transaction has written them first from then on, before the
/// batches after that one are applied. A commit of another transaction
/// that writes those nodes between two batches of the group is a commit
/// after the transaction first wrote them, and the transaction
/// conflicts where it commits. The same commit before the first batch
/// is applied is one that the batch read, and the transaction commits.
#[test]
fn a_commit_between_two_batches_of_a_group_that_writes_a_held_node_is_a_conflict() {
    for between in [false, true] {
        let lignin = Lignin::new();
        let made_by = lignin.path().begin();
        let [parent_root, first_root, ..] = made(&made_by);
        assert!(matches!(lignin.commit(&made_by), Outcome::Committed(_)));
        drop(made_by);
        let transaction = lignin.path().begin();
        let mut writing = part(&transaction, &[(FIRST, HOST, &first_root)], &None, &[]);
        writing.host(&element(HOST, &[FIRST, THIRD]));
        let second = record(&transaction);
        // Another transaction writes the element's node, and with it the
        // nodes from the root to it, and commits.
        let other = || {
            let other = lignin.path().begin();
            let upper = LigninNodeStore::immediate(&other, PARENT);
            let again = [basic(HOST, b"another")];
            apply(&mut open_layered(upper, parent_root.clone()), &again);
            assert!(matches!(lignin.commit(&other), Outcome::Committed(_)));
        };
        let nothing = || ();
        let before_the_second: &dyn Fn() = match between {
            true => &other,
            false => {
                other();
                &nothing
            }
        };
        let parts: [&dyn BatchPart; 2] = [&writing, &After(&second, before_the_second)];
        let applied = Access::apply_parts(&transaction, &parts, &[], &mut None);
        applied.expect("the group is applied");
        let outcome = lignin.commit(&transaction);
        match between {
            true => assert_eq!(outcome, Outcome::Conflict),
            false => assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}"),
        }
    }
}

/// The residual of a race that is on record (`engine.md`, "An element
/// written over a tree"; D-350). Two writers can leave Merks that have
/// roots under an element whose node names none of them. The store
/// takes the cell of an element to name a Merk that is entered under it
/// at its place and has a root. So a batch of a group whose node above
/// is left out holds that element alone and names the Merks in it: the
/// cell grows by the place of each pointer, with nothing in the write
/// to carry it. Here its page has room, and the group is applied and
/// committed. In a page with no room for the pointers and with nothing
/// below the element, the tree pages refuse such a write.
///
/// `GROVE-03-F3` turns this test: with a read of how many Merks a cell
/// names, the element is not held here.
#[test]
fn a_batch_of_a_group_holds_alone_an_element_that_names_none_of_the_merks_it_hosts() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, ..] = made(&transaction);
    // The node of the element is written as one that hosts nothing, and
    // its two Merks stay entered under it with their roots.
    let again = [basic(HOST, b"element")];
    let mut naming_none = part(&transaction, &[], &parent_root, &again);
    naming_none.host(&element(HOST, &[]));
    let applied = LigninSettled::apply(vec![naming_none], &transaction, &[]);
    applied.unwrap().expect("the element is written");

    let mut writing = part(&transaction, &[(FIRST, HOST, &first_root)], &None, &[]);
    writing.host(&element(HOST, &[FIRST, THIRD]));
    writing.drop_subtree(&SECOND);
    let held = shown(&transaction, &writing, true, PARENT.prefix).expect("the parent");
    assert_eq!(keys(&held.nodes), [be(HOST)]);
    assert_eq!(held.kept, [be(HOST)]);
    assert_eq!(held.names, [(be(HOST), vec![FIRST, THIRD])]);
    let merk = shown(&transaction, &writing, true, FIRST).expect("the Merk");
    assert!(!merk.anchored, "{merk:?}");

    let puts = [basic(OTHER, b"an element of no tree")];
    let mut last = part(&transaction, &[], &parent_root, &puts);
    last.host(&element(OTHER, &[]));
    let applied = LigninSettled::apply(vec![writing, last], &transaction, &[]);
    applied.unwrap().expect("the group is applied");
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}

/// The Merk [`LOWER`] under the root of the first Merk: that root is
/// written again as an element, in one batch with nine nodes of the
/// Merk below it. The subtree as its handle names it, and its root.
const LOWER: Prefix = [224; 32];

fn below_the_root(
    transaction: &WriteTxn<'_>,
    first_root: &Option<Vec<u8>>,
) -> (LigninSubtree, Option<Vec<u8>>) {
    let root_key = first_root.clone().expect("a root");
    let lower = LigninSubtree {
        prefix: LOWER,
        host: Some(LigninHost {
            subtree: FIRST,
            key: root_key.clone(),
            hosted: vec![LOWER],
        }),
    };
    let staged = LigninBatch::new();
    staged.enter(&lower, b"tree").expect("the Merk is entered");
    let upper = LigninNodeStore::deferred(transaction, under(FIRST, HOST, FIRST), Some(&staged));
    let again = Op::Put(b"an element".to_vec(), TreeFeatureType::BasicMerkNode);
    apply(
        &mut open_layered(upper, first_root.clone()),
        &[(root_key, again)],
    );
    let below = LigninNodeStore::deferred(transaction, lower.clone(), Some(&staged));
    let mut below = open_layered(below, None);
    apply(&mut below, &nodes());
    let root = below.root_key();
    drop(below);
    (staged.apply(transaction).unwrap()).expect("the batch is applied");
    (lower, root)
}

/// An element whose node is the root of its Merk has no node above it:
/// it tops what is put again, and grows where its cell comes to name a
/// Merk. Where that Merk has a host and the batch is told of it, the
/// element above is held as well, and the Merk between the two is
/// handed over as one that is not anchored; where the batch is told of
/// none, that Merk stays anchored and nothing above it is written. A
/// root that is said to host a Merk more, which is not entered, would
/// grow by naming it: it is held where the element above it is, which
/// is looked for first, and its Merk is then not anchored; where the
/// batch is told of no element above, it is not held.
#[test]
fn a_batch_of_a_group_holds_the_element_above_an_element_that_is_a_root() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [parent_root, first_root, ..] = made(&transaction);
    let (lower, lower_root) = below_the_root(&transaction, &first_root);
    let root_key = first_root.expect("a root");

    let one_more = LigninHost {
        subtree: FIRST,
        key: root_key.clone(),
        hosted: vec![LOWER, [237; 32]],
    };
    let growing = |told_of_the_element_above: bool| {
        let staged = LigninBatch::new();
        let below = LigninNodeStore::deferred(&transaction, lower.clone(), Some(&staged));
        apply(&mut open_layered(below, lower_root.clone()), &more());
        let mut settled = staged
            .settle(&transaction)
            .unwrap()
            .expect("a settled batch");
        settled.host(&one_more);
        if told_of_the_element_above {
            settled.host(&element(HOST, &[FIRST, THIRD]));
        }
        settled
    };
    let untold = growing(false);
    for of in [PARENT.prefix, FIRST, LOWER] {
        let as_alone = shown(&transaction, &untold, false, of);
        assert_eq!(shown(&transaction, &untold, true, of), as_alone, "{of:?}");
    }
    let merk = shown(&transaction, &untold, true, LOWER).expect("the Merk");
    assert!(merk.anchored, "{merk:?}");
    let told = growing(true);
    let grown = shown(&transaction, &told, true, FIRST).expect("the Merk of the element");
    assert_eq!(keys(&grown.nodes), std::slice::from_ref(&root_key));
    assert_eq!(grown.names, [(root_key.clone(), one_more.hosted.clone())]);
    assert!(!grown.anchored && grown.root_kept, "{grown:?}");
    let above = shown(&transaction, &told, true, PARENT.prefix).expect("the parent");
    assert_eq!(above.nodes, held_for(&transaction, &parent_root, HOST));
    assert_eq!(above.names, [(be(HOST), vec![FIRST, THIRD])]);
    assert!(!shown(&transaction, &told, true, LOWER).unwrap().anchored);

    let staged = LigninBatch::new();
    let below = LigninNodeStore::deferred(&transaction, lower, Some(&staged));
    apply(&mut open_layered(below, lower_root), &more());
    let mut writing = staged
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");

    let between = shown(&transaction, &writing, true, FIRST).expect("the Merk of the element");
    assert_eq!(keys(&between.nodes), std::slice::from_ref(&root_key));
    assert_eq!(between.names, [(root_key.clone(), vec![LOWER])]);
    assert!(between.anchored && between.root_kept, "{between:?}");
    assert_eq!(shown(&transaction, &writing, true, PARENT.prefix), None);
    assert!(!shown(&transaction, &writing, true, LOWER).unwrap().anchored);

    writing.host(&element(HOST, &[FIRST, THIRD]));
    let told = shown(&transaction, &writing, true, FIRST).expect("the Merk of the element");
    assert_eq!(
        told,
        Handed {
            anchored: false,
            ..between
        }
    );
    let above = shown(&transaction, &writing, true, PARENT.prefix).expect("the parent");
    assert_eq!(above.nodes, held_for(&transaction, &parent_root, HOST));
    assert_eq!(above.names, [(be(HOST), vec![FIRST, THIRD])]);
    assert!(!above.anchored && above.root_kept, "{above:?}");

    let second = record(&transaction);
    let applied = LigninSettled::apply(vec![writing, second], &transaction, &[]);
    applied.unwrap().expect("the group is applied");
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}

/// A Merk that an immediate handle committed by itself is named in its
/// element by a write of the element's node and the node above it: two
/// writes of the transaction. For an element that is the root of its
/// Merk it is one write, and three where the element above is given:
/// that element's node and the node above it. A naming holds its
/// element whatever its cell does: a root that comes to name a Merk
/// with no node as well is named with no element above it given. The
/// transaction commits after the last naming, and the store is sound
/// at its close. A handle
/// that does not write at once, a key with no node, and an element that
/// the Merk is not entered under are refused, and write nothing.
#[test]
fn a_merk_is_named_in_its_element_by_a_write_that_holds_the_element() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let [_, first_root, _, third_root] = made(&transaction);
    let (lower, lower_root) = below_the_root(&transaction, &first_root);
    let root_key = first_root.expect("a root");
    let writes = || transaction.stats().writes;

    // Nodes of the third Merk and of the Merk below the root of the
    // first, each committed by itself.
    let alone = LigninNodeStore::immediate(&transaction, under(THIRD, HOST, FIRST));
    apply(&mut open_layered(alone, third_root), &more());
    let alone = LigninNodeStore::immediate(&transaction, lower);
    apply(&mut open_layered(alone, lower_root), &more());

    let upper = || LigninNodeStore::immediate(&transaction, PARENT);
    let middle = || LigninNodeStore::immediate(&transaction, under(FIRST, HOST, FIRST));
    let before = writes();
    upper()
        .name_hosted(&be(HOST), &[FIRST, THIRD], &[])
        .expect("the element is named");
    assert_eq!(writes() - before, 2);

    let before = writes();
    middle()
        .name_hosted(&root_key, &[LOWER], &[])
        .expect("the root is named");
    assert_eq!(writes() - before, 1);
    let before = writes();
    middle()
        .name_hosted(&root_key, &[LOWER], &[element(HOST, &[FIRST, THIRD])])
        .expect("the root is named with the element above");
    assert_eq!(writes() - before, 3);

    // A Merk with no node, entered under the root as its second Merk.
    let empty = LigninSubtree {
        prefix: [238; 32],
        host: Some(LigninHost {
            subtree: FIRST,
            key: root_key.clone(),
            hosted: vec![LOWER, [238; 32]],
        }),
    };
    let staged = LigninBatch::new();
    staged.enter(&empty, b"tree").expect("the Merk is entered");
    (staged.apply(&transaction).unwrap()).expect("the batch is applied");
    let before = writes();
    middle()
        .name_hosted(&root_key, &[LOWER, [238; 32]], &[])
        .expect("the root is named with a Merk more");
    assert_eq!(writes() - before, 1);

    let before = writes();
    let reading = LigninNodeStore::deferred(&transaction, PARENT, None);
    let refused = reading.name_hosted(&be(HOST), &[FIRST, THIRD], &[]);
    let refused = refused.expect_err("a handle that reads").to_string();
    assert!(refused.contains("does not write at once"), "{refused}");
    for (key, hosted) in [
        (61, [FIRST, THIRD]),
        (OTHER, [FIRST, THIRD]),
        (HOST, [THIRD, FIRST]),
    ] {
        let refused = upper().name_hosted(&be(key), &hosted, &[]);
        let refused = refused
            .expect_err("an element that is not held")
            .to_string();
        assert!(refused.contains("no node of its Merk"), "{key}: {refused}");
    }
    assert_eq!(writes(), before);
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}
