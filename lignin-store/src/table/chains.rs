//! The chains of every page, in memory: a directly indexed array with the
//! newest link of each page, and an arena with the older ones.

use std::mem::size_of;

use lignin_format::{
    table::{self, BlockPointer},
    CommitVersion, FileId, PageId,
};

use super::{
    error::LinkError,
    page_table::{Link, LinkKind},
};
use crate::segment::BlockLocation;

const CHUNK_BITS: u32 = 12;
const CHUNK_LEN: usize = 1 << CHUNK_BITS;
const CHUNK_MASK: usize = CHUNK_LEN - 1;

const KIND_SHIFT: u32 = 28;
const LEN_MASK: u32 = (1 << KIND_SHIFT) - 1;
const BASE: u32 = 1;
const DELTA: u32 = 2;
const FREE: u32 = 3;

#[cfg(test)]
thread_local! {
    /// How many nodes this thread has read from the direct array and the
    /// arena.
    pub(super) static NODES_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

const _: () = assert!(table::MAX_BLOCK_LEN <= LEN_MASK);
const _: () = assert!(size_of::<Node>() == Chains::LINK_BYTES);

/// One link, or an empty slot.
///
/// `word` holds the block length in its low 28 bits and the kind above
/// them; a word of zero is an empty slot. `older` is the place in the arena
/// of the next older link of the same chain, counted from 1, or zero.
#[derive(Clone, Copy)]
#[repr(C, packed(4))]
struct Node {
    version: u64,
    offset: u64,
    file: u32,
    word: u32,
    older: u32,
}

impl Node {
    const EMPTY: Self = Self {
        version: 0,
        offset: 0,
        file: 0,
        word: 0,
        older: 0,
    };

    fn of(link: Link, block: Option<BlockPointer>, older: u32) -> Self {
        let code = match link.kind {
            LinkKind::Base(_) => BASE,
            LinkKind::Delta(_) => DELTA,
            LinkKind::Free => FREE,
        };
        Self {
            version: link.version.get(),
            offset: block.map_or(0, |block| block.offset()),
            file: block.map_or(0, |block| block.file().get()),
            word: code << KIND_SHIFT | block.map_or(0, |block| block.len()),
            older,
        }
    }

    fn is_empty(&self) -> bool {
        self.word == 0
    }

    fn code(&self) -> u32 {
        self.word >> KIND_SHIFT
    }

    fn version(&self) -> CommitVersion {
        CommitVersion::new(self.version).unwrap_or(CommitVersion::MAX)
    }

    fn location(&self) -> Option<BlockLocation> {
        let file = FileId::new(self.file)?;
        Some(BlockLocation {
            file,
            offset: self.offset,
            len: self.word & LEN_MASK,
        })
    }

    fn points_to(&self, location: BlockLocation) -> bool {
        self.location() == Some(location)
    }

    fn set_block(&mut self, block: BlockPointer) {
        self.offset = block.offset();
        self.file = block.file().get();
        self.word = self.code() << KIND_SHIFT | block.len();
    }

    fn link(&self) -> Option<Link> {
        let kind = match (self.code(), self.location()) {
            (BASE, Some(location)) => LinkKind::Base(location),
            (DELTA, Some(location)) => LinkKind::Delta(location),
            (FREE, _) => LinkKind::Free,
            _ => return None,
        };
        Some(Link {
            version: self.version(),
            kind,
        })
    }
}

fn pointer(location: BlockLocation) -> Result<BlockPointer, LinkError> {
    BlockPointer::new(location.file, location.offset, location.len)
        .map_err(|_| LinkError::InvalidLocation { location })
}

fn new_chunk() -> Box<[Node]> {
    vec![Node::EMPTY; CHUNK_LEN].into_boxed_slice()
}

/// Where a link of a chain is kept: in its page's slot of the direct array,
/// or in the arena.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Head,
    Arena(u32),
}

