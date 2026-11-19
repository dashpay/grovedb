//! What stops an evaluation, and what it leaves behind.

use std::error::Error;

use lignin_format::tree_page::{CellContent, TreeLink};

use super::{
    acceptance::{key, value},
    grove::Grove,
    *,
};

const LEFT: HashField = HashField::Child(Side::Left);
const RIGHT: HashField = HashField::Child(Side::Right);

/// A small grove of two Merks with one value changed and not evaluated: the
/// Merk that changed, and the key.
fn marked() -> (Grove, usize, Vec<u8>) {
    let mut grove = Grove::new(false);
    for number in 0..12 {
        grove.put(0, &key(0, number), &value(number, 30));
    }
    let hosted = grove.host(0, &key(0, 4));
    for number in 0..12 {
        grove.put(hosted, &key(hosted, number), &value(number, 30));
    }
    grove.lazy = true;
    let changed = key(hosted, 9);
    grove.put(hosted, &changed, &value(99, 30));
    assert!(grove.hashes.len() > 4);
    (grove, hosted, changed)
}

fn root_hash(grove: &mut Grove) -> Result<Hash, HashError> {
    let root = grove.root_at.expect("a root");
    let (mem, log, rules) = (&mut grove.mem, &mut grove.log, &mut grove.rules);
    grove.hashes.hash_of(mem, log, rules, root)
}

/// After an error every stale hash is still marked, and the evaluation can
/// be asked for again.
fn recovers(grove: &mut Grove) {
    grove.check_marks();
    assert_eq!(grove.root_hash(), grove.expected_root());
    assert!(grove.hashes.is_empty());
    grove.check_hashes();
}

fn leaf<'a>(key: &'a [u8], value: &'a [u8]) -> CellContent<'a> {
    CellContent {
        key,
        feature: &[0],
        value_hash: &[0; 32],
        kv_hash: &[0; 32],
        left: None,
        right: None,
        hosted_roots: &[],
        value: CellValue::Inline(value),
        fragment_root: true,
    }
}

fn link(child: CellAddress) -> TreeLink<'static> {
    TreeLink {
        child,
        hash: &[0; 32],
        heights: (0, 0),
        child_key_len: 1,
        aggregate: &[0],
    }
}

/// One page in memory with the cells `cells`, in the slots 1, 2 and so on.
fn page_of(cells: &[CellContent<'_>]) -> (Mem, PageId) {
    let mut mem = Mem::new(SIZE);
    let id = mem.create().expect("a page");
    for (index, cell) in cells.iter().enumerate() {
        let handle = mem.write(id, |page| page.insert(cell));
        let at = handle.expect("the cell fits").address();
        assert_eq!(at, CellAddress::new(id, slot(index as u16 + 1)));
    }
    (mem, id)
}

fn evaluate(mem: &mut Mem, hashes: &mut Hashes, at: CellAddress) -> Result<(), HashError> {
    hashes.evaluate(mem, &mut Log::default(), &mut Plain, at)
}

/// Pages that are changed by someone else before one of their steps.
struct Meddled<'m, F> {
    mem: &'m mut Mem,
    before: usize,
    meddle: Option<F>,
}

impl<F: FnOnce(&mut Mem)> Meddled<'_, F> {
    fn step(&mut self) {
        match self.before.checked_sub(1) {
            Some(left) => self.before = left,
            None => {
                if let Some(meddle) = self.meddle.take() {
                    meddle(self.mem);
                }
            }
        }
    }
}

impl<F: FnOnce(&mut Mem)> Pages for Meddled<'_, F> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        self.step();
        self.mem.read(page, read)
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        self.step();
        self.mem.write(page, change)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        self.mem.create()
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        self.mem.free(page)
    }
}

#[test]
fn a_step_that_fails_loses_no_mark() {
    let steps = {
        let (mut grove, ..) = marked();
        let before = grove.mem.steps.len();
        grove.root_hash();
        grove.mem.steps.len() - before
    };
    assert!(steps > 8);
    for allowed in 0..steps {
        let (mut grove, ..) = marked();
        grove.mem.steps_left = Some(allowed);
        let error = root_hash(&mut grove).expect_err("the pages are closed");
        assert!(
            matches!(error, HashError::Pages(FragmentError::Pages(_))),
            "{error}"
        );
        assert_eq!(error.to_string(), "pages: pages: the pages are closed");
        assert!(error.source().is_some());
        grove.mem.steps_left = None;
        recovers(&mut grove);
    }
    let (mut grove, ..) = marked();
    grove.mem.steps_left = Some(steps);
    assert_eq!(
        root_hash(&mut grove).expect("enough"),
        grove.expected_root()
    );
}

