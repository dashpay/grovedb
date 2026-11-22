//! What the cleaner says when something fails, and what it does with a
//! block that is damaged.

use std::{
    error::Error,
    io::{Read, Seek, SeekFrom, Write},
};

use lignin_format::{checksum::Checksum, segment::Compression, FormatError};

use super::*;
use crate::{
    flush::FlushError,
    segment::{FileNames, SegmentStoreError},
    table::LinkError,
    values::ValueStoreError,
};

fn at(file: u32, offset: u64) -> BlockLocation {
    BlockLocation {
        file: FileId::new(file).expect("a file id"),
        offset,
        len: 64,
    }
}

/// Every error says what failed and names its cause.
#[test]
fn every_error_says_what_failed() {
    let unknown = || SegmentStoreError::UnknownSegment {
        file: FileId::new(3).expect("a file id"),
    };
    let truncated = FormatError::Truncated {
        needed: 9,
        available: 4,
    };
    let cases: Vec<(CleanerError, &str, bool)> = vec![
        (
            CleanerError::from(unknown()),
            "the cleaner's segments: no segment 3",
            false,
        ),
        (
            CleanerError::from(ValueStoreError::ReadOnly),
            "the cleaner's values: the value log is read-only",
            false,
        ),
        (
            CleanerError::from(FlushError::SegmentsNotDurable),
            "write-back, for the cleaner: ",
            false,
        ),
        (
            CleanerError::block(page(7), at(3, 20))(unknown()),
            "the block of page 7 at offset 20 of segment 3 could not be read: no segment 3",
            true,
        ),
        (
            CleanerError::payload(page(7), at(3, 20))(truncated),
            "the block of page 7 at offset 20 of segment 3 is not what its tag says: ",
            true,
        ),
        (
            CleanerError::Link {
                page: page(7),
                from: at(3, 20),
                to: at(4, 84),
                source: LinkError::TableFull,
            },
            "the link of page 7 to offset 20 of segment 3 was not pointed at offset 84 of \
             segment 4: ",
            false,
        ),
        (
            CleanerError::SegmentNotDurable {
                file: FileId::new(4).expect("a file id"),
                source: unknown(),
            },
            "segment 4, which takes the moved blocks, could not be made durable: no segment 3",
            false,
        ),
    ];
    for (error, expected, damage) in cases {
        let said = error.to_string();
        assert!(said.starts_with(expected), "{said}");
        let source = error.source().expect("a cause").to_string();
        assert!(said.ends_with(&source), "{said} does not end with {source}");
        assert_eq!(error.is_damage(), damage, "{said}");
        assert!(!format!("{error:?}").is_empty());
    }
    // A read that the file layer failed can be made again: it is no
    // damage.
    let io = SegmentStoreError::Io {
        op: "read",
        path: "seg/00000003.seg".into(),
        source: std::io::Error::other("a fault"),
    };
    assert!(!CleanerError::block(page(7), at(3, 20))(io).is_damage());
}

/// The defaults are what the documentation says, and a pass that did
/// nothing says so.
#[test]
fn the_defaults_and_what_counts_as_progress() {
    let config = CleanerConfig::default();
    assert_eq!(config, CleanerConfig::new());
    assert_eq!(config.target_dead_percent, DEFAULT_TARGET_DEAD_PERCENT);
    assert_eq!(config.max_pass_bytes, DEFAULT_PASS_BYTES);
    assert!(config.target_dead_percent <= 20);
    assert!(config.checkpoint_dead_percent < config.target_dead_percent);
    let state = CleanerState::new(config);
    assert_eq!(state.config(), config);
    assert_eq!(state.stats(), CleanerStats::default());

    assert!(!Pass::default().progressed());
    for pass in [
        Pass {
            links_trimmed: 1,
            ..Pass::default()
        },
        Pass {
            blocks_lost: 1,
            ..Pass::default()
        },
        Pass {
            retired: 1,
            ..Pass::default()
        },
        Pass {
            sealed: vec![FileId::MIN],
            ..Pass::default()
        },
    ] {
        assert!(pass.progressed(), "{pass:?}");
    }
    let put_off = Pass {
        scanned: true,
        waiting: 3,
        more: true,
        checkpoint: Some(Checkpointed::PutOff { blocked: 1 }),
        ..Pass::default()
    };
    assert!(!put_off.progressed());
    assert!(!ValuePass::default().progressed());
    let dropped = ValuePass {
        dropped: vec![FileId::MIN],
        ..ValuePass::default()
    };
    assert!(dropped.progressed());
}

