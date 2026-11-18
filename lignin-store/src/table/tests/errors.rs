//! What the errors say.

use std::{error::Error, io, path::PathBuf};

use lignin_format::{FormatError, FormatVersion};

use super::*;

#[test]
fn a_link_error_says_which_rule_was_broken() {
    let location = at(3, 400);
    let cases = [
        (
            LinkError::InvalidLocation { location },
            "64 bytes at offset 400 of segment 3 cannot be a block",
        ),
        (
            LinkError::OutOfOrder {
                newest: version(9),
                version: version(7),
            },
            "a link of version 7 cannot follow the chain's newest, of version 9",
        ),
        (
            LinkError::DeltaWithoutBase,
            "a delta needs a base image beneath it",
        ),
        (LinkError::AlreadyFree, "the page is already freed"),
        (
            LinkError::NotInChain { location },
            "no link of the chain points to offset 400 of segment 3",
        ),
        (
            LinkError::TableFull,
            "the page table holds as many links as it can",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        assert!(error.source().is_none());
    }
}

#[test]
fn a_page_table_error_says_what_failed_and_where() {
    let path = || PathBuf::from("/db/table/00000007.pt");
    let io_error = || io::Error::other("the disk failed");
    let cases = [
        (
            PageTableError::io("append to", &path())(io_error()),
            "cannot append to /db/table/00000007.pt: the disk failed",
            true,
        ),
        (
            PageTableError::format(&path(), 20)(FormatError::MissingId),
            "/db/table/00000007.pt at offset 20: identifier field holds no identifier",
            true,
        ),
        (
            PageTableError::from(SegmentStoreError::ReadOnly),
            "the segment store is read-only",
            true,
        ),
        (
            PageTableError::Link {
                page: page(12),
                source: LinkError::TableFull,
            },
            "page 12: the page table holds as many links as it can",
            true,
        ),
        (
            PageTableError::FileTooShort {
                path: path(),
                valid_len: 500,
                len: 53,
            },
            "/db/table/00000007.pt is 53 bytes long, shorter than its valid length of 500",
            false,
        ),
        (
            PageTableError::FileNewerThanManifest {
                path: path(),
                file: FormatVersion::V1,
                manifest: FormatVersion::V1,
            },
            "/db/table/00000007.pt has format version 1, newer than the manifest's 1",
            false,
        ),
        (
            PageTableError::CoversMismatch {
                path: path(),
                manifest: version(8),
                file: version(6),
            },
            "/db/table/00000007.pt covers version 6, but the manifest says 8",
            false,
        ),
        (
            PageTableError::NotCovered {
                segment: id(4),
                needed: 900,
                valid_len: Some(700),
            },
            "the checkpoint points to a block that ends at offset 900 of segment 4, whose valid \
             length is 700",
            false,
        ),
        (
            PageTableError::NotCovered {
                segment: id(4),
                needed: 900,
                valid_len: None,
            },
            "the checkpoint points into segment 4, which the manifest does not name",
            false,
        ),
        (
            PageTableError::CoversWentBack {
                current: version(8),
                offered: version(6),
            },
            "a checkpoint that covers version 6 cannot replace one that covers 8",
            false,
        ),
        (
            PageTableError::LogBehind {
                needed: version(9),
                durable: version(7),
            },
            "the checkpoint needs the log durable through version 9, and it is durable through 7",
            false,
        ),
        (
            PageTableError::LogBehindSample {
                needed: LogPlace::new(id(4), 900).unwrap(),
                durable: LogPlace::new(id(3), 62).ok(),
            },
            "the checkpoint's sample of the value counts was taken at offset 900 of log file 4, \
             and the log is durable up to offset 62 of log file 3",
            false,
        ),
        (
            PageTableError::LogBehindSample {
                needed: LogPlace::new(id(4), 900).unwrap(),
                durable: None,
            },
            "the checkpoint's sample of the value counts was taken at offset 900 of log file 4, \
             and none of the log is durable",
            false,
        ),
        (
            PageTableError::NotDeleted {
                path: path(),
                source: io_error(),
            },
            "the manifest no longer names /db/table/00000007.pt, but it could not be deleted: \
             the disk failed",
            true,
        ),
        (
            PageTableError::FileIdsExhausted,
            "every checkpoint id has been used",
            false,
        ),
    ];
    for (error, text, has_source) in cases {
        assert_eq!(error.to_string(), text);
        assert_eq!(error.source().is_some(), has_source, "{error:?}");
    }
}
