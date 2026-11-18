//! Visualize

use std::io::{Result, Write};

use bincode::{
    config,
    config::{BigEndian, Configuration},
};
use grovedb_merk::{store::NodeStore, Merk, VisualizeableMerk};
use grovedb_path::SubtreePathBuilder;
use grovedb_version::version::GroveVersion;
use grovedb_visualize::{visualize_stdout, Drawer, Visualize};

use crate::{
    element::{elements_iterator::ElementIteratorExtensions, Element},
    util::TxRef,
    Engine, GroveDb, TransactionArg,
};

impl<E: Engine> GroveDb<E> {
    fn draw_subtree<'db, W: Write, B: AsRef<[u8]>>(
        &'db self,
        mut drawer: Drawer<W>,
        path: SubtreePathBuilder<'_, B>,
        transaction: TransactionArg<'db, '_, E>,
        grove_version: &GroveVersion,
    ) -> Result<Drawer<W>> {
        drawer.down();

        let tx = TxRef::for_read(&self.db, transaction);
        let transaction = Some(tx.as_ref());

        let storage = self.db.subtree((&path).into(), None, tx.as_ref()).unwrap(); // unwraps CostContext (infallible — discards cost)

        let mut iter = Element::iterator(NodeStore::cursor(&storage)).unwrap(); // CostContext unwrap
        while let Some((key, element)) = iter
            .next_element(grove_version)
            .unwrap() // unwraps CostContext (discards cost)
            .map_err(|e| std::io::Error::other(format!("cannot get next element: {e}")))?
        {
            drawer.write(b"\n[key: ")?;
            drawer = key.visualize(drawer)?;
            drawer.write(b" ")?;
            if element.uses_non_merk_data_storage() {
                // Non-Merk data trees store entries as non-Element
                // data in the data namespace — cannot recurse.
                drawer.write(b"[non-Merk tree] ")?;
                drawer = element.visualize(drawer)?;
            } else if element.is_any_tree() {
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
            } else {
                drawer = element.visualize(drawer)?;
            }
        }

        drawer.up();
        Ok(drawer)
    }

    fn draw_root_tree<'db, W: Write>(
        &'db self,
        mut drawer: Drawer<W>,
        transaction: TransactionArg<'db, '_, E>,
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

    fn visualize_start<'db, W: Write>(
        &'db self,
        mut drawer: Drawer<W>,
        transaction: TransactionArg<'db, '_, E>,
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
pub fn visualize_merk_stdout<'db, S: NodeStore<'db>>(merk: &Merk<S>) {
    visualize_stdout(&VisualizeableMerk::new(merk, |bytes: &[u8]| {
        let config = config::standard().with_big_endian().with_no_limit();
        bincode::decode_from_slice_untrusted::<Element, Configuration<BigEndian>>(bytes, config)
            .expect("unable to deserialize Element")
            .0
    }));
}