/// The bytes of the block at `location`, as they are in its file.
fn raw(db: &Db, location: BlockLocation) -> Vec<u8> {
    let path = FileNames::SEGMENT.path(&db.path, location.file);
    let mut file = std::fs::File::open(path).expect("the segment");
    file.seek(SeekFrom::Start(location.offset)).expect("a seek");
    let mut bytes = vec![0; location.len as usize];
    file.read_exact(&mut bytes).expect("the block");
    bytes
}

fn overwrite(db: &Db, location: BlockLocation, bytes: &[u8]) {
    let path = FileNames::SEGMENT.path(&db.path, location.file);
    let mut file = (std::fs::OpenOptions::new().write(true).open(path)).expect("the segment");
    file.seek(SeekFrom::Start(location.offset)).expect("a seek");
    file.write_all(bytes).expect("written");
}

/// What is dead in a segment that cannot be read stays, so it is no
/// reason to move the blocks of the others.
#[test]
fn no_segment_is_cleaned_for_the_sake_of_one_that_cannot_be_read() {
    use std::collections::BTreeSet;

    use lignin_format::manifest::FileState;

    use crate::cleaner::clean::victims;

    let file = |raw| FileId::new(raw).expect("an id");
    let segment = |raw, live_bytes| SegmentSpace {
        file: file(raw),
        state: FileState::Sealed,
        has_writer: false,
        block_bytes: 1000,
        live_bytes,
        live_blocks: 1,
        pinned_bytes: 0,
        retired: false,
    };
    let space = Space {
        segments: vec![segment(1, 100), segment(2, 950), segment(3, 1000)],
        oldest_pin: None,
    };
    let config = CleanerConfig {
        target_dead_percent: 10,
        ..CleanerConfig::new()
    };
    let (none, damaged) = (BTreeSet::new(), BTreeSet::from([file(1)]));
    // 950 bytes of 3000 are dead: the segment that holds most of them is
    // emptied, and that is enough.
    let chosen = victims(&space, &none, &none, &config, 0);
    assert_eq!(chosen.segments, [file(1)]);
    // It cannot be read: of the others, 50 bytes of 2000 are dead.
    let chosen = victims(&space, &none, &damaged, &config, 0);
    assert!(chosen.segments.is_empty() && !chosen.no_room, "{chosen:?}");
    // And it waits to be deleted: the same.
    let chosen = victims(&space, &damaged, &none, &config, 0);
    assert!(chosen.segments.is_empty() && !chosen.no_room, "{chosen:?}");
}

/// The segments a round moves the blocks out of: the one with the largest
/// share of dead bytes first, as many as bring the dead space within its
/// target, and no more than can wait to be deleted at once.
#[test]
fn a_round_chooses_no_more_than_can_wait_to_be_deleted() {
    use std::collections::BTreeSet;

    use lignin_format::manifest::FileState;

    use crate::cleaner::clean::victims;

    let file = |raw| FileId::new(raw).expect("an id");
    let segment = |raw, live_bytes| SegmentSpace {
        file: file(raw),
        state: FileState::Sealed,
        has_writer: false,
        block_bytes: 1000,
        live_bytes,
        live_blocks: 1,
        pinned_bytes: 0,
        retired: false,
    };
    // Ten segments of 1000 bytes: 700 bytes are dead in the first, 50
    // fewer in each one up to the sixth, none in the rest. And one of 400
    // bytes, half of it dead. Of 10,400 bytes 3,650 are dead.
    let live = |raw: u32| match raw {
        1..=6 => 250 + 50 * u64::from(raw),
        _ => 1000,
    };
    let mut segments: Vec<SegmentSpace> = (1..=10).map(|raw| segment(raw, live(raw))).collect();
    segments.push(SegmentSpace {
        block_bytes: 400,
        ..segment(11, 200)
    });
    let space = Space {
        segments,
        oldest_pin: None,
    };
    assert_eq!((space.block_bytes(), space.dead_bytes()), (10_400, 3650));
    let none = BTreeSet::new();
    let chosen = |target, room, waiting| {
        let config = CleanerConfig {
            target_dead_percent: target,
            checkpoint_dead_percent: room,
            ..CleanerConfig::new()
        };
        let chosen = victims(&space, &none, &none, &config, waiting);
        let files: Vec<u32> = chosen.segments.iter().map(|file| file.get()).collect();
        (files, chosen.no_room)
    };
    // With room for everything: the five worst, after which 650 bytes of
    // 7,400 are dead.
    assert_eq!(chosen(10, 100, 0), (vec![1, 2, 3, 4, 5], false));
    // 2,600 bytes may wait: two of the large ones, and then the small one,
    // which fits where a third large one does not.
    assert_eq!(chosen(10, 25, 0), (vec![1, 2, 11], false));
    // 1,000 wait already: one large one, and the small one.
    assert_eq!(chosen(10, 25, 1000), (vec![1, 11], false));
    // 2,000 wait already: no large one fits, and the small one does. The
    // worst segment was passed over, which the caller is told.
    assert_eq!(chosen(10, 25, 2000), (vec![11], true));
    // 2,400 wait already: none fits, and that is why none is chosen.
    assert_eq!(chosen(10, 25, 2400), (vec![], true));
    // Nothing waits, and no segment fits: one is taken all the same.
    assert_eq!(chosen(10, 5, 0), (vec![1], false));
    // Within the target nothing is chosen, whatever room there is.
    assert_eq!(chosen(36, 100, 0), (vec![], false));
    assert_eq!(chosen(36, 25, 2400), (vec![], false));
}

