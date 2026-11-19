//! What the fuzz targets of `lignin-format` share.
//!
//! Every target reads its input as one control byte and then the bytes under
//! test. Bit 0 of the control byte keeps the checksums as the input has them.
//! When it is clear, the target writes the right checksum into every frame it
//! can find first, so that the fuzzer gets past them to the decoders behind
//! them. The other bits are the target's own.

pub mod tree;

use std::fmt::Write as _;

use lignin_format::{Checksum, FileHeader, FormatError, PageSize};

/// The control byte and the bytes under test.
pub struct Input<'a> {
    pub control: u8,
    pub bytes: &'a [u8],
}

impl<'a> Input<'a> {
    pub fn split(data: &'a [u8]) -> Option<Self> {
        let (&control, bytes) = data.split_first()?;
        Some(Self { control, bytes })
    }

    pub fn keeps_checksums(&self) -> bool {
        self.control & 1 != 0
    }

    pub fn bit(&self, bit: u32) -> bool {
        self.control >> bit & 1 != 0
    }

    pub fn page_size(&self, shift: u32) -> PageSize {
        page_size(self.control >> shift)
    }
}

/// One of the four page sizes, chosen by the two low bits of `choice`.
pub fn page_size(choice: u8) -> PageSize {
    PageSize::new(PageSize::MIN.get() << (choice & 3)).expect("a page size")
}

/// Reads the length field of a frame from its first four bytes.
pub type FrameLen = fn(u32) -> usize;

/// The default frame: the first four bytes are the payload length.
pub fn default_frame_len(word: u32) -> usize {
    word as usize
}

/// A segment's frame: bits 0 to 26 of the tag are the body length.
pub fn segment_frame_len(word: u32) -> usize {
    (word & ((1 << 27) - 1)) as usize
}

/// Writes the checksum of the file header at the start of `bytes`.
pub fn seal_file_header(bytes: &mut [u8]) {
    if let Some(header) = bytes.get_mut(..FileHeader::ENCODED_LEN) {
        let (covered, checksum) = header.split_at_mut(16);
        checksum.copy_from_slice(&Checksum::of(&[covered]).encode());
    }
}

/// Writes the checksum of every whole frame of the chain that starts at
/// `start`: a frame is four bytes that give its length, the checksum of
/// those four bytes and the body, and the body. Stops at the first frame
/// that does not fit.
pub fn seal_frames(bytes: &mut [u8], start: usize, len: FrameLen) {
    let mut at = start;
    while let Some(word) = bytes.get(at..at + 4) {
        let body_len = len(u32::from_le_bytes(word.try_into().expect("four bytes")));
        let Some(end) = (at + 8).checked_add(body_len).filter(|&end| end <= bytes.len()) else {
            return;
        };
        let checksum = Checksum::of(&[&bytes[at..at + 4], &bytes[at + 8..end]]);
        bytes[at + 4..at + 8].copy_from_slice(&checksum.encode());
        at = end;
    }
}

/// The bytes of a file whose records are framed from offset 20 on: the input
/// as it is, or with every checksum made right.
pub fn sealed_file(input: &Input<'_>, len: FrameLen) -> Vec<u8> {
    let mut bytes = input.bytes.to_vec();
    if !input.keeps_checksums() {
        seal_file_header(&mut bytes);
        seal_frames(&mut bytes, FileHeader::ENCODED_LEN, len);
    }
    bytes
}

/// The bytes of a run of records that starts at offset 0: the input as it
/// is, or with every checksum made right.
pub fn sealed_records(input: &Input<'_>, len: FrameLen) -> Vec<u8> {
    let mut bytes = input.bytes.to_vec();
    if !input.keeps_checksums() {
        seal_frames(&mut bytes, 0, len);
    }
    bytes
}

/// Formats an error, so that every `Display` arm a decoder can reach runs.
pub fn show(error: &FormatError) {
    let mut text = String::new();
    write!(text, "{error} {error:?}").expect("formatting into a string");
    assert!(!text.is_empty());
}

/// Formats the error of a result, if it has one, and gives the value back.
pub fn ok<T>(result: Result<T, FormatError>) -> Option<T> {
    result.inspect_err(show).ok()
}

/// Reads a little-endian `u16` at the front of `bytes` and moves past it.
pub fn take_u16(bytes: &mut &[u8]) -> Option<u16> {
    let (head, rest) = bytes.split_first_chunk::<2>()?;
    *bytes = rest;
    Some(u16::from_le_bytes(*head))
}

/// Takes up to `len` bytes off the front of `bytes`.
pub fn take_up_to<'a>(bytes: &mut &'a [u8], len: usize) -> &'a [u8] {
    let (head, rest) = bytes.split_at(len.min(bytes.len()));
    *bytes = rest;
    head
}
