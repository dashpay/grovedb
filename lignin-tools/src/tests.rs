//! The subcommands on small databases of GroveDB's shape, written through
//! the engine as GroveDB writes them: every hash made by Merk's functions,
//! so that `verify` makes each one again and finds it.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use grovedb_element::Element;
use grovedb_merk::tree::{combine_hash, kv_digest_to_kv_hash, node_hash, value_hash, NULL_HASH};
use grovedb_version::version::GroveVersion;
use lignin_format::tree_page::Hash;
use lignin_tree::hash::Stale;
use lignindb::{
    commit::NoStaleHashes,
    txn::{
        Batch, Column, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut, Subtree,
        SubtreeWrite, ValuePut,
    },
    Db, Options,
};

use crate::{
    args::{self, Command, Common, DumpWhat},
    verify::{INCOMPLETE_RESTORE_KEY, ROOT},
    Outcome, ToolError,
};

mod commands;
mod dumps;

/// The subtree that the root's element under [`DOCS_KEY`] hosts.
const DOCS: Subtree = Subtree::new([7; 32]);
/// A subtree with flat records and no Merk, as an append-only tree is.
const FLAT: Subtree = Subtree::new([9; 32]);
/// The Merk of one node that the element of the document `doc00001` hosts,
/// as an owner's tree of the reference database is: its one node is its
/// root, and hosts [`SUB`].
const MID: Subtree = Subtree::new([6; 32]);
/// The Merk below [`MID`]: a fourth level.
const SUB: Subtree = Subtree::new([8; 32]);
const DOCS_KEY: &[u8] = b"docs";
/// The tree type GroveDB enters a normal tree with.
const TREE: &[u8] = &[0];
/// The tree type GroveDB enters an MMR tree with, which keeps its data as
/// flat records (`grovedb_merk::store::lignin::tree_type_bytes`).
const FLAT_TREE: &[u8] = &[8];
/// A value long enough to be stored out of line.
const LONG: usize = 3_000;
/// The byte the long values are made of.
const LONG_BYTE: u8 = 0xab;

/// A node of a Merk that is built: its key, its value as serialized, and
/// the root hash of the Merk it hosts, if it hosts one.
struct Item {
    key: Vec<u8>,
    value: Vec<u8>,
    hosts: Option<(Subtree, Hash)>,
}

impl Item {
    fn element(key: &[u8], element: Element) -> Self {
        Self {
            key: key.to_vec(),
            value: element
                .serialize(GroveVersion::latest())
                .expect("serialized"),
            hosts: None,
        }
    }
}

/// A node as it is written, with the hashes Merk makes.
struct Built {
    key: Vec<u8>,
    value: Vec<u8>,
    value_hash: Hash,
    kv_hash: Hash,
    left: Option<(Vec<u8>, Hash, (u8, u8))>,
    right: Option<(Vec<u8>, Hash, (u8, u8))>,
    hosted: Vec<Subtree>,
}

