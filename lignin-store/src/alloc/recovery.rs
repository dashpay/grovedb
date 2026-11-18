//! The allocator of a database that is being opened: what the page table
//! and the replayed log say about each page id.

use std::collections::BTreeMap;

use lignin_format::{CommitVersion, PageId};

use super::{allocator::PageAllocator, error::RecoveryError, states::States, PageState};
use crate::table::{LinkKind, PageTable};

/// What the newest replayed commit that touched a page did to it. A commit
/// that changes a page and then frees it has freed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Replayed {
    Wrote,
    Freed,
}

/// Collects what replay finds about page ids, and makes the allocator of
/// the recovered database.
///
/// A page-table checkpoint lists no free ids (`page-table.md`, choice 14).
/// The allocator of a session is derived instead:
///
/// - A page whose newest link in the table is a base image or a delta run
///   is in use. One whose newest link is a free link was freed at that
///   link's version.
/// - Replay adds the pages that the commits after the checkpoint wrote and
///   freed, with [`Self::wrote`] and [`Self::freed`].
/// - Every other id is free.
///
/// A link of version *v* says that its chain holds every change to the
/// page up to *v* (`page-table.md` K3). So for one page the table is
/// believed up to the version of its newest link, and replay after it:
/// what is reported here for a version at or below that link is ignored.
/// Replay can therefore report every page change and every free of every
/// commit it takes, in any order. It reports nothing of a group it does not
/// take: [`Self::finish`] refuses a report above the version recovery
/// landed on.
#[derive(Debug, Default)]
pub struct Recovery {
    replayed: BTreeMap<PageId, (CommitVersion, Replayed)>,
}

impl Recovery {
    /// Nothing replayed yet.
    pub fn new() -> Self {
        Self::default()
    }

    fn replayed(&mut self, page: PageId, version: CommitVersion, what: Replayed) {
        let newest = self.replayed.entry(page).or_insert((version, what));
        *newest = (*newest).max((version, what));
    }

    /// The commit of `version` wrote `page`: it created the page or changed
    /// it, and did not free it.
    pub fn wrote(&mut self, page: PageId, version: CommitVersion) {
        self.replayed(page, version, Replayed::Wrote);
    }

    /// The commit of `version` freed `page`.
    pub fn freed(&mut self, page: PageId, version: CommitVersion) {
        self.replayed(page, version, Replayed::Freed);
    }

    /// The allocator of the recovered database, from `table` as it is when
    /// replay has ended and from what was reported. `landed` is the commit
    /// version recovery landed on.
    ///
    /// No id is private, since no transaction survives a crash. The ids of
    /// freed pages are retired with the versions that freed them. No pin
    /// and no guard survives either, so the first
    /// [`PageAllocator::reclaim`] frees them.
    ///
    /// Fails if a link of the table or a report is of a later version than
    /// `landed`, or a report is of version 0. Such a link or report is of a
    /// commit that is not part of the database, and the version numbers
    /// after `landed` are given to new commits: an id that seemed freed at
    /// one of them would be handed out while its page is in use.
    ///
    /// The table must not change while this runs.
    pub fn finish(
        self,
        table: &PageTable,
        landed: CommitVersion,
    ) -> Result<PageAllocator, RecoveryError> {
        for (&page, &(version, _)) in &self.replayed {
            if version == CommitVersion::EMPTY {
                return Err(RecoveryError::ReportOfNoCommit { page });
            }
            if version > landed {
                return Err(RecoveryError::ReportAfterLanding {
                    page,
                    version,
                    landed,
                });
            }
        }
        let mut states = States::new(PageId::MAX);
        let mut waiting: BTreeMap<CommitVersion, Vec<PageId>> = BTreeMap::new();
        let mut settle = |page: PageId, freed_at: Option<CommitVersion>| match freed_at {
            Some(freed_at) => {
                states.set(page, PageState::Retired);
                waiting.entry(freed_at).or_default().push(page);
            }
            None => {
                states.set(page, PageState::Live);
            }
        };
        let freed = |version, what| (what == Replayed::Freed).then_some(version);

        let mut links = Vec::new();
        let mut next = Some(PageId::MIN);
        while let Some(from) = next {
            next = table.scan(from, &mut links);
            let mut chains = links.drain(..).peekable();
            while let Some((page, newest)) = chains.next() {
                if chains.peek().is_some_and(|(after, _)| *after == page) {
                    continue;
                }
                if newest.version > landed {
                    return Err(RecoveryError::LinkAfterLanding {
                        page,
                        version: newest.version,
                        landed,
                    });
                }
                let freed_at = match self.replayed.get(&page) {
                    Some(&(version, what)) if newest.version < version => freed(version, what),
                    _ => (newest.kind == LinkKind::Free).then_some(newest.version),
                };
                settle(page, freed_at);
            }
        }
        for (&page, &(version, what)) in &self.replayed {
            if table.newest(page).is_none() {
                settle(page, freed(version, what));
            }
        }
        Ok(PageAllocator::from_parts(states, waiting))
    }
}
