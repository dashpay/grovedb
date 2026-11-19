//! Writes that are refused, and pages that fail.

use std::error::Error;

use lignin_format::tree_page::{Hash, Side};

use super::*;

const HASH: Hash = [7; 32];

fn node(at: Option<CellAddress>, key: &[u8]) -> NodeWrite<'_> {
    NodeWrite {
        at,
        key,
        feature: &[0],
        value_hash: &HASH,
        kv_hash: &HASH,
        left: None,
        right: None,
        hosted: HostedWrite::Keep,
        value: match at {
            Some(_) => ValueWrite::Keep,
            None => ValueWrite::Inline(b"value"),
        },
    }
}

fn link(child: Target) -> Option<LinkWrite<'static>> {
    Some(LinkWrite {
        child,
        hash: &HASH,
        heights: (0, 0),
        child_key_len: 1,
        aggregate: &[0],
    })
}

fn root(was: Option<CellAddress>, now: Target) -> RootWrite {
    RootWrite {
        was,
        now: Some(now),
    }
}

/// Pages with no grove to drive them.
struct Bare {
    mem: Mem,
    log: Log,
    fragments: Fragments,
}

impl Bare {
    /// One page with the node `m`, and `c` and `x` below it.
    fn new() -> (Self, [CellAddress; 3]) {
        let mut bare = Self {
            mem: Mem::new(SIZE),
            log: Log::default(),
            fragments: Fragments::new(Layout::proposed(SIZE)),
        };
        let top = NodeWrite {
            left: link(Target::Node(1)),
            right: link(Target::Node(2)),
            ..node(None, b"m")
        };
        let report = bare
            .apply(&Write {
                nodes: &[top, node(None, b"c"), node(None, b"x")],
                removed: &[],
                roots: &[RootWrite {
                    was: None,
                    now: Some(Target::Node(0)),
                }],
            })
            .expect("the write is applied");
        assert_eq!(report.roots, [Some(address(1, 3))]);
        let at = [0, 1, 2].map(|node| report.nodes[node].address());
        (bare, at)
    }

    fn apply(&mut self, write: &Write<'_>) -> Result<Report, FragmentError> {
        self.fragments.apply(&mut self.mem, &mut self.log, write)
    }

    /// Why `write` is refused. A write that is refused changes no page.
    fn refused(&mut self, write: &Write<'_>) -> WriteError {
        let (frames, records) = (self.mem.frames.clone(), self.mem.shadow.records.len());
        let error = self.apply(write).expect_err("the write is refused");
        assert_eq!(self.mem.frames, frames);
        assert_eq!(self.mem.shadow.records.len(), records);
        assert!(error.to_string().starts_with("write: "));
        assert!(error.source().is_some());
        match error {
            FragmentError::Write(error) => error,
            other => panic!("refused for another reason: {other}"),
        }
    }

