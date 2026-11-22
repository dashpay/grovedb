//! Appending values to one value-log file, and holding the values that no
//! count includes yet.

use std::sync::Arc;

use lignin_format::{
    value_log::{write_value, ValuePointer},
    FileId, PageId,
};
use lignin_io::VfsFile;

use super::{error::ValueStoreError, file::ValueFile};

const KEPT_BUFFER_LEN: usize = 1 << 20;

/// Keeps the files of values that no count includes yet.
///
/// A value is appended ahead of the page step that makes a cell hold its
/// pointer: the step of write-back that places a value a commit carried in
/// its log group, or the step that installs a moved value. Until that change
/// is applied the value is in no file's count, and its file could be taken
/// for empty. The hold a value was appended under keeps its file from being
/// deleted.
///
/// One run of background work (the write-back of a page, a cleaning pass)
/// has one hold and passes it to every [`ValueWriter::append`] it makes.
/// Drop the hold once the change that counts the values has been applied
/// ([`ValueStore::apply`](super::ValueStore::apply)), or once the values are
/// given up, as when the step is refused: they are then dead space, which
/// goes with their file.
#[derive(Debug, Default)]
pub struct ValueHold {
    files: Vec<Arc<ValueFile>>,
}

impl ValueHold {
    /// A hold on no file.
    pub fn new() -> Self {
        Self::default()
    }

    /// The files the hold keeps.
    pub fn files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.files.iter().map(|file| file.id())
    }

    pub(super) fn add(&mut self, file: &Arc<ValueFile>) {
        if !self.files.iter().any(|held| Arc::ptr_eq(held, file)) {
            file.hold();
            self.files.push(file.clone());
        }
    }
}

impl Drop for ValueHold {
    fn drop(&mut self) {
        for file in &self.files {
            file.release_hold();
        }
    }
}

/// The writer of one value-log file, from
/// [`ValueStore::create`](super::ValueStore::create).
///
/// It is the only appender of its file. Values it has appended can be read
/// through the store at once, from any thread.
///
/// When its file [is full](Self::is_full), [finish](Self::finish) the writer
/// and create another. Dropping a writer leaves its file taking no more
/// values. What was appended stays readable, and
/// [`ValueStore::sync`](super::ValueStore::sync) still makes it durable, after
/// which the file is closed. If a sync of the file has failed, dropping the
/// writer closes the file at the last length that was synced, and the values
/// after it can no longer be read.
#[derive(Debug)]
pub struct ValueWriter {
    file: Arc<ValueFile>,
    appender: Arc<dyn VfsFile>,
    end: u64,
    buf: Vec<u8>,
    target_len: u64,
    session: u64,
}

impl ValueWriter {
    pub(super) fn new(
        file: Arc<ValueFile>,
        appender: Arc<dyn VfsFile>,
        target_len: u64,
        session: u64,
    ) -> Self {
        let end = file.end();
        Self {
            file,
            appender,
            end,
            buf: Vec::new(),
            target_len,
            session,
        }
    }

    pub(super) fn session(&self) -> u64 {
        self.session
    }

    /// The id of the file.
    pub fn file_id(&self) -> FileId {
        self.file.id()
    }

    /// The file offset where the values appended so far end, which is where
    /// the next one starts.
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Whether the file has reached the length at which the store starts
    /// another one. A value is never split over two files, so a file can end
    /// up longer than that.
    pub fn is_full(&self) -> bool {
        self.end >= self.target_len
    }

    /// Appends `value` for a cell of the page `owner`, under `hold`, and
    /// returns the pointer the cell stores.
    ///
    /// The value's records go into the file back to back, in one append. The
    /// value is not durable until [`Self::sync`], [`Self::finish`] or
    /// [`ValueStore::sync`] returns, and no count includes it until the
    /// change that acquires its pointer is applied.
    ///
    /// Fails with [`ValueStoreError::NotNamed`] until a manifest names the
    /// file. An empty value has no record and cannot be appended. If the file
    /// layer fails the append, the file's values end where they did and the
    /// writer can go on. If the file layer could not take the failed append
    /// back, every later append fails as well: drop the writer and go on in
    /// another file.
    ///
    /// [`ValueStore::sync`]: super::ValueStore::sync
    pub fn append(
        &mut self,
        hold: &mut ValueHold,
        owner: PageId,
        value: &[u8],
    ) -> Result<ValuePointer, ValueStoreError> {
        self.file.check_appendable()?;
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        let appended = write_value(owner, value, &mut buf)
            .map_err(self.file.format_error(self.end))
            .and_then(|()| self.append_records(hold, value.len() as u32, &buf));
        if buf.capacity() <= KEPT_BUFFER_LEN {
            self.buf = buf;
        }
        appended
    }

    pub(super) fn append_records(
        &mut self,
        hold: &mut ValueHold,
        value_len: u32,
        records: &[u8],
    ) -> Result<ValuePointer, ValueStoreError> {
        // Admitted and handed to the file layer in one order with the
        // syncs of the file: none comes between the two, so no append
        // is handed to the file layer after a sync of the file has failed.
        let _in_order = self.file.in_order();
        self.file.check_appendable()?;
        #[cfg(test)]
        super::file::hooks::run(super::file::hooks::Point::Admitted);
        hold.add(&self.file);
        let offset = self
            .appender
            .append(records)
            .map_err(ValueStoreError::io("append to", self.file.path()))?;
        self.end = offset + records.len() as u64;
        self.file.appended(self.end);
        ValuePointer::new(self.file.id(), offset, value_len).map_err(self.file.format_error(offset))
    }

    /// Makes every value appended so far durable, and returns the length of
    /// the file that is durable.
    ///
    /// If the sync fails, the values appended since the last sync that
    /// succeeded cannot be made durable in this file any more. The file takes
    /// no more values ([`ValueStoreError::FileFailed`]): append those values
    /// to another file, give their cells the new pointers, and drop this
    /// writer.
    pub fn sync(&mut self) -> Result<u64, ValueStoreError> {
        self.file.sync()
    }

    /// Makes every value appended durable and closes the file. Returns the
    /// file's length.
    ///
    /// The manifest names the file closed from its next replacement on. If
    /// the sync fails, the file has failed as described at [`Self::sync`].
    pub fn finish(mut self) -> Result<u64, ValueStoreError> {
        self.sync()
    }

    /// Ends the use of a file that takes no more values because an append
    /// to it failed, or a step that wrote to it did: makes every value
    /// appended durable **through the handle that appended them**, which
    /// is open until that sync has returned, and closes the file. Returns
    /// the file's length.
    ///
    /// If the sync fails, or one of this file failed before, the file is
    /// closed at the length that was durable before, and is never synced
    /// again: the values behind that length are in no file for this
    /// session, and no manifest covers them
    /// ([`ValueStore::covers`](super::ValueStore::covers)).
    pub fn give_up(self) -> Result<u64, ValueStoreError> {
        self.file.sync()
    }
}

impl Drop for ValueWriter {
    fn drop(&mut self) {
        self.file.release_writer();
    }
}