#[test]
fn a_rule_that_refuses_loses_no_mark() {
    let calls = {
        let (mut grove, ..) = marked();
        grove.root_hash();
        grove.rules.calls.clone()
    };
    assert!(calls.iter().any(|(rule, _)| *rule == Rule::Value));
    assert!(calls.iter().any(|(rule, _)| *rule == Rule::Kv));
    for refused in calls {
        let (mut grove, ..) = marked();
        grove.rules.refuse = Some(refused.clone());
        let error = root_hash(&mut grove).expect_err("the rule refuses");
        assert!(matches!(error, HashError::Rule(_)), "{error}");
        let text = format!("hash rule: no {:?} hash for this node", refused.0);
        assert_eq!(error.to_string(), text);
        assert!(error.source().is_some());
        grove.rules.refuse = None;
        recovers(&mut grove);
    }

    // The rule that hashes a node which is not marked, and the rules that
    // `check` calls.
    let (mut grove, hosted, changed) = marked();
    let leaf = grove.at(hosted, &changed);
    grove.rules.refuse = Some((Rule::Node, changed.clone()));
    let (mem, log, rules) = (&mut grove.mem, &mut grove.log, &mut grove.rules);
    let mut clean = Hashes::new();
    let error = clean.hash_of(mem, log, rules, leaf);
    assert!(matches!(error, Err(HashError::Rule(_))));
    assert_eq!(*clean.counters(), Counters::default());
    for rule in [Rule::Value, Rule::Kv] {
        rules.refuse = Some((rule, changed.clone()));
        let error = check(mem, log, rules, leaf);
        assert!(matches!(error, Err(HashError::Rule(_))));
    }
    let root = grove.root_of(hosted);
    let below = grove.nodes[grove.merks[hosted].root.expect("a root")].left;
    let below = grove.nodes[below.expect("a child")].key.clone();
    grove.rules.refuse = Some((Rule::Node, below));
    let error = check(&mut grove.mem, &mut grove.log, &mut grove.rules, root);
    assert!(matches!(error, Err(HashError::Rule(_))));
}

#[test]
fn a_value_that_cannot_be_read_loses_no_mark() {
    let large = || {
        let (mut grove, hosted, changed) = marked();
        grove.put(hosted, &changed, &value(5, 600));
        let at = grove.at(hosted, &changed);
        assert!(grove.mem.node(at).value().pending().is_some());
        (grove, at)
    };

    // No reader was given.
    let (mut grove, at) = large();
    let error = grove
        .hashes
        .evaluate(&mut grove.mem, &mut (), &mut grove.rules, at);
    let error = error.expect_err("no reader");
    assert!(matches!(error, HashError::Value(_)));
    assert_eq!(
        error.to_string(),
        "value: no reader was given for a value that is not in its cell"
    );
    let source = error.source().expect("the reader's error");
    assert!(source.downcast_ref::<NoValueReader>().is_some());
    assert!(matches!(
        check(&mut grove.mem, &mut (), &mut Plain, at),
        Err(HashError::Value(_))
    ));
    recovers(&mut grove);

    // The reader does not have the value.
    let (mut grove, _) = large();
    let kept = std::mem::take(&mut grove.log.pending);
    let error = root_hash(&mut grove).expect_err("the value is not kept");
    assert_eq!(error.to_string(), "value: the value is not kept");
    grove.log.pending = kept;
    recovers(&mut grove);

    // The reader gives another number of bytes than the cell states.
    let (mut grove, at) = large();
    grove.log.long = true;
    let error = root_hash(&mut grove).expect_err("a byte too many");
    assert!(
        matches!(error, HashError::ValueLen { node, len: 600, read: 601 } if node == at),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!("the value of {at} is 600 bytes and 601 were read")
    );
    assert!(error.source().is_none());
    grove.log.long = false;
    recovers(&mut grove);
}

