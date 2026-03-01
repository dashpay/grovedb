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
    use super::*;

    // ── to_hex ──────────────────────────────────────────────────────────

    #[test]
    fn to_hex_empty_bytes() {
        assert_eq!(to_hex(&[]), "");
    }

    #[test]
    fn to_hex_short_bytes_no_truncation() {
        // 4 bytes = 8 hex chars = HEX_LEN, remaining = 0 < 8 → no truncation
        assert_eq!(to_hex(&[0xab, 0xcd, 0xef, 0x12]), "abcdef12");
    }

    #[test]
    fn to_hex_single_byte() {
        assert_eq!(to_hex(&[0xff]), "ff");
    }

    #[test]
    fn to_hex_medium_bytes_no_truncation() {
        // 7 bytes = 14 hex chars, remaining = 14 - 8 = 6 < 8 → full hex
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        assert_eq!(to_hex(&bytes), "01020304050607");
    }

    #[test]
    fn to_hex_long_bytes_truncated() {
        // 12 bytes = 24 hex chars, remaining = 24 - 8 = 16 >= 8 → truncated
        let bytes = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let result = to_hex(&bytes);
        assert!(result.starts_with("01020304.."));
        assert!(result.ends_with("0c"));
    }

    #[test]
    fn to_hex_boundary_8_bytes_is_truncated() {
        // 8 bytes = 16 hex chars, remaining = 16 - 8 = 8 >= 8 → truncated
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let result = to_hex(&bytes);
        assert!(
            result.contains(".."),
            "8 bytes should be truncated: {result}"
        );
        assert!(result.starts_with("01020304.."));
    }

    #[test]
    fn to_hex_7_bytes_not_truncated() {
        // 7 bytes = 14 hex chars, remaining = 14 - 8 = 6 < 8 → no truncation
        let bytes = [0xaa; 7];
        let result = to_hex(&bytes);
        assert!(!result.contains(".."), "7 bytes should not be truncated");
        assert_eq!(result, "aaaaaaaaaaaaaa");
    }

    // ── Drawer ──────────────────────────────────────────────────────────

    #[test]
    fn drawer_write_no_indent() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.write(b"hello").unwrap();
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn drawer_write_with_indent_level_1() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.down();
        drawer.write(b"line1\nline2").unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("line1\n   line2"));
    }

    #[test]
    fn drawer_write_with_indent_level_2() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.down();
        drawer.down();
        drawer.write(b"a\nb").unwrap();
        let output = String::from_utf8(buf).unwrap();
        // level 2 → INDENT_SPACES * 2 - 1 = 7 spaces
        assert!(output.contains("a\n       b"));
    }

    #[test]
    fn drawer_down_up_returns_to_zero() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.down();
        drawer.down();
        drawer.up();
        drawer.up();
        drawer.write(b"line1\nline2").unwrap();
        let output = String::from_utf8(buf).unwrap();
        // At level 0, newlines are replaced with empty separator
        assert_eq!(output, "line1line2");
    }

    #[test]
    fn drawer_flush() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.write(b"data").unwrap();
        drawer.flush().unwrap();
        assert_eq!(buf, b"data\n");
    }

    #[test]
    fn drawer_write_no_newlines_at_level() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.down();
        drawer.write(b"single_line").unwrap();
        assert_eq!(buf, b"single_line");
    }

    #[test]
    fn drawer_write_multiple_newlines() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.down();
        drawer.write(b"a\nb\nc").unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "a\n   b\n   c");
    }

    // ── Visualize for [u8] ──────────────────────────────────────────────

    #[test]
    fn visualize_bytes_valid_short_utf8() {
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, b"hello".as_slice());
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("hex:"));
        assert!(output.contains("str: hello"));
    }

    #[test]
    fn visualize_bytes_valid_long_utf8_truncated() {
        // Longer than STR_LEN (32) → truncated
        let data = b"abcdefghijklmnopqrstuvwxyz1234567890ABCDEF";
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, data.as_slice());
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("str: "));
        assert!(
            !output.contains("ABCDEF"),
            "long string should be truncated"
        );
    }

    #[test]
    fn visualize_bytes_invalid_utf8() {
        let data: &[u8] = &[0xff, 0xfe, 0xfd];
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, data);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("hex:"));
        assert!(
            !output.contains("str:"),
            "invalid UTF-8 should have no str part"
        );
    }

    #[test]
    fn visualize_bytes_empty() {
        let data: &[u8] = &[];
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, data);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("[hex: "));
        assert!(output.contains(", str: "));
    }

    #[test]
    fn visualize_bytes_exactly_str_len() {
        let data = b"abcdefghijklmnopqrstuvwxyz123456"; // 32 bytes
        assert_eq!(data.len(), 32);
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, data.as_slice());
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("str: abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn visualize_bytes_str_len_plus_one() {
        let data = b"abcdefghijklmnopqrstuvwxyz1234567"; // 33 bytes
        assert_eq!(data.len(), 33);
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, data.as_slice());
        let output = String::from_utf8(buf).unwrap();
        // [..=STR_LEN] is inclusive, so 33 chars from a 33-char string = full
        assert!(output.contains("str: abcdefghijklmnopqrstuvwxyz1234567"));
    }

    // ── Visualize for Vec<u8> ───────────────────────────────────────────

    #[test]
    fn visualize_vec_u8_delegates_to_slice() {
        let data = vec![0x41, 0x42, 0x43]; // "ABC"
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, &data);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("hex:"));
        assert!(output.contains("str: ABC"));
    }

    // ── Visualize for &T ────────────────────────────────────────────────

    #[test]
    fn visualize_ref_delegates() {
        let data: &[u8] = &[0x41, 0x42];
        let reference: &&[u8] = &data;
        let mut buf = Vec::new();
        let drawer = Drawer::new(&mut buf);
        reference.visualize(drawer).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("hex:"));
        assert!(output.contains("str: AB"));
    }

    // ── Visualize for Option<T> ─────────────────────────────────────────

    #[test]
    fn visualize_option_some() {
        let data: Option<Vec<u8>> = Some(vec![0x41]); // "A"
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, &data);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("hex:"));
        assert!(output.contains("str: A"));
    }

    #[test]
    fn visualize_option_none() {
        let data: Option<Vec<u8>> = None;
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, &data);
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "None");
    }

    // ── DebugBytes ──────────────────────────────────────────────────────

    #[test]
    fn debug_bytes_formatting() {
        let db = DebugBytes(vec![0x41, 0x42, 0x43]);
        let output = format!("{:?}", db);
        assert!(output.contains("hex:"));
        assert!(output.contains("str: ABC"));
    }

    #[test]
    fn debug_bytes_non_utf8() {
        let db = DebugBytes(vec![0xff, 0xfe]);
        let output = format!("{:?}", db);
        assert!(output.contains("hex:"));
        assert!(!output.contains("str:"));
    }

    #[test]
    fn debug_bytes_empty() {
        let db = DebugBytes(vec![]);
        let output = format!("{:?}", db);
        assert!(output.contains("hex:"));
    }

    // ── DebugByteVectors ────────────────────────────────────────────────

    #[test]
    fn debug_byte_vectors_multiple_elements() {
        let dbv = DebugByteVectors(vec![vec![0x41], vec![0x42, 0x43]]);
        let output = format!("{:?}", dbv);
        assert!(output.contains("[ "));
        assert!(output.contains(" ]"));
        assert!(output.contains("hex:"));
    }

    #[test]
    fn debug_byte_vectors_empty() {
        let dbv = DebugByteVectors(vec![]);
        let output = format!("{:?}", dbv);
        assert!(output.contains("[ "));
        assert!(output.contains(" ]"));
    }

    #[test]
    fn debug_byte_vectors_single_element() {
        let dbv = DebugByteVectors(vec![vec![0x01, 0x02]]);
        let output = format!("{:?}", dbv);
        assert!(output.contains("hex:"));
    }

    // ── Derived trait impls ─────────────────────────────────────────────

    #[test]
    fn debug_bytes_ord_and_eq() {
        let a = DebugBytes(vec![1, 2, 3]);
        let b = DebugBytes(vec![1, 2, 3]);
        let c = DebugBytes(vec![1, 2, 4]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a < c);
    }

    #[test]
    fn debug_byte_vectors_ord_and_eq() {
        let a = DebugByteVectors(vec![vec![1], vec![2]]);
        let b = DebugByteVectors(vec![vec![1], vec![2]]);
        let c = DebugByteVectors(vec![vec![1], vec![3]]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a < c);
    }

    #[test]
    fn debug_bytes_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(DebugBytes(vec![1, 2]));
        set.insert(DebugBytes(vec![1, 2]));
        assert_eq!(set.len(), 1);
        set.insert(DebugBytes(vec![3, 4]));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn debug_byte_vectors_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(DebugByteVectors(vec![vec![1]]));
        set.insert(DebugByteVectors(vec![vec![1]]));
        assert_eq!(set.len(), 1);
    }

    // ── visualize_stderr / visualize_stdout ──────────────────────────────

    #[test]
    fn visualize_stderr_does_not_panic() {
        visualize_stderr(&[0x41u8, 0x42][..]);
    }

    #[test]
    fn visualize_stdout_does_not_panic() {
        visualize_stdout(&[0x41u8, 0x42][..]);
    }

    // ── visualize_to_vec ────────────────────────────────────────────────

    #[test]
    fn visualize_to_vec_produces_expected_output() {
        let mut buf = Vec::new();
        visualize_to_vec(&mut buf, &[0x48u8, 0x69][..]); // "Hi"
        let output = String::from_utf8(buf).unwrap();
        assert!(output.starts_with("[hex: "));
        assert!(output.contains("str: Hi"));
        assert!(output.ends_with(']'));
    }

    // ── Integration: Drawer + Visualize combined ────────────────────────

    #[test]
    fn visualize_with_nested_drawer_levels() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.write(b"root:").unwrap();
        drawer.down();
        drawer.write(b"\nchild1").unwrap();
        drawer.down();
        drawer.write(b"\ngrandchild").unwrap();
        drawer.up();
        drawer.write(b"\nchild2").unwrap();
        drawer.up();
        drawer.flush().unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("root:"));
        assert!(output.contains("child1"));
        assert!(output.contains("grandchild"));
        assert!(output.contains("child2"));
    }

    #[test]
    fn visualize_bytes_with_drawer_indent() {
        let mut buf = Vec::new();
        let mut drawer = Drawer::new(&mut buf);
        drawer.down();
        let _drawer = [0x41u8, 0x42].visualize(drawer).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("hex:"));
        assert!(output.contains("str: AB"));
    }
}
