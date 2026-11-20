//! What the errors say.

use std::error::Error;

use super::*;

#[test]
fn every_error_says_what_was_wrong() {
    let cases = [
        (
            CacheError::CapacityTooSmall {
                capacity: 10,
                frame: 9_000,
            },
            "a buffer pool of 10 bytes does not hold one frame of 9000 bytes",
        ),
        (
            CacheError::CapacityTooLarge,
            "the buffer pool is too large to allocate",
        ),
        (
            CacheError::LoadedTooNew {
                page: page(1),
                born: version(6),
                at: version(5),
            },
            "page 1 was loaded as born at version 6 for a reader at version 5",
        ),
        (
            CacheError::LoadedSuperseded {
                page: page(1),
                born: version(3),
                superseded: version(4),
                at: version(5),
            },
            "page 1 was loaded as born at version 3 for a reader at version 5, and it was \
             superseded at version 4",
        ),
        (
            CacheError::WrongPool,
            "the handle belongs to another buffer pool",
        ),
        (
            CacheError::Latched { page: page(2) },
            "an operation still holds the latch of the private copy of page 2",
        ),
        (
            CacheError::AlreadyPrivate {
                page: page(2),
                owner: owner(7),
            },
            "owner 7 already holds a private copy of page 2",
        ),
        (
            CacheError::NotPrivate { page: page(2) },
            "the frame of page 2 is not a private copy any more",
        ),
        (
            CacheError::BaseNotCurrent {
                page: page(2),
                base: Some(version(3)),
                current: Some(version(4)),
            },
            "a private copy of page 2 made from version 3 was installed over version 4",
        ),
        (
            CacheError::BaseNotCurrent {
                page: page(2),
                base: None,
                current: Some(version(4)),
            },
            "a private copy of page 2 made from no version was installed over version 4",
        ),
        (
            CacheError::VersionNotAfter {
                page: page(2),
                version: version(3),
                current: Some(version(4)),
            },
            "a commit of version 3 cannot follow version 4 of page 2",
        ),
        (
            CacheError::VersionNotAfter {
                page: page(2),
                version: version(0),
                current: None,
            },
            "a commit of version 0 cannot follow no version of page 2",
        ),
        (
            CacheError::NotUnwritten {
                page: page(2),
                born: version(3),
            },
            "page 2 as born at version 3 is not waiting to be written back",
        ),
        (
            CacheError::AttachedValues {
                page: page(2),
                born: version(3),
            },
            "page 2 as born at version 3 still has attached values",
        ),
        (
            CacheError::Replaced {
                page: page(2),
                born: version(3),
            },
            "the frame of page 2 as born at version 3 is no page version of the pool any more: \
             a page step replaced it, or its page was freed and the pool dropped it",
        ),
        (
            CacheError::StillCurrent {
                page: page(2),
                born: version(3),
            },
            "page 2 as born at version 3 is the current version of its page",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        assert!(error.source().is_none());
    }
}

#[test]
fn a_refused_page_step_says_why() {
    let cases = [
        (
            StepRefusal::NotCurrent,
            "the frame is not the current version of its page",
        ),
        (
            StepRefusal::PrivateCopy,
            "an open write transaction holds a private copy of the page",
        ),
        (
            StepRefusal::WrongPool,
            "the new frame belongs to another buffer pool",
        ),
    ];
    for (refusal, text) in cases {
        assert_eq!(refusal.to_string(), text);
        assert!(refusal.source().is_none());
    }
}