    fn refused_nodes(&mut self, nodes: &[NodeWrite<'_>]) -> WriteError {
        self.refused(&Write {
            nodes,
            ..Write::default()
        })
    }
}

#[test]
fn a_write_that_names_what_is_not_there_is_refused() {
    let (mut bare, [m, c, x]) = Bare::new();
    let free = address(1, 9);
    let top = |left: Target, right: Target| NodeWrite {
        left: link(left),
        right: link(right),
        ..node(Some(m), b"m")
    };

    let error = bare.refused_nodes(&[node(Some(free), b"m")]);
    assert_eq!(error, WriteError::NoNode { address: free });
    let error = bare.refused(&Write {
        removed: &[free],
        ..Write::default()
    });
    assert_eq!(error, WriteError::NoNode { address: free });

    let error = bare.refused_nodes(&[node(Some(c), b"other")]);
    assert_eq!(error, WriteError::KeyChanged { node: 0 });

    let error = bare.refused_nodes(&[node(Some(c), b"c"), node(Some(c), b"c")]);
    assert_eq!(error, WriteError::Twice { address: c });
    let error = bare.refused(&Write {
        removed: &[x, x],
        ..Write::default()
    });
    assert_eq!(error, WriteError::Twice { address: x });
    let error = bare.refused(&Write {
        nodes: &[node(Some(c), b"c")],
        removed: &[c],
        ..Write::default()
    });
    assert_eq!(error, WriteError::Twice { address: c });

    let error = bare.refused_nodes(&[top(Target::At(c), Target::At(c))]);
    assert_eq!(error, WriteError::TwoPointers { address: Some(c) });
    let error = bare.refused_nodes(&[top(Target::Node(0), Target::Kept)]);
    assert_eq!(error, WriteError::TwoPointers { address: Some(m) });
    let error = bare.refused_nodes(&[top(Target::Node(1), Target::Node(1)), node(None, b"n")]);
    assert_eq!(error, WriteError::TwoPointers { address: None });
    let error = bare.refused(&Write {
        nodes: &[top(Target::Kept, Target::Kept)],
        roots: &[root(None, Target::At(c))],
        ..Write::default()
    });
    assert_eq!(error, WriteError::TwoPointers { address: Some(c) });
    let error = bare.refused(&Write {
        roots: &[root(Some(m), Target::Kept), root(None, Target::At(m))],
        ..Write::default()
    });
    assert_eq!(error, WriteError::TwoPointers { address: Some(m) });

    let error = bare.refused_nodes(&[top(Target::Node(7), Target::Kept)]);
    assert_eq!(error, WriteError::NoSuchNode { node: 7 });

    let error = bare.refused(&Write {
        nodes: &[top(Target::At(c), Target::Kept)],
        removed: &[c],
        ..Write::default()
    });
    assert_eq!(error, WriteError::Removed { address: c });
    let error = bare.refused(&Write {
        removed: &[m],
        roots: &[root(Some(m), Target::Kept)],
        ..Write::default()
    });
    assert_eq!(error, WriteError::Removed { address: m });

    let leaf = NodeWrite {
        left: link(Target::Kept),
        ..node(Some(c), b"c")
    };
    assert_eq!(
        bare.refused_nodes(&[leaf]),
        WriteError::NothingKept { node: 0 }
    );
    let new = NodeWrite {
        right: link(Target::Kept),
        ..node(None, b"n")
    };
    let error = bare.refused_nodes(&[node(Some(x), b"x"), new]);
    assert_eq!(error, WriteError::NothingKept { node: 1 });

    let new = NodeWrite {
        value: ValueWrite::Keep,
        ..node(None, b"n")
    };
    assert_eq!(bare.refused_nodes(&[new]), WriteError::NoValue { node: 0 });

    let circle = [
        NodeWrite {
            left: link(Target::Node(1)),
            ..node(None, b"a")
        },
        NodeWrite {
            left: link(Target::Node(0)),
            ..node(None, b"b")
        },
    ];
    assert_eq!(bare.refused_nodes(&circle), WriteError::Cycle);

    // The pages are as they were, and take a write.
    let report = bare.apply(&Write {
        nodes: &[top(Target::Kept, Target::Kept)],
        roots: &[root(Some(m), Target::Kept)],
        ..Write::default()
    });
    let report = report.expect("the write is applied");
    assert_eq!(report.nodes[0].address(), m);
    assert_eq!(report.roots, [Some(m)]);
    assert_eq!(
        report,
        Report {
            nodes: report.nodes.clone(),
            roots: vec![Some(m)],
            ..Report::default()
        }
    );
}

#[test]
fn a_cell_that_is_too_large_is_refused() {
    let (mut bare, [_, c, _]) = Bare::new();
    let max = SIZE.max_cell_len();
    let large = vec![0; max];
    let grown = NodeWrite {
        value: ValueWrite::Inline(&large),
        ..node(Some(c), b"c")
    };
    let error = bare.refused_nodes(&[node(None, b"n"), grown]);
    let len = max + 72 + 1 + 1;
    assert_eq!(error, WriteError::CellTooLarge { node: 1, len, max });

    let long = [0; 256];
    let empty = [None; 256];
    let wide = LinkWrite {
        aggregate: &long,
        ..link(Target::Kept).expect("a link")
    };
    let cases = [
        node(None, &long),
        NodeWrite {
            feature: &long,
            ..node(None, b"n")
        },
        NodeWrite {
            hosted: HostedWrite::Set(&empty),
            ..node(None, b"n")
        },
        NodeWrite {
            right: Some(wide),
            ..node(None, b"n")
        },
    ];
    for case in cases {
        assert_eq!(
            bare.refused_nodes(&[case]),
            WriteError::FieldTooLong { node: 0 }
        );
    }
}

#[test]
fn a_cell_that_still_points_at_a_removed_cell_is_found_when_it_is_read() {
    let mut bare = Bare::new().0;
    bare.mem = Mem::new(SIZE);
    let large = vec![1; 3900];
    let nodes = [
        NodeWrite {
            left: link(Target::Node(1)),
            right: link(Target::Node(2)),
            ..node(None, b"m")
        },
        NodeWrite {
            left: link(Target::Node(3)),
            value: ValueWrite::Inline(&large),
            ..node(None, b"c")
        },
        node(None, b"x"),
        node(None, b"a"),
    ];
    let report = bare.apply(&Write {
        nodes: &nodes,
        removed: &[],
        roots: &[RootWrite {
            was: None,
            now: Some(Target::Node(0)),
        }],
    });
    let at = report.expect("the write is applied").nodes;
    let [m, x, a] = [0, 2, 3].map(|node| at[node].address());

    // `a` goes, and the cell of `c`, which the write does not write, still
    // points at it. The write has to make room, so it reads `c`.
    let error = bare.refused(&Write {
        nodes: &[
            NodeWrite {
                left: link(Target::Kept),
                right: link(Target::Kept),
                ..node(Some(m), b"m")
            },
            NodeWrite {
                value: ValueWrite::Inline(&[2; 3990]),
                ..node(Some(x), b"x")
            },
        ],
        removed: &[a],
        roots: &[root(Some(m), Target::Kept)],
    });
    assert_eq!(error, WriteError::Removed { address: a });
}

/// The cell at `at`, read from its page, with the page checked.
fn stored(bare: &Bare, at: CellAddress) -> lignin_format::tree_page::TreeCell<'_> {
    let view = bare.mem.view(at.page());
    let page = view.tree_page();
    page.check(SIZE).expect("a valid page");
    page.check_fragments()
        .expect("the marks are what the links say");
    let shadow = NodePage::open(&bare.mem.shadow.frames[&at.page()], bare.mem.lease);
    let shadow = shadow.expect("a page").tree_page().to_image();
    assert_eq!(shadow, page.to_image(), "the records give the page");
    view.node(at.slot()).expect("the cell").cell()
}

