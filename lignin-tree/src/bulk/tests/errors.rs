//! What ends a load.

use std::error::Error;

use lignin_format::SlotId;

use super::*;

fn failed(bench: &mut Bench) {
    assert!(matches!(bench.leaf(b"next"), Err(LoadError::Failed)));
}

#[test]
fn a_fill_that_is_none_is_refused() {
    let layout = Layout::proposed(SIZE);
    for (least, most) in [(91, 90), (0, 0), (50, 101)] {
        let params = LoadParams {
            least_fill_percent: least,
            most_fill_percent: most,
        };
        let refused = Loader::new(layout, params).expect_err("no fill");
        assert!(matches!(refused, LoadError::Fill { .. }), "{refused}");
    }
}

#[test]
fn a_subtree_is_given_once() {
    let twice = |bench: &mut Bench, node: &NodeLoad<'_>, subtree: Loaded| {
        let refused = bench.node(node).expect_err("the subtree is taken");
        assert!(
            matches!(refused, LoadError::NotLoaded { subtree: named } if named == subtree),
            "{refused}"
        );
        failed(bench);
    };

    let mut bench = Bench::proposed();
    let leaf = bench.leaf(b"a").expect("a leaf");
    let parent = NodeLoad {
        left: link(leaf),
        ..plain(b"b", b"")
    };
    bench.node(&parent).expect("a parent");
    twice(&mut bench, &parent, leaf);

    let mut bench = Bench::proposed();
    let leaf = bench.leaf(b"a").expect("a leaf");
    let both = NodeLoad {
        left: link(leaf),
        right: link(leaf),
        ..plain(b"b", b"")
    };
    twice(&mut bench, &both, leaf);

    let mut bench = Bench::proposed();
    let leaf = bench.leaf(b"a").expect("a leaf");
    let hosted = [HostedLoad::Loaded(leaf), HostedLoad::Loaded(leaf)];
    let host = NodeLoad {
        hosted: &hosted,
        ..plain(b"b", b"")
    };
    twice(&mut bench, &host, leaf);

    let mut bench = Bench::proposed();
    let leaf = bench.leaf(b"a").expect("a leaf");
    bench.root(leaf).expect("a root");
    twice(
        &mut bench,
        &NodeLoad {
            right: link(leaf),
            ..plain(b"b", b"")
        },
        leaf,
    );
}

#[test]
fn a_subtree_of_another_load_is_none() {
    let mut other = Bench::proposed();
    let first = other.leaf(b"a").expect("a leaf");
    let second = other.leaf(b"b").expect("a leaf");

    let mut bench = Bench::proposed();
    assert!(matches!(
        bench.root(second),
        Err(LoadError::NotLoaded { .. })
    ));
    failed(&mut bench);

    let mut bench = Bench::proposed();
    let own = [b"a", b"b"].map(|key| bench.leaf(key).expect("a leaf"));
    assert_eq!((own[1].index, own[1].serial), (second.index, second.serial));
    let refused = bench.loader.hash_of(second);
    assert!(matches!(refused, Err(LoadError::NotLoaded { .. })));
    assert!(bench.loader.hash_of(own[1]).is_ok(), "asking ends no load");
    assert!(other.loader.hash_of(first).is_ok());
}

#[test]
fn a_cell_that_no_page_takes_is_refused() {
    let mut bench = Bench::proposed();
    let refused = bench
        .node(&plain(&[b'k'; 256], b""))
        .expect_err("a key too long");
    assert!(matches!(refused, LoadError::Node(_)), "{refused}");
    assert!(refused.source().is_some());
    failed(&mut bench);
}

