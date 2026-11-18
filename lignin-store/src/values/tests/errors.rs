//! What the errors say and carry.

use std::{error::Error, io, path::PathBuf};

use lignin_format::{value_log::ValuePointer, FormatError, FormatVersion};

use super::*;
use crate::segment::ManifestChange;

fn pointer() -> ValuePointer {
    ValuePointer::new(file_id(4), 120, 300).expect("a valid pointer")
}

fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "denied")
}

#[test]
fn errors_say_what_is_wrong() {
    let path = || PathBuf::from("/db/val/00000004.val");
    let messages = [
        (
            ValueStoreError::Io {
                op: "append to",
                path: path(),
                source: denied(),
            },
            "cannot append to /db/val/00000004.val: denied",
        ),
        (
            ValueStoreError::Format {
                path: path(),
                offset: 120,
                source: FormatError::BadMagic,
            },
            "/db/val/00000004.val at offset 120: not a LigninDB file: bad magic",
        ),
        (
            ValueStoreError::Manifest(SegmentStoreError::NoManifest { dir: "/db".into() }),
            "/db has no MANIFEST",
        ),
        (
            ValueStoreError::WrongDirectory {
                expected: "/db".into(),
                found: "/other".into(),
            },
            "manifest of /other passed to the value log of /db",
        ),
        (
            ValueStoreError::FileTooShort {
                path: path(),
                valid_len: 500,
                len: 20,
            },
            "/db/val/00000004.val is 20 bytes long, shorter than its valid length of 500",
        ),
        (
            ValueStoreError::FileNewerThanManifest {
                path: path(),
                file: FormatVersion::V1,
                manifest: FormatVersion::V1,
            },
            "/db/val/00000004.val has format version 1, newer than the manifest's 1",
        ),
        (
            ValueStoreError::WrongFileId {
                path: path(),
                found: 9,
            },
            "the header of /db/val/00000004.val says it is value-log file 9",
        ),
        (
            ValueStoreError::UnnamedFileHoldsValues {
                path: path(),
                len: 900,
            },
            "/db/val/00000004.val is 900 bytes long, but the manifest does not name it and \
             records no id that high",
        ),
        (
            ValueStoreError::LiveValuesInMissingFile {
                file: file_id(4),
                values: 3,
            },
            "value-log file 4 has 3 live values, but the manifest does not name it",
        ),
        (
            ValueStoreError::NotDeleted {
                path: path(),
                source: denied(),
            },
            "the manifest no longer names /db/val/00000004.val, but it could not be deleted: \
             denied",
        ),
        (
            ValueStoreError::UnknownFile { file: file_id(4) },
            "no value-log file 4",
        ),
        (
            ValueStoreError::OutOfBounds {
                pointer: pointer(),
                end: 200,
            },
            "a value of 300 bytes at offset 120 of value-log file 4 is not inside its values, \
             which end at 200",
        ),
        (
            ValueStoreError::ValueTooLong { extent: 1 << 33 },
            "a value whose records take 8589934592 bytes does not fit in memory here",
        ),
        (
            ValueStoreError::NotNamed { file: file_id(4) },
            "no manifest names value-log file 4 yet",
        ),
        (
            ValueStoreError::NotOpen { file: file_id(4) },
            "value-log file 4 takes no more values",
        ),
        (
            ValueStoreError::NotClosed { file: file_id(4) },
            "value-log file 4 is still being written",
        ),
        (
            ValueStoreError::FileFailed { file: file_id(4) },
            "a sync of value-log file 4 failed",
        ),
        (
            ValueStoreError::FileIdsExhausted,
            "every value-log file id has been used",
        ),
        (ValueStoreError::ReadOnly, "the value log is read-only"),
        (
            ValueStoreError::Miscounted {
                pointer: pointer(),
                why: Miscount::UnknownFile,
            },
            "the value at offset 120 of value-log file 4 cannot be counted: there is no such \
             value-log file",
        ),
        (
            ValueStoreError::Miscounted {
                pointer: pointer(),
                why: Miscount::OutOfBounds,
            },
            "the value at offset 120 of value-log file 4 cannot be counted: it ends past the \
             end of its file",
        ),
        (
            ValueStoreError::Miscounted {
                pointer: pointer(),
                why: Miscount::CountIsZero,
            },
            "the value at offset 120 of value-log file 4 cannot be counted: its file's count \
             of live values is already zero",
        ),
        (
            ValueStoreError::Owners("the page is gone".into()),
            "the owner of a value could not be asked: the page is gone",
        ),
    ];
    for (error, message) in messages {
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn an_error_carries_the_error_it_was_made_from() {
    let io = ValueStoreError::Io {
        op: "read",
        path: "/db/val/00000004.val".into(),
        source: denied(),
    };
    assert_eq!(io.source().expect("a source").to_string(), "denied");
    assert_eq!(
        io.io_error().map(io::Error::kind),
        Some(io::ErrorKind::PermissionDenied)
    );

    let not_deleted = ValueStoreError::NotDeleted {
        path: "/db/val/00000004.val".into(),
        source: denied(),
    };
    assert_eq!(
        not_deleted.source().expect("a source").to_string(),
        "denied"
    );
    assert!(not_deleted.io_error().is_some());

    let format = ValueStoreError::Format {
        path: "/db/val/00000004.val".into(),
        offset: 20,
        source: FormatError::MissingId,
    };
    assert_eq!(
        format.source().expect("a source").to_string(),
        FormatError::MissingId.to_string()
    );
    assert!(format.io_error().is_none());

    let manifest =
        ValueStoreError::Manifest(SegmentStoreError::ManifestInDoubt { dir: "/db".into() });
    assert!(manifest
        .source()
        .expect("a source")
        .to_string()
        .contains("/db"));

    let owners = ValueStoreError::Owners(Box::new(denied()));
    assert_eq!(owners.source().expect("a source").to_string(), "denied");
    assert!(owners.io_error().is_none());

    assert!(ValueStoreError::ReadOnly.source().is_none());
    assert!(ValueStoreError::ReadOnly.io_error().is_none());
}

#[test]
fn an_error_of_the_shared_file_code_keeps_its_kind() {
    let io: ValueStoreError = SegmentStoreError::Io {
        op: "create",
        path: "/db/val/00000004.val".into(),
        source: denied(),
    }
    .into();
    assert!(matches!(
        &io,
        ValueStoreError::Io { op: "create", path, .. } if path.ends_with("00000004.val")
    ));
    assert!(io.io_error().is_some());

    let format: ValueStoreError = SegmentStoreError::Format {
        path: "/db/MANIFEST".into(),
        offset: 0,
        source: FormatError::BadMagic,
    }
    .into();
    assert!(matches!(
        format,
        ValueStoreError::Format {
            offset: 0,
            source: FormatError::BadMagic,
            ..
        }
    ));

    let change = ManifestChange::FileShortened {
        kind: FileKind::ValueLog,
        id: file_id(4),
    };
    let refused: ValueStoreError = SegmentStoreError::ManifestChange(change).into();
    assert!(matches!(
        refused,
        ValueStoreError::Manifest(SegmentStoreError::ManifestChange(found)) if found == change
    ));
}
