//! Writes that are refused, and pages that fail.

use std::error::Error;

use lignin_format::tree_page::Hash;

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
    assert_eq!(bare.refused_nodes(&[leaf]), WriteError::NothingKept { node: 0 });
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
    assert_eq!(report, Report {
        nodes: report.nodes.clone(),
        roots: vec![Some(m)],
        ..Report::default()
    });
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
        assert_eq!(bare.refused_nodes(&[case]), WriteError::FieldTooLong { node: 0 });
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
        (WriteError::Twice { address: at }, "the cell at 3/4 is named twice"),
        (
            WriteError::TwoPointers { address: Some(at) },
            "two pointers name the node at 3/4",
        ),
        (
            WriteError::TwoPointers { address: None },
            "two pointers name one new node",
        ),
        (WriteError::NoSuchNode { node: 5 }, "the write has no node 5"),
        (
            WriteError::Removed { address: at },
            "a pointer names the cell at 3/4, which the write removes",
        ),
        (
            WriteError::NothingKept { node: 1 },
            "node 1 keeps a pointer its cell does not have",
        ),
        (WriteError::NoValue { node: 0 }, "the new node 0 has no value"),
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
        (WriteError::FieldTooLong { node: 6 }, "a part of node 6 is too long"),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        assert_eq!(FragmentError::from(error).to_string(), format!("write: {text}"));
    }
}
