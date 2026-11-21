//! What a recovery's error says, for the errors that no test of a store
//! brings about: the parts of the store that refuse what recovery hands
//! them do so only for what recovery never hands them.

use std::error::Error;

use lignin_format::{delta::DeltaError, FormatError};

use super::*;
use crate::{
    alloc::RecoveryError as AllocatorError,
    cache::{CacheError, StepRefusal},
    table::LinkError,
};

#[test]
fn every_error_says_what_failed_and_where() {
    let at = LogPlace::new(lignin_format::FileId::new(3).expect("an id"), 99).expect("a place");
    let not_a_page = || FormatError::from(DeltaError::MissingBase);
    let cases: Vec<(RecoveryError, &str, bool)> = vec![
        (
            RecoveryError::Pool(CacheError::WrongPool),
            "the buffer pool refused a rebuilt page: ",
            false,
        ),
        (
            RecoveryError::Step {
                page: page(4),
                source: StepRefusal::NotCurrent,
            },
            "the buffer pool refused the rebuilt frame of page 4: ",
            false,
        ),
        (
            RecoveryError::Allocator(AllocatorError::ReportOfNoCommit { page: page(4) }),
            "the page ids were not recovered: ",
            false,
        ),
        (
            RecoveryError::Loader {
                source: LoadError::FrameSize {
                    page_size: 8192,
                    frame_len: 4096,
                },
            },
            "the store's pages cannot be read: ",
            false,
        ),
        (
            RecoveryError::Rebuilt {
                page: page(4),
                source: not_a_page(),
            },
            "page 4 is no page of its format as the log leaves it: ",
            true,
        ),
        (
            RecoveryError::Link {
                page: page(4),
                source: LinkError::AlreadyFree,
            },
            "the page table refused a link of page 4 that replay made: ",
            false,
        ),
        (
            RecoveryError::Change {
                page: page(4),
                at,
                source: not_a_page(),
            },
            "the item at offset 99 of log file 3 does not apply to page 4: ",
            true,
        ),
    ];
    for (error, begins, damage) in cases {
        let said = error.to_string();
        assert!(said.starts_with(begins), "{said}");
        assert!(said.len() > begins.len(), "{said}");
        assert_eq!(error.is_damage(), damage, "{said}");
        let source = error.source().expect("what the part reported");
        assert!(said.ends_with(&source.to_string()), "{said}");
        assert!(!format!("{error:?}").is_empty());
    }
}

#[test]
fn the_errors_of_the_parts_become_the_error_of_recovery() {
    let pool: RecoveryError = CacheError::WrongPool.into();
    assert!(matches!(pool, RecoveryError::Pool(CacheError::WrongPool)));
    let ids: RecoveryError = AllocatorError::ReportOfNoCommit { page: page(1) }.into();
    assert!(matches!(ids, RecoveryError::Allocator(_)));
}

#[test]
fn the_options_are_the_defaults_of_the_parts() {
    let options = RecoveryOptions::default();
    assert_eq!(options.pool_capacity, DEFAULT_POOL_CAPACITY);
    assert_eq!(options.log, LogOptions::default());
    assert_eq!((options.flush, options.max_replay_pages), (None, None));
    assert_eq!(options.read_window, DEFAULT_READ_WINDOW_LEN);

    // With no thresholds given, write-back gets those of the store's
    // page size, and replay keeps as many pages as a committer may leave
    // waiting.
    let dir = Dir::new();
    let store = recover(dir.manifest(), &options).expect("the store recovers");
    assert_eq!(store.flush.config(), FlushConfig::new(PageSize::MIN));
    assert_eq!(store.pool.frame_len(), PageSize::MIN.get());
    assert!(store.pool.capacity() >= DEFAULT_POOL_CAPACITY - PageSize::MIN.get());

    // A pool that cannot hold one page is refused before anything is
    // read.
    let tiny = RecoveryOptions {
        pool_capacity: 16,
        ..options
    };
    drop(store);
    let error = recover(dir.manifest(), &tiny).expect_err("no room for a page");
    assert!(matches!(error, RecoveryError::Pool(_)), "{error}");
}
