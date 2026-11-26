mod damage;
mod drops;
mod errors;
mod fragments;
mod lookup;
mod model;
mod moves;
mod pending;
mod records;
mod restated;
mod roots;
mod store;

use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap},
};

use lignin_format::{
    flat_page::FlatPage,
    value_log::{PendingValue, ValuePlace},
    CellAddress, FormatVersion, PageId, PageSize, SlotId,
};

use super::{
    record::RecordKind, Census, Child, Directory, DirectoryError, Dropped, Entry, Host, MerkId,
    Prefix, Root, RootChange,
};
use crate::flat::{FlatConfig, FlatError, FlatPages, FlatPagesMut, Revision};

const V1: FormatVersion = FormatVersion::V1;

/// The flat pages of one writer, in memory. They count what is read of
/// them, and a copy of them is a snapshot.
#[derive(Debug, Clone)]
struct Pages {
    config: FlatConfig,
    pages: BTreeMap<PageId, Vec<u8>>,
    /// The values out of line, by the bytes that name them.
    values: HashMap<[u8; 8], Vec<u8>>,
    next_page: u32,
    next_value: u64,
    revision: u64,
    /// How many pages were read, and how many values out of line.
    read: Cell<(u64, u64)>,
    /// How many more pages may be written before a write fails.
    writes_left: Option<u64>,
    /// How many images of leaves were written, and how many pages were
    /// freed.
    leaf_writes: u64,
    frees: u64,
}

impl Pages {
    fn new() -> Self {
        Self {
            config: FlatConfig::new(PageSize::MIN),
            pages: BTreeMap::new(),
            values: HashMap::new(),
            next_page: 0,
            next_value: 0,
            revision: 0,
            read: Cell::new((0, 0)),
            writes_left: None,
            leaf_writes: 0,
            frees: 0,
        }
    }

    /// An empty directory in pages of the smallest size.
    fn directory() -> (Self, Directory) {
        let mut pages = Self::new();
        let config = pages.config;
        let directory = Directory::create(&mut pages, config).expect("a directory");
        (pages, directory)
    }

    /// The pages and the values out of line that `read` reads.
    fn reads<R>(&self, read: impl FnOnce() -> R) -> (R, (u64, u64)) {
        let (pages, values) = self.read.get();
        let out = read();
        let (pages_after, values_after) = self.read.get();
        (out, (pages_after - pages, values_after - values))
    }

    /// How many pages lie on the way from the root of the directory to a
    /// leaf.
    fn levels(&self, directory: &Directory) -> u64 {
        let root = &self.pages[&directory.root_page()];
        u64::from(FlatPage::decode(root, V1).expect("a page").level()) + 1
    }
}

impl FlatPages for Pages {
    type Page<'a> = &'a [u8];

    fn page(&self, page: PageId) -> Result<&[u8], FlatError> {
        let (pages, values) = self.read.get();
        self.read.set((pages + 1, values));
        let image = self.pages.get(&page).ok_or(FlatError::Freed { page })?;
        Ok(image)
    }

    fn read_value(
        &self,
        _held: &&[u8],
        page: PageId,
        _slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        let (pages, values) = self.read.get();
        self.read.set((pages, values + 1));
        let value = place.pending().and_then(|held| self.values.get(&held.id()));
        out.extend_from_slice(value.ok_or(FlatError::NoValue { page })?);
        Ok(())
    }

    fn revision(&self) -> Revision {
        Revision::Written(self.revision)
    }
}

impl FlatPagesMut for Pages {
    fn allocate(&mut self) -> Result<PageId, FlatError> {
        self.next_page += 1;
        Ok(page(self.next_page))
    }

    fn take(&mut self, page: PageId) -> Result<Vec<u8>, FlatError> {
        Ok(self.page(page)?.to_vec())
    }

    fn write(&mut self, page: PageId, image: &[u8]) -> Result<(), FlatError> {
        if let Some(left) = &mut self.writes_left {
            *left = left.checked_sub(1).ok_or(FlatError::Freed { page })?;
        }
        let read = FlatPage::decode(image, V1).expect("a page");
        self.leaf_writes += u64::from(read.leaf().is_some());
        self.pages.insert(page, image.to_vec());
        self.revision += 1;
        Ok(())
    }

    fn free(&mut self, page: PageId) -> Result<(), FlatError> {
        self.frees += 1;
        self.pages.remove(&page);
        self.revision += 1;
        Ok(())
    }

    fn hold_value(&mut self, _page: PageId, value: &[u8]) -> Result<PendingValue, FlatError> {
        self.next_value += 1;
        let name = (1 << 63 | self.next_value).to_le_bytes();
        self.values.insert(name, value.to_vec());
        Ok(PendingValue::new(name, value.len() as u32)?)
    }

