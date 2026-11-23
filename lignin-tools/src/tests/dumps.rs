//! `dump` and `load`.

use std::{fs, path::Path};

use grovedb_element::Element;
use lignin_format::tree_page::Side;
use lignin_tree::{hash::Stale, value::ValueBuf};
use lignindb::{
    commit::NoStaleHashes,
    txn::{Batch, Hosted, LinkPut, MerkWrite, NodePut, RootPut, ValuePut},
    Db,
};

use super::{contains, fixture, grove, ran, run, small, temp, Item, AGGREGATE, DOCS, FEATURE};
use crate::{hex, Outcome, ToolError};

/// A database in `dir` with nothing in it.
fn empty(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    Db::open(&path, &small())
        .expect("a new database")
        .close()
        .expect("a close");
    path
}

/// The entries of the directory of `path`, without the addresses of the
/// roots, which a load chooses anew.
fn entries(path: &Path) -> Vec<String> {
    let report = ran(&format!("directory {}", path.display()), Outcome::Done);
    report
        .lines()
        .filter(|line| line.contains(" type "))
        .map(|line| {
            let mut words: Vec<&str> = line.split(' ').collect();
            if let Some(at) = words.iter().position(|word| *word == "root") {
                words.remove(at + 1);
            }
            words.join(" ")
        })
        .collect()
}

#[test]
fn a_whole_database_is_dumped_and_loaded_as_it_was() {
    let dir = temp();
    let source = fixture(dir.path(), 300);
    let target = empty(dir.path(), "target");
    let (first, second) = (
        dir.path().join("first.dump"),
        dir.path().join("second.dump"),
    );
    let dumped = ran(
        &format!("dump {} --all {}", source.display(), first.display()),
        Outcome::Done,
    );
    contains(
        &dumped,
        &["dumped version ", ": 5 subtrees, 310 nodes, 43 records, "],
    );
    let loaded = ran(
        &format!("load {} {}", target.display(), first.display()),
        Outcome::Done,
    );
    contains(
        &loaded,
        &[
            "loaded 5 subtrees, 310 nodes, 43 records from ",
            "committed version ",
        ],
    );

    // The loaded database holds what the source held, byte for byte as a
    // dump says it, and every hash in it is Merk's.
    ran(
        &format!("dump {} --all {}", target.display(), second.display()),
        Outcome::Done,
    );
    assert!(
        fs::read(&first).unwrap() == fs::read(&second).unwrap(),
        "the two dumps differ"
    );
    assert_eq!(entries(&source), entries(&target));
    let verified = ran(&format!("verify {}", target.display()), Outcome::Done);
    contains(&verified, &["sound: every rule holds"]);

    // A load into a database that has the subtrees is refused, and writes
    // nothing.
    let before = ran(&format!("stats {}", source.display()), Outcome::Done);
    let (result, _) = run(&format!("load {} {}", source.display(), first.display()));
    let error = result.expect_err("the subtrees are there");
    assert!(matches!(error, ToolError::Txn(_)), "{error:?}");
    let after = ran(&format!("stats {}", source.display()), Outcome::Done);
    assert_eq!(
        before.lines().next(),
        after.lines().next(),
        "the version is the same"
    );
}

/// A subtree below another is dumped with what is below it, and loaded as
/// a Merk that stands alone: the element that hosted it is not in the
/// dump.
#[test]
fn a_nested_subtree_is_loaded_with_no_host() {
    let dir = temp();
    let source = fixture(dir.path(), 50);
    let target = empty(dir.path(), "target");
    let (first, second) = (dir.path().join("docs.dump"), dir.path().join("again.dump"));
    let docs = hex(DOCS.as_bytes());
    let dumped = ran(
        &format!("dump {} {docs} {}", source.display(), first.display()),
        Outcome::Done,
    );
    contains(&dumped, &[": 3 subtrees, 56 nodes, 1 records, "]);
    ran(
        &format!("load {} {}", target.display(), first.display()),
        Outcome::Done,
    );
    // In the order of the prefixes: MID, DOCS, SUB.
    let listed = entries(&target);
    assert_eq!(listed.len(), 3, "{listed:?}");
    assert!(listed[0].contains(&format!(" host {docs} ")), "{listed:?}");
    assert!(
        listed[1].starts_with(&docs) && !listed[1].contains("host"),
        "{listed:?}"
    );
    assert!(
        listed[2].contains(&format!(" host {} ", hex(&[6; 32]))),
        "{listed:?}"
    );
    ran(
        &format!("dump {} {docs} {}", target.display(), second.display()),
        Outcome::Done,
    );
    assert!(
        fs::read(&first).unwrap() == fs::read(&second).unwrap(),
        "the two dumps differ"
    );
    let verified = ran(&format!("verify {}", target.display()), Outcome::Done);
    contains(&verified, &["sound: every rule holds"]);
}

