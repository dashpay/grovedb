//! The two acceptance tests of the task: a hash-only change to a page
//! writes no value bytes, and a chain never passes the threshold.

use lignin_format::segment::BlockContent;

use super::*;

fn value_log_len(db: &Db) -> u64 {
    let files = db.values.files_named().expect("the files");
    files.iter().map(|file| file.valid_len()).sum()
}

/// **Acceptance test.** A page holds a value in a cell and a value in the
/// value log. A commit then changes one hash of it. Write-back writes one
/// block for that: a run of 39 bytes, 47 in the segment, which is the page
/// id and the record of the hash. Nothing is appended to the value log, no
/// count changes, and the block holds no byte of either value.
#[test]
fn a_hash_only_change_to_a_page_writes_no_value_bytes() {
    let db = Db::new();
    let inline_value = [0xAB; 300];
    let Val::Out(long_value) = long(7, 3000) else {
        unreachable!()
    };
    db.commit(&[
        insert((7, 1), b"in the cell", inline(&inline_value)),
        insert((7, 2), b"out of line", Val::Out(long_value.clone())),
    ]);
    let first = db.wb().pass().expect("a pass");
    assert_eq!((first.written, first.placed, first.waiting), (1, 1, 0));
    assert_eq!(db.shape(page(7)), "B");
    db.check_cold();

    let before = db.stats();
    let values_before = (value_log_len(&db), db.values.counts());
    assert_eq!(before.value_bytes, 3000);

    db.commit(&[hash((7, 2), 0x5C)]);
    let second = db.wb().pass().expect("a pass");
    assert_eq!((second.written, second.placed, second.waiting), (1, 0, 0));

    let after = db.stats();
    assert_eq!(after.delta_blocks, before.delta_blocks + 1);
    assert_eq!(after.base_blocks, before.base_blocks);
    assert_eq!(after.values_appended, before.values_appended);
    assert_eq!(after.value_bytes, before.value_bytes);
    assert_eq!(after.delta_bytes - before.delta_bytes, 39);
    assert_eq!(after.stored_bytes - before.stored_bytes, 47);
    assert_eq!((value_log_len(&db), db.values.counts()), values_before);
    assert_eq!(after.runs_refused, 0);

    // The block, byte for byte: the page id, the kind of the record, the
    // slot and the hash.
    let chain = db.chain(page(7));
    assert_eq!(db.shape(page(7)), "Bd");
    let location = chain[1].location().expect("a block");
    assert_eq!(location.len, 47);
    let mut buf = Vec::new();
    let block = db.segments.read(location, 64, &mut buf).expect("the block");
    assert_eq!(block.content(), BlockContent::Delta);
    let mut expected = vec![7, 0, 0, 0, 5, 2, 0];
    expected.extend_from_slice(&[0x5C; 32]);
    assert_eq!(block.payload().expect("a payload"), expected.as_slice());

    // And the page still has both values, from its segments alone.
    db.check_cold();
}

/// A hash-only change to a page whose values are still pending writes no
/// value byte to a segment either: the values go to the value log once,
/// and the run holds the hash and the 19 bytes that place each value.
#[test]
fn the_run_of_a_page_holds_the_place_of_a_value_and_never_its_bytes() {
    let db = Db::new();
    db.commit(&[insert((3, 1), b"k", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    let before = db.stats();

    db.commit(&[insert((3, 2), b"long", long(1, 2000)), hash((3, 1), 9)]);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed), (1, 1));
    let after = db.stats();
    assert_eq!(after.value_bytes - before.value_bytes, 2000);
    assert_eq!(after.delta_blocks - before.delta_blocks, 1);
    // The run: the page id, the inserted cell with its pending place, the
    // hash, and record 21 with the pointer. None of the 2000 bytes.
    assert!(after.delta_bytes - before.delta_bytes < 200);
    assert_eq!(db.shape(page(3)), "Bd");
    assert!(db.pending(page(3)).is_empty());
    db.check_cold();
}

/// **Acceptance test.** With a threshold of three, a page that is changed
/// by one commit after another has at most three runs behind its base
/// image at any time, for the latest version and for every version a
/// reader can still ask for.
#[test]
fn chain_length_never_exceeds_the_threshold() {
    let config = FlushConfig {
        max_chain_deltas: 3,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    db.commit(&[
        insert((1, 1), b"k", inline(b"v")),
        insert((2, 1), b"k", inline(b"v")),
    ]);
    db.wb().pass().expect("a pass");
    let mut shapes = Vec::new();
    for round in 0..9u8 {
        db.commit(&[hash((1, 1), round)]);
        db.wb().pass().expect("a pass");
        db.check_chains();
        shapes.push(db.shape(page(1)));
    }
    // No reader pins an old version, so each new base image lets the links
    // before it go.
    assert_eq!(
        shapes,
        ["Bd", "Bdd", "Bddd", "B", "Bd", "Bdd", "Bddd", "B", "Bd"]
    );
    assert_eq!(db.stats().consolidations, 2);
    assert_eq!(db.shape(page(2)), "B");
    db.check_cold();

    // With a snapshot held, the old links stay for it, and every chain a
    // reader is given is still within the threshold.
    db.versions.pin(db.latest());
    for round in 0..9u8 {
        db.commit(&[hash((1, 1), 100 + round)]);
        db.wb().pass().expect("a pass");
        db.check_chains();
    }
    assert_eq!(db.shape(page(1)), "BdddBdddBdd");
    db.check_cold();
    db.versions.unpin();
    db.commit(&[hash((2, 1), 1)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "Bdd");
    let dead: u64 = db.state.dead_space().values().map(|dead| dead.blocks).sum();
    assert!(dead >= 8, "{dead} blocks were reported dead");
    db.check_cold();
}

/// The byte threshold: runs that together pass it are followed by a base
/// image, however few they are.
#[test]
fn a_chain_is_consolidated_when_its_runs_pass_the_byte_threshold() {
    let config = FlushConfig {
        max_chain_deltas: 100,
        max_chain_bytes: 2000,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    db.versions.pin(db.latest());
    for round in 0..12u16 {
        // An inserted cell with 500 bytes in it that do not compress: a
        // block of some 600 bytes.
        let value = noise(round.into(), 500);
        db.commit(&[insert((1, 2 + round), &round.to_le_bytes(), inline(&value))]);
        db.wb().pass().expect("a pass");
        db.check_chains();
    }
    assert_eq!(db.shape(page(1)), "BdddBdddBdddB");
    db.check_cold();
}

/// With a threshold of zero every block is a base image.
#[test]
fn with_no_runs_allowed_every_block_is_a_base_image() {
    let config = FlushConfig {
        max_chain_deltas: 0,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    db.versions.pin(version(0));
    db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    db.commit(&[hash((1, 1), 1)]);
    db.commit(&[hash((1, 1), 2)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BBB");
    assert_eq!(db.stats().delta_blocks, 0);
    db.check_cold();
}
