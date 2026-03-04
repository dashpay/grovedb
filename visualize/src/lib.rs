// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Visualize

#![deny(missing_docs)]

use core::fmt;
use std::io::{Result, Write};

use itertools::Itertools;

static HEX_LEN: usize = 8;
static STR_LEN: usize = 32;
static INDENT_SPACES: usize = 4;

/// Pretty visualization of GroveDB components.
pub trait Visualize {
    /// Visualize
    fn visualize<W: Write>(&self, drawer: Drawer<W>) -> Result<Drawer<W>>;
}

/// Wrapper struct with a `Debug` implementation to represent bytes vector in
/// human-friendly way.
#[derive(PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct DebugBytes(pub Vec<u8>);

impl fmt::Debug for DebugBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        visualize_to_vec(&mut v, self.0.as_slice());

        f.write_str(&String::from_utf8_lossy(&v))
    }
}

/// Wrapper struct with a `Debug` implementation to represent vector of bytes
/// vectors in human-friendly way.
#[derive(PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct DebugByteVectors(pub Vec<Vec<u8>>);

impl fmt::Debug for DebugByteVectors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        let mut drawer = Drawer::new(&mut v);

        drawer.write(b"[ ").expect("write to a vector");

        for v in self.0.iter() {
            drawer = v.visualize(drawer).expect("write to a vector");
            drawer.write(b", ").expect("write to a vector");
        }

        drawer.write(b" ]").expect("write to a vector");

        f.write_str(&String::from_utf8_lossy(&v))
    }
}

/// A `io::Write` proxy to prepend padding and symbols to draw trees
pub struct Drawer<W: Write> {
    level: usize,
    write: W,
}

impl<W: Write> Drawer<W> {
    /// New
    pub fn new(write: W) -> Self {
        Drawer { level: 0, write }
    }

    /// Down
    pub fn down(&mut self) {
        self.level += 1;
    }

    /// Up
    pub fn up(&mut self) {
        self.level -= 1;
    }

    /// Write
    pub fn write(&mut self, buf: &[u8]) -> Result<()> {
        let lines_iter = buf.split(|c| *c == b'\n');
        let sep = if self.level > 0 {
            let mut result = " ".repeat(INDENT_SPACES * self.level - 1);
            result.insert(0, '\n');
            result
        } else {
            String::new()
        };
        let interspersed_lines_iter = Itertools::intersperse(lines_iter, sep.as_bytes());
        for line in interspersed_lines_iter {
            self.write.write_all(line)?;
        }
        Ok(())
    }

    /// Flush
    pub fn flush(&mut self) -> Result<()> {
        self.write.write_all(b"\n")?;
        self.write.flush()?;
        Ok(())
    }
}

/// To hex
pub fn to_hex(bytes: &[u8]) -> String {
    let encoded = hex::encode(bytes);
    let remaining = encoded.len().saturating_sub(HEX_LEN);
    if remaining >= 8 {
        format!("{}..{}", &encoded[0..HEX_LEN], &encoded[remaining..])
    } else {
        encoded
    }
}

impl Visualize for [u8] {
    fn visualize<'a, W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        let hex_repr = to_hex(self);
        let str_repr = String::from_utf8(self.to_vec());
        drawer.write(format!("[hex: {hex_repr}").as_bytes())?;
        if let Ok(str_repr) = str_repr {
            let str_part = if str_repr.len() > STR_LEN {
                &str_repr[..=STR_LEN]
            } else {
                &str_repr
            };
            drawer.write(format!(", str: {str_part}").as_bytes())?;
        }
        drawer.write(b"]")?;
        Ok(drawer)
    }
}

impl Visualize for Vec<u8> {
    fn visualize<W: Write>(&self, drawer: Drawer<W>) -> Result<Drawer<W>> {
        self.as_slice().visualize(drawer)
    }
}

impl<T: Visualize + ?Sized> Visualize for &T {
    fn visualize<'a, W: Write>(&self, drawer: Drawer<W>) -> Result<Drawer<W>> {
        (*self).visualize(drawer)
    }
}

impl<T: Visualize> Visualize for Option<T> {
    fn visualize<'a, W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        Ok(if let Some(v) = self {
            v.visualize(drawer)?
        } else {
            drawer.write(b"None")?;
            drawer
        })
    }
}

/// `visualize` shortcut to write straight into stderr offhand
pub fn visualize_stderr<T: Visualize + ?Sized>(value: &T) {
    let mut out = std::io::stderr();
    let drawer = Drawer::new(&mut out);
    value
        .visualize(drawer)
        .expect("IO error when trying to `visualize`");
}

/// `visualize` shortcut to write straight into stdout offhand
pub fn visualize_stdout<T: Visualize + ?Sized>(value: &T) {
    let mut out = std::io::stdout();
    let drawer = Drawer::new(&mut out);
    value
        .visualize(drawer)
        .expect("IO error when trying to `visualize`");
}

