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

use std::io::{Result, Write};

use bincode::{
    config,
    config::{BigEndian, Configuration},
};
use grovedb_merk::{Merk, VisualizeableMerk};
use grovedb_path::SubtreePathBuilder;
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;
use grovedb_visualize::{visualize_stdout, Drawer, Visualize};

use crate::{
    element::Element, reference_path::ReferencePathType, util::storage_context_optional_tx,
    GroveDb, TransactionArg,
};

impl Visualize for Element {
    fn visualize<W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        match self {
            Element::Item(value, flags) => {
                drawer.write(b"item: ")?;
                drawer = value.visualize(drawer)?;

                if let Some(f) = flags {
                    if !f.is_empty() {
                        drawer = f.visualize(drawer)?;
                    }
                }
            }
            Element::SumItem(value, flags) => {
                drawer.write(format!("sum_item: {value}").as_bytes())?;

                if let Some(f) = flags {
                    if !f.is_empty() {
                        drawer = f.visualize(drawer)?;
                    }
                }
            }
            Element::Reference(_ref, ..) => {
                drawer.write(b"ref")?;
                // drawer.write(b"ref: [path: ")?;
                // let mut path_iter = path.iter();
                // if let Some(first) = path_iter.next() {
                //     drawer = first.visualize(drawer)?;
                // }
                // for p in path_iter {
                //     drawer.write(b", ")?;
                //     drawer = p.visualize(drawer)?;
                // }
                // drawer.write(b"]")?;
            }
            Element::Tree(root_key, flags) => {
                drawer.write(b"tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;

                if let Some(f) = flags {
                    if !f.is_empty() {
                        drawer = f.visualize(drawer)?;
                    }
                }
            }
            Element::SumTree(root_key, value, flags) => {
                drawer.write(b"sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags {
                    if !f.is_empty() {
                        drawer = f.visualize(drawer)?;
                    }
                }
            }
            Element::BigSumTree(root_key, value, flags) => {
                drawer.write(b"big_sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags {
                    if !f.is_empty() {
                        drawer = f.visualize(drawer)?;
                    }
                }
            }
            Element::CountTree(root_key, value, flags) => {
                drawer.write(b"count_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags {
                    if !f.is_empty() {
                        drawer = f.visualize(drawer)?;
                    }
                }
            }
            Element::CountSumTree(root_key, count_value, sum_value, flags) => {
                drawer.write(b"count_sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!("count: {count_value}, sum {sum_value}").as_bytes())?;

                if let Some(f) = flags {
                    if !f.is_empty() {
                        drawer = f.visualize(drawer)?;
                    }
                }
            }
        }
        Ok(drawer)
    }
}

impl Visualize for ReferencePathType {
    fn visualize<W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        match self {
            ReferencePathType::AbsolutePathReference(path) => {
                drawer.write(b"absolute path reference: ")?;
                drawer.write(
                    path.iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                        .join("/")
                        .as_bytes(),
                )?;
            }
            ReferencePathType::UpstreamRootHeightReference(height, end_path) => {
                drawer.write(b"upstream root height reference: ")?;
                drawer.write(format!("[height: {height}").as_bytes())?;
                drawer.write(
                    end_path
                        .iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                        .join("/")
                        .as_bytes(),
                )?;
            }
            ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(
                height,
                end_path,
            ) => {
                drawer.write(b"upstream root height with parent path addition reference: ")?;
                drawer.write(format!("[height: {height}").as_bytes())?;
                drawer.write(
                    end_path
                        .iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                        .join("/")
                        .as_bytes(),
                )?;
            }
            ReferencePathType::UpstreamFromElementHeightReference(up, end_path) => {
                drawer.write(b"upstream from element reference: ")?;
                drawer.write(format!("[up: {up}").as_bytes())?;
                drawer.write(
                    end_path
                        .iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                        .join("/")
                        .as_bytes(),
                )?;
            }
            ReferencePathType::CousinReference(key) => {
                drawer.write(b"cousin reference: ")?;
                drawer = key.visualize(drawer)?;
            }
            ReferencePathType::RemovedCousinReference(middle_path) => {
                drawer.write(b"removed cousin reference: ")?;
                drawer.write(
                    middle_path
                        .iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                        .join("/")
                        .as_bytes(),
                )?;
            }
            ReferencePathType::SiblingReference(key) => {
                drawer.write(b"sibling reference: ")?;
                drawer = key.visualize(drawer)?;
            }
        }
        Ok(drawer)
    }
}

impl GroveDb {
    fn draw_subtree<W: Write, B: AsRef<[u8]>>(
        &self,
        mut drawer: Drawer<W>,
        path: SubtreePathBuilder<'_, B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> Result<Drawer<W>> {
        drawer.down();

        storage_context_optional_tx!(self.db, (&path).into(), None, transaction, storage, {
            let mut iter = Element::iterator(storage.unwrap().raw_iter()).unwrap();
            while let Some((key, element)) = iter
                .next_element(grove_version)
                .unwrap()
                .expect("cannot get next element")
            {
                drawer.write(b"\n[key: ")?;
                drawer = key.visualize(drawer)?;
                drawer.write(b" ")?;
                match element {
                    Element::Tree(..) => {
                        drawer.write(b"Merk root is: ")?;
                        drawer = element.visualize(drawer)?;
                        drawer.down();
                        drawer = self.draw_subtree(
                            drawer,
                            path.derive_owned_with_child(key),
                            transaction,
                            grove_version,
                        )?;
                        drawer.up();
                    }
                    other => {
                        drawer = other.visualize(drawer)?;
                    }
                }
            }
        });

        drawer.up();
        Ok(drawer)
    }

    fn draw_root_tree<W: Write>(
        &self,
        mut drawer: Drawer<W>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> Result<Drawer<W>> {
        drawer.down();

        drawer = self.draw_subtree(
            drawer,
            SubtreePathBuilder::new(),
            transaction,
            grove_version,
        )?;

        drawer.up();
        Ok(drawer)
    }

    fn visualize_start<W: Write>(
        &self,
        mut drawer: Drawer<W>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> Result<Drawer<W>> {
        drawer.write(b"root")?;
        drawer = self.draw_root_tree(drawer, transaction, grove_version)?;
        drawer.flush()?;
        Ok(drawer)
    }
}

impl Visualize for GroveDb {
    fn visualize<W: Write>(&self, drawer: Drawer<W>) -> Result<Drawer<W>> {
        self.visualize_start(drawer, None, GroveVersion::latest())
    }
}

#[allow(dead_code)]
pub fn visualize_merk_stdout<'db, S: StorageContext<'db>>(merk: &Merk<S>) {
    visualize_stdout(&VisualizeableMerk::new(merk, |bytes: &[u8]| {
        let config = config::standard().with_big_endian().with_no_limit();
        bincode::decode_from_slice::<Element, Configuration<BigEndian>>(bytes, config)
            .expect("unable to deserialize Element")
            .0
    }));
}

#[cfg(test)]
mod tests {
    use grovedb_visualize::to_hex;

    use super::*;
    use crate::reference_path::ReferencePathType;

    #[test]
    fn test_element_item_str() {
        let v = b"ayylmao".to_vec();
        let e = Element::new_item(v.clone());
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
        let e = Element::new_item(v.clone());
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
    #[ignore]
    fn test_visualize_reference() {
        let p1 = b"ayy".to_vec();
        let p2 = b"lmao".to_vec();
        let e = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            p1.clone(),
            p2.clone(),
        ]));
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
