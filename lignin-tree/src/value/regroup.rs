//! What write-back takes from the tree layer: the scan order of a page's
//! cells, and the regrouping of its values.

use lignin_format::{tree_page::TreePage, PageId, PageSize, SlotId};
use lignin_store::{
    cache::BufferPool,
    flush::{FlushError, PageValues, RegroupError},
    load::{PageLoader, FRAME_FORMAT},
    values::{ValueStore, ValueWriter},
};

use super::{
    order::{scan_order, RegroupPolicy},
    owners::TreeOwners,
};
use crate::flat::StepLog;

/// The tree layer's part in write-back
/// ([`PageValues`](lignin_store::flush::PageValues)): the store knows no
/// tree, so the flusher is handed this to learn the order in which a scan
/// reads the cells of a page, and to have the values of a page regrouped
/// before it writes a new base image of it.
///
/// It is [`scan_order`] and the two halves of [`TreeOwners`]' regrouping
/// behind the one trait the store defines, with the policy that says which
/// pages are worth a regrouping. `log` takes the record of each page step:
/// in a store with write-back that is
/// [`WriteBack::append_change`](lignin_store::flush::WriteBack::append_change),
/// so that the flusher knows of the step.
#[derive(Debug)]
pub struct Regrouping<'s, L> {
    loader: PageLoader<'s>,
    pool: &'s BufferPool,
    values: &'s ValueStore,
    page_size: PageSize,
    log: L,
    policy: RegroupPolicy,
}

impl<'s, L: StepLog> Regrouping<'s, L> {
    /// The scan order and the regrouping for the tree pages that `loader`
    /// reads into `pool`, which are pages of `page_size` whose values are
    /// in `values`. A page is regrouped when `policy` asks for it.
    pub fn new(
        loader: PageLoader<'s>,
        pool: &'s BufferPool,
        values: &'s ValueStore,
        page_size: PageSize,
        log: L,
        policy: RegroupPolicy,
    ) -> Self {
        Self {
            loader,
            pool,
            values,
            page_size,
            log,
            policy,
        }
    }
}

impl<L: StepLog + Sync> PageValues for Regrouping<'_, L> {
    fn scan_order(&self, frame: &[u8], out: &mut Vec<SlotId>) {
        if let Ok(page) = TreePage::from_validated(frame, FRAME_FORMAT) {
            out.extend(scan_order(&page));
        }
    }

    fn regroup(
        &self,
        page: PageId,
        to: &mut ValueWriter,
        cover: &mut dyn FnMut(&mut ValueWriter) -> Result<(), FlushError>,
    ) -> Result<u64, RegroupError> {
        let owners = TreeOwners::new(self.loader, self.pool, self.page_size, &self.log);
        let Some(grouped) = owners.copy_grouped(self.values, page, to, self.policy)? else {
            return Ok(0);
        };
        cover(to)?;
        Ok(owners.install_grouped(self.values, grouped)?.moved)
    }
}