/// A Merk of `items`, which are in key order: a balanced search tree,
/// each hash made by Merk's functions for nodes with no aggregate. Returns
/// the nodes, the root's key and the root hash.
fn merk(items: &[Item]) -> (Vec<Built>, Option<Vec<u8>>, Hash) {
    /// A subtree that is built: its root's key and hash, and the heights
    /// of its root's two sides, as the link to it states them.
    type Side = Option<(Vec<u8>, Hash, (u8, u8))>;
    fn height(side: &Side) -> u8 {
        side.as_ref()
            .map_or(0, |(_, _, (left, right))| left.max(right) + 1)
    }
    fn build(items: &[Item], out: &mut Vec<Built>) -> Side {
        if items.is_empty() {
            return None;
        }
        let top = items.len() / 2;
        let item = &items[top];
        let left = build(&items[..top], out);
        let right = build(&items[top + 1..], out);
        let own = value_hash(&item.value).unwrap();
        let value_hash = match item.hosts {
            Some((_, root)) => combine_hash(&own, &root).unwrap(),
            None => own,
        };
        let kv_hash = kv_digest_to_kv_hash(&item.key, &value_hash).unwrap();
        let hash_of = |side: &Side| side.as_ref().map_or(NULL_HASH, |(_, hash, _)| *hash);
        let hash = node_hash(&kv_hash, &hash_of(&left), &hash_of(&right)).unwrap();
        let heights = (height(&left), height(&right));
        out.push(Built {
            key: item.key.clone(),
            value: item.value.clone(),
            value_hash,
            kv_hash,
            left,
            right,
            hosted: item.hosts.iter().map(|(subtree, _)| *subtree).collect(),
        });
        Some((item.key.clone(), hash, heights))
    }
    let mut nodes = Vec::new();
    let root = build(items, &mut nodes);
    let (root_key, root_hash) = root.map_or((None, NULL_HASH), |(key, hash, _)| (Some(key), hash));
    (nodes, root_key, root_hash)
}

const FEATURE: &[u8] = &[0];
const AGGREGATE: &[u8] = &[0];

/// The change set of `subtree` with the nodes `nodes`.
fn puts(nodes: &[Built]) -> Vec<NodePut<'_>> {
    fn link(link: &Option<(Vec<u8>, Hash, (u8, u8))>) -> Option<LinkPut<'_>> {
        link.as_ref().map(|(key, hash, heights)| LinkPut {
            key: key.as_slice(),
            hash,
            heights: *heights,
            aggregate: AGGREGATE,
        })
    }
    (nodes.iter())
        .map(|node| NodePut {
            key: &node.key,
            feature: FEATURE,
            value_hash: &node.value_hash,
            kv_hash: &node.kv_hash,
            left: link(&node.left),
            right: link(&node.right),
            hosted: Hosted::Set(&node.hosted),
            value: ValuePut::Value(&node.value),
            stale: Stale::NONE,
            at: None,
        })
        .collect()
}

/// What the fixture holds besides its Merks.
fn long_value(number: u8) -> Vec<u8> {
    let mut value = vec![LONG_BYTE; LONG];
    value[0] = number;
    value
}

/// Writes a grove of GroveDB's shape into `db`, in one commit: a root Merk
/// whose element under [`DOCS_KEY`] hosts the Merk [`DOCS`] of
/// `documents` items, every fourth of them long enough for the value
/// log, and the second the host of the Merk [`MID`] of one node, which hosts
/// the Merk [`SUB`] of five; a subtree [`FLAT`] of flat records; aux and meta records.
fn grove(db: &Db, documents: u32) {
    write_grove(db, documents, None);
}

/// Writes every node of the Merks of [`grove`] again, with values of the
/// `round`: the earlier values and pages are dead space.
fn rewrite(db: &Db, documents: u32, round: u8) {
    write_grove(db, documents, Some(round));
}

