use std::io::{Result, Write};

use itertools::Itertools;
use merk::Merk;
use serde::de::IntoDeserializer;
use storage::Storage;

use crate::{subtree::Element, GroveDb};

static HEX_LEN: usize = 8;
static STR_LEN: usize = 32;
static INDENT_SPACES: usize = 4;

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

    pub(crate) fn write(&mut self, buf: &[u8]) -> Result<()> {
        let lines_iter = buf.split(|c| *c == b'\n');
        let sep = if self.level > 0 {
            let mut result = " ".repeat(INDENT_SPACES * self.level - 1);
            result.insert(0, '\n');
            result
        } else {
            String::new()
        };
        let interspersed_lines_iter = lines_iter.intersperse(sep.as_bytes());
        for line in interspersed_lines_iter {
            self.write.write(line)?;
        }
        Ok(())
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut result = hex::encode(bytes);
    result.truncate(HEX_LEN);
    result
}

impl Visualize for [u8] {
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

impl GroveDb {
    fn draw_subtree<'a, W: Write>(
        &self,
        mut drawer: Drawer<'a, W>,
        path: Vec<Vec<u8>>,
    ) -> Result<Drawer<'a, W>> {
        drawer.down();
        let (merk, _) = self
            .get_subtrees()
            .get(path.iter().map(|x| x.as_slice()), None)
            .expect("cannot find Merk");
        let mut iter = Element::iterator(merk.raw_iter(None));
        while let Some((key, element)) = iter.next().expect("cannot get next element") {
            drawer.write(b"\n[key: ")?;
            drawer = key.visualize(drawer)?;
            drawer.write(b" ")?;
            match element {
                Element::Tree(_) => {
                    drawer.write(b"tree:")?;
                    drawer.down();
                    let mut inner_path = path.clone();
                    inner_path.push(key);
                    drawer = self.draw_subtree(drawer, inner_path)?;
                    drawer.up();
                }
                other => {
                    drawer = other.visualize(drawer)?;
                }
            }
        }
        drawer.up();
        Ok(drawer)
    }

    fn draw_root_tree<'a, W: Write>(&self, mut drawer: Drawer<'a, W>) -> Result<Drawer<'a, W>> {
        drawer.down();
        let keys = self.root_leaf_keys.iter().fold(
            vec![Vec::new(); self.root_leaf_keys.len()],
            |mut acc, (key, idx)| {
                acc[*idx] = key.clone();
                acc
            },
        );
        for k in keys {
            drawer.write(b"\n")?;
            drawer.write(&k)?;
            drawer = self.draw_subtree(drawer, vec![k])?
        }
        drawer.up();
        Ok(drawer)
    }
}

impl Visualize for GroveDb {
    fn visualize<'a, W: Write>(&self, mut drawer: Drawer<'a, W>) -> Result<Drawer<'a, W>> {
        drawer.write(b"root")?;
        drawer = self.draw_root_tree(drawer)?;
        drawer.write(b"\n")?;
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
