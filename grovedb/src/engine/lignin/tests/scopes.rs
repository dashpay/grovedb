//! Write scopes of the engine that are open side by side
//! (`engine.md`, "Write scopes").

use grovedb_storage::Error as StorageError;

use super::{check, version, Lignin, LigninEngine};
use crate::{engine::Engine, Element, Error};

const ROOT: &[&[u8]] = &[];

/// A write scope that is opened beside another keeps its writes in the
/// write set until it merges, and reads a Merk's nodes by their keys
/// while it holds a writer's claim on the Merk's data. A read through its
/// view that needs the key of a child through the location of a cell is
/// then refused (`engine.md`, "Write scopes"). The refusal is a read that
/// failed: `get_raw_optional` answers a key that is there by its element
/// or by that error, and not as a key that is not there.
#[test]
fn a_read_through_a_scope_beside_another_is_refused_and_never_answered_as_absent() {
    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let top: &[&[u8]] = &[b"top"];
        let made = db.insert(ROOT, b"top", Element::empty_tree(), None, None, v);
        made.unwrap().expect("a tree");
        let names: [&[u8]; 7] = [b"a", b"b", b"c", b"d", b"e", b"f", b"g"];
        for name in names {
            let made = db.insert(top, name, Element::empty_tree(), None, None, v);
            made.unwrap().expect("a tree in it");
            let under: &[&[u8]] = &[b"top", name];
            let made = db.insert(under, b"x", Element::new_item(vec![1]), None, None, v);
            made.unwrap().expect("an item in that");
        }

        let engine = db.engine();
        let tx = db.start_transaction();
        let holder = engine.open_write_scope(&tx).expect("a scope");
        let writer = engine.open_write_scope(&tx).expect("a second scope");
        let view = LigninEngine::scope_transaction(&writer);
        let read = |name: &[u8]| {
            let under: &[&[u8]] = &[b"top", name];
            (db.get_raw_optional(under.into(), b"x", Some(view), v)).unwrap()
        };
        // With nothing written, the scope reads every item.
        for name in names {
            assert!(matches!(read(name), Ok(Some(_))), "{name:?}");
        }
        // It writes into the tree `g`: the nodes of `top` on the way to
        // the element of `g` are its records, and it holds `top`.
        let under: &[&[u8]] = &[b"top", b"g"];
        let put = db.insert(under, b"y", Element::new_item(vec![2]), None, Some(view), v);
        put.unwrap().expect("an item through the scope");
        let mut refused = 0;
        for name in names {
            match read(name) {
                Ok(Some(_)) => {}
                Err(Error::StorageError(StorageError::NotSupported(what))) => {
                    assert!(
                        what.contains("reads the children of a node by key"),
                        "{what}"
                    );
                    refused += 1;
                }
                other => panic!("the item of {name:?}: {other:?}"),
            }
        }
        // The element of a tree whose node the scope did not write and
        // that has children is not read.
        assert!(refused > 0);
        engine.rollback_write_scope(writer).expect("a rollback");
        engine.rollback_write_scope(holder).expect("a rollback");
        drop(tx);
        db.flush().expect("a flush");
    }
    check(dir.path());
}