fn write_grove(db: &Db, documents: u32, round: Option<u8>) {
    let sub_items: Vec<Item> = (0u8..5)
        .map(|number| Item::element(&[b's', number], Element::new_item(vec![number; 20])))
        .collect();
    let (sub_nodes, sub_root, sub_hash) = merk(&sub_items);
    let mut mid_item = Item::element(b"m", Element::new_tree(sub_root.clone()));
    mid_item.hosts = Some((SUB, sub_hash));
    let (mid_nodes, mid_root, mid_hash) = merk(&[mid_item]);
    let docs: Vec<Item> = (0..documents)
        .map(|number| {
            if number == 1 {
                let mut host = Item::element(b"doc00001", Element::new_tree(mid_root.clone()));
                host.hosts = Some((MID, mid_hash));
                return host;
            }
            let mut value = match number % 4 {
                0 => long_value(number as u8),
                _ => format!("document {number}").into_bytes(),
            };
            value.extend(round);
            Item::element(
                format!("doc{number:05}").as_bytes(),
                Element::new_item(value),
            )
        })
        .collect();
    let (doc_nodes, doc_root, doc_hash) = merk(&docs);
    let mut root_items = vec![
        Item::element(b"a", Element::new_item(b"short".to_vec())),
        Item::element(b"big", Element::new_item(long_value(0xb0))),
    ];
    let mut host = Item::element(DOCS_KEY, Element::new_tree(doc_root.clone()));
    host.hosts = Some((DOCS, doc_hash));
    root_items.push(host);
    root_items.push(Item::element(b"z", Element::new_item(b"last".to_vec())));
    let (root_nodes, root_root, _) = merk(&root_items);
    let (root_puts, doc_puts) = (puts(&root_nodes), puts(&doc_nodes));
    let (mid_puts, sub_puts) = (puts(&mid_nodes), puts(&sub_nodes));
    let merks = [
        MerkWrite {
            subtree: ROOT,
            nodes: &root_puts,
            removed: &[],
            root: RootPut::Set(root_root.as_deref()),
            anchored: false,
            whole: false,
        },
        MerkWrite {
            subtree: DOCS,
            nodes: &doc_puts,
            removed: &[],
            root: RootPut::Set(doc_root.as_deref()),
            anchored: false,
            whole: false,
        },
        MerkWrite {
            subtree: MID,
            nodes: &mid_puts,
            removed: &[],
            root: RootPut::Set(mid_root.as_deref()),
            anchored: false,
            whole: false,
        },
        MerkWrite {
            subtree: SUB,
            nodes: &sub_puts,
            removed: &[],
            root: RootPut::Set(sub_root.as_deref()),
            anchored: false,
            whole: false,
        },
    ];
    let enters = [
        SubtreeWrite::Enter {
            subtree: ROOT,
            tree_type: TREE,
            host: None,
        },
        SubtreeWrite::Enter {
            subtree: DOCS,
            tree_type: TREE,
            host: Some(HostPut {
                subtree: ROOT,
                key: DOCS_KEY,
                position: 0,
            }),
        },
        SubtreeWrite::Enter {
            subtree: MID,
            tree_type: TREE,
            host: Some(HostPut {
                subtree: DOCS,
                key: b"doc00001",
                position: 0,
            }),
        },
        SubtreeWrite::Enter {
            subtree: SUB,
            tree_type: TREE,
            host: Some(HostPut {
                subtree: MID,
                key: b"m",
                position: 0,
            }),
        },
        SubtreeWrite::Enter {
            subtree: FLAT,
            tree_type: FLAT_TREE,
            host: None,
        },
    ];
    let subtrees = match round {
        None => &enters[..],
        Some(_) => &[],
    };
    let flat: Vec<(Vec<u8>, Vec<u8>)> = (0u32..40)
        .map(|number| {
            (
                number.to_be_bytes().to_vec(),
                format!("entry {number}").into_bytes(),
            )
        })
        .collect();
    // Longer than a page: out of line in a flat keyspace too.
    let long = vec![0xf0; 40_000];
    let mut records: Vec<RecordWrite<'_>> = (flat.iter())
        .map(|(key, value)| RecordWrite::put(Column::Data, FLAT, key, value))
        .collect();
    records.push(RecordWrite::put(
        Column::Aux,
        ROOT,
        b"aux key",
        b"aux value",
    ));
    records.push(RecordWrite::put(Column::Meta, FLAT, b"height", b"7"));
    records.push(RecordWrite::put(Column::Roots, DOCS, b"long", &long));
    db.write(
        &Batch {
            records: &records,
            merks: &merks,
            subtrees,
        },
        &mut NoStaleHashes,
    )
    .expect("the grove is written");
}

/// A directory for a database, in the temporary directory.
fn temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a directory")
}

fn small() -> Options {
    Options {
        cache_size: 8 << 20,
        ..Options::default()
    }
}