#[test]
fn every_part_of_a_cell_can_get_longer_and_shorter() {
    let (mut bare, [m, c, x]) = Bare::new();
    let wide = [9; 9];
    let long = |child: Target| {
        Some(LinkWrite {
            aggregate: &wide,
            ..link(child).expect("a link")
        })
    };
    let grown = NodeWrite {
        feature: &wide,
        left: long(Target::At(c)),
        right: long(Target::Kept),
        hosted: HostedWrite::Set(&[Some(Target::Node(1)), None, Some(Target::Node(2))]),
        value: ValueWrite::Inline(&[5; 300]),
        ..node(Some(m), b"m")
    };
    let report = bare.apply(&Write {
        nodes: &[grown, node(None, b"p"), node(None, b"q")],
        removed: &[],
        roots: &[root(Some(m), Target::Kept)],
    });
    let report = report.expect("the write is applied");
    let [p, q] = [1, 2].map(|node| report.nodes[node].address());
    let cell = stored(&bare, m);
    assert_eq!(cell.feature(), wide);
    assert_eq!(
        cell.hosted_roots().collect::<Vec<_>>(),
        [Some(p), None, Some(q)]
    );
    assert_eq!(cell.value().inline(), Some(&[5; 300][..]));
    for (side, child) in Side::BOTH.into_iter().zip([c, x]) {
        let link = cell.link(side).expect("a link");
        assert_eq!((link.child, link.aggregate), (child, &wide[..]));
    }

    // Each part shorter again, with a hosted root that is new.
    let shrunk = NodeWrite {
        left: link(Target::Kept),
        right: link(Target::At(x)),
        hosted: HostedWrite::Set(&[Some(Target::Node(1))]),
        value: ValueWrite::Inline(b"v"),
        ..node(Some(m), b"m")
    };
    let report = bare.apply(&Write {
        nodes: &[shrunk, node(None, b"r")],
        removed: &[],
        roots: &[root(Some(m), Target::Kept)],
    });
    let mut report = report.expect("the write is applied");
    let r = report.nodes[1].address();
    report.orphaned.sort_unstable();
    assert_eq!(report.orphaned, [p.min(q), p.max(q)]);
    let cell = stored(&bare, m);
    assert_eq!(cell.feature(), [0]);
    assert_eq!(cell.hosted_roots().collect::<Vec<_>>(), [Some(r)]);
    assert_eq!(cell.value().inline(), Some(&b"v"[..]));
    for (side, child) in Side::BOTH.into_iter().zip([c, x]) {
        let link = cell.link(side).expect("a link");
        assert_eq!((link.child, link.aggregate), (child, &[0][..]));
    }
    for orphan in [p, q] {
        assert!(stored(&bare, orphan).is_fragment_root());
    }
}