/// What a trim took out of one chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Trimmed {
    /// Whether the page is left without a link.
    pub(super) vacant: bool,
}

/// The chains of every page.
pub(super) struct Chains {
    /// The newest link of page `id` is slot `id - 1`.
    heads: Vec<Option<Box<[Node]>>>,
    head_chunks: usize,
    /// The older links. Place `n` of the arena is slot `n - 1`.
    arena: Vec<Box<[Node]>>,
    arena_used: u32,
    /// The first unused place of the arena that held a link before, or zero.
    /// Each such place names the next one in `older`.
    spare: u32,
    pages: u64,
    links: u64,
    high_water: u32,
}

impl Chains {
    /// Bytes one link takes, in the direct array and in the arena.
    pub(super) const LINK_BYTES: usize = 28;

    pub(super) fn new() -> Self {
        Self {
            heads: Vec::new(),
            head_chunks: 0,
            arena: Vec::new(),
            arena_used: 0,
            spare: 0,
            pages: 0,
            links: 0,
            high_water: 0,
        }
    }

    pub(super) fn pages(&self) -> u64 {
        self.pages
    }

    pub(super) fn links(&self) -> u64 {
        self.links
    }

    pub(super) fn high_water(&self) -> Option<PageId> {
        PageId::new(self.high_water)
    }

    pub(super) fn memory_usage(&self) -> usize {
        let chunk_bytes = CHUNK_LEN * size_of::<Node>();
        size_of::<Self>()
            + self.heads.capacity() * size_of::<Option<Box<[Node]>>>()
            + self.arena.capacity() * size_of::<Box<[Node]>>()
            + (self.head_chunks + self.arena.len()) * chunk_bytes
    }

    fn head(&self, page: PageId) -> Node {
        #[cfg(test)]
        NODES_READ.with(|nodes| nodes.set(nodes.get() + 1));
        let index = (page.get() - 1) as usize;
        match self.heads.get(index >> CHUNK_BITS) {
            Some(Some(chunk)) => chunk[index & CHUNK_MASK],
            _ => Node::EMPTY,
        }
    }

    fn head_mut(&mut self, page: PageId) -> &mut Node {
        let index = (page.get() - 1) as usize;
        let chunk = index >> CHUNK_BITS;
        if chunk >= self.heads.len() {
            self.heads.resize_with(chunk + 1, || None);
        }
        let head_chunks = &mut self.head_chunks;
        let chunk = self.heads[chunk].get_or_insert_with(|| {
            *head_chunks += 1;
            new_chunk()
        });
        &mut chunk[index & CHUNK_MASK]
    }

    fn older(&self, place: u32) -> Option<Node> {
        let index = place.checked_sub(1)? as usize;
        #[cfg(test)]
        NODES_READ.with(|nodes| nodes.set(nodes.get() + 1));
        Some(self.arena[index >> CHUNK_BITS][index & CHUNK_MASK])
    }

    fn older_mut(&mut self, place: u32) -> &mut Node {
        let index = (place - 1) as usize;
        &mut self.arena[index >> CHUNK_BITS][index & CHUNK_MASK]
    }

    fn node_mut(&mut self, page: PageId, place: Place) -> &mut Node {
        match place {
            Place::Head => self.head_mut(page),
            Place::Arena(place) => self.older_mut(place),
        }
    }

    /// Puts `node` into the arena and returns its place.
    fn keep(&mut self, node: Node) -> Result<u32, LinkError> {
        let place = if self.spare != 0 {
            let place = self.spare;
            self.spare = self.older_mut(place).older;
            place
        } else {
            let place = self.arena_used.checked_add(1).ok_or(LinkError::TableFull)?;
            if (place - 1) as usize >> CHUNK_BITS == self.arena.len() {
                self.arena.push(new_chunk());
            }
            self.arena_used = place;
            place
        };
        *self.older_mut(place) = node;
        Ok(place)
    }