/// A database in `dir` with [`grove`] of `documents`, closed.
fn fixture(dir: &Path, documents: u32) -> PathBuf {
    let path = dir.join("db");
    let db = Db::open(&path, &small()).expect("a new database");
    grove(&db, documents);
    db.close().expect("a close");
    path
}

fn words(line: &str) -> Vec<OsString> {
    line.split_whitespace().map(OsString::from).collect()
}

/// Runs `line` and returns its outcome and report.
fn run(line: &str) -> (Result<Outcome, ToolError>, String) {
    let mut out = Vec::new();
    let outcome = crate::run(&words(line), &mut out);
    (outcome, String::from_utf8(out).expect("text"))
}

/// Runs `line`, which must succeed with `outcome`, and returns its report.
fn ran(line: &str, outcome: Outcome) -> String {
    let (result, report) = run(line);
    match result {
        Ok(got) => assert_eq!(got, outcome, "{line}:\n{report}"),
        Err(error) => panic!("{line}: {error}\n{report}"),
    }
    report
}

fn contains(report: &str, parts: &[&str]) {
    for part in parts {
        assert!(report.contains(part), "{part:?} in:\n{report}");
    }
}

#[test]
fn the_arguments_name_each_subcommand_and_nothing_else() {
    let parse = |line: &str| args::parse(&words(line));
    let dir = PathBuf::from("d");
    let open = |cache_size| Common {
        dir: dir.clone(),
        cache_size,
    };
    assert_eq!(parse("help").unwrap(), Command::Help);
    assert_eq!(
        parse("manifest d").unwrap(),
        Command::Manifest { dir: dir.clone() }
    );
    assert_eq!(
        parse("segments d --blocks").unwrap(),
        Command::Segments {
            dir: dir.clone(),
            blocks: true
        }
    );
    assert_eq!(
        parse("pages d 3 9 --cells --cache 2").unwrap(),
        Command::Pages {
            common: open(2 << 20),
            pages: [3, 9]
                .map(|page| lignin_format::PageId::new(page).unwrap())
                .to_vec(),
            cells: true
        }
    );
    assert_eq!(
        parse("verify d --no-hashes --beside-background").unwrap(),
        Command::Verify {
            common: open(args::DEFAULT_CACHE),
            hashes: false,
            beside_background: true
        }
    );
    assert_eq!(
        parse("compact d --timeout 5").unwrap(),
        Command::Compact {
            common: open(args::DEFAULT_CACHE),
            timeout: std::time::Duration::from_secs(5)
        }
    );
    let prefix = "07".repeat(32);
    assert_eq!(
        parse(&format!("dump d {prefix} f")).unwrap(),
        Command::Dump {
            common: open(args::DEFAULT_CACHE),
            what: DumpWhat::Subtree(DOCS),
            file: "f".into()
        }
    );
    assert_eq!(
        parse("dump d --all f").unwrap(),
        Command::Dump {
            common: open(args::DEFAULT_CACHE),
            what: DumpWhat::All,
            file: "f".into()
        }
    );
    assert_eq!(
        parse("load d f").unwrap(),
        Command::Load {
            common: open(args::DEFAULT_CACHE),
            file: "f".into()
        }
    );
    assert!(matches!(
        parse("directory d").unwrap(),
        Command::Directory { .. }
    ));
    assert!(matches!(parse("stats d").unwrap(), Command::Stats { .. }));

    for (line, says) in [
        ("", "no subcommand"),
        ("frobnicate d", "no subcommand \"frobnicate\""),
        ("manifest", "manifest takes one directory"),
        ("manifest a b", "manifest takes one directory"),
        ("manifest d --cache 4", "manifest takes no --cache"),
        ("stats d --blocks", "stats takes no --blocks"),
        ("segments d --cells", "segments takes no --cells"),
        ("pages d --no-hashes", "pages takes no --no-hashes"),
        (
            "stats d --beside-background",
            "stats takes no --beside-background",
        ),
        ("stats d --all", "stats takes no --all"),
        ("stats d --timeout 3", "stats takes no --timeout"),
        ("pages", "pages takes a directory"),
        ("pages d 0", "page ids begin at 1"),
        ("pages d x", "\"x\" is not a page id"),
        ("stats d --cache", "--cache takes a value"),
        ("stats d --cache 0", "--cache takes 1 MiB or more"),
        (
            "compact d --timeout soon",
            "\"soon\" is not a number of seconds",
        ),
        ("stats d --verbose", "no option --verbose"),
        ("dump d f", "dump takes DIR, a SUBTREE or --all, and FILE"),
        ("dump d 0707 f", "\"0707\" is not a subtree: 64 hex digits"),
        ("load d", "load takes DIR and FILE"),
        ("help me", "no subcommand \"help\""),
    ] {
        match parse(line) {
            Err(ToolError::Usage(what)) => assert_eq!(what, says, "{line}"),
            other => panic!("{line}: {other:?}"),
        }
    }
    let not_hex = format!("{}zz", "0".repeat(62));
    assert!(matches!(args::subtree(&not_hex), Err(ToolError::Usage(_))));
    assert!(matches!(
        args::subtree(&"é".repeat(32)),
        Err(ToolError::Usage(_))
    ));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0xff]);
        let parsed = args::parse(&[OsString::from("manifest"), bad]);
        assert!(matches!(parsed, Err(ToolError::Usage(what)) if what.contains("not UTF-8")));
    }
}

