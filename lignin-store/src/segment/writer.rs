//! Appending blocks to one segment, and sealing it.

use std::sync::Arc;

use lignin_format::{
    segment::{self, BlockContent, Compression, IndexEntry, DATA_START, MAX_BLOCKS},
    FileId, PageId,
};
use lignin_io::VfsFile;

use super::{
    error::SegmentStoreError,
    store::{BlockLocation, Segment},
};

/// The writer of one segment, from [`SegmentStore::create`].
///
/// It is the only appender of its file. Blocks it has appended can be read
/// through the store at once, from any thread.
///
/// Dropping a writer leaves its segment as it is: open in the manifest, and
/// taking no more blocks. What was appended stays readable, and
/// [`SegmentStore::sync`] still makes it durable. If a sync of the segment
/// has failed, dropping the writer closes the segment at the last length
/// that was synced, and the blocks after it can no longer be read.
///
/// [`SegmentStore::create`]: super::SegmentStore::create
/// [`SegmentStore::sync`]: super::SegmentStore::sync
#[derive(Debug)]
pub struct SegmentWriter {
    segment: Arc<Segment>,
    file: Box<dyn VfsFile>,
    index: Vec<IndexEntry>,
    end: u64,
    buf: Vec<u8>,
    max_blocks: usize,
}

impl SegmentWriter {
    pub(super) fn new(segment: Arc<Segment>, file: Box<dyn VfsFile>) -> Self {
        Self {
            segment,
            file,
            index: Vec::new(),
            end: DATA_START,
            buf: Vec::new(),
            max_blocks: MAX_BLOCKS,
        }
    }

    #[cfg(test)]
    pub(super) fn limit_blocks(&mut self, max_blocks: usize) {
        self.max_blocks = max_blocks;
    }

    /// The id of the segment.
    pub fn file_id(&self) -> FileId {
        self.segment.id()
    }

    /// The file offset where the blocks appended so far end, which is where
    /// the next one starts.
    pub fn end(&self) -> u64 {
        self.end
    }

    /// How many blocks have been appended.
    pub fn block_count(&self) -> usize {
        self.index.len()
    }

    /// Appends `payload` as one block of `page` and returns where it is.
    ///
    /// With [`Compression::Lz4`] the block is compressed if that makes it
    /// smaller. The block is not durable until [`Self::sync`],
    /// [`Self::seal`] or [`SegmentStore::sync`] returns.
    ///
    /// Fails with [`SegmentStoreError::NotNamed`] until a manifest names the
    /// segment, and with [`SegmentStoreError::SegmentFull`] once the segment
    /// holds [`MAX_BLOCKS`] blocks. If the file layer fails the append,
    /// nothing was appended and the writer can go on.
    ///
    /// [`SegmentStore::sync`]: super::SegmentStore::sync
    pub fn append(
        &mut self,
        page: PageId,
        content: BlockContent,
        compression: Compression,
        payload: &[u8],
    ) -> Result<BlockLocation, SegmentStoreError> {
        self.segment.check_appendable()?;
        let file = self.segment.id();
        if self.index.len() >= self.max_blocks {
            return Err(SegmentStoreError::SegmentFull { file });
        }
        self.buf.clear();
        let tag = segment::write_block(content, compression, payload, &mut self.buf)
            .map_err(SegmentStoreError::format(self.segment.path(), self.end))?;
        let offset = self
            .file
            .append(&self.buf)
            .map_err(SegmentStoreError::io("append to", self.segment.path()))?;
        self.index.push(IndexEntry { page, tag });
        self.end = offset + self.buf.len() as u64;
        self.segment.appended(self.end);
        Ok(BlockLocation {
            file,
            offset,
            len: self.buf.len() as u32,
        })
    }

    /// Makes every block appended so far durable, and returns the length of
    /// the segment that is durable.
    ///
    /// If the sync fails, the blocks appended since the last sync that
    /// succeeded cannot be made durable in this segment any more. The
    /// segment takes no more blocks and cannot be sealed
    /// ([`SegmentStoreError::SegmentFailed`]): write those blocks to another
    /// segment and drop this writer.
    pub fn sync(&mut self) -> Result<u64, SegmentStoreError> {
        self.segment.sync()
    }

    /// Seals the segment: appends its footer and makes the whole segment
    /// durable. Returns the sealed length.
    ///
    /// The segment takes no more blocks afterwards. The manifest names it
    /// sealed from its next replacement on.
    ///
    /// If the footer cannot be appended, nothing has changed and the writer
    /// can go on. If the sync fails, the segment has failed as described at
    /// [`Self::sync`].
    pub fn seal(&mut self) -> Result<u64, SegmentStoreError> {
        let Self {
            segment,
            file,
            index,
            end,
            buf,
            ..
        } = self;
        segment.seal_with(|| {
            buf.clear();
            segment::write_footer(index, *end, buf)
                .map_err(SegmentStoreError::format(segment.path(), *end))?;
            let offset = file
                .append(buf)
                .map_err(SegmentStoreError::io("append to", segment.path()))?;
            Ok(offset + buf.len() as u64)
        })
    }
}

impl Drop for SegmentWriter {
    fn drop(&mut self) {
        self.segment.release_writer();
    }
}