#[test]
fn a_mark_names_a_hash_that_a_cell_stores() {
    let (mut grove, ..) = marked();
    grove.evaluate_all();
    let mut leaves = grove
        .nodes
        .iter()
        .filter(|node| node.left.or(node.right).is_none());
    let leaf = leaves.next().and_then(|node| node.at).expect("a leaf");
    assert!(grove.mem.node(leaf).link(Side::Left).is_none());
    let free = CellAddress::new(leaf.page(), slot(999));
    let nowhere = address(999, 1);
    let (mem, log, rules) = (&mut grove.mem, &mut grove.log, &mut grove.rules);

    // The hash of a child the node does not have.
    grove.hashes.mark(leaf, LEFT);
    let error = grove.hashes.evaluate(mem, log, rules, leaf);
    let error = error.expect_err("the node has no left child");
    assert!(
        matches!(error, HashError::NoHash { node, field: LEFT } if node == leaf),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!("the node at {leaf} stores no hash of the left child")
    );
    assert!(error.source().is_none());
    assert_eq!(grove.hashes.forget(leaf), Stale::of(LEFT));
    grove.hashes.mark(leaf, RIGHT);
    let error = grove.hashes.evaluate_all(mem, log, rules);
    assert_eq!(
        error.expect_err("nor a right one").to_string(),
        format!("the node at {leaf} stores no hash of the right child")
    );
    grove.hashes.clear();
    // `check` asks only for what the cell stores.
    assert_eq!(check(mem, log, rules, leaf).expect("checked"), Stale::NONE);

    // A slot that holds no cell.
    for field in [HashField::Value, HashField::KeyValue] {
        grove.hashes.mark(free, field);
        let error = grove.hashes.evaluate(mem, log, rules, free);
        let error = error.expect_err("no cell");
        assert!(
            matches!(error, HashError::NoNode { address } if address == free),
            "{error}"
        );
        assert_eq!(error.to_string(), format!("no node at {free}"));
        assert!(error.source().is_none());
        assert!(!grove.hashes.forget(free).is_empty());
    }
    let error = grove.hashes.hash_of(mem, log, rules, free);
    assert!(matches!(error, Err(HashError::NoNode { address }) if address == free));
    let error = check(mem, log, rules, free);
    assert!(matches!(error, Err(HashError::NoNode { address }) if address == free));
    // A node with no mark is not read, so its slot is not looked at.
    grove
        .hashes
        .evaluate(mem, log, rules, free)
        .expect("no mark");

    // A page that is not there.
    grove.hashes.mark(nowhere, HashField::KeyValue);
    let error = grove.hashes.evaluate(mem, log, rules, nowhere);
    let error = error.expect_err("no page");
    assert!(matches!(error, HashError::Pages(_)), "{error}");
    assert_eq!(grove.hashes.forget(nowhere), Stale::of(HashField::KeyValue));
    assert!(matches!(
        check(mem, log, rules, nowhere),
        Err(HashError::Pages(_))
    ));
}

#[test]
fn a_pointer_names_a_cell() {
    let free = address(1, 9);
    let roots = [Some(free)];
    let (mut mem, id) = page_of(&[
        CellContent {
            left: Some(link(free)),
            ..leaf(b"a", b"value")
        },
        CellContent {
            hosted_roots: &roots,
            ..leaf(b"b", b"value")
        },
    ]);
    let (parent, host) = (CellAddress::new(id, slot(1)), CellAddress::new(id, slot(2)));
    for (at, field) in [(parent, LEFT), (host, HashField::Value)] {
        let mut hashes = Hashes::new();
        hashes.mark(at, field);
        let error = evaluate(&mut mem, &mut hashes, at).expect_err("no cell there");
        assert!(
            matches!(error, HashError::NoNode { address } if address == free),
            "{error}"
        );
        assert!(hashes.stale(at).has(field), "still marked");
        let error = check(&mut mem, &mut (), &mut Plain, at);
        assert!(matches!(error, Err(HashError::NoNode { address }) if address == free));
    }
}