/// A sealed segment whose footer cannot be read is accounted for by the
/// end of its file, and is cleaned like any other: no reader uses the
/// footer, and the cleaner does not stop for it.
#[test]
fn a_segment_whose_footer_cannot_be_read_is_cleaned_all_the_same() {
    use lignin_format::manifest::FileState;

    let db = churned(13);
    let first = CleanerState::new(moving_only());
    let space = Cleaner::new(db.wb(), &first).space().expect("the space");
    let held = (space.segments.iter())
        .find(|segment| {
            segment.state == FileState::Sealed
                && segment.live_blocks != 0
                && segment.dead_bytes() != 0
        })
        .expect("a sealed segment with live and dead blocks");
    let end = (db.segments.extents().iter())
        .find(|extent| extent.file == held.file)
        .map(|extent| extent.end)
        .expect("its extent");
    let blocks_end = db.segments.blocks_end(held.file).expect("the footer");
    let footer = BlockLocation {
        file: held.file,
        offset: blocks_end,
        len: 0,
    };
    overwrite(&db, footer, &vec![0xA5; (end - blocks_end) as usize]);
    assert!(db.segments.blocks_end(held.file).is_err());

    // A cleaner that has not read the footer before.
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let found = cleaner.space().expect("the space is read");
    let counted = found.segment(held.file).expect("the segment");
    assert_eq!(counted.block_bytes, held.block_bytes + (end - blocks_end));
    assert_eq!(counted.live_bytes, held.live_bytes);
    let done = settle(&cleaner);
    assert!(done.blocks_moved > 0 && done.dropped > 0, "{done:?}");
    assert!(!on_disk(&db, held.file), "the segment was emptied and went");
    assert_eq!(state.stats().errors, 0);
    db.check_warm();
    db.check_cold();
}