    /// Takes the links from `place` on to the oldest out of the arena, and
    /// tells `removed` of each one that points to a block.
    fn release(&mut self, mut place: u32, removed: &mut Vec<Link>) {
        while let Some(node) = self.older(place) {
            removed.extend(node.link().filter(|link| link.location().is_some()));
            self.links -= 1;
            *self.older_mut(place) = Node {
                older: self.spare,
                ..Node::EMPTY
            };
            self.spare = place;
            place = node.older;
        }
    }

    /// The links of `page` from the newest to the oldest, each with its
    /// place.
    fn walk(&self, page: PageId) -> impl Iterator<Item = (Place, Node)> + '_ {
        let head = self.head(page);
        let first = (!head.is_empty()).then_some((Place::Head, head));
        std::iter::successors(first, |(_, node)| {
            let place = node.older;
            self.older(place).map(|older| (Place::Arena(place), older))
        })
    }

    pub(super) fn newest(&self, page: PageId) -> Option<Link> {
        self.head(page).link()
    }

    pub(super) fn chain(&self, page: PageId, out: &mut Vec<Link>) {
        let start = out.len();
        out.extend(self.walk(page).filter_map(|(_, node)| node.link()));
        out[start..].reverse();
    }

    pub(super) fn read(
        &self,
        page: PageId,
        at: CommitVersion,
        chain: &mut Vec<BlockLocation>,
    ) -> Option<CommitVersion> {
        chain.clear();
        let mut visible = self
            .walk(page)
            .map(|(_, node)| node)
            .skip_while(|node| node.version() > at);
        let newest = visible.next()?;
        let mut node = newest;
        while let Some(location) = node.location() {
            chain.push(location);
            if node.code() == BASE {
                chain.reverse();
                return Some(newest.version());
            }
            match visible.next() {
                Some(older) => node = older,
                None => break,
            }
        }
        chain.clear();
        None
    }

    pub(super) fn push(&mut self, page: PageId, link: Link) -> Result<(), LinkError> {
        let block = link.location().map(pointer).transpose()?;
        let head = self.head(page);
        let older = if head.is_empty() {
            if matches!(link.kind, LinkKind::Delta(_)) {
                return Err(LinkError::DeltaWithoutBase);
            }
            0
        } else {
            if link.version < head.version() {
                return Err(LinkError::OutOfOrder {
                    newest: head.version(),
                    version: link.version,
                });
            }
            match (head.code(), link.kind) {
                (FREE, LinkKind::Delta(_)) => return Err(LinkError::DeltaWithoutBase),
                (FREE, LinkKind::Free) => return Err(LinkError::AlreadyFree),
                _ => {}
            }
            self.keep(head)?
        };
        *self.head_mut(page) = Node::of(link, block, older);
        self.links += 1;
        if older == 0 {
            self.pages += 1;
            self.high_water = self.high_water.max(page.get());
        }
        Ok(())
    }

    pub(super) fn consolidate(
        &mut self,
        page: PageId,
        through: BlockLocation,
        base: BlockLocation,
    ) -> Result<CommitVersion, LinkError> {
        let block = pointer(base)?;
        let mut newer = None;
        let mut found = None;
        for (place, node) in self.walk(page) {
            if node.points_to(through) {
                found = Some((place, node));
                break;
            }
            newer = Some(place);
        }
        let (place, node) = found.ok_or(LinkError::NotInChain { location: through })?;
        let link = Link {
            version: node.version(),
            kind: LinkKind::Base(base),
        };
        match (place, newer) {
            (Place::Arena(place), Some(newer)) => {
                let base = self.keep(Node::of(link, Some(block), place))?;
                self.node_mut(page, newer).older = base;
            }
            _ => {
                let older = self.keep(node)?;
                *self.head_mut(page) = Node::of(link, Some(block), older);
            }
        }
        self.links += 1;
        Ok(link.version)
    }

    pub(super) fn relocate(
        &mut self,
        page: PageId,
        from: BlockLocation,
        to: BlockLocation,
    ) -> Result<(), LinkError> {
        let block = pointer(to)?;
        let place = self
            .walk(page)
            .find_map(|(place, node)| node.points_to(from).then_some(place))
            .ok_or(LinkError::NotInChain { location: from })?;
        self.node_mut(page, place).set_block(block);
        Ok(())
    }

    pub(super) fn trim(
        &mut self,
        page: PageId,
        oldest: CommitVersion,
        removed: &mut Vec<Link>,
    ) -> Trimmed {
        let mut newer = None;
        let mut anchor = None;
        for (place, node) in self.walk(page) {
            if node.version() <= oldest && node.code() != DELTA {
                anchor = Some((place, node));
                break;
            }
            newer = Some(place);
        }
        let Some((place, node)) = anchor else {
            return Trimmed {
                vacant: self.head(page).is_empty(),
            };
        };
        if node.code() == BASE {
            self.node_mut(page, place).older = 0;
            self.release(node.older, removed);
            return Trimmed { vacant: false };
        }
        match (place, newer) {
            (Place::Arena(place), Some(newer)) => {
                self.node_mut(page, newer).older = 0;
                self.release(place, removed);
                Trimmed { vacant: false }
            }
            _ => {
                *self.head_mut(page) = Node::EMPTY;
                self.links -= 1;
                self.pages -= 1;
                self.release(node.older, removed);
                Trimmed { vacant: true }
            }
        }
    }

    /// Takes the newest link off the chain in slot `index` of chunk `chunk`.
    fn pop(&mut self, chunk: usize, index: usize) {
        let Some(heads) = &self.heads[chunk] else {
            return;
        };
        let head = heads[index];
        let older = self.older(head.older);
        if older.is_some() {
            *self.older_mut(head.older) = Node {
                older: self.spare,
                ..Node::EMPTY
            };
            self.spare = head.older;
        } else {
            self.pages -= 1;
        }
        self.links -= 1;
        if let Some(heads) = &mut self.heads[chunk] {
            heads[index] = older.unwrap_or(Node::EMPTY);
        }
    }

    pub(super) fn discard_after(&mut self, version: CommitVersion) -> u64 {
        let before = self.links;
        for chunk in 0..self.heads.len() {
            if self.heads[chunk].is_none() {
                continue;
            }
            for index in 0..CHUNK_LEN {
                while matches!(
                    &self.heads[chunk],
                    Some(heads) if !heads[index].is_empty() && heads[index].version() > version
                ) {
                    self.pop(chunk, index);
                }
            }
        }
        before - self.links
    }

    /// Adds to `out` every link of the next pages that have one, from slot
    /// `from` on: those of one chunk of the array. They are in ascending
    /// order of page id, and each chain is from its oldest link. Returns the
    /// slot to go on from, or `None` if no page from `from` on has a link.
    pub(super) fn collect(&self, from: usize, out: &mut Vec<(PageId, Link)>) -> Option<usize> {
        let first = from >> CHUNK_BITS;
        let (chunk, heads) = self
            .heads
            .iter()
            .enumerate()
            .skip(first)
            .find_map(|(chunk, heads)| Some((chunk, heads.as_ref()?)))?;
        let skip = if chunk == first { from & CHUNK_MASK } else { 0 };
        let next = (chunk + 1) << CHUNK_BITS;
        for (slot, head) in heads.iter().enumerate().skip(skip) {
            let index = (chunk << CHUNK_BITS) + slot;
            let page = u32::try_from(index + 1).ok().and_then(PageId::new);
            let (false, Some(page)) = (head.is_empty(), page) else {
                continue;
            };
            let start = out.len();
            out.extend(
                self.walk(page)
                    .filter_map(|(_, node)| Some((page, node.link()?))),
            );
            out[start..].reverse();
        }
        Some(next)
    }
}
