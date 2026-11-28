//! What each error says.

use std::{error::Error, io};

use lignin_format::{
    wal::{LogAddress, WalError},
    FormatError, FormatVersion,
};

use super::*;
use crate::segment::SegmentStoreError;

#[test]
fn every_error_says_what_failed_and_where() {
    let path = || PathBuf::from("/db/wal/00000007.log");
    let io = || io::Error::other("the disk is gone");
    let format = || FormatError::Wal(WalError::UnfinishedGroup);
    let cases: Vec<(LogError, &[&str], bool)> = vec![
        (
            LogError::Io {
                op: "sync",
                path: path(),
                source: io(),
            },
            &["cannot sync /db/wal/00000007.log", "the disk is gone"],
            true,
        ),
        (
            LogError::Format {
                path: path(),
                offset: 99,
                source: format(),
            },
            &["00000007.log at offset 99", "inside a commit group"],
            true,
        ),
        (
            LogError::Damaged {
                path: path(),
                offset: 99,
                valid_len: 400,
                source: format(),
            },
            &["damaged at offset 99", "valid length of 400"],
            true,
        ),
        (
            LogError::Manifest(SegmentStoreError::ReadOnly),
            &["read-only"],
            true,
        ),
        (
            LogError::WrongDirectory {
                expected: PathBuf::from("/db"),
                found: PathBuf::from("/other"),
            },
            &["manifest of /other", "log of /db"],
            false,
        ),
        (
            LogError::FileTooShort {
                path: path(),
                valid_len: 400,
                len: 300,
            },
            &["300 bytes long", "valid length of 400"],
            false,
        ),
        (
            LogError::FileNewerThanManifest {
                path: path(),
                file: FormatVersion::V1,
                manifest: FormatVersion::V1,
            },
            &["format version 1", "newer than the manifest's 1"],
            false,
        ),
        (
            LogError::WrongFileId {
                path: path(),
                found: 9,
            },
            &["says it is log file 9"],
            false,
        ),
        (
            LogError::UnnamedFileHoldsItems {
                path: path(),
                len: 77,
            },
            &["77 bytes long", "does not name it"],
            false,
        ),
        (
            LogError::WrongLogEnd { file: file_id(7) },
            &["disagree on log file 7"],
            false,
        ),
        (
            LogError::NotAtRecordEnd {
                path: path(),
                valid_len: 30,
                offset: 20,
            },
            &["valid length 30", "begins at offset 20"],
            false,
        ),
        (
            LogError::VouchedBehindEnd {
                path: path(),
                valid_len: 900,
            },
            &["vouches for 900 bytes", "the log ends before that file"],
            false,
        ),
        (LogError::ReaderFailed, &["has failed before"], false),
        (
            LogError::CutNotDurable { path: path() },
            &["failed", "no manifest names the file closed"],
            false,
        ),
        (
            LogError::TakenBack {
                path: path(),
                source: io(),
            },
            &[
                "as long as it was",
                "bring the item back",
                "the disk is gone",
            ],
            true,
        ),
        (
            LogError::ItemTooLong {
                path: path(),
                offset: 45,
                len: 9000,
            },
            &["9000 bytes", "from offset 45 on", "do not fit in memory"],
            false,
        ),
        (
            LogError::BeyondEnd {
                place: place(7, 100),
                end: Some(place(7, 60)),
            },
            &[
                "through offset 100 of file 7",
                "ends at offset 60 of file 7",
            ],
            false,
        ),
        (
            LogError::BeyondEnd {
                place: place(7, 100),
                end: None,
            },
            &["through offset 100 of file 7", "holds nothing"],
            false,
        ),
        (
            LogError::NotDeleted {
                path: path(),
                source: io(),
            },
            &["could not be deleted", "the disk is gone"],
            true,
        ),
        (LogError::FileIdsExhausted, &["every log file id"], false),
        (
            LogError::SpareExists { file: file_id(7) },
            &["already has file 7"],
            false,
        ),
        (LogError::NoSpare, &["no file to turn to"], false),
        (
            LogError::SpareNotNamed { file: file_id(7) },
            &["no manifest names log file 7"],
            false,
        ),
        (LogError::NoActiveFile, &["no file that takes items"], false),
        (
            LogError::PlaceTaken {
                expected: place(7, 100),
                found: Some(place(8, 45)),
            },
            &["offset 100 of log file 7", "offset 45 of file 8"],
            false,
        ),
        (
            LogError::PlaceTaken {
                expected: place(7, 100),
                found: None,
            },
            &["offset 100 of log file 7", "no file that takes items"],
            false,
        ),
        (
            LogError::Refused {
                offset: 12,
                source: format(),
            },
            &["offset 12 of what was offered", "inside a commit group"],
            true,
        ),
        (
            LogError::NotOneItem {
                len: 90,
                item_len: 48,
            },
            &["90 bytes", "48 bytes long"],
            false,
        ),
        (
            LogError::WrongItem {
                expected: "a commit group",
            },
            &["not a commit group"],
            false,
        ),
        (
            LogError::NotAppended {
                path: path(),
                source: io(),
            },
            &["is not in the log", "the disk is gone"],
            true,
        ),
        (
            LogError::NoActiveFileSince {
                path: path(),
                source: io(),
            },
            &["no file that takes items since", "the disk is gone"],
            true,
        ),
        (
            LogError::Unsettled {
                path: path(),
                source: io(),
            },
            &["not known whether", "the disk is gone"],
            true,
        ),
        (
            LogError::SyncFailed {
                path: path(),
                source: io(),
            },
            &["a sync of /db/wal/00000007.log failed", "the disk is gone"],
            true,
        ),
        (
            LogError::NotDurable {
                place: place(7, 100),
                durable: Some(place(6, 50)),
            },
            &[
                "kept from offset 100 of file 7",
                "up to offset 50 of file 6",
            ],
            false,
        ),
        (
            LogError::NotDurable {
                place: place(7, 100),
                durable: None,
            },
            &["kept from offset 100 of file 7", "nowhere"],
            false,
        ),
        (LogError::WrongStore, &["another log"], false),
        (
            LogError::UnknownFile { file: file_id(7) },
            &["no log file 7"],
            false,
        ),
        (
            LogError::UnknownAddress {
                address: LogAddress::new(4711).unwrap(),
            },
            &["address 4711"],
            false,
        ),
        (
            LogError::ValueTooLong { extent: 1 << 40 },
            &["1099511627776 bytes"],
            false,
        ),
    ];
    for (error, parts, has_source) in cases {
        let message = error.to_string();
        for part in parts {
            assert!(message.contains(part), "{message:?} lacks {part:?}");
        }
        assert_eq!(error.source().is_some(), has_source, "{message}");
        let reports_io = matches!(
            error,
            LogError::Io { .. }
                | LogError::NotDeleted { .. }
                | LogError::NotAppended { .. }
                | LogError::NoActiveFileSince { .. }
                | LogError::TakenBack { .. }
                | LogError::Unsettled { .. }
                | LogError::SyncFailed { .. }
        );
        assert_eq!(error.io_error().is_some(), reports_io, "{message}");
        assert!(!format!("{error:?}").is_empty());
    }
}