#[test]
fn a_rule_that_refuses_ends_the_load() {
    for rule in [Rule::Value, Rule::Kv, Rule::Node] {
        let mut bench = Bench::proposed();
        bench.rules.refuse = Some((rule, b"refused".to_vec()));
        let refused = bench.leaf(b"refused").expect_err("the rule refuses");
        assert!(matches!(refused, LoadError::Rule(_)), "{refused}");
        assert!(refused.source().is_some());
        failed(&mut bench);
    }

    let mut bench = Bench::proposed();
    let merk = bench.leaf(b"a root").expect("a leaf");
    let placed = bench.root(merk).expect("a root");
    bench.rules.refuse = Some((Rule::Node, b"a root".to_vec()));
    let refused = bench.node(&NodeLoad {
        hosted: &[HostedLoad::At(placed.address)],
        ..plain(b"host", b"")
    });
    assert!(matches!(refused, Err(LoadError::Rule(_))));
    failed(&mut bench);
}

#[test]
fn values_that_refuse_end_the_load() {
    let mut bench = Bench::proposed();
    bench.log.refuse = true;
    let refused = bench.leaf(b"a").expect_err("no value is taken");
    assert!(matches!(refused, LoadError::Value(_)), "{refused}");
    assert!(refused.source().is_some());
    failed(&mut bench);
}

#[test]
fn pages_that_fail_end_the_load() {
    let mut bench = Bench::proposed();
    bench.mem.creates_left = Some(0);
    let leaf = bench.leaf(b"a").expect("a leaf needs no page yet");
    let refused = bench.root(leaf).expect_err("no page is started");
    assert!(matches!(refused, LoadError::Pages(_)), "{refused}");
    assert!(refused.source().is_some());
    failed(&mut bench);

    let mut bench = Bench::proposed();
    bench.mem.writes_left = Some(0);
    let leaf = bench.leaf(b"a").expect("a leaf");
    assert!(matches!(bench.root(leaf), Err(LoadError::Pages(_))));
    assert!(matches!(bench.root(leaf), Err(LoadError::Failed)));
    assert!(matches!(bench.loader.hash_of(leaf), Err(LoadError::Failed)));
    let loader = std::mem::replace(&mut bench.loader, Bench::proposed().loader);
    assert!(matches!(loader.finish(), Err(LoadError::Failed)));
}

#[test]
fn a_hosted_root_that_is_not_there_ends_the_load() {
    let mut bench = Bench::proposed();
    let merk = bench.leaf(b"a root").expect("a leaf");
    let placed = bench.root(merk).expect("a root");
    let nowhere = CellAddress::new(placed.address.page(), SlotId::MAX);
    let refused = bench.node(&NodeLoad {
        hosted: &[HostedLoad::At(nowhere)],
        ..plain(b"host", b"")
    });
    let refused = refused.expect_err("no cell is there");
    assert!(
        matches!(refused, LoadError::NoRoot { address } if address == nowhere),
        "{refused}"
    );
    failed(&mut bench);

    let mut bench = Bench::proposed();
    let nowhere = CellAddress::new(PageId::MAX, SlotId::MIN);
    let refused = bench.node(&NodeLoad {
        hosted: &[HostedLoad::At(nowhere)],
        ..plain(b"host", b"")
    });
    assert!(matches!(refused, Err(LoadError::Pages(_))), "no such page");
}

#[test]
fn a_page_that_was_not_empty_ends_the_load() {
    let mut bench = Bench::proposed();
    bench.mem.dirty = true;
    let leaf = bench.leaf(b"a").expect("a leaf");
    let refused = bench.root(leaf).expect_err("the slot is taken");
    let LoadError::Slot { expected, found } = refused else {
        panic!("{refused}");
    };
    assert_eq!(expected.slot(), SlotId::MIN);
    assert_eq!(found.page(), expected.page());
    assert_ne!(found.slot(), expected.slot());
    failed(&mut bench);
}

#[test]
fn a_load_ends_with_every_subtree_in_its_place() {
    let mut bench = Bench::proposed();
    let leaves = [b"a", b"b", b"c"].map(|key| bench.leaf(key).expect("a leaf"));
    bench.root(leaves[1]).expect("a root");
    let loader = std::mem::replace(&mut bench.loader, Bench::proposed().loader);
    let refused = loader.finish().expect_err("two subtrees are left");
    assert!(
        matches!(refused, LoadError::Unfinished { subtrees: 2 }),
        "{refused}"
    );
}