/// A live block that is damaged is not moved, and its segment is left
/// alone for the session: it keeps a link, so it can never be deleted.
/// Every other segment is cleaned.
#[test]
fn a_segment_with_a_damaged_block_is_left_alone() {
    for forged in [false, true] {
        let db = churned(13);
        let state = CleanerState::new(moving_only());
        let cleaner = Cleaner::new(db.wb(), &state);
        let space = cleaner.space().expect("the space");
        // A base image beside dead blocks, stored compressed.
        let mut buf = Vec::new();
        let (id, location) = links_of(&db)
            .into_iter()
            .filter_map(|(id, link)| Some((id, link.location()?)))
            .find(|(_, location)| {
                let held = space.segment(location.file).expect("the segment");
                let block = (db.segments.read(*location, db.size.get(), &mut buf)).expect("it");
                !held.has_writer
                    && held.dead_bytes() != 0
                    && block.compression() == Compression::Lz4
            })
            .expect("a compressed block beside dead ones");
        let mut bytes = raw(&db, location);
        if forged {
            // The length the payload states is wrong, under a checksum
            // that is right: no read finds it, and no payload comes of it.
            let stated = u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes"));
            bytes[8..12].copy_from_slice(&(stated + 1).to_le_bytes());
            let (frame, body) = bytes.split_at_mut(8);
            let checksum = Checksum::of(&[&frame[..4], body]);
            frame[4..].copy_from_slice(&checksum.encode());
        } else {
            let last = bytes.len() - 1;
            bytes[last] ^= 0x40;
        }
        overwrite(&db, location, &bytes);

        let error = cleaner.pass().expect_err("a block is damaged");
        assert!(error.is_damage(), "{error}");
        match (&error, forged) {
            (
                CleanerError::Payload {
                    page, location: at, ..
                },
                true,
            )
            | (
                CleanerError::Block {
                    page, location: at, ..
                },
                false,
            ) => {
                assert_eq!((*page, *at), (id, location));
            }
            _ => panic!("{error}"),
        }
        let stats = state.stats();
        assert_eq!((stats.segments_unreadable, stats.errors), (1, 1));
        // The block has its link and its segment its file.
        assert!(db
            .chain(id)
            .iter()
            .any(|link| link.location() == Some(location)));
        assert!(on_disk(&db, location.file));
        // The other segments were cleaned in that same pass, and later
        // passes leave the damaged one alone without failing.
        assert!(stats.segments_dropped > 0, "{stats:?}");
        for _ in 0..3 {
            let pass = cleaner.pass().expect("the segment is passed over");
            assert_eq!(pass.blocks_moved, 0, "{pass:?}");
        }
        let after = cleaner.space().expect("the space");
        for segment in &after.segments {
            let alone = segment.file == location.file;
            assert!(
                segment.has_writer || alone == (segment.dead_bytes() != 0),
                "{}",
                describe(&db, &after)
            );
        }
        assert_eq!(state.stats().segments_unreadable, 1);
    }
}

/// **The damage a pass found is what it ends with, also when a step of
/// the pass fails afterwards.** A live block of the segment a round takes
/// first is damaged, and the file of the segment it takes second is cut
/// short, so that the file layer fails the read of its first block. The
/// pass puts the damage aside, and the failed read ends it. The segment
/// with the damaged block is left alone for the rest of the session, so
/// no later pass of it names the block again: the pass that found it has
/// to.
#[test]
fn the_damage_a_pass_found_is_not_replaced_by_a_later_failure() {
    use std::collections::BTreeSet;

    use crate::cleaner::clean::victims;

    let db = churned(13);
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.space().expect("the space");
    let none = BTreeSet::new();
    let chosen = victims(&space, &none, &none, &moving_only(), 0).segments;
    let (first, second) = (chosen[0], chosen[1]);
    let (id, location) = links_of(&db)
        .into_iter()
        .filter_map(|(id, link)| Some((id, link.location()?)))
        .find(|(_, location)| location.file == first)
        .expect("a block of the first segment that a link points to");
    let mut bytes = raw(&db, location);
    let last = bytes.len() - 1;
    bytes[last] ^= 0x40;
    overwrite(&db, location, &bytes);
    let path = FileNames::SEGMENT.path(&db.path, second);
    let file = (std::fs::OpenOptions::new().write(true).open(path)).expect("the segment");
    file.set_len(0).expect("the file is cut short");

    let error = cleaner.pass().expect_err("a block is damaged");
    match &error {
        CleanerError::Block {
            page, location: at, ..
        } => assert_eq!((*page, *at), (id, location), "{error}"),
        other => panic!("the pass ended with: {other}"),
    }
    assert!(error.is_damage(), "{error}");
    let stats = state.stats();
    assert_eq!((stats.segments_unreadable, stats.errors), (1, 1));
    // The failed read ended the round. It had retired the segments that
    // held no live block when it began, and it retired none of those it
    // had chosen, which a round that only meets the damage does.
    let empty = (space.segments.iter())
        .filter(|segment| !segment.has_writer && segment.live_blocks == 0)
        .count();
    assert_eq!(stats.segments_retired, empty as u64, "{stats:?}");
}

