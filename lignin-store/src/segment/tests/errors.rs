use std::{error::Error, io};

use super::*;

#[test]
fn every_error_says_what_went_wrong() {
    let path = || PathBuf::from("db/seg/00000007.seg");
    let dir = || PathBuf::from("db");
    let file = id(7);
    let location = BlockLocation {
        file,
        offset: 4000,
        len: 96,
    };
    let cases = [
        (
            SegmentStoreError::Io {
                op: "sync",
                path: path(),
                source: io::Error::other("disk on fire"),
            },
            "cannot sync db/seg/00000007.seg: disk on fire",
        ),
        (
            SegmentStoreError::Format {
                path: path(),
                offset: 34,
                source: FormatError::BadMagic,
            },
            "db/seg/00000007.seg at offset 34: not a LigninDB file: bad magic",
        ),
        (
            SegmentStoreError::NoManifest { dir: dir() },
            "db has no MANIFEST",
        ),
        (
            SegmentStoreError::ManifestExists { dir: dir() },
            "db already has a MANIFEST",
        ),
        (
            SegmentStoreError::ManifestInDoubt { dir: dir() },
            "a replacement of the manifest of db did not finish; open it again",
        ),
        (
            SegmentStoreError::ManifestChange(ManifestChange::FileShortened {
                kind: FileKind::Segment,
                id: file,
            }),
            "invalid manifest replacement: Segment file 7 has a lower valid length than it had",
        ),
        (
            SegmentStoreError::WrongDirectory {
                expected: dir(),
                found: PathBuf::from("checkpoint"),
            },
            "manifest of checkpoint passed to the segment store of db",
        ),
        (
            SegmentStoreError::FileTooShort {
                path: path(),
                valid_len: 500,
                len: 499,
            },
            "db/seg/00000007.seg is 499 bytes long, shorter than its valid length of 500",
        ),
        (
            SegmentStoreError::FileNewerThanManifest {
                path: path(),
                file: FormatVersion::V1,
                manifest: FormatVersion::V1,
            },
            "db/seg/00000007.seg has format version 1, newer than the manifest's 1",
        ),
        (
            SegmentStoreError::UnnamedSegmentHoldsBlocks {
                path: path(),
                len: 9000,
            },
            "db/seg/00000007.seg is 9000 bytes long, but the manifest does not name it and \
             records no id that high",
        ),
        (SegmentStoreError::UnknownSegment { file }, "no segment 7"),
        (
            SegmentStoreError::OutOfBounds {
                location,
                end: 4090,
            },
            "96 bytes at offset 4000 of segment 7 are not inside its blocks, which end at 4090",
        ),
        (
            SegmentStoreError::NotSealed { file },
            "segment 7 is not sealed",
        ),
        (
            SegmentStoreError::NotNamed { file },
            "no manifest names segment 7 yet",
        ),
        (
            SegmentStoreError::NotOpen { file },
            "segment 7 takes no more blocks",
        ),
        (
            SegmentStoreError::SegmentFailed { file },
            "a sync of segment 7 failed",
        ),
        (SegmentStoreError::SegmentFull { file }, "segment 7 is full"),
        (
            SegmentStoreError::SegmentBusy { file },
            "segment 7 still has a writer",
        ),
        (
            SegmentStoreError::FileIdsExhausted,
            "every segment id has been used",
        ),
        (
            SegmentStoreError::ReadOnly,
            "the segment store is read-only",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
        let has_source = matches!(
            error,
            SegmentStoreError::Io { .. } | SegmentStoreError::Format { .. }
        );
        assert_eq!(error.source().is_some(), has_source, "{error:?}");
    }
}

#[test]
fn every_refused_manifest_change_names_the_file() {
    let (kind, id) = (FileKind::Wal, id(44));
    let cases = [
        (
            ManifestChange::LastFileIdLowered { kind },
            "the last Wal file id is lower than it was",
        ),
        (
            ManifestChange::FileIdReused { kind, id },
            "Wal file 44 is new, but its id is not above the last id of its kind",
        ),
        (
            ManifestChange::FileShortened { kind, id },
            "Wal file 44 has a lower valid length than it had",
        ),
        (
            ManifestChange::FinishedFileChanged { kind, id },
            "Wal file 44 was closed or sealed and has another state or length",
        ),
        (
            ManifestChange::TableCheckpointChanged { id },
            "page-table checkpoint 44 has another length or commit version than it had",
        ),
    ];
    for (change, expected) in cases {
        assert_eq!(change.to_string(), expected);
    }
}