#[test]
fn every_error_says_what_it_is() {
    let subtree = Bench::proposed().leaf(b"a").expect("a leaf");
    let address = CellAddress::new(PageId::MIN, SlotId::MIN);
    let node = || NodeError::NoNode { address };
    let said = |error: LoadError, text: &str, has_source: bool| {
        assert_eq!(error.to_string(), text);
        assert_eq!(error.source().is_some(), has_source, "{text}");
    };
    said(
        LoadError::Fill { least: 9, most: 5 },
        "no fill: at least 9% and at most 5%",
        false,
    );
    said(
        LoadError::NotLoaded { subtree },
        "subtree 1 is no subtree of the load that is free",
        false,
    );
    said(node().into(), &format!("cell: no node at {address}"), true);
    said(LoadError::Rule("no".into()), "hash rule: no", true);
    said(LoadError::Value("full".into()), "value: full", true);
    said(
        FragmentError::Node(node()).into(),
        &format!("pages: tree page: no node at {address}"),
        true,
    );
    said(
        LoadError::NoRoot { address },
        &format!("no hosted root at {address}"),
        false,
    );
    said(
        LoadError::Slot {
            expected: address,
            found: address,
        },
        &format!("a cell counted for {address} was put at {address}"),
        false,
    );
    said(
        LoadError::Unfinished { subtrees: 3 },
        "3 subtrees of the load have no place",
        false,
    );
    said(LoadError::Failed, "the load failed before", false);

    let shapes = [
        (
            ShapeError::NoLeft,
            "a node has a left child that the stream did not give",
        ),
        (
            ShapeError::LeftHeights {
                stated: (1, 0),
                found: (2, 1),
            },
            "a left child of heights (1, 0) is stated, and one of (2, 1) was given",
        ),
        (ShapeError::NoParent, "a subtree is the child of no node"),
        (
            ShapeError::NoRight { waiting: 2 },
            "2 nodes have a right child that was not given",
        ),
    ];
    for (shape, text) in shapes {
        assert_eq!(shape.to_string(), text);
        said(shape.into(), &format!("shape: {text}"), true);
    }
}

/// The values of a load that gives each its place when its cell gets its
/// page ([`LoadValues::place`]): what is out of line goes into `log`, for
/// the page, and the cell is written with a pointer.
#[derive(Debug, Default)]
struct Placed {
    held: HashMap<[u8; 8], Vec<u8>>,
    next: u64,
    /// The values in the order they were placed, each with its page.
    log: Vec<(PageId, Vec<u8>)>,
    /// Refuses the value that would be placed at this position.
    refuse_at: Option<usize>,
    /// Gives the value that is placed at this position a place that is
    /// one byte longer than it.
    longer_at: Option<usize>,
}

impl Placed {
    const SPACING: u64 = 4096;

    /// The position in `log` of the value `pointer` names.
    fn position(pointer: lignin_format::value_log::ValuePointer) -> usize {
        (pointer.offset() / Self::SPACING) as usize - 1
    }
}

impl Values for Placed {
    fn released(&mut self, _: ValuePlace, _: PageId) {
        unreachable!("a load releases no value");
    }

    fn placed(&mut self, _: PendingValue, _: PageId) {
        unreachable!("a load that places its values leaves none pending");
    }

    fn carried(&mut self, _: ValuePlace, _: PageId, _: PageId) -> PendingValue {
        unreachable!("a load carries no cell");
    }
}

impl LoadValues for Placed {
    fn hold(&mut self, value: &[u8]) -> Result<Option<PendingValue>, Refusal> {
        if value.len() <= THRESHOLD {
            return Ok(None);
        }
        self.next += 1;
        let id = self.next.to_le_bytes();
        self.held.insert(id, value.to_vec());
        let pending = PendingValue::new(id, value.len() as u32).expect("a pending value");
        Ok(Some(pending))
    }