#[test]
fn an_error_of_the_manifest_keeps_what_the_file_layer_said() {
    let io = SegmentStoreError::Io {
        op: "rename",
        path: PathBuf::from("/db/MANIFEST.tmp"),
        source: io::Error::other("no"),
    };
    let error = LogError::from(io);
    assert!(
        matches!(&error, LogError::Io { op: "rename", .. }),
        "{error}"
    );
    assert!(error.io_error().is_some());

    let format = SegmentStoreError::Format {
        path: PathBuf::from("/db/MANIFEST"),
        offset: 3,
        source: FormatError::BadMagic,
    };
    let error = LogError::from(format);
    assert!(
        matches!(error, LogError::Format { offset: 3, .. }),
        "{error}"
    );

    let other = SegmentStoreError::NoManifest {
        dir: PathBuf::from("/db"),
    };
    let error = LogError::from(other);
    assert!(matches!(error, LogError::Manifest(_)), "{error}");
    assert_eq!(error.to_string(), "/db has no MANIFEST");
    assert!(error.io_error().is_none());

    // An error of the file layer inside an error of the manifest is found.
    let inside = LogError::Manifest(SegmentStoreError::Io {
        op: "sync",
        path: PathBuf::from("/db"),
        source: io::Error::other("no"),
    });
    assert!(inside.io_error().is_some());
}