/// `visualize` shortcut to write into provided buffer, should be a `Vec` not a
/// slice because slices won't grow if needed.
pub fn visualize_to_vec<T: Visualize + ?Sized>(v: &mut Vec<u8>, value: &T) {
    let drawer = Drawer::new(v);
    value
        .visualize(drawer)
        .expect("error while writing into slice");
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind, Write};

    use super::{
        to_hex, visualize_stderr, visualize_stdout, visualize_to_vec, DebugByteVectors, DebugBytes,
        Drawer, Visualize,
    };

    fn visualized<T: Visualize + ?Sized>(value: &T) -> String {
        let mut out = Vec::new();
        visualize_to_vec(&mut out, value);
        String::from_utf8(out).expect("visualization is utf8")
    }

    #[derive(Default)]
    struct RecordingWriter {
        buf: Vec<u8>,
        flushes: usize,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    struct FailWriteWriter;

    impl Write for FailWriteWriter {
        fn write(&mut self, _data: &[u8]) -> std::io::Result<usize> {
            Err(Error::other("write failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailFlushWriter(Vec<u8>);

    impl Write for FailFlushWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(Error::other("flush failure"))
        }
    }

    struct AlwaysErrVisualize;

    impl Visualize for AlwaysErrVisualize {
        fn visualize<W: Write>(&self, _drawer: Drawer<W>) -> std::io::Result<Drawer<W>> {
            Err(Error::other("visualize failure"))
        }
    }

    #[test]
    fn drawer_write_respects_indentation_levels() {
        let mut writer = RecordingWriter::default();
        let mut drawer = Drawer::new(&mut writer);
        drawer.write(b"a\nb").expect("write at root level");
        drawer.down();
        drawer.write(b"\nc\nd").expect("write at level 1");
        drawer.down();
        drawer.write(b"\ne").expect("write at level 2");
        drawer.up();
        drawer.write(b"\nf").expect("write after up");

        let got = String::from_utf8(writer.buf).expect("valid utf8");
        assert_eq!(got, "ab\n   c\n   d\n       e\n   f");
    }

    #[test]
    fn drawer_write_propagates_inner_write_errors() {
        let mut drawer = Drawer::new(FailWriteWriter);
        let err = drawer
            .write(b"data")
            .expect_err("must propagate writer error");
        assert_eq!(err.kind(), ErrorKind::Other);
    }

    #[test]
    fn drawer_flush_writes_trailing_newline_then_flushes() {
        let mut writer = RecordingWriter::default();
        let mut drawer = Drawer::new(&mut writer);
        drawer.write(b"line").expect("write");
        drawer.flush().expect("flush");

        assert_eq!(String::from_utf8(writer.buf).expect("utf8"), "line\n");
        assert_eq!(writer.flushes, 1);
    }

    #[test]
    fn drawer_flush_propagates_flush_errors() {
        let mut drawer = Drawer::new(FailFlushWriter::default());
        let err = drawer.flush().expect_err("must propagate flush error");
        assert_eq!(err.kind(), ErrorKind::Other);
    }

    #[test]
    fn to_hex_returns_full_for_short_values() {
        assert_eq!(to_hex(b""), "");
        assert_eq!(to_hex(b"abc"), "616263");
        assert_eq!(to_hex(&[1, 2, 3, 4, 5, 6, 7]), "01020304050607");
    }

    #[test]
    fn to_hex_shortens_long_values() {
        let bytes: Vec<u8> = (0..8).collect();
        assert_eq!(to_hex(&bytes), "00010203..04050607");
    }

    #[test]
    fn bytes_visualize_with_utf8_includes_string_part() {
        let got = visualized(&b"hello"[..]);
        assert_eq!(got, "[hex: 68656c6c6f, str: hello]");
    }

    #[test]
    fn bytes_visualize_truncates_long_utf8_string() {
        let input = vec![b'x'; 40];
        let got = visualized(input.as_slice());
        assert_eq!(
            got,
            format!(
                "[hex: {}..{}, str: {}]",
                "78787878",
                "78787878",
                "x".repeat(33)
            )
        );
    }

    #[test]
    fn bytes_visualize_non_utf8_omits_string_part() {
        let got = visualized(&[0xff, 0xfe, 0xfd][..]);
        assert_eq!(got, "[hex: fffefd]");
    }

    #[test]
    fn vec_and_reference_visualize_delegate_correctly() {
        let vec_value = vec![1u8, 2, 3];
        assert_eq!(
            visualized(&vec_value),
            "[hex: 010203, str: \u{1}\u{2}\u{3}]"
        );

        let slice: &[u8] = b"ab";
        let reference = &slice;
        assert_eq!(visualized(&reference), "[hex: 6162, str: ab]");
    }

    #[test]
    fn option_visualize_handles_some_and_none() {
        let some = Some(vec![0xabu8, 0xcdu8]);
        let none: Option<Vec<u8>> = None;
        assert_eq!(visualized(&some), "[hex: abcd]");
        assert_eq!(visualized(&none), "None");
    }

    #[test]
    fn debug_bytes_formats_using_visualization() {
        let value = DebugBytes(b"test".to_vec());
        assert_eq!(format!("{value:?}"), "[hex: 74657374, str: test]");
    }

    #[test]
    fn debug_byte_vectors_formats_collection_with_elements() {
        let value = DebugByteVectors(vec![b"a".to_vec(), vec![0xff]]);
        assert_eq!(format!("{value:?}"), "[ [hex: 61, str: a], [hex: ff],  ]");
    }

    #[test]
    fn debug_byte_vectors_formats_empty_collection() {
        let value = DebugByteVectors(Vec::new());
        assert_eq!(format!("{value:?}"), "[  ]");
    }

    #[test]
    fn visualize_stdout_and_stderr_do_not_panic_for_valid_values() {
        visualize_stdout(&b"ok"[..]);
        visualize_stderr(&b"ok"[..]);
    }

    #[test]
    #[should_panic(expected = "error while writing into slice")]
    fn visualize_to_vec_panics_when_visualize_returns_error() {
        let mut out = Vec::new();
        visualize_to_vec(&mut out, &AlwaysErrVisualize);
    }

    #[test]
    #[should_panic(expected = "IO error when trying to `visualize`")]
    fn visualize_stdout_panics_when_visualize_returns_error() {
        visualize_stdout(&AlwaysErrVisualize);
    }

    #[test]
    #[should_panic(expected = "IO error when trying to `visualize`")]
    fn visualize_stderr_panics_when_visualize_returns_error() {
        visualize_stderr(&AlwaysErrVisualize);
    }
}
