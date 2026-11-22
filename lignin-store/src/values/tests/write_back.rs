//! A write-back that fails unreported (`lignin_io::sim`, "A write-back that
//! fails"), on a file layer that tells of it only the handle that wrote:
//! the least the file layer's contract promises.
//!
//! A value-log file is synced through the handle that appended to it, so
//! the first sync of the file after the failure is told of it and fails,
//! whichever way that sync is asked for. The file has then failed, and
//! nothing vouches for the bytes.

use lignin_io::sim::{Fault, SimConfig, SimError, SimVfs, Told, UnknownFate, WriteBackRule};

use super::{sim::*, store::Store, *};

const FATES: [UnknownFate; 3] = [UnknownFate::Lost, UnknownFate::Kept, UnknownFate::Drawn(3)];

/// The ways a value-log file that is being written is made durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// [`ValueWriter::sync`].
    Writer,
    /// [`ValueWriter::finish`].
    Finish,
    /// [`ValueWriter::give_up`].
    GiveUp,
    /// [`ValueStore::sync`] while the file has its writer.
    Store,
    /// [`ValueStore::sync`] once the writer is dropped.
    StoreAfterTheWriter,
}

const WAYS: [Way; 5] = [
    Way::Writer,
    Way::Finish,
    Way::GiveUp,
    Way::Store,
    Way::StoreAfterTheWriter,
];

fn store(unknown: UnknownFate) -> Store {
    Store::on(sim_fs(SimConfig {
        unknown,
        ..traced(5, 512)
    }))
}

/// Fails the write-back of the value-log file `raw_id` after the append
/// that follows `skip` more of them.
fn fail_write_back(store: &Store, raw_id: u32, skip: u64, forgotten: bool) {
    let rule = WriteBackRule::new(Fault::Eio)
        .at(store.path(raw_id))
        .skip(skip)
        .told(Told::Writers)
        .forgotten(forgotten);
    store.fs.add_write_back(rule);
}

/// Whether `error` is a sync of the value-log file `raw_id` that the file
/// layer failed with the error of the write-back.
fn is_told(store: &Store, error: &ValueStoreError, raw_id: u32) -> bool {
    let ValueStoreError::Io { op, path, source } = error else {
        return false;
    };
    let injected = matches!(
        SimError::of(source),
        Some(SimError::Injected {
            fault: Fault::Eio,
            ..
        })
    );
    *op == "sync" && *path == store.path(raw_id) && injected
}

/// What the next session reads of `pointers`, in the store of `fs`.
fn read_in(fs: &SimVfs, pointers: &[ValuePointer]) -> Vec<Vec<u8>> {
    let manifest = ManifestStore::open(shared(fs), db_dir()).expect("the manifest opens");
    let values = ValueStore::open(&manifest, &[]).expect("the value log opens");
    let mut buf = Vec::new();
    let read = |pointer: &ValuePointer| {
        let value = values.read(*pointer, page(1), &mut buf);
        value.expect("the value is read").to_vec()
    };
    pointers.iter().map(read).collect()
}

#[test]
fn no_sync_of_a_value_log_file_vouches_for_values_whose_write_back_failed() {
    for (way, unknown, forgotten) in WAYS
        .into_iter()
        .flat_map(|way| FATES.map(|unknown| (way, unknown)))
        .flat_map(|(way, unknown)| [false, true].map(|forgotten| (way, unknown, forgotten)))
    {
        let context = || format!("{way:?}, {unknown:?}, forgotten: {forgotten}");
        let _context = OnFailure(&context);
        let mut store = store(unknown);
        let mut writer = store.writer();
        let mut hold = ValueHold::new();
        let contents = [b"the first".to_vec(), b"the second".to_vec()];
        let synced: Vec<ValuePointer> = contents
            .iter()
            .map(|value| writer.append(&mut hold, page(1), value).unwrap())
            .collect();
        let durable = writer.sync().expect("the first values are synced");

        // The write-back fails after the second of three more values.
        fail_write_back(&store, 1, 1, forgotten);
        let lost: Vec<ValuePointer> = (0..3)
            .map(|at| writer.append(&mut hold, page(1), &[at; 700]).unwrap())
            .collect();
        assert_eq!(store.fs.stats().write_backs, 1);

        let mut writer = Some(writer);
        let refused = match way {
            Way::Writer => writer.as_mut().unwrap().sync().map(drop),
            Way::Finish => writer.take().unwrap().finish().map(drop),
            Way::GiveUp => writer.take().unwrap().give_up().map(drop),
            Way::Store => store.values.sync(),
            Way::StoreAfterTheWriter => {
                writer = None;
                store.values.sync()
            }
        };
        let error = refused.expect_err("the sync is told of the write-back that failed");
        assert!(is_told(&store, &error, 1), "{error}");

        // The file has failed as after any failed sync, and no later sync
        // of it says otherwise.
        if let Some(writer) = &mut writer {
            for _ in 0..2 {
                assert!(matches!(
                    writer.sync(),
                    Err(ValueStoreError::FileFailed { .. })
                ));
                assert!(matches!(
                    store.values.sync(),
                    Err(ValueStoreError::FileFailed { .. })
                ));
            }
        }
        drop(writer);
        store.values.sync().expect("a closed file is not synced");

        // The manifest names what was synced before the failure and covers
        // nothing behind it, and every state a crash can leave has it.
        store.commit();
        assert_eq!(store.named(), [value_file(1, FileState::Closed, durable)]);
        assert!(synced.iter().all(|pointer| store.values.covers(*pointer)));
        assert!(!lost.iter().any(|pointer| store.values.covers(*pointer)));
        let states = crash_states(&store.fs.crash_image(), &EVERY_STATE);
        assert!(states.exhaustive);
        for (plan, state) in &states.states {
            assert_eq!(read_in(state, &synced), contents, "{plan:?}");
        }
        drop(hold);
        let store = store.reopen(&[]);
        assert_eq!(read_in(&store.fs, &synced), contents);
    }
}

#[test]
fn a_write_back_that_fails_under_the_header_of_a_value_log_file_fails_its_creation() {
    for unknown in FATES {
        let mut store = store(unknown);
        fail_write_back(&store, 1, 0, false);
        let error = store
            .values
            .create()
            .expect_err("the sync of the header is told");
        assert!(is_told(&store, &error, 1), "{error}");
        assert_eq!(store.fs.stats().write_backs, 1);
        assert!(!store.exists(1));

        let mut writer = store.writer();
        assert_eq!(writer.file_id(), file_id(2));
        let mut hold = ValueHold::new();
        let pointer = writer.append(&mut hold, page(1), b"a value").unwrap();
        let end = writer.finish().expect("the file is synced and closed");
        store.commit();
        assert_eq!(store.named(), [value_file(2, FileState::Closed, end)]);
        let states = crash_states(&store.fs.crash_image(), &EVERY_STATE);
        for (plan, state) in &states.states {
            assert_eq!(read_in(state, &[pointer]), [b"a value"], "{plan:?}");
        }
    }
}