/// A record of the data column of a Merk with a root that no node links
/// is dumped among the nodes of its Merk and loaded as such a record
/// again: found under its key, in no tree, with every part it was written
/// with, the key and the hash of each link among them.
#[test]
fn a_record_that_no_node_links_is_dumped_and_loaded_as_one() {
    const STRAY: &[u8] = b"doc00009x";
    let dir = temp();
    let source = dir.path().join("db");
    let db = Db::open(&source, &small()).expect("a new database");
    grove(&db, 20);
    // A node of the documents' Merk that the root does not reach, with
    // two links to keys that hold nothing.
    let stray = Item::element(STRAY, Element::new_item(b"in no tree".to_vec()));
    let hashes = [[1; 32], [2; 32], [3; 32], [4; 32]];
    let put = NodePut {
        key: STRAY,
        feature: FEATURE,
        value_hash: &hashes[0],
        kv_hash: &hashes[1],
        left: Some(LinkPut {
            key: b"nowhere",
            hash: &hashes[2],
            heights: (1, 0),
            aggregate: AGGREGATE,
        }),
        right: Some(LinkPut {
            key: b"nowhere either",
            hash: &hashes[3],
            heights: (0, 2),
            aggregate: AGGREGATE,
        }),
        hosted: Hosted::Set(&[]),
        value: ValuePut::Value(&stray.value),
        stale: Stale::NONE,
        at: None,
    };
    let beside = MerkWrite {
        subtree: DOCS,
        nodes: &[put],
        removed: &[],
        root: RootPut::Keep,
        anchored: true,
        whole: false,
    };
    db.write(
        &Batch {
            records: &[],
            merks: &[beside],
            subtrees: &[],
        },
        &mut NoStaleHashes,
    )
    .expect("the record is written");
    db.close().expect("a close");
    // Whether the record under the key is one that no node links, and
    // each of its parts.
    let held = |path: &Path| {
        let db = Db::open(path, &small()).expect("the database opens");
        let found = {
            let snapshot = db.snapshot();
            let operation = snapshot.operation();
            let docs = operation.open(DOCS).expect("a read").expect("the Merk");
            docs.node(STRAY, None, |view| {
                view.map(|view| {
                    let link = |side| {
                        let link = view.link(side).expect("a link");
                        let key = link.key.map(<[u8]>::to_vec);
                        (key, *link.hash, link.heights, link.aggregate.to_vec())
                    };
                    let value = view.value(&mut ValueBuf::new()).map(<[u8]>::to_vec);
                    (
                        view.is_unlinked(),
                        view.feature().to_vec(),
                        (*view.value_hash(), *view.kv_hash()),
                        [link(Side::Left), link(Side::Right)],
                        value.expect("the value"),
                    )
                })
            })
            .expect("a read")
        };
        db.close().expect("a close");
        found
    };
    let link = |key: &[u8], hash, heights| (Some(key.to_vec()), hash, heights, AGGREGATE.to_vec());
    let as_written = Some((
        true,
        FEATURE.to_vec(),
        (hashes[0], hashes[1]),
        [
            link(b"nowhere", hashes[2], (1, 0)),
            link(b"nowhere either", hashes[3], (0, 2)),
        ],
        stray.value.clone(),
    ));
    assert_eq!(held(&source), as_written);

    let target = empty(dir.path(), "target");
    let (first, second) = (
        dir.path().join("first.dump"),
        dir.path().join("second.dump"),
    );
    let dumped = ran(
        &format!("dump {} --all {}", source.display(), first.display()),
        Outcome::Done,
    );
    // The thirty nodes of the four Merks, and the record.
    contains(&dumped, &[": 5 subtrees, 31 nodes, 43 records, "]);
    ran(
        &format!("load {} {}", target.display(), first.display()),
        Outcome::Done,
    );
    assert_eq!(held(&target), as_written);
    ran(
        &format!("dump {} --all {}", target.display(), second.display()),
        Outcome::Done,
    );
    assert!(
        fs::read(&first).unwrap() == fs::read(&second).unwrap(),
        "the two dumps differ"
    );
    assert_eq!(entries(&source), entries(&target));
    let verified = ran(&format!("verify {}", target.display()), Outcome::Done);
    contains(&verified, &["sound: every rule holds"]);
}