    fn place(&mut self, pending: PendingValue, page: PageId) -> Result<ValuePlace, Refusal> {
        if self.refuse_at == Some(self.log.len()) {
            return Err("the log takes no value".into());
        }
        let value = self
            .held
            .remove(&pending.id())
            .expect("a value that was held");
        let file = lignin_format::FileId::new(3).expect("not zero");
        let offset = (self.log.len() as u64 + 1) * Self::SPACING;
        let longer = u32::from(self.longer_at == Some(self.log.len()));
        let len = value.len() as u32 + longer;
        let pointer = lignin_format::value_log::ValuePointer::new(file, offset, len);
        self.log.push((page, value));
        Ok(ValuePlace::Placed(pointer.expect("a pointer")))
    }
}

/// A chain of `count` nodes, every second one with a value out of line,
/// handed to `loader` with `values`.
fn chain(
    loader: &mut Loader,
    mem: &mut Mem,
    values: &mut Placed,
    count: usize,
) -> Result<Root, LoadError> {
    let mut rules = Rules::default();
    let long = vec![7u8; THRESHOLD + 40];
    let mut below = None;
    for index in 0..count {
        let key = format!("key-{index:04}");
        let value: &[u8] = if index % 2 == 0 { &long } else { b"short" };
        let node = NodeLoad {
            left: below.take().and_then(link),
            ..plain(key.as_bytes(), value)
        };
        below = Some(loader.node(mem, values, &mut rules, &node)?);
    }
    loader.root(mem, values, below.expect("a chain with a node"))
}

#[test]
fn a_load_that_places_its_values_writes_its_cells_with_their_places() {
    let layout = Layout::proposed(SIZE);
    let mut loader = Loader::new(layout, LoadParams::DEFAULT).expect("a fill");
    let (mut mem, mut values) = (Mem::new(SIZE), Placed::default());
    chain(&mut loader, &mut mem, &mut values, 300).expect("a load");
    let report = loader.finish().expect("every subtree has its place");
    assert_eq!(report.counters.values, 150);
    assert_eq!(values.log.len(), 150);
    assert!(
        values.held.is_empty(),
        "every value that was held was placed"
    );
    assert!(report.pages.len() > 2, "{report:?}");

    // Every cell whose value is out of line holds the pointer that its
    // value was given, for the page the cell is in, from its first write.
    let mut seen = vec![false; values.log.len()];
    for page in &report.pages {
        let view = mem.view(page.page);
        for (_, cell) in view.tree_page().cells() {
            match cell.value() {
                CellValue::Inline(bytes) => assert_eq!(bytes, b"short"),
                CellValue::Pending(_) => panic!("a value that was placed is pending"),
                CellValue::OutOfLine(pointer) => {
                    let position = Placed::position(pointer);
                    let (owner, value) = &values.log[position];
                    assert_eq!(
                        *owner, page.page,
                        "the value was placed for the cell's page"
                    );
                    assert_eq!(pointer.value_len() as usize, value.len());
                    assert!(!std::mem::replace(&mut seen[position], true));
                }
            }
        }
    }
    assert!(seen.iter().all(|seen| *seen), "a value that no cell holds");
    mem.check();
}

/// A balanced Merk of `count` nodes, every second one with a value out
/// of line, handed to `loader` with `values`: a child before its parent.
fn balanced(
    loader: &mut Loader,
    mem: &mut Mem,
    values: &mut Placed,
    count: usize,
) -> Result<Root, LoadError> {
    fn below(
        (loader, mem, values): (&mut Loader, &mut Mem, &mut Placed),
        (from, to): (usize, usize),
    ) -> Result<Option<Loaded>, LoadError> {
        if from == to {
            return Ok(None);
        }
        let top = from + (to - from) / 2;
        let left = below((loader, mem, values), (from, top))?;
        let right = below((loader, mem, values), (top + 1, to))?;
        let key = format!("key-{top:05}");
        let long = [7u8; THRESHOLD + 40];
        let value: &[u8] = if top % 2 == 0 { &long } else { b"short" };
        let node = NodeLoad {
            left: left.and_then(link),
            right: right.and_then(link),
            ..plain(key.as_bytes(), value)
        };
        let taken = loader.node(mem, values, &mut Rules::default(), &node);
        taken.map(Some)
    }
    let top = below((loader, mem, values), (0, count))?;
    loader.root(mem, values, top.expect("a Merk with a node"))
}