/// D-204, for the cleaner's loop: the turn after one in which a pass
/// failed waits the idle time, and twice as long after each further such
/// turn in a row, up to the bound of the store's write-back. The first
/// turn in which no pass fails brings the idle time back.
#[test]
fn a_turn_in_which_a_pass_failed_is_followed_by_a_wait_that_grows_to_a_bound() {
    use std::time::Duration;

    let ms = Duration::from_millis;
    let db = Db::with(FlushConfig {
        max_failed_wait: ms(70),
        ..small_segments()
    });
    let state = CleanerState::new(CleanerConfig {
        idle: ms(10),
        ..eager()
    });
    let cleaner = Cleaner::new(db.wb(), &state);
    let mut failed_turns = 0;
    let mut reported = 0;
    let mut turn = |fails: bool| {
        let mut values = |_: &Cleaner<'_>| match fails {
            true => Err(CleanerError::Values(ValueStoreError::FileIdsExhausted)),
            false => Ok(ValuePass::default()),
        };
        let wait = cleaner.paced_turn(&mut values, &mut |_| reported += 1, &mut failed_turns);
        wait.map(|wait| wait.as_millis())
    };
    let waits = [true, true, true, true, true, false, true, true].map(&mut turn);
    assert_eq!(
        waits,
        [
            Some(10),
            Some(20),
            Some(40),
            Some(70),
            Some(70),
            None,
            Some(10),
            Some(20)
        ]
    );
    assert_eq!(reported, 7);
}

/// The cleaner's loop waits an hour after a turn in which a pass
/// failed. Told that room was made, it takes its next turn at once, each
/// time it is told: during its wait, or in the turn before it. A call
/// that came during an earlier sleep ends no later one.
#[test]
fn the_cleaners_loop_takes_its_next_turn_at_once_when_it_is_told_that_room_was_made() {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    let hour = Duration::from_secs(3600);
    let flush = FlushConfig {
        max_failed_wait: hour,
        ..small_segments()
    };
    let db = Db::with(flush);
    let state = CleanerState::new(CleanerConfig {
        idle: hour,
        ..eager()
    });
    let cleaner = Cleaner::new(db.wb(), &state);
    let turns = AtomicU64::new(0);
    let reached = |wanted: u64| {
        let began = Instant::now();
        while turns.load(Ordering::Acquire) < wanted && began.elapsed() < Duration::from_secs(60) {
            std::thread::yield_now();
        }
        turns.load(Ordering::Acquire) >= wanted
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            cleaner.run(
                |_| {
                    turns.fetch_add(1, Ordering::AcqRel);
                    Err(CleanerError::Values(ValueStoreError::FileIdsExhausted))
                },
                |_| {},
            )
        });
        let first = reached(1);
        let mut told = Vec::new();
        for wanted in 2..=4 {
            cleaner.room_made();
            told.push(reached(wanted));
        }
        cleaner.stop();
        assert!(first, "the loop made no turn");
        assert_eq!(told, [true; 3], "the loop went on waiting");
    });

    // The sleep by itself: a call ends the one that is running or that
    // begins next, and no later one.
    let state = CleanerState::new(eager());
    state.room_made();
    let began = Instant::now();
    state.sleep(Some(hour));
    assert!(
        began.elapsed() < Duration::from_secs(60),
        "the wait went on"
    );
    let wait = Duration::from_millis(60);
    let began = Instant::now();
    state.sleep(Some(wait));
    assert!(began.elapsed() >= wait, "{:?}", began.elapsed());
}

/// The loop itself, with somebody who wakes it all the time: after each
/// turn in which a pass failed it waits as long as it was to, at least.
#[test]
fn the_cleaners_loop_waits_after_a_failed_turn_whoever_wakes_it() {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
        time::{Duration, Instant},
    };

    let ms = Duration::from_millis;
    let flush = FlushConfig {
        max_failed_wait: ms(40),
        ..small_segments()
    };
    let db = Db::with(flush);
    let state = CleanerState::new(CleanerConfig {
        idle: ms(10),
        ..eager()
    });
    let cleaner = Cleaner::new(db.wb(), &state);
    let turns = Mutex::new(Vec::new());
    let enough = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            cleaner.run(
                |_| {
                    let mut turns = turns.lock().expect("the turns");
                    turns.push(Instant::now());
                    if turns.len() == 5 {
                        enough.store(true, Ordering::Release);
                    }
                    Err(CleanerError::Values(ValueStoreError::FileIdsExhausted))
                },
                |_| {},
            )
        });
        while !enough.load(Ordering::Acquire) {
            state.wake();
            std::thread::yield_now();
        }
        cleaner.stop();
    });
    let turns = turns.into_inner().expect("the turns");
    let pacing = FlushConfig {
        idle: ms(10),
        ..flush
    };
    for (turn, pair) in turns.windows(2).take(4).enumerate() {
        let waited = pair[1] - pair[0];
        let least = pacing.wait_after_failure(turn as u32 + 1);
        assert!(
            waited >= least,
            "the turn after failed turn {} came after {waited:?}, and not after {least:?}",
            turn + 1
        );
    }
}
