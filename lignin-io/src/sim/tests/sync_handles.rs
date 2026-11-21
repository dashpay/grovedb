//! A file as the store keeps a segment or a value-log file, synced in one
//! of two ways: through the handle that wrote it, or through another. Only
//! a write-back that fails tells the two apart
//! (`docs/lignindb/sync-handles.md`).

use super::*;

const HEADER: &[u8] = b"HEAD";
const BLOCKS: [&[u8]; 3] = [b"block-01", b"block-02", b"block-03"];

/// Which handle syncs the segment once its blocks are written.
#[derive(Clone, Copy, Debug)]
enum Through {
    /// The handle that appended them.
    Writer,
    /// The handle that was opened beside the writer when the file was made,
    /// and that reads it.
    Reader,
    /// A handle that is opened for the sync, after the two were closed.
    Reopened,
}

#[derive(Debug, PartialEq)]
struct Life {
    /// Whether the sync returned an error.
    reported: bool,
    /// Whether some crash leaves less of the file than was vouched for.
    lost: bool,
}

/// Makes a segment, appends its blocks and syncs it through `through`. The
/// write-back fails after the second block if `told` says who is told.
///
/// A sync that returns `Ok` vouches for the length the file has then, as
/// the manifest does that names a segment's valid length.
fn life(told: Option<Told>, through: Through, unknown: UnknownFate) -> Life {
    let fs = SimVfs::new(SimConfig {
        unknown,
        ..SimConfig::new(31)
    });
    let path = p("/segment");
    let writer = fs.open(path, OpenMode::Append(Create::New)).unwrap();
    writer.append(HEADER).unwrap();
    writer.sync_data().unwrap();
    fs.sync_dir(p("/")).unwrap();
    let reader = fs.open(path, OpenMode::Read).unwrap();

    if let Some(told) = told {
        let rule = WriteBackRule::new(Fault::Eio).at(path).skip(1);
        fs.add_write_back(rule.told(told));
    }
    for block in BLOCKS {
        writer.append(block).unwrap();
    }
    assert_eq!(fs.stats().write_backs, u64::from(told.is_some()));

    let whole = [HEADER, &BLOCKS.concat()].concat();
    let synced = match through {
        Through::Writer => writer.sync_data(),
        Through::Reader => reader.sync_data(),
        Through::Reopened => {
            drop((writer, reader));
            fs.open(path, OpenMode::Read).unwrap().sync_data()
        }
    };
    let vouched = match &synced {
        Ok(()) => whole.len(),
        Err(error) => {
            assert_eq!(SimError::injected(error), Some(Fault::Eio));
            HEADER.len()
        }
    };

    let image = fs.crash_image();
    let lost = every_plan(&image).iter().any(|plan| {
        let left = read(&recover(&image, plan), path);
        left.get(..vouched) != Some(&whole[..vouched])
    });
    Life {
        reported: synced.is_err(),
        lost,
    }
}

const EVERY_FATE: [UnknownFate; 6] = [
    UnknownFate::Lost,
    UnknownFate::Kept,
    UnknownFate::Drawn(0),
    UnknownFate::Drawn(1),
    UnknownFate::Drawn(2),
    UnknownFate::Drawn(3),
];

const SOUND: Life = Life {
    reported: false,
    lost: false,
};
const REPORTED: Life = Life {
    reported: true,
    lost: false,
};
const LOST: Life = Life {
    reported: false,
    lost: true,
};

#[test]
fn without_a_write_back_that_fails_the_two_ways_of_syncing_are_the_same() {
    for through in [Through::Writer, Through::Reader, Through::Reopened] {
        for unknown in EVERY_FATE {
            assert_eq!(life(None, through, unknown), SOUND, "{through:?}");
        }
    }
}

#[test]
fn a_sync_through_the_handle_that_wrote_reports_the_write_back() {
    for told in [Told::Writers, Told::OpenHandles] {
        for unknown in EVERY_FATE {
            let through = Through::Writer;
            assert_eq!(life(Some(told), through, unknown), REPORTED, "{told:?}");
        }
    }
}

#[test]
fn a_sync_through_another_handle_vouches_for_bytes_that_are_gone() {
    let lost = UnknownFate::Lost;
    assert_eq!(life(Some(Told::Writers), Through::Reader, lost), LOST);
    assert_eq!(life(Some(Told::Writers), Through::Reopened, lost), LOST);
    // Storage has the bytes after all in some fates, and a crash then finds
    // them. No call said which fate it was.
    let kept = UnknownFate::Kept;
    assert_eq!(life(Some(Told::Writers), Through::Reader, kept), SOUND);
}

/// What Linux tells is more than the contract promises: the handle that
/// was opened beside the writer is told as well, and reports. A handle that
/// is opened once the file was closed is told on neither.
#[test]
fn every_open_handle_told_covers_the_reader_and_not_a_file_that_was_closed() {
    let linux = Some(Told::OpenHandles);
    for unknown in EVERY_FATE {
        assert_eq!(life(linux, Through::Reader, unknown), REPORTED);
    }
    assert_eq!(life(linux, Through::Reopened, UnknownFate::Lost), LOST);
}