#[test]
fn a_dump_that_is_not_whole_or_not_a_dump_is_refused_and_loads_nothing() {
    let dir = temp();
    let source = fixture(dir.path(), 30);
    let target = empty(dir.path(), "target");
    let file = dir.path().join("whole.dump");
    ran(
        &format!("dump {} --all {}", source.display(), file.display()),
        Outcome::Done,
    );
    let whole = fs::read(&file).unwrap();
    let broken = dir.path().join("broken.dump");
    let refused = |bytes: &[u8], says: &str| {
        fs::write(&broken, bytes).unwrap();
        let (result, _) = run(&format!("load {} {}", target.display(), broken.display()));
        let error = result.expect_err("refused");
        assert!(matches!(error, ToolError::Dump(_)), "{says}: {error:?}");
        assert!(error.to_string().contains(says), "{says}: {error}");
    };
    refused(
        &whole[..whole.len() - 3],
        "it ends before its end record: it was cut",
    );
    refused(&whole[..whole.len() / 2], "it was cut");
    refused(&whole[..5], "it was cut");
    refused(b"NOTADUMP\x01", "it is no dump of the lignin command");
    let mut later = whole.clone();
    later[8] = 2;
    refused(&later, "format 2, and this build reads 1");
    let mut more = whole.clone();
    more.push(0);
    refused(&more, "bytes follow its end record");
    let mut counted = whole.clone();
    let last = counted.len() - 1;
    counted[last] ^= 1;
    refused(&counted, "its end record counts");
    let mut flag = whole.clone();
    // The flag of the first subtree's host, after the magic, the format,
    // the tag, the prefix and the tree type.
    let at = 8 + 1 + 1 + 32;
    let at = at + 1 + usize::from(flag[at]);
    flag[at] = 7;
    refused(&flag, "7 where 0 or 1 belongs");

    // A dump that does not agree with itself, and that every count of its
    // end record agrees with. The hashes are none: the load refuses before
    // it writes a node.
    let (top, first, second) = ([1; 32], [2; 32], [3; 32]);
    let rooted = Dump::new()
        .subtree(top, None, Some(b"k"))
        .data(top)
        .end(1, 0, 0);
    refused(&rooted, "the root its entry gives");
    let unrooted = Dump::new()
        .subtree(top, None, None)
        .data(top)
        .node(b"k", &[])
        .end(1, 1, 0);
    refused(&unrooted, "the root its entry gives");
    // The node names the two Merks it hosts in the other order than their
    // entries give their positions.
    let swapped = Dump::new()
        .subtree(top, None, Some(b"k"))
        .subtree(first, Some((top, b"k", 0)), None)
        .subtree(second, Some((top, b"k", 1)), None)
        .data(top)
        .node(b"k", &[second, first])
        .data(first)
        .data(second)
        .end(3, 1, 0);
    refused(&swapped, "and that entry does not name the node there");
    // An entry whose host names no Merk.
    let unnamed = Dump::new()
        .subtree(top, None, Some(b"k"))
        .subtree(first, Some((top, b"k", 0)), None)
        .data(top)
        .node(b"k", &[])
        .data(first)
        .end(2, 1, 0);
    refused(&unnamed, "does not name it at position 0");
    assert!(entries(&target).is_empty(), "nothing was loaded");
}

/// The bytes of a dump, written by hand.
struct Dump(Vec<u8>);

impl Dump {
    fn new() -> Self {
        let mut bytes = b"LIGNDUMP".to_vec();
        bytes.push(1);
        Self(bytes)
    }

    fn bytes32(mut self, bytes: &[u8]) -> Self {
        self.0.extend((bytes.len() as u32).to_le_bytes());
        self.0.extend(bytes);
        self
    }

    fn subtree(
        mut self,
        prefix: [u8; 32],
        host: Option<([u8; 32], &[u8], u8)>,
        root: Option<&[u8]>,
    ) -> Self {
        self.0.push(b'S');
        self.0.extend(prefix);
        self.0.extend([1, 0]);
        match host {
            None => self.0.push(0),
            Some((parent, key, position)) => {
                self.0.push(1);
                self.0.extend(parent);
                self = self.bytes32(key);
                self.0.push(position);
            }
        }
        match root {
            None => {
                self.0.push(0);
                self
            }
            Some(root) => {
                self.0.push(1);
                self.bytes32(root)
            }
        }
    }