/// `migrate` takes a source and a target, or a source to survey, and its
/// options are refused by every other subcommand.
#[test]
fn the_arguments_of_migrate() {
    use std::time::Duration;

    use args::Unreachable;

    let parse = |line: &str| args::parse(&words(line));
    assert_eq!(
        parse("migrate s t --batch 2 --progress 0 --unreachable carry --cache 3").unwrap(),
        Command::Migrate {
            source: "s".into(),
            target: Some("t".into()),
            cache_size: 3 << 20,
            batch_bytes: 2 << 20,
            progress: Duration::ZERO,
            unreachable: Unreachable::Carry,
        }
    );
    assert_eq!(
        parse("migrate s --survey --unreachable leave").unwrap(),
        Command::Migrate {
            source: "s".into(),
            target: None,
            cache_size: args::DEFAULT_CACHE,
            batch_bytes: args::DEFAULT_MIGRATE_BATCH,
            progress: args::DEFAULT_MIGRATE_PROGRESS,
            unreachable: Unreachable::Leave,
        }
    );
    assert!(matches!(
        parse("migrate s t").unwrap(),
        Command::Migrate {
            unreachable: Unreachable::Refuse,
            ..
        }
    ));
    for choice in Unreachable::ALL {
        assert_eq!(Unreachable::named(choice.name()), Some(choice));
    }
    assert_eq!(Unreachable::named("maybe"), None);
    assert_eq!(Unreachable::default(), Unreachable::Refuse);

    for (line, says) in [
        ("migrate s", "migrate takes SOURCE and TARGET"),
        ("migrate s t u", "migrate takes SOURCE and TARGET"),
        (
            "migrate s t --survey",
            "migrate --survey takes SOURCE and no other argument",
        ),
        (
            "migrate s --survey --batch 2",
            "migrate --survey takes SOURCE and no other argument",
        ),
        (
            "migrate s --survey --cache 2",
            "migrate --survey takes SOURCE and no other argument",
        ),
        (
            "migrate s --survey --progress 2",
            "migrate --survey takes SOURCE and no other argument",
        ),
        ("migrate s t --batch 0", "--batch takes 1 MiB or more"),
        ("migrate s t --batch", "--batch takes a value"),
        (
            "migrate s t --progress soon",
            "\"soon\" is not a number of seconds",
        ),
        (
            "migrate s t --unreachable maybe",
            "--unreachable takes refuse, leave or carry",
        ),
        ("migrate s t --unreachable", "--unreachable takes a value"),
        ("stats d --survey", "stats takes no --survey"),
        ("stats d --batch 1", "stats takes no --batch"),
        ("stats d --progress 1", "stats takes no --progress"),
        (
            "stats d --unreachable leave",
            "stats takes no --unreachable",
        ),
    ] {
        match parse(line) {
            Err(ToolError::Usage(what)) => assert_eq!(what, says, "{line}"),
            other => panic!("{line}: {other:?}"),
        }
    }
    assert_eq!(Outcome::IncompleteMigration.status(), 4);
    contains(
        args::USAGE,
        &[
            "migrate   SOURCE TARGET [--batch MIB] [--progress SECS]",
            "migrate   SOURCE --survey",
            "is made again at such a path is refused by the engine as",
            "left without stopping is migrated as the state that node",
            "2 the arguments are not a",
            "A SOURCE with a keyed child under a tree of flat records",
            "keeps a node from starting on it while it runs",
        ],
    );
}