#[test]
fn marked_nodes_that_point_in_a_circle_are_refused() {
    let (first, second, own) = (address(1, 1), address(1, 2), address(1, 3));
    let (mut mem, _) = page_of(&[
        CellContent {
            left: Some(link(second)),
            ..leaf(b"a", b"value")
        },
        CellContent {
            right: Some(link(first)),
            ..leaf(b"b", b"value")
        },
        CellContent {
            left: Some(link(own)),
            ..leaf(b"c", b"value")
        },
    ]);
    let mut hashes = Hashes::new();
    hashes.mark(first, LEFT);
    hashes.mark(second, RIGHT);
    hashes.mark(own, LEFT);
    let marks = hashes.clone();
    // Asked again, the answer is the same: no node is left open.
    for _ in 0..2 {
        let error = evaluate(&mut mem, &mut hashes, first).expect_err("a circle");
        assert!(
            matches!(error, HashError::Cycle { node } if node == first),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            format!("the marked nodes point in a circle through {first}")
        );
        assert!(error.source().is_none());
        let error = evaluate(&mut mem, &mut hashes, own).expect_err("a circle");
        assert!(matches!(error, HashError::Cycle { node } if node == own));
        assert_eq!(hashes, marks);
    }
    let error = hashes.evaluate_all(&mut mem, &mut (), &mut Plain);
    assert!(matches!(error, Err(HashError::Cycle { .. })));
    assert_eq!(hashes, marks);
    assert!(mem.shadow.hashes.is_empty());
}

#[test]
fn a_cell_that_changed_during_an_evaluation_is_refused() {
    let (first, second, third) = (address(1, 1), address(1, 2), address(1, 3));
    let roots = [Some(second), None];
    let build = || {
        page_of(&[
            CellContent {
                left: Some(link(second)),
                hosted_roots: &roots,
                ..leaf(b"a", b"value")
            },
            leaf(b"b", b"value"),
            leaf(b"c", b"value"),
        ])
        .0
    };
    type Meddle = fn(&mut Mem);
    let other_child: Meddle = |mem| {
        mem.write(page(1), |page| {
            let mut node = page.node_mut(slot(1)).expect("the node");
            node.set_child(Side::Left, address(1, 3))
        })
        .expect("the link changes");
    };
    let no_child: Meddle = |mem| {
        mem.write(page(1), |page| {
            let mut node = page.node_mut(slot(1)).expect("the node");
            node.set_link(Side::Left, None)
        })
        .expect("the link goes");
    };
    let other_root: Meddle = |mem| {
        mem.write(page(1), |page| {
            let mut node = page.node_mut(slot(1)).expect("the node");
            node.set_hosted_root(0, Some(address(1, 3)))
        })
        .expect("the pointer changes");
    };
    let one_more_root: Meddle = |mem| {
        mem.write(page(1), |page| {
            let mut node = page.node_mut(slot(1)).expect("the node");
            node.set_hosted_root(1, Some(address(1, 3)))
        })
        .expect("the pointer changes");
    };
    let fewer_roots: Meddle = |mem| {
        mem.write(page(1), |page| {
            let mut node = page.node_mut(slot(1)).expect("the node");
            node.set_hosted_roots(&[Some(address(1, 2))])
        })
        .expect("the pointers change");
    };
    let value_elsewhere: Meddle = |mem| {
        mem.write(page(1), |page| {
            let pending = PendingValue::new([7; 8], 5).expect("a pending value");
            let mut node = page.node_mut(slot(1)).expect("the node");
            node.set_value(CellValue::Pending(pending))
        })
        .expect("the value changes");
    };
    let cases: [(HashField, Meddle); 6] = [
        (LEFT, other_child),
        (LEFT, no_child),
        (HashField::Value, other_root),
        (HashField::Value, one_more_root),
        (HashField::Value, fewer_roots),
        (HashField::Value, value_elsewhere),
    ];
    for (number, (field, meddle)) in cases.into_iter().enumerate() {
        let mut mem = build();
        let mut hashes = Hashes::new();
        hashes.mark(first, field);
        // The node is read, the node it points at is read, and then someone
        // else changes the cell before it is written.
        let mut meddled = Meddled {
            mem: &mut mem,
            before: 2,
            meddle: Some(meddle),
        };
        let error = hashes.evaluate(&mut meddled, &mut (), &mut Plain, first);
        let error = error.expect_err("the cell changed");
        assert!(
            matches!(error, HashError::Changed { node } if node == first),
            "case {number}: {error}"
        );
        assert_eq!(
            error.to_string(),
            format!("the cell at {first} changed during the evaluation")
        );
        assert!(error.source().is_none());
        assert!(hashes.stale(first).has(field), "still marked");
        assert!(mem.shadow.hashes.is_empty(), "nothing was written");
    }

    // A cell that is gone when its hashes are to be written.
    let mut mem = build();
    let mut hashes = Hashes::new();
    hashes.mark(third, HashField::KeyValue);
    let gone: Meddle = |mem| {
        mem.write(page(1), |page| {
            page.node_mut(slot(3)).expect("the node").remove()
        })
        .expect("the cell goes");
    };
    let mut meddled = Meddled {
        mem: &mut mem,
        before: 1,
        meddle: Some(gone),
    };
    let error = hashes.evaluate(&mut meddled, &mut (), &mut Plain, third);
    assert!(matches!(error, Err(HashError::NoNode { address }) if address == third));
    assert_eq!(hashes.stale(third), Stale::of(HashField::KeyValue));

    // Left alone, each of them is evaluated.
    for field in [LEFT, HashField::Value, HashField::KeyValue] {
        let mut mem = build();
        let mut hashes = Hashes::new();
        hashes.mark(first, field);
        let mut meddled = Meddled {
            mem: &mut mem,
            before: usize::MAX,
            meddle: None::<Meddle>,
        };
        let evaluated = hashes.evaluate(&mut meddled, &mut (), &mut Plain, first);
        evaluated.expect("nothing changed");
        assert!(hashes.is_empty());
        assert_eq!(meddled.create().expect("a page"), page(2));
        meddled.write(page(2), |_| Ok(())).expect("a page");
        meddled.free(page(2)).expect("freed");
        assert!(!meddled.host_is_behind(first));
    }
}

