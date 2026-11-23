//! The subcommands that look at a database.

use std::{
    fs,
    path::{Path, PathBuf},
};

use grovedb_element::Element;
use grovedb_merk::{
    store::lignin::tree_type_bytes,
    tree::{
        combine_hash, kv_digest_to_kv_hash, node_hash, node_hash_with_count, value_hash, NULL_HASH,
    },
    TreeType,
};
use grovedb_version::version::GroveVersion;
use lignin_format::tree_page::Hash;
use lignin_store::cleaner::CleanerConfig;
use lignin_tree::hash::Stale;
use lignindb::{
    commit::NoStaleHashes,
    txn::{
        Batch, Column, HostPut, Hosted, MerkWrite, NodePut, RecordWrite, RootPut, Subtree,
        SubtreeWrite, ValuePut,
    },
    Db, Options,
};

use super::{contains, fixture, ran, run, small, temp, DOCS, FEATURE, FLAT, LONG_BYTE, TREE};
use crate::{hex, verify::ROOT, Outcome, ToolError};

#[test]
fn the_manifest_and_the_segments_are_read_from_the_files() {
    let dir = temp();
    let path = fixture(dir.path(), 400);
    let shown = path.display();
    let report = ran(&format!("manifest {shown}"), Outcome::Done);
    contains(
        &report,
        &[
            "format version V1\n",
            "page size 32768 bytes\n",
            "last file ids: log ",
            "table checkpoint: table/",
            "live files: ",
            "wal/",
            "seg/",
            "val/",
            " closed ",
        ],
    );
    assert!(
        !report.contains("MISSING") && !report.contains("SHORT"),
        "{report}"
    );

    let report = ran(&format!("segments {shown}"), Outcome::Done);
    contains(
        &report,
        &["segments: ", " page images, ", " bytes stored, "],
    );
    assert!(!report.contains("  at "), "{report}");
    let blocks = ran(&format!("segments {shown} --blocks"), Outcome::Done);
    contains(&blocks, &["  at 20 page ", " image ", " bytes ("]);

    // A block whose bytes changed is found by its checksum, and the
    // listing says where.
    let segment = fs::read_dir(path.join("seg"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = fs::read(&segment).unwrap();
    bytes[30] ^= 0xff;
    fs::write(&segment, &bytes).unwrap();
    let (result, report) = run(&format!("segments {shown}"));
    contains(&report, &["DAMAGED at byte "]);
    let error = result.expect_err("a damaged segment");
    assert!(matches!(error, ToolError::Format { .. }), "{error:?}");
    assert!(error.to_string().contains("seg/"), "{error}");

    // A file the manifest names that is gone is said by both.
    fs::remove_file(&segment).unwrap();
    let report = ran(&format!("manifest {shown}"), Outcome::Done);
    contains(&report, &[", MISSING", "files are missing or shorter"]);
    let (result, report) = run(&format!("segments {shown}"));
    contains(&report, &["missing or shorter than its"]);
    assert!(matches!(result, Err(ToolError::Format { .. })));
}

#[test]
fn a_directory_with_no_manifest_is_refused_by_the_readers_of_files() {
    let dir = temp();
    for subcommand in ["manifest", "segments"] {
        let (result, _) = run(&format!("{subcommand} {}", dir.path().display()));
        let error = result.expect_err("no manifest");
        assert!(matches!(error, ToolError::Files(_)), "{error:?}");
        assert_eq!(error.status(), 1);
    }
}

/// A part of a checkpoint and a database that is being removed are no
/// databases, for every subcommand, and nothing of them is read.
#[test]
fn a_part_of_a_checkpoint_and_a_directory_being_removed_are_refused() {
    let dir = temp();
    let creating = dir.path().join("checkpoint.Creating");
    fs::create_dir(&creating).unwrap();
    let deleting = fixture(dir.path(), 3);
    let dumped = dir.path().join("all.dump");
    ran(
        &format!("dump {} --all {}", deleting.display(), dumped.display()),
        Outcome::Done,
    );
    fs::write(deleting.join("MANIFEST.deleting"), b"").unwrap();
    let not_written = dir.path().join("refused.dump");
    for subcommand in [
        "manifest {}",
        "segments {}",
        "pages {}",
        "directory {}",
        "stats {}",
        "verify {}",
        "compact {}",
        "dump {} --all REFUSED",
        "load {} DUMPED",
    ] {
        for (target, says) in [
            (&creating, "a checkpoint that is being made"),
            (&deleting, "MANIFEST.deleting"),
        ] {
            let line = subcommand
                .replace("{}", &target.display().to_string())
                .replace("REFUSED", &not_written.display().to_string())
                .replace("DUMPED", &dumped.display().to_string());
            let (result, _) = run(&line);
            let error = result.expect_err("no database");
            assert!(
                matches!(error, ToolError::NotADatabase { .. }),
                "{subcommand}: {error:?}"
            );
            assert!(error.to_string().contains(says), "{error}");
            assert!(std::error::Error::source(&error).is_none());
            assert!(!not_written.exists(), "{subcommand}");
        }
    }
    assert_eq!(
        fs::read_dir(&creating).unwrap().count(),
        0,
        "nothing was made in it"
    );
}

#[test]
fn a_database_that_a_process_has_open_is_not_opened_again() {
    let dir = temp();
    let path = fixture(dir.path(), 3);
    let db = Db::open(&path, &small()).expect("the database");
    let (result, _) = run(&format!("stats {}", path.display()));
    let error = result.expect_err("locked");
    assert!(
        error.to_string().contains("the database did not open"),
        "{error}"
    );
    assert!(std::error::Error::source(&error).is_some());
    db.close().expect("a close");
}

#[test]
fn a_directory_with_no_database_gets_none() {
    let dir = temp();
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    for subcommand in ["pages", "directory", "stats", "verify", "compact"] {
        let (result, _) = run(&format!("{subcommand} {}", empty.display()));
        assert!(
            matches!(result, Err(ToolError::Open(_))),
            "{subcommand}: {result:?}"
        );
    }
    assert_eq!(
        fs::read_dir(&empty).unwrap().count(),
        0,
        "no database was made"
    );
}

#[test]
fn the_pages_are_decoded_tree_and_flat() {
    let dir = temp();
    let path = fixture(dir.path(), 400);
    let shown = path.display();
    let report = ran(&format!("pages {shown}"), Outcome::Done);
    contains(
        &report,
        &[
            "version ",
            "page 1 (born at version ",
            "flat leaf, ",
            "tree page, ",
        ],
    );
    assert!(!report.contains("  slot "), "{report}");
    let cells = ran(&format!("pages {shown} --cells"), Outcome::Done);
    contains(
        &cells,
        &[
            "  slot 1 key ",
            " bytes in the cell",
            " bytes in val/",
            "; hosts ",
            "; fragment root",
            "heights ",
            "  key ",
            " bytes in the page",
            "value in the value log",
            &format!(
                "  key {}{} value 9 bytes",
                hex(ROOT.as_bytes()),
                hex(b"aux key")
            ),
        ],
    );
    let one = ran(&format!("pages {shown} 1 --cells"), Outcome::Done);
    contains(&one, &["page 1 (born at version 1): flat leaf, "]);
    assert!(!one.contains("page 2 "), "{one}");
    // An id that is named and has no page is told and is the error, after
    // the pages that are there.
    for line in [
        format!("pages {shown} 999999"),
        format!("pages {shown} 999999 1"),
    ] {
        let (result, report) = run(&line);
        assert!(matches!(result, Err(ToolError::Page(_))), "{result:?}");
        contains(&report, &["page 999999: no page at this version"]);
        assert_eq!(
            report.contains("page 1 (born"),
            line.ends_with(" 1"),
            "{report}"
        );
    }
}

#[test]
fn the_directory_lists_every_entry_with_its_host() {
    let dir = temp();
    let path = fixture(dir.path(), 20);
    let report = ran(&format!("directory {}", path.display()), Outcome::Done);
    let line = |subtree: lignindb::txn::Subtree| {
        report
            .lines()
            .find(|line| line.starts_with(&hex(subtree.as_bytes())))
            .unwrap_or_else(|| panic!("{subtree} in:\n{report}"))
            .to_owned()
    };
    assert!(line(ROOT).contains(" type 00 root "), "{report}");
    assert!(!line(ROOT).contains("host"), "{report}");
    let docs = line(DOCS);
    assert!(
        docs.contains(&format!(
            " host {} key {} position 0 depth 1",
            hex(ROOT.as_bytes()),
            hex(b"docs")
        )),
        "{docs}"
    );
    assert!(
        line(FLAT).contains(&format!(" type {} empty", hex(b"flat"))),
        "{report}"
    );
    contains(
        &report,
        &["5 entries: 5 in use, 0 left over; 2 with no host; the deepest below 3 others"],
    );
}

#[test]
fn the_counts_are_printed_whole_with_the_files_on_disk() {
    let dir = temp();
    let path = fixture(dir.path(), 40);
    let report = ran(&format!("stats {}", path.display()), Outcome::Done);
    contains(
        &report,
        &[
            "opened in ",
            "values read from the value log: ",
            "pool memory: ",
            "page ids: ",
            "cleaner: ",
            "on disk: wal/ ",
            " seg/ ",
            " val/ ",
            " table/ ",
        ],
    );
}

#[test]
fn verify_makes_every_hash_again_and_finds_a_value_that_changed() {
    let dir = temp();
    let path = fixture(dir.path(), 200);
    let shown = path.display();
    for (flags, says) in [
        (
            "",
            "checked the still store, after the close, every hash made again by Merk's rules",
        ),
        (
            " --beside-background",
            "checked beside the background work, every hash made again",
        ),
        (" --no-hashes", "no hash made again"),
    ] {
        let report = ran(&format!("verify {shown}{flags}"), Outcome::Done);
        contains(&report, &[says, "sound: every rule holds"]);
        if flags != " --no-hashes" {
            assert!(!report.contains("the hashes of 0 cells"), "{report}");
        }
    }

    // One byte of a value in the value log changes: its record's checksum
    // finds it.
    let file = fs::read_dir(path.join("val"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = fs::read(&file).unwrap();
    let run_of = [LONG_BYTE; 64];
    let at = bytes
        .windows(64)
        .position(|window| window == run_of)
        .expect("a long value");
    bytes[at + 32] ^= 1;
    fs::write(&file, &bytes).unwrap();
    for flags in ["", " --beside-background"] {
        let report = ran(&format!("verify {shown}{flags}"), Outcome::NotSound);
        contains(&report, &["NOT SOUND: "]);
    }
}

#[test]
fn compact_moves_what_is_live_and_leaves_a_sound_store() {
    let dir = temp();
    let path = dir.path().join("db");
    // The sessions that make the store run a cleaner that moves no block
    // (no share of dead bytes is over 100 in a hundred) and cleans no
    // value-log file, whenever its passes run. Every document is written
    // again a few times, each time in a session of its own, whose close
    // makes what it changed durable. The last session writes a new value
    // under an aux key of the root: the page that holds it is written
    // anew, and the segment that held its earlier image, which no session
    // writes any more, holds that dead block beside live ones. Only the
    // cleaner of `compact` moves them.
    let still = CleanerConfig {
        target_dead_percent: 100,
        checkpoint_dead_percent: 100,
        seal_dead_percent: 100,
        value_dead_percent: 100,
        ..CleanerConfig::new()
    };
    let options = Options {
        cache_size: 8 << 20,
        log_file_len: Some(256 << 10),
        cleaner: Some(still),
        ..Options::default()
    };
    let db = Db::open(&path, &options).expect("a new database");
    super::grove(&db, 300);
    db.close().expect("a close");
    for round in 0..4 {
        let db = Db::open(&path, &options).expect("the database");
        super::rewrite(&db, 300, round);
        db.close().expect("a close");
    }
    let db = Db::open(&path, &options).expect("the database");
    let record = [RecordWrite::put(Column::Aux, ROOT, b"aux key", b"another")];
    let batch = Batch {
        records: &record,
        ..Batch::default()
    };
    db.write(&batch, &mut NoStaleHashes).expect("the record");
    db.close().expect("a close");
    let report = ran(
        &format!("compact {} --timeout 120", path.display()),
        Outcome::Done,
    );
    contains(
        &report,
        &[
            "before: on disk: ",
            "cleaner: ",
            "after: on disk: ",
            " bytes on disk before, ",
        ],
    );
    assert!(!report.contains("the timeout ended the work"), "{report}");
    // The segment with a dead block beside live ones was cleaned.
    let counted = |what: &str| -> u64 {
        let before = &report[..report.find(what).expect(what)];
        let number = before
            .trim_end()
            .rsplit([' ', '('])
            .next()
            .expect("a number");
        number.parse().expect("a count")
    };
    assert!(counted(" blocks moved") > 0, "{report}");
    let verified = ran(&format!("verify {}", path.display()), Outcome::Done);
    contains(&verified, &["sound: every rule holds"]);

    // A timeout ends the work, and says so; one past what a clock holds
    // is refused before anything is opened.
    let report = ran(
        &format!("compact {} --timeout 0", path.display()),
        Outcome::Done,
    );
    contains(&report, &["the timeout ended the work before a whole pass"]);
    let (result, _) = run(&format!(
        "compact {} --timeout {}",
        path.display(),
        u64::MAX
    ));
    let error = result.expect_err("too long");
    assert!(matches!(error, ToolError::Usage(_)), "{error:?}");
}

/// The Merk of one node that the provable count tree of
/// [`counted_grove`] hosts.
const COUNTED: Subtree = Subtree::new([5; 32]);

/// How [`counted_grove`] makes the value hash of its provable count tree.
#[derive(Clone, Copy, PartialEq)]
enum HostHash {
    /// As Merk's rules do: with the root's hash, which takes its count.
    OfTheRules,
    /// With the root's hash made with no count: what the delete of a tree
    /// with elements left on grove versions 1 to 3.
    Left,
}

/// The one node of [`COUNTED`] with the item `value`: its change set, and
/// the hash of the node with its count and without.
fn counted_node(value: &[u8], write: impl FnOnce(MerkWrite<'_>)) -> (Hash, Hash) {
    // A provable counted node that counts one (`tree-page.md` TP12).
    let mut feature = vec![5];
    feature.extend(1u64.to_le_bytes());
    let value = Element::new_item(value.to_vec())
        .serialize(GroveVersion::latest())
        .expect("serialized");
    let own = value_hash(&value).unwrap();
    let kv_hash = kv_digest_to_kv_hash(b"k", &own).unwrap();
    let nodes = [NodePut {
        key: b"k",
        feature: &feature,
        value_hash: &own,
        kv_hash: &kv_hash,
        left: None,
        right: None,
        hosted: Hosted::Set(&[]),
        value: ValuePut::Value(&value),
        stale: Stale::NONE,
        at: None,
    }];
    write(MerkWrite {
        subtree: COUNTED,
        nodes: &nodes,
        removed: &[],
        root: RootPut::Set(Some(b"k")),
        anchored: true,
        whole: false,
    });
    (
        node_hash_with_count(&kv_hash, &NULL_HASH, &NULL_HASH, 1).unwrap(),
        node_hash(&kv_hash, &NULL_HASH, &NULL_HASH).unwrap(),
    )
}

/// A database in `dir` whose root Merk holds one element, a provable
/// count tree, with the Merk of one counted item that the tree hosts.
/// Closed, with no check at the close: `verify` makes the check.
fn counted_grove(dir: &Path, host: HostHash) -> PathBuf {
    let path = dir.join("db");
    let db = Db::open(&path, &small()).expect("a new database");
    let (with_count, with_no_count) = counted_node(b"first", |_| ());
    let tree = Element::new_provable_count_tree(Some(b"k".to_vec()))
        .serialize(GroveVersion::latest())
        .expect("serialized");
    let root = match host {
        HostHash::OfTheRules => with_count,
        HostHash::Left => with_no_count,
    };
    let host_hash = combine_hash(&value_hash(&tree).unwrap(), &root).unwrap();
    let kv_hash = kv_digest_to_kv_hash(b"p", &host_hash).unwrap();
    let hosted = [COUNTED];
    let host_nodes = [NodePut {
        key: b"p",
        feature: FEATURE,
        value_hash: &host_hash,
        kv_hash: &kv_hash,
        left: None,
        right: None,
        hosted: Hosted::Set(&hosted),
        value: ValuePut::Value(&tree),
        stale: Stale::NONE,
        at: None,
    }];
    let tree_type = tree_type_bytes(TreeType::ProvableCountTree);
    let enters = [
        SubtreeWrite::Enter {
            subtree: ROOT,
            tree_type: TREE,
            host: None,
        },
        SubtreeWrite::Enter {
            subtree: COUNTED,
            tree_type: &tree_type,
            host: Some(HostPut {
                subtree: ROOT,
                key: b"p",
                position: 0,
            }),
        },
    ];
    counted_node(b"first", |counted| {
        let merks = [
            MerkWrite {
                subtree: ROOT,
                nodes: &host_nodes,
                removed: &[],
                root: RootPut::Set(Some(b"p")),
                anchored: false,
                whole: false,
            },
            MerkWrite {
                anchored: false,
                ..counted
            },
        ];
        let batch = Batch {
            records: &[],
            merks: &merks,
            subtrees: &enters,
        };
        db.write(&batch, &mut NoStaleHashes)
            .expect("the grove is written");
    });
    db.unchecked_at_close();
    db.close().expect("a close");
    path
}

/// The line `verify` adds for the value hash that a delete on grove
/// versions 1 to 3 left, up to the cell.
const LEFT_LINE: &str = "1 value hash of a tree is the one grove versions 1 to 3 left after a \
                         delete below it (GroveDB #686), not the one the rules make: cell ";

/// A database that holds the value hash which the delete of a tree with
/// elements left on grove versions 1 to 3 is no damaged store: `verify`
/// finds it sound, says how many such hashes there are and in which
/// cells, and ends with the status of a sound store. A write below the
/// tree that leaves the hash of its host behind is found, whichever hash
/// the host had.
#[test]
fn verify_tells_the_hash_a_delete_left_from_a_hash_that_was_left_behind() {
    for host in [HostHash::OfTheRules, HostHash::Left] {
        let dir = temp();
        let path = counted_grove(dir.path(), host);
        let shown = path.display();
        for flags in ["", " --beside-background"] {
            let report = ran(&format!("verify {shown}{flags}"), Outcome::Done);
            contains(
                &report,
                &["every hash made again", "sound: every rule holds"],
            );
            assert!(!report.contains("NOT SOUND"), "{report}");
            assert!(!report.to_lowercase().contains("damage"), "{report}");
            match host {
                HostHash::OfTheRules => {
                    assert!(!report.contains("grove versions 1 to 3"), "{report}");
                    assert!(!report.contains("another hash"), "{report}");
                }
                HostHash::Left => {
                    let line = report.lines().last().expect("a last line");
                    let cell = line.strip_prefix(LEFT_LINE);
                    let cell = cell.unwrap_or_else(|| panic!("{report}"));
                    // The cell is the one the report names, by its key.
                    contains(
                        &report,
                        &[
                            "as made with another hash of a hosted root: 1\n",
                            &format!("  cell {cell}, key 70\n"),
                        ],
                    );
                }
            }
        }
        // With no hash made again nothing is said of a hash.
        let report = ran(&format!("verify {shown} --no-hashes"), Outcome::Done);
        assert!(!report.contains("grove versions 1 to 3"), "{report}");

        // The item below the tree is written again, and nothing above it:
        // the value hash of the tree is behind the root of its Merk.
        let db = Db::open(&path, &small()).expect("the database");
        counted_node(b"second", |counted| {
            let batch = Batch {
                records: &[],
                merks: &[counted],
                subtrees: &[],
            };
            db.write(&batch, &mut NoStaleHashes)
                .expect("the write is taken");
        });
        db.unchecked_at_close();
        db.close().expect("a close");
        for flags in ["", " --beside-background"] {
            let report = ran(&format!("verify {shown}{flags}"), Outcome::NotSound);
            contains(
                &report,
                &[
                    "NOT SOUND: 1 findings",
                    "the stored hash differs from its recomputation (a value hash is the hash \
                     of its value)",
                ],
            );
            assert!(!report.contains("grove versions 1 to 3"), "{report}");
            assert!(!report.contains("another hash"), "{report}");
        }
        // The other rules of the store hold: it is the hash alone.
        ran(&format!("verify {shown} --no-hashes"), Outcome::Done);
    }
}