/// `migrate` is in a build with the feature of its name. In another build
/// the command says so, and in one that has it a source that is no
/// RocksDB database is refused in the tool's words.
#[test]
fn migrate_is_in_a_build_with_its_feature() {
    let dir = temp();
    let line = format!("migrate {} --survey", dir.path().join("none").display());
    let (outcome, report) = run(&line);
    let error = outcome
        .expect_err("nothing is there to migrate")
        .to_string();
    assert_eq!(report, "");
    #[cfg(feature = "migrate")]
    assert!(error.starts_with("the migration: "), "{error}");
    #[cfg(not(feature = "migrate"))]
    assert_eq!(
        error,
        "this build of lignin has no migrate: build lignin-tools with the Cargo feature migrate"
    );
}

/// The command line: the report to one writer, an error to the other, and
/// the exit status by what happened.
#[test]
fn the_command_line_exits_with_the_status_of_what_happened() {
    let main = |line: &str| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = std::iter::once(OsString::from("lignin")).chain(words(line));
        let status = crate::main(args, &mut out, &mut err);
        let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("text");
        (status, text(out), text(err))
    };
    let (status, out, err) = main("help");
    assert_eq!((status, out.as_str(), err.as_str()), (0, args::USAGE, ""));
    let (status, out, err) = main("stats");
    assert_eq!((status, out.as_str()), (2, ""));
    assert!(
        err.starts_with("lignin: stats takes one directory\nusage:"),
        "{err}"
    );
    let dir = temp();
    let (status, _, err) = main(&format!("stats {}", dir.path().join("none").display()));
    assert_eq!(status, 1);
    assert!(
        err.starts_with("lignin: the database did not open: "),
        "{err}"
    );
    assert!(!err.contains("usage"), "{err}");
    assert_eq!(Outcome::NotSound.status(), 3);
    assert_eq!(Outcome::IncompleteRestore.status(), 4);
}

/// The mark of an incomplete restore is told, the hashes are not made
/// again, and the status is 4 whatever the check finds.
#[test]
fn a_database_with_the_mark_of_an_incomplete_restore_is_told_to_be_discarded() {
    let dir = temp();
    let path = fixture(dir.path(), 9);
    let db = Db::open(&path, &small()).expect("the database");
    let mark = [RecordWrite::put(
        Column::Aux,
        ROOT,
        INCOMPLETE_RESTORE_KEY,
        b"1",
    )];
    let batch = Batch {
        records: &mark,
        ..Batch::default()
    };
    db.write(&batch, &mut NoStaleHashes).expect("the mark");
    db.close().expect("a close");
    for flags in ["", " --beside-background"] {
        let report = ran(
            &format!("verify {}{flags}", path.display()),
            Outcome::IncompleteRestore,
        );
        contains(
            &report,
            &[
                "INCOMPLETE RESTORE: the root holds the mark",
                "discard it",
                "no hash made again",
                "sound: every rule holds",
            ],
        );
    }
}
