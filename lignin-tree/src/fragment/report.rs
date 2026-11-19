//! What a write did to the pages, as it reports it.

use std::ops::{Index, IndexMut};

use lignin_format::{CellAddress, PageId};

use crate::node::NodeHandle;

/// The rule that carried a cell (`tree-layout.md`, sections 6 to 8).
///
/// A rotation is not among them: it changes links and carries nothing (R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cause {
    /// A full page or a fragment above the largest size: L1, L2, V2, V3.
    Split,
    /// A small fragment that joined its parent's page, or a page a delete
    /// left nearly empty: V1, M1, M2.
    Merge,
    /// A node that took the place of a deleted node, and its page: R2.
    Delete,
}

impl Cause {
    /// Every cause.
    pub const ALL: [Self; 3] = [Self::Split, Self::Merge, Self::Delete];
}

/// The cells one cause carried.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Moves {
    /// How many cells were carried to another page.
    pub cells: u64,
    /// The bytes of those cells, with their slots.
    pub cell_bytes: u64,
    /// How many of them took an out-of-line value with them, which is
    /// appended again (`tree-layout.md` X1). A value the write wrote anyway
    /// is not counted.
    pub values: u64,
    /// The bytes of those values, without the headers of their records.
    pub value_bytes: u64,
}

/// The cells that were carried, by cause.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Carries([Moves; Cause::ALL.len()]);

impl Carries {
    /// The cells carried by all causes together.
    pub fn total(&self) -> Moves {
        self.0.iter().fold(Moves::default(), |mut total, moves| {
            total.cells += moves.cells;
            total.cell_bytes += moves.cell_bytes;
            total.values += moves.values;
            total.value_bytes += moves.value_bytes;
            total
        })
    }

    fn add(&mut self, other: &Self) {
        for cause in Cause::ALL {
            let (into, from) = (&mut self[cause], other[cause]);
            into.cells += from.cells;
            into.cell_bytes += from.cell_bytes;
            into.values += from.values;
            into.value_bytes += from.value_bytes;
        }
    }
}

impl Index<Cause> for Carries {
    type Output = Moves;

    fn index(&self, cause: Cause) -> &Moves {
        &self.0[cause as usize]
    }
}

impl IndexMut<Cause> for Carries {
    fn index_mut(&mut self, cause: Cause) -> &mut Moves {
        &mut self.0[cause as usize]
    }
}

/// A cell that the write carried to another page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carried {
    /// Where the cell was.
    pub from: CellAddress,
    /// The node where it is now.
    pub to: NodeHandle,
    /// The rule that carried it last.
    pub cause: Cause,
    /// The node's position in [`Write::nodes`](super::Write::nodes), if it
    /// is a node of the write. The others travelled with a fragment.
    pub node: Option<usize>,
    /// For the root of a hosted Merk: the address of its host's cell after
    /// the write, and the root's position among the Merks the host has. The
    /// Merk's directory entry names the root, and is the caller's to
    /// rewrite (`tree-layout.md` S3).
    pub hosted_by: Option<(CellAddress, u8)>,
}

/// What one write did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The nodes of the write after it, in the order of
    /// [`Write::nodes`](super::Write::nodes). Their handles are good from
    /// here on, under the lease the pages were opened with.
    pub nodes: Vec<NodeHandle>,
    /// What each root pointer of the write names now, in the order of
    /// [`Write::roots`](super::Write::roots).
    pub roots: Vec<Option<CellAddress>>,
    /// The cells that were carried, a child before its parent.
    pub carried: Vec<Carried>,
    /// The cells that nothing points at any more and that the write did not
    /// remove: the roots of what a dropped or cleared Merk left
    /// (`tree-layout.md` D1). The reclaimer starts from them.
    pub orphaned: Vec<CellAddress>,
    /// The pages a cell left. Every handle into them that was issued before
    /// the write has ended, also those of the nodes that stayed, which are
    /// read again by their address.
    pub left: Vec<PageId>,
    /// The pages the write started.
    pub created: Vec<PageId>,
    /// The pages the write emptied and freed.
    pub freed: Vec<PageId>,
    /// The cells the write carried, by cause.
    pub carries: Carries,
}

/// What the writes of one [`Fragments`](super::Fragments) did, in all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// The writes that were applied.
    pub writes: u64,
    /// The cells they put into pages for new nodes.
    pub created: u64,
    /// The cells they took out for deleted nodes.
    pub removed: u64,
    /// The cells they carried, by cause, and the values that went with them.
    pub carries: Carries,
    /// The pages they started.
    pub pages_created: u64,
    /// The pages they freed.
    pub pages_freed: u64,
}

impl Counters {
    pub(super) fn count(&mut self, report: &Report, created: usize, removed: usize) {
        self.writes += 1;
        self.created += created as u64;
        self.removed += removed as u64;
        self.carries.add(&report.carries);
        self.pages_created += report.created.len() as u64;
        self.pages_freed += report.freed.len() as u64;
    }
}
