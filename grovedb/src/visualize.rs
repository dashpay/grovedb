#[cfg(feature = "full")]
use std::io::{Result, Write};

#[cfg(feature = "full")]
use bincode::Options;
#[cfg(feature = "full")]
use merk::{Merk, VisualizeableMerk};
#[cfg(feature = "full")]
use storage::StorageContext;
#[cfg(feature = "full")]
use visualize::{visualize_stdout, Drawer, Visualize};

#[cfg(feature = "full")]
use crate::{
    reference_path::ReferencePathType, subtree::Element, util::storage_context_optional_tx,
    GroveDb, TransactionArg,
};

#[cfg(feature = "full")]
impl Visualize for Element {
    fn visualize<W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        match self {
            Element::Item(value, _) => {
                drawer.write(b"item: ")?;
                drawer = value.visualize(drawer)?;
            }
            Element::SumItem(value, _) => {
                drawer.write(format!("sum_item: {}", value).as_bytes())?;
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
            Element::Tree(root_key, _) => {
                drawer.write(b"tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
            }
            Element::SumTree(root_key, ..) => {
                drawer.write(b"sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
            }
        }
        Ok(drawer)
    }
}

#[cfg(feature = "full")]
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

#[cfg(feature = "full")]
impl GroveDb {
    fn draw_subtree<W: Write>(
        &self,
        mut drawer: Drawer<W>,
        path: Vec<Vec<u8>>,
        transaction: TransactionArg,
    ) -> Result<Drawer<W>> {
        drawer.down();

        storage_context_optional_tx!(
            self.db,
            path.iter().map(|x| x.as_slice()),
            transaction,
            storage,
            {
                let mut iter = Element::iterator(storage.unwrap().raw_iter()).unwrap();
                while let Some((key, element)) =
                    iter.next().unwrap().expect("cannot get next element")
                {
                    drawer.write(b"\n[key: ")?;
                    drawer = key.visualize(drawer)?;
                    drawer.write(b" ")?;
                    match element {
                        Element::Tree(..) => {
                            drawer.write(b"Merk root is: ")?;
                            drawer = element.visualize(drawer)?;
                            drawer.down();
                            let mut inner_path = path.clone();
                            inner_path.push(key);
                            drawer = self.draw_subtree(drawer, inner_path, transaction)?;
                            drawer.up();
                        }
                        other => {
                            drawer = other.visualize(drawer)?;
                        }
                    }
                }
            }
        );

        drawer.up();
        Ok(drawer)
    }

    fn draw_root_tree<W: Write>(
        &self,
        mut drawer: Drawer<W>,
        transaction: TransactionArg,
    ) -> Result<Drawer<W>> {
        drawer.down();

        drawer = self.draw_subtree(drawer, vec![], transaction)?;

        drawer.up();
        Ok(drawer)
    }

    fn visualize_start<W: Write>(
        &self,
        mut drawer: Drawer<W>,
        transaction: TransactionArg,
    ) -> Result<Drawer<W>> {
        drawer.write(b"root")?;
        drawer = self.draw_root_tree(drawer, transaction)?;
        drawer.flush()?;
        Ok(drawer)
    }
}

#[cfg(feature = "full")]
impl Visualize for GroveDb {
    fn visualize<W: Write>(&self, drawer: Drawer<W>) -> Result<Drawer<W>> {
        self.visualize_start(drawer, None)
    }
}

#[cfg(feature = "full")]
#[allow(dead_code)]
pub fn visualize_merk_stdout<'db, S: StorageContext<'db>>(merk: &Merk<S>) {
    visualize_stdout(&VisualizeableMerk::new(merk, |bytes: &[u8]| {
        bincode::DefaultOptions::default()
            .with_varint_encoding()
            .reject_trailing_bytes()
            .deserialize::<Element>(bytes)
            .expect("unable to deserialize Element")
    }));
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use visualize::to_hex;

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