    fn drop_value(&mut self, _page: PageId, place: ValuePlace) -> Result<(), FlatError> {
        if let Some(held) = place.pending() {
            self.values.remove(&held.id());
        }
        Ok(())
    }
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

/// The address of the cell in slot `slot` of page `page`.
fn cell(page_id: u32, slot: u16) -> CellAddress {
    CellAddress::new(page(page_id), SlotId::new(slot).expect("not zero"))
}

fn id(raw: u64) -> MerkId {
    MerkId::new(raw).expect("not zero")
}

/// One step of a path: the key of a host, and the position of the Merk
/// among the host's.
type Seg = (Vec<u8>, u8);

/// The root of a Merk as a test keeps it: the cell, and the root's key.
type RootAt = Option<(CellAddress, Vec<u8>)>;

/// The prefix of the subtree at `path`, as these tests derive it: all zero
/// for the root of the grove, and otherwise 32 bytes that follow from the
/// whole path, as the hash of a path does.
fn prefix_of(path: &[Seg]) -> Prefix {
    let mut prefix = Prefix::default();
    if path.is_empty() {
        return prefix;
    }
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |byte: u8| state = (state ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    for (key, position) in path {
        key.iter().copied().for_each(&mut mix);
        mix(*position);
        mix(0xff);
        mix(key.len() as u8);
    }
    for chunk in prefix.as_chunks_mut::<8>().0 {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut word = state;
        word = (word ^ (word >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        word = (word ^ (word >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        chunk.copy_from_slice(&(word ^ (word >> 31)).to_le_bytes());
    }
    prefix
}

/// A path of one-key steps at position zero.
fn path(keys: &[&str]) -> Vec<Seg> {
    keys.iter()
        .map(|key| (key.as_bytes().to_vec(), 0))
        .collect()
}

fn named<'a>(prefix: &'a Prefix, root: Option<(CellAddress, &'a [u8])>) -> RootChange<'a> {
    let root = root.map(|(address, key)| Root { address, key });
    RootChange::Named { prefix, root }
}

/// A grove for the tests that need a few Merks: every Merk is entered by
/// its path, under the host its path ends with, and has a root when it is
/// asked for.
struct Grove {
    pages: Pages,
    directory: Directory,
    next_cell: u16,
}

impl Grove {
    fn new() -> Self {
        let (pages, directory) = Pages::directory();
        let mut grove = Self {
            pages,
            directory,
            next_cell: 0,
        };
        grove.add(&[]);
        grove
    }

    /// Enters the Merk at `keys`, and gives the Merk above it a root if it
    /// has none: it holds the host now.
    fn add(&mut self, keys: &[&str]) -> MerkId {
        let at = path(keys);
        let Some(((key, position), above)) = at.split_last() else {
            let added = self
                .directory
                .add(&mut self.pages, &prefix_of(&[]), b"tree", None);
            return added.expect("the root of the grove");
        };
        let parent = prefix_of(above);
        if self.entry(&keys[..keys.len() - 1]).root().is_none() {
            self.root(&keys[..keys.len() - 1]);
        }
        let host = Host {
            parent: &parent,
            key,
            position: *position,
        };
        let added = self
            .directory
            .add(&mut self.pages, &prefix_of(&at), b"tree", Some(host));
        added.expect("entered")
    }

    /// Gives the Merk at `keys` a root in a cell of its own, and returns
    /// the cell's address.
    fn root(&mut self, keys: &[&str]) -> CellAddress {
        self.next_cell += 1;
        let address = cell(9, self.next_cell);
        let prefix = prefix_of(&path(keys));
        let change = named(&prefix, Some((address, b"root")));
        self.directory
            .set_roots(&mut self.pages, &[change])
            .expect("a root");
        address
    }

    fn open(&self, keys: &[&str]) -> Option<Entry> {
        let prefix = prefix_of(&path(keys));
        self.directory.open(&self.pages, &prefix).expect("a lookup")
    }

    fn entry(&self, keys: &[&str]) -> Entry {
        self.open(keys).expect("the Merk is entered")
    }

    fn stored(&self, keys: &[&str]) -> Option<Entry> {
        let prefix = prefix_of(&path(keys));
        self.directory
            .stored(&self.pages, &prefix)
            .expect("a lookup")
    }

    fn unlink(&mut self, keys: &[&str]) -> Dropped {
        let prefix = prefix_of(&path(keys));
        self.directory
            .unlink(&mut self.pages, &prefix)
            .expect("dropped")
    }

    fn children(&self, parent: MerkId) -> Vec<Child> {
        let mut walk = self.directory.children(parent);
        std::iter::from_fn(|| walk.next(&self.pages).expect("a child record")).collect()
    }

    fn drops(&self) -> Vec<Dropped> {
        let mut walk = self.directory.drops();
        std::iter::from_fn(|| walk.next(&self.pages).expect("a record")).collect()
    }

    /// Reclaims all that the dropped Merk `dropped` left, the innermost
    /// Merks first, as the reclaimer does, and returns how many Merks
    /// below it had a record.
    fn reclaim(&mut self, dropped: MerkId) -> usize {
        let mut taken = 0;
        let mut above = vec![dropped];
        while let Some(&parent) = above.last() {
            match self.children(parent).into_iter().next() {
                Some(child) if !self.children(child.id).is_empty() => above.push(child.id),
                Some(child) => {
                    self.directory
                        .reclaim_child(&mut self.pages, parent, &child)
                        .expect("reclaimed");
                    taken += 1;
                }
                None => {
                    above.pop();
                }
            }
        }
        self.directory
            .reclaimed(&mut self.pages, dropped)
            .expect("all of it is reclaimed");
        taken
    }

    fn check(&self) -> Census {
        self.directory.check(&self.pages).expect("a directory")
    }

    /// The records of one kind, as their keys without the code.
    fn keys(&self, kind: RecordKind) -> Vec<Vec<u8>> {
        let mut cursor = self.directory.tree().cursor_within(&[kind.code()]);
        let mut out = Vec::new();
        cursor.seek_to_first(&self.pages).expect("a walk");
        while let Some(key) = cursor.key() {
            out.push(key.to_vec());
            cursor.next(&self.pages).expect("a walk");
        }
        out
    }
}