    fn data(mut self, prefix: [u8; 32]) -> Self {
        self.0.push(b'D');
        self.0.extend(prefix);
        self
    }

    fn node(mut self, key: &[u8], hosted: &[[u8; 32]]) -> Self {
        self.0.push(b'N');
        self = self.bytes32(key).bytes32(&[0]);
        self.0.extend([0; 64]);
        self.0.extend([0, 0, hosted.len() as u8]);
        self.0.extend(hosted.iter().flatten());
        self.bytes32(b"value")
    }

    fn end(mut self, subtrees: u64, nodes: u64, records: u64) -> Vec<u8> {
        self.0.push(b'E');
        for count in [subtrees, nodes, records] {
            self.0.extend(count.to_le_bytes());
        }
        self.0
    }
}

#[test]
fn a_dump_is_written_to_a_new_file_and_left_whole_or_not_at_all() {
    let dir = temp();
    let source = fixture(dir.path(), 5);
    let file = dir.path().join("there.dump");
    fs::write(&file, b"kept").unwrap();
    let (result, _) = run(&format!(
        "dump {} --all {}",
        source.display(),
        file.display()
    ));
    assert!(matches!(result, Err(ToolError::Io(_))), "{result:?}");
    assert_eq!(fs::read(&file).unwrap(), b"kept");

    let absent = dir.path().join("absent.dump");
    let (result, _) = run(&format!(
        "dump {} {} {}",
        source.display(),
        "11".repeat(32),
        absent.display()
    ));
    let error = result.expect_err("no such subtree");
    assert!(error.to_string().contains("has no subtree 1111"), "{error}");
    assert!(!absent.exists(), "a dump that failed leaves no file");
}

/// The rule `load` keeps itself: a batch that writes a Merk below the top
/// with its host, and not the node above the host, has to mark the host's
/// change set anchored. The transaction takes the batch without the mark,
/// and the store it commits is not sound; with the mark it is.
///
/// The batch: the five items of [`SUB`] with new values, set as its
/// change set; the one node `m` of [`MID`], which hosts `SUB`, written
/// again with the new hashes and the root of `MID` kept; and nothing of
/// [`DOCS`], whose node `doc00001` hosts `MID`. Every rule but the hashes
/// is checked: the hash of `doc00001` is stale in both stores.
#[test]
fn a_host_written_again_without_the_node_above_it_is_anchored_or_the_store_is_not_sound() {
    use grovedb_element::Element;
    use lignindb::{
        commit::NoStaleHashes,
        txn::{Batch, MerkWrite, RootPut},
    };

    use super::{merk, puts, Item, MID, SUB};

    for anchored in [false, true] {
        let dir = temp();
        let path = fixture(dir.path(), 3);
        let db = Db::open(&path, &small()).expect("the database");
        let sub_items: Vec<Item> = (0u8..5)
            .map(|number| Item::element(&[b's', number], Element::new_item(vec![!number; 20])))
            .collect();
        let (sub_nodes, sub_root, sub_hash) = merk(&sub_items);
        let mut host = Item::element(b"m", Element::new_tree(sub_root.clone()));
        host.hosts = Some((SUB, sub_hash));
        let (mid_nodes, _, _) = merk(&[host]);
        let (sub_puts, mid_puts) = (puts(&sub_nodes), puts(&mid_nodes));
        let merks = [
            MerkWrite {
                subtree: SUB,
                nodes: &sub_puts,
                removed: &[],
                root: RootPut::Set(sub_root.as_deref()),
                anchored: false,
                whole: false,
            },
            MerkWrite {
                subtree: MID,
                nodes: &mid_puts,
                removed: &[],
                root: RootPut::Keep,
                anchored,
                whole: false,
            },
        ];
        let batch = Batch {
            records: &[],
            merks: &merks,
            subtrees: &[],
        };
        db.write(&batch, &mut NoStaleHashes)
            .expect("the transaction takes the batch either way");
        // `verify` makes the check; the one a debug build's close makes
        // would stop the test.
        db.unchecked_at_close();
        db.close().expect("a close");

        let line = format!("verify {} --no-hashes", path.display());
        let (result, report) = run(&line);
        let outcome = result.expect("a check");
        if anchored {
            assert_eq!(outcome, Outcome::Done, "{report}");
        } else {
            assert_eq!(outcome, Outcome::NotSound, "{report}");
            // The node of `MID` moved within its page, and the page's
            // fragment roots are not what the store marks.
            assert!(
                report.contains("the fragment-root mark of slot"),
                "{report}"
            );
        }
    }
}