#[test]
fn a_write_is_not_refused_for_room_that_it_makes_itself() {
    let mut bare = Bare::new().0;
    bare.mem = Mem::new(SIZE);
    let large = [1; 3900];
    let nodes = [
        NodeWrite {
            left: link(Target::Node(1)),
            right: link(Target::Node(2)),
            ..node(None, b"m")
        },
        NodeWrite {
            value: ValueWrite::Inline(&large),
            ..node(None, b"c")
        },
        NodeWrite {
            value: ValueWrite::Inline(&large),
            ..node(None, b"x")
        },
    ];
    let report = bare.apply(&Write {
        nodes: &nodes,
        removed: &[],
        roots: &[RootWrite {
            was: None,
            now: Some(Target::Node(0)),
        }],
    });
    let at = report.expect("the write is applied").nodes;
    let [m, c] = [0, 1].map(|node| at[node].address());
    let free = bare.mem.view(page(1)).tree_page().free_bytes(SIZE);
    assert!(free < 100, "the page is full: {free} bytes are left");

    // The first node of the write grows by more than the page has left,
    // and the second gives up more than that.
    let grown = NodeWrite {
        left: link(Target::Kept),
        right: link(Target::Kept),
        value: ValueWrite::Inline(&[3; 800]),
        ..node(Some(m), b"m")
    };
    let shrunk = NodeWrite {
        value: ValueWrite::Inline(&large[..3000]),
        ..node(Some(c), b"c")
    };
    let report = bare.apply(&Write {
        nodes: &[grown, shrunk],
        removed: &[],
        roots: &[root(Some(m), Target::Kept)],
    });
    let report = report.expect("the write is applied");
    assert!(report.carried.is_empty() && report.created.is_empty());
    assert_eq!(stored(&bare, m).value().inline(), Some(&[3; 800][..]));
    assert_eq!(stored(&bare, c).value().inline(), Some(&large[..3000]));
}

#[test]
fn a_failure_of_the_pages_is_passed_on() {
    let mut outcomes = Vec::new();
    for writes in 0..5 {
        let (mut bare, [m, _, x]) = Bare::new();
        bare.mem.writes_left = Some(writes);
        let top = NodeWrite {
            left: link(Target::Kept),
            right: link(Target::Node(1)),
            ..node(Some(m), b"m")
        };
        let result = bare.apply(&Write {
            nodes: &[top, node(None, b"y")],
            removed: &[x],
            roots: &[],
        });
        if let Err(error) = &result {
            assert!(matches!(error, FragmentError::Pages(_)));
            assert_eq!(error.to_string(), "pages: the pages are closed");
            assert!(error.source().is_some());
        }
        outcomes.push(result.is_ok());
    }
    assert_eq!(outcomes, [false, false, false, true, true]);

    let (mut bare, [m, ..]) = Bare::new();
    let error = bare.apply(&Write {
        nodes: &[node(Some(address(9, 1)), b"m")],
        ..Write::default()
    });
    let error = error.expect_err("there is no page 9");
    assert!(matches!(error, FragmentError::Pages(_)));

    bare.mem.frames.insert(page(1), vec![0; SIZE.get()]);
    let error = bare.apply(&Write {
        nodes: &[node(Some(m), b"m")],
        ..Write::default()
    });
    let error = error.expect_err("the frame holds no page");
    assert!(matches!(error, FragmentError::Node(NodeError::Page(_))));
    assert!(error.to_string().starts_with("tree page: "));
    assert!(error.source().is_some());
}

#[test]
fn every_error_says_what_is_wrong() {
    let at = address(3, 4);
    let cases = [
        (WriteError::NoNode { address: at }, "no node at 3/4"),
        (
            WriteError::KeyChanged { node: 2 },
            "node 2 has another key than the cell at its address",
        ),
        (
            WriteError::Twice { address: at },
            "the cell at 3/4 is named twice",
        ),
        (
            WriteError::TwoPointers { address: Some(at) },
            "two pointers name the node at 3/4",
        ),
        (
            WriteError::TwoPointers { address: None },
            "two pointers name one new node",
        ),
        (
            WriteError::NoSuchNode { node: 5 },
            "the write has no node 5",
        ),
        (
            WriteError::Removed { address: at },
            "a pointer names the cell at 3/4, which the write removes",
        ),
        (
            WriteError::NothingKept { node: 1 },
            "node 1 keeps a pointer its cell does not have",
        ),
        (
            WriteError::NoValue { node: 0 },
            "the new node 0 has no value",
        ),
        (
            WriteError::Cycle,
            "the nodes of the write point at each other in a circle",
        ),
        (
            WriteError::CellTooLarge {
                node: 1,
                len: 9,
                max: 8,
            },
            "the cell of node 1 takes 9 bytes, above 8",
        ),
        (
            WriteError::FieldTooLong { node: 6 },
            "a part of node 6 is too long",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        assert_eq!(
            FragmentError::from(error).to_string(),
            format!("write: {text}")
        );
    }
}