/// The values of a fragment are given their places in the order a scan
/// reads its cells. So values that are appended as they are placed lie in
/// the value log as a regrouping would leave them: a scan of a page
/// leaves the place it reads at only between two fragments.
#[test]
fn the_values_of_a_fragment_are_placed_in_the_order_of_a_scan() {
    use crate::value::scan_order;

    let layout = Layout::proposed(SIZE);
    let mut loader = Loader::new(layout, LoadParams::DEFAULT).expect("a fill");
    let (mut mem, mut values) = (Mem::new(SIZE), Placed::default());
    balanced(&mut loader, &mut mem, &mut values, 3_000).expect("a load");
    let report = loader.finish().expect("every subtree has its place");
    assert_eq!(report.counters.values, 1_500);
    let (mut fragments, mut in_fragments_of_many) = (0, 0);
    for page in &report.pages {
        let view = mem.view(page.page);
        let tree_page = view.tree_page();
        let placed = scan_order(&tree_page)
            .into_iter()
            .filter_map(|slot| tree_page.cell(slot)?.value_pointer());
        let positions: Vec<usize> = placed.map(Placed::position).collect();
        let jumps = (positions.windows(2))
            .filter(|pair| pair[1] != pair[0] + 1)
            .count();
        let roots = tree_page.fragment_roots().count();
        assert!(
            jumps < roots.max(1),
            "{}: a scan jumps {jumps} times in {roots} fragments",
            page.page
        );
        fragments += roots;
        in_fragments_of_many += positions.len().saturating_sub(roots);
    }
    assert!(fragments >= report.pages.len(), "{fragments} fragments");
    assert!(
        in_fragments_of_many > 1_000,
        "{in_fragments_of_many} values follow another of their fragment"
    );
    mem.check();
}

#[test]
fn a_value_that_is_refused_its_place_ends_the_load() {
    let layout = Layout::proposed(SIZE);
    let mut loader = Loader::new(layout, LoadParams::DEFAULT).expect("a fill");
    let (mut mem, mut values) = (Mem::new(SIZE), Placed::default());
    values.refuse_at = Some(3);
    let refused = chain(&mut loader, &mut mem, &mut values, 300).expect_err("a refusal");
    assert!(matches!(refused, LoadError::Value(_)), "{refused}");
    assert_eq!(values.log.len(), 3);
    let leaf = loader.node(
        &mut mem,
        &mut values,
        &mut Rules::default(),
        &plain(b"next", b"v"),
    );
    assert!(matches!(leaf, Err(LoadError::Failed)));
}

/// The cell of a value is counted with the length the value has. A place
/// of another length is a well-formed pointer, which a page would take:
/// the load refuses it before the cell is written.
#[test]
fn a_place_of_another_length_than_its_value_ends_the_load() {
    let layout = Layout::proposed(SIZE);
    let mut loader = Loader::new(layout, LoadParams::DEFAULT).expect("a fill");
    let (mut mem, mut values) = (Mem::new(SIZE), Placed::default());
    values.longer_at = Some(2);
    let refused = chain(&mut loader, &mut mem, &mut values, 300).expect_err("a refusal");
    let LoadError::Value(said) = &refused else {
        panic!("{refused}");
    };
    let (held, placed) = (THRESHOLD + 40, THRESHOLD + 41);
    assert_eq!(
        said.to_string(),
        format!("a value of {held} bytes was given a place of {placed}")
    );
    assert_eq!(values.log.len(), 3, "the place was asked for and refused");
    let leaf = loader.node(
        &mut mem,
        &mut values,
        &mut Rules::default(),
        &plain(b"next", b"v"),
    );
    assert!(matches!(leaf, Err(LoadError::Failed)));
}
