//! The state of every page id: two bits for each id, in chunks of 4096 ids.

use lignin_format::PageId;

use super::PageState;

const CHUNK_BITS: u32 = 12;
const CHUNK_IDS: u64 = 1 << CHUNK_BITS;
const WORD_IDS: usize = 32;
const CHUNK_WORDS: usize = CHUNK_IDS as usize / WORD_IDS;
const EVERY_FIELD: u64 = 0x5555_5555_5555_5555;

const FREE: u64 = 0;
const PRIVATE: u64 = 1;
const LIVE: u64 = 2;
const RETIRED: u64 = 3;

type Chunk = [u64; CHUNK_WORDS];

const fn code(state: PageState) -> u64 {
    match state {
        PageState::Free => FREE,
        PageState::Private => PRIVATE,
        PageState::Live => LIVE,
        PageState::Retired => RETIRED,
    }
}

const fn state(code: u64) -> PageState {
    match code {
        PRIVATE => PageState::Private,
        LIVE => PageState::Live,
        RETIRED => PageState::Retired,
        _ => PageState::Free,
    }
}

/// The first id of chunk `chunk`, as a number.
const fn first_id(chunk: usize) -> u64 {
    chunk as u64 * CHUNK_IDS + 1
}

/// The id with number `raw`, which is from 1 to the limit of a map.
fn id(raw: u64) -> Option<PageId> {
    u32::try_from(raw).ok().and_then(PageId::new)
}

/// The state of every page id from 1 to a limit.
///
/// An id is free until it is given another state. The state of id `n` is
/// field `n - 1`, and a chunk that has never held anything but free ids is
/// not allocated.
#[derive(Clone)]
pub(super) struct States {
    chunks: Vec<Option<Box<Chunk>>>,
    /// The free ids of each chunk of `chunks`, up to the limit.
    free: Vec<u16>,
    /// No chunk before this one has a free id.
    search_from: usize,
    limit: u32,
    high_water: u32,
    /// The number of ids in each state but the free one, by its code.
    counts: [u64; 4],
}

impl States {
    pub(super) fn new(limit: PageId) -> Self {
        Self {
            chunks: Vec::new(),
            free: Vec::new(),
            search_from: 0,
            limit: limit.get(),
            high_water: 0,
            counts: [0; 4],
        }
    }

    /// The ids of chunk `chunk` that are not above the limit.
    fn capacity(&self, chunk: usize) -> u16 {
        (u64::from(self.limit) + 1)
            .saturating_sub(first_id(chunk))
            .min(CHUNK_IDS) as u16
    }

    fn place(page: PageId) -> (usize, usize, u32) {
        let index = (page.get() - 1) as usize;
        let field = index & (CHUNK_IDS as usize - 1);
        (
            index >> CHUNK_BITS,
            field / WORD_IDS,
            (field % WORD_IDS) as u32 * 2,
        )
    }

    pub(super) fn get(&self, page: PageId) -> PageState {
        let (chunk, word, shift) = Self::place(page);
        match self.chunks.get(chunk) {
            Some(Some(chunk)) => state(chunk[word] >> shift & 3),
            _ => PageState::Free,
        }
    }

    /// Gives `page` the state `new` and returns the state it had.
    pub(super) fn set(&mut self, page: PageId, new: PageState) -> PageState {
        let (chunk, word, shift) = Self::place(page);
        while self.chunks.len() <= chunk {
            self.free.push(self.capacity(self.chunks.len()));
            self.chunks.push(None);
        }
        let words = self.chunks[chunk].get_or_insert_with(|| Box::new([0; CHUNK_WORDS]));
        let old = words[word] >> shift & 3;
        let new = code(new);
        words[word] = words[word] & !(3 << shift) | new << shift;
        if old != FREE {
            self.counts[old as usize] -= 1;
        }
        if new != FREE {
            self.counts[new as usize] += 1;
            self.high_water = self.high_water.max(page.get());
        }
        if old == FREE && new != FREE {
            self.free[chunk] -= 1;
        } else if old != FREE && new == FREE {
            self.free[chunk] += 1;
            self.search_from = self.search_from.min(chunk);
        }
        state(old)
    }

    /// The lowest free id, or `None` if every id up to the limit has
    /// another state.
    pub(super) fn lowest_free(&mut self) -> Option<PageId> {
        let mut chunk = self.search_from;
        while chunk < self.chunks.len() && self.free[chunk] == 0 {
            chunk += 1;
        }
        self.search_from = chunk;
        let field = match self.chunks.get(chunk) {
            Some(Some(words)) => words.iter().enumerate().find_map(|(at, word)| {
                let free = !(word | word >> 1) & EVERY_FIELD;
                (free != 0).then(|| (at * WORD_IDS) as u64 + u64::from(free.trailing_zeros() / 2))
            })?,
            None if self.capacity(chunk) == 0 => return None,
            _ => 0,
        };
        id(first_id(chunk) + field)
    }

    /// The highest id that has had a state other than free.
    pub(super) fn high_water(&self) -> Option<PageId> {
        PageId::new(self.high_water)
    }

    pub(super) fn count(&self, of: PageState) -> u64 {
        self.counts[code(of) as usize]
    }

    /// Adds to `out` the ids that are not free, with their states, of the
    /// first chunk from the page `from` on that was ever used. Returns the
    /// page to go on from, or `None` if there is no such chunk.
    pub(super) fn scan(&self, from: PageId, out: &mut Vec<(PageId, PageState)>) -> Option<PageId> {
        let (first, ..) = Self::place(from);
        let (chunk, words) = self
            .chunks
            .iter()
            .enumerate()
            .skip(first)
            .find_map(|(chunk, words)| Some((chunk, words.as_ref()?)))?;
        let start = if chunk == first {
            u64::from(from.get())
        } else {
            first_id(chunk)
        };
        for raw in start..first_id(chunk + 1) {
            let Some(page) = id(raw) else { break };
            let (_, word, shift) = Self::place(page);
            let code = words[word] >> shift & 3;
            if code != FREE {
                out.push((page, state(code)));
            }
        }
        id(first_id(chunk + 1))
    }
}
