use std::io::{Result, Write};

use crate::subtree::Element;

static HEX_LEN: usize = 8;
static STR_LEN: usize = 32;

/// Pretty visualization of GroveDB components.
pub(crate) trait Visualize {
    fn visualize<'a, W: Write>(&self, drawer: Drawer<'a, W>) -> Result<Drawer<'a, W>>;
}

/// A `io::Write` proxy to prepend padding and symbols to draw trees
pub(crate) struct Drawer<'a, W: Write> {
    level: usize,
    write: &'a mut W,
}

impl<'a, W: Write> Drawer<'a, W> {
    pub(crate) fn new(write: &'a mut W) -> Self {
        Drawer { level: 0, write }
    }

    pub(crate) fn down(&mut self) {
        self.level += 1;
    }

    pub(crate) fn up(&mut self) {
        self.level -= 1;
    }

    pub(crate) fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.write.write(buf)
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut result = hex::encode(bytes);
    result.truncate(HEX_LEN);
    result
}

impl<B: AsRef<[u8]>> Visualize for B {
    fn visualize<'a, W: Write>(&self, mut drawer: Drawer<'a, W>) -> Result<Drawer<'a, W>> {
        let value = self.as_ref();
        let hex_repr = to_hex(value);
        let str_repr = String::from_utf8(value.to_vec());
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

impl Visualize for Element {
    fn visualize<'a, W: Write>(&self, mut drawer: Drawer<'a, W>) -> Result<Drawer<'a, W>> {
        match self {
            Element::Item(value) => {
                drawer.write(b"item: ")?;
                drawer = value.visualize(drawer)?;
            }
            Element::Reference(path) => {
                drawer.write(b"ref: [path: ")?;
                let mut path_iter = path.iter();
                if let Some(first) = path_iter.next() {
                    drawer = first.visualize(drawer)?;
                }
                for p in path_iter {
                    drawer.write(b", ")?;
                    drawer = p.visualize(drawer)?;
                }
                drawer.write(b"]")?;
            }
            Element::Tree(hash) => {
                drawer.write(b"tree: ")?;
                drawer = hash.visualize(drawer)?;
            }
        }
        Ok(drawer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_element_item_str() {
        let v = b"ayylmao".to_vec();
        let e = Element::Item(v.clone());
        let element_hex = to_hex(&v);
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        assert_eq!(
            format!(
                "item: [hex: {element_hex}, str: {}]",
                String::from_utf8_lossy(&v)
            ),
            String::from_utf8_lossy(result.as_ref())
        );
    }

    #[test]
    fn test_element_item_no_tr() {
        let v = vec![1, 3, 3, 7, 255];
        let e = Element::Item(v.clone());
        let element_hex = to_hex(&v);
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        assert_eq!(
            format!("item: [hex: {element_hex}]"),
            String::from_utf8_lossy(result.as_ref())
        );
    }

    #[test]
    fn test_visualize_reference() {
        let p1 = b"ayy".to_vec();
        let p2 = b"lmao".to_vec();
        let e = Element::Reference(vec![p1.clone(), p2.clone()]);
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        assert_eq!(
            format!(
                "ref: [path: [hex: {p1_hex}, str: {p1}], [hex: {p2_hex}, str: {p2}]]",
                p1 = String::from_utf8_lossy(&p1),
                p2 = String::from_utf8_lossy(&p2),
                p1_hex = to_hex(&p1),
                p2_hex = to_hex(&p2),
            ),
            String::from_utf8_lossy(result.as_ref())
        );
    }
}