#[test]
fn an_error_says_what_stopped_the_evaluation() {
    let at = address(7, 3);
    let inner = || -> Refusal { "the reason".into() };
    let node_error = NodeError::NoNode { address: at };
    let errors = [
        (
            HashError::Pages(FragmentError::Node(node_error)),
            format!("pages: tree page: no node at {at}"),
            true,
        ),
        (
            HashError::Value(inner()),
            "value: the reason".to_owned(),
            true,
        ),
        (
            HashError::Rule(inner()),
            "hash rule: the reason".to_owned(),
            true,
        ),
        (
            HashError::NoHash {
                node: at,
                field: HashField::Value,
            },
            format!("the node at {at} stores no value hash"),
            false,
        ),
        (
            HashError::NoHash {
                node: at,
                field: HashField::KeyValue,
            },
            format!("the node at {at} stores no key-value hash"),
            false,
        ),
        (
            HashError::HostBehind { host: at },
            format!("the host at {at} is behind the roots it hosts"),
            false,
        ),
    ];
    for (error, text, has_source) in errors {
        assert_eq!(error.to_string(), text);
        assert_eq!(error.source().is_some(), has_source, "{error:?}");
    }
    assert_eq!(
        NoValueReader.to_string(),
        "no reader was given for a value that is not in its cell"
    );
}

#[test]
fn rules_and_readers_are_passed_on_by_reference() {
    let (mut grove, hosted, changed) = marked();
    grove.put(hosted, &changed, &value(5, 600));
    let at = grove.at(hosted, &changed);
    let expected = grove.expected(hosted, &changed).hash;
    let mut rules: &mut dyn HashRules = &mut grove.rules;
    let mut reader: &mut dyn ValueReader = &mut grove.log;
    let mem = &mut grove.mem;
    let evaluated = grove.hashes.evaluate_all(mem, &mut reader, &mut rules);
    evaluated.expect("evaluated through the references");
    let hash = grove.hashes.hash_of(mem, &mut reader, &mut rules, at);
    assert_eq!(hash.expect("made"), expected);
    let differing = check(mem, &mut reader, &mut rules, at);
    assert_eq!(differing.expect("checked"), Stale::NONE);
    assert!(
        grove.log.reads >= 2,
        "the value was read through the reference"
    );
    grove.check_hashes();
}
