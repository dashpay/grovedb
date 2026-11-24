//! Which engine `GroveDb` with no engine named is on, as a dependent of the
//! crate sees it.
//!
//! It is the RocksDB engine wherever that is compiled in, and the LigninDB
//! engine where that alone is. In a build with both engines that sets the
//! cfg `grovedb_default_engine = "either"` it is the engine that is chosen
//! when a database is opened: the tests of `chosen_at_the_open` are
//! compiled there, as by
//! `GROVEDB_DEFAULT_ENGINE=either cargo test -p grovedb --features lignin_storage --test default_engine`.

#![cfg(any(feature = "rocksdb_storage", feature = "lignin_storage"))]
#![allow(clippy::result_large_err)]

use std::any::TypeId;

use grovedb::{
    engine::DefaultEngine, replication::MultiStateSyncSession, Engine, GroveDb, Transaction,
    TransactionArg,
};

/// Whether `GroveDb`, `Transaction`, `TransactionArg` and the session of
/// state sync, each with no engine named, are those of engine `E`.
fn the_names_with_no_engine_are_of<E: Engine>() -> bool {
    TypeId::of::<DefaultEngine>() == TypeId::of::<E>()
        && TypeId::of::<GroveDb>() == TypeId::of::<GroveDb<E>>()
        && TypeId::of::<Transaction<'static>>() == TypeId::of::<Transaction<'static, E>>()
        && TypeId::of::<Transaction<'static>>() == TypeId::of::<E::Transaction<'static>>()
        && TypeId::of::<TransactionArg<'static, 'static>>()
            == TypeId::of::<TransactionArg<'static, 'static, E>>()
        && TypeId::of::<MultiStateSyncSession<'static>>()
            == TypeId::of::<MultiStateSyncSession<'static, E>>()
}

/// A build that does not set the cfg has the surface it had: the RocksDB
/// engine where it is compiled in, whether or not the LigninDB engine is
/// compiled beside it. So has a build that sets the cfg and has one engine.
#[cfg(all(
    feature = "rocksdb_storage",
    not(all(grovedb_default_engine = "either", feature = "lignin_storage"))
))]
#[test]
fn with_no_engine_named_a_database_is_on_rocksdb() {
    assert!(the_names_with_no_engine_are_of::<grovedb::RocksDbEngine>());
    let _: fn(&GroveDb) -> &grovedb::RocksDbEngine = GroveDb::engine;
    let dir = tempfile::tempdir().expect("a directory");
    let db: GroveDb<grovedb::RocksDbEngine> = GroveDb::open(dir.path()).expect("a database");
    let _: &grovedb_storage::rocksdb_storage::RocksDbStorage = db.engine().storage();
    assert!(dir.path().join("CURRENT").exists());
}

/// With both engines compiled in, the engine that is chosen at the open is
/// there under its own name, whatever the default is.
#[cfg(all(feature = "rocksdb_storage", feature = "lignin_storage"))]
#[test]
fn the_engine_that_is_chosen_at_the_open_is_there_by_name() {
    use grovedb::{EitherEngine, EngineChoice, EngineKind};

    let dir = tempfile::tempdir().expect("a directory");
    let engine = EitherEngine::open_with(dir.path(), &EngineChoice::RocksDb).expect("a database");
    let db: GroveDb<EitherEngine> = GroveDb::from_engine(engine);
    assert_eq!(db.engine().kind(), EngineKind::RocksDb);
    assert_eq!(
        the_names_with_no_engine_are_of::<EitherEngine>(),
        cfg!(grovedb_default_engine = "either")
    );
}

#[cfg(all(feature = "lignin_storage", not(feature = "rocksdb_storage")))]
#[test]
fn with_no_engine_named_a_database_is_on_lignindb_where_that_is_the_one_engine() {
    assert!(the_names_with_no_engine_are_of::<grovedb::LigninEngine>());
}

#[cfg(all(
    grovedb_default_engine = "either",
    feature = "rocksdb_storage",
    feature = "lignin_storage"
))]
mod chosen_at_the_open {
    use grovedb::{
        engine::either::lignindb::Options, EitherEngine, Element, EngineChoice, EngineKind,
        GroveDb, PathQuery, Query, Transaction, TransactionArg,
    };
    use grovedb_version::version::GroveVersion;

    use super::the_names_with_no_engine_are_of;

    const ROOT: &[&[u8]] = &[];

    /// The database and the transaction have lifetimes of their own, as in
    /// a function that was written for the RocksDB engine.
    fn tree_with_an_item(db: &GroveDb, transaction: TransactionArg, v: &GroveVersion) {
        let (tree, item) = (Element::empty_tree(), Element::new_item(b"value".to_vec()));
        (db.insert(ROOT, b"tree", tree, None, transaction, v)
            .unwrap())
        .expect("a tree");
        let put = db.insert(&[b"tree".as_slice()], b"key", item, None, transaction, v);
        put.unwrap().expect("an item");
    }

    #[test]
    fn with_no_engine_named_a_database_is_on_the_engine_that_is_chosen_at_the_open() {
        assert!(the_names_with_no_engine_are_of::<EitherEngine>());
        assert!(!the_names_with_no_engine_are_of::<grovedb::RocksDbEngine>());
        let _: fn(&GroveDb) -> &EitherEngine = GroveDb::engine;
    }

    /// `GroveDb::open` makes RocksDB's database where there is none, and
    /// opens the database that is there, of either engine.
    #[test]
    fn an_open_that_names_no_engine_makes_rocksdb_and_opens_what_is_there() {
        let v = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("a directory");
        let (made, of_lignin) = (dir.path().join("made"), dir.path().join("of lignindb"));
        {
            let db = GroveDb::open(&made).expect("a database");
            assert_eq!(db.engine().kind(), EngineKind::RocksDb);
            assert!(made.join("CURRENT").exists());
            let transaction: Transaction = db.start_transaction();
            tree_with_an_item(&db, Some(&transaction), v);
            db.commit_transaction(transaction)
                .unwrap()
                .expect("a commit");
        }
        let root = {
            let db = GroveDb::open(&made).expect("the database that is there");
            assert_eq!(db.engine().kind(), EngineKind::RocksDb);
            db.root_hash(None, v).unwrap().expect("a root hash")
        };
        {
            let choice = EngineChoice::Lignin(Options::default());
            let engine = EitherEngine::open_with(&of_lignin, &choice).expect("a database");
            let db: GroveDb = GroveDb::from_engine(engine);
            assert_eq!(db.engine().kind(), EngineKind::Lignin);
            tree_with_an_item(&db, None, v);
        }
        let db = GroveDb::open(&of_lignin).expect("the database that is there");
        assert_eq!(db.engine().kind(), EngineKind::Lignin);
        assert_eq!(db.root_hash(None, v).unwrap().expect("a root hash"), root);

        // Checkpoints with no engine named are of the engine that made
        // them.
        let checkpoint = dir.path().join("checkpoint");
        db.create_checkpoint(&checkpoint).expect("a checkpoint");
        let opened = GroveDb::open_checkpoint(&checkpoint).expect("the checkpoint");
        assert_eq!(opened.engine().kind(), EngineKind::Lignin);
        drop(opened);
        GroveDb::delete_checkpoint(&checkpoint).expect("a removal");
        assert!(!checkpoint.exists());
        let checked = GroveDb::open_with_cidx_integrity_check(&made, v).expect("a sound grove");
        assert_eq!(checked.engine().kind(), EngineKind::RocksDb);
    }

    /// The functions of `GroveDb` that take no database are called with no
    /// engine named, as where the RocksDB engine is the default: each
    /// resolves, and a proof that a database of either engine made is
    /// verified.
    #[test]
    fn the_functions_that_take_no_database_resolve_with_no_engine_named() {
        macro_rules! resolve {
            ($($name:ident),* $(,)?) => {
                $(let _ = GroveDb::$name;)*
            };
        }
        resolve!(
            verify_aggregate_big_sum_query,
            verify_aggregate_big_sum_query_per_key,
            verify_aggregate_big_sum_query_per_key_up_to_limit,
            verify_aggregate_count_and_big_sum_query,
            verify_aggregate_count_and_big_sum_query_per_key,
            verify_aggregate_count_and_big_sum_query_per_key_up_to_limit,
            verify_aggregate_count_and_sum_query,
            verify_aggregate_count_and_sum_query_per_key,
            verify_aggregate_count_and_sum_query_per_key_up_to_limit,
            verify_aggregate_count_query,
            verify_aggregate_count_query_per_key,
            verify_aggregate_count_query_per_key_up_to_limit,
            verify_aggregate_sum_query,
            verify_aggregate_sum_query_per_key,
            verify_aggregate_sum_query_per_key_up_to_limit,
            verify_branch_chunk_proof,
            verify_bulk_position_range_proof,
            verify_path_query,
            verify_query,
            verify_query_get_parent_tree_info,
            verify_query_get_parent_tree_info_with_options,
            verify_query_raw,
            verify_query_with_absence_proof,
            verify_query_with_options,
            verify_subset_query,
            verify_subset_query_get_parent_tree_info,
            verify_subset_query_with_absence_proof,
            verify_trunk_chunk_proof,
        );
        #[cfg(feature = "estimated_costs")]
        resolve!(average_case_for_get, worst_case_for_get);

        let v = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("a directory");
        let mut all = Query::new();
        all.insert_all();
        let all = PathQuery::new_unsized(vec![b"tree".to_vec()], all);
        let choices = [
            EngineChoice::RocksDb,
            EngineChoice::Lignin(Options::default()),
        ];
        for choice in choices {
            let at = dir.path().join(choice.kind().to_string());
            let engine = EitherEngine::open_with(at, &choice).expect("a database");
            let db: GroveDb = GroveDb::from_engine(engine);
            tree_with_an_item(&db, None, v);
            let root = db.root_hash(None, v).unwrap().expect("a root hash");
            let proof = db.prove_query(&all, None, v).unwrap().expect("a proof");
            let (proved, rows) =
                GroveDb::verify_query(&proof, &all, v).expect("a proof that holds");
            assert_eq!((proved, rows.len()), (root, 1));
            let (proved, _) = GroveDb::verify_query_raw(&proof, &all, v).expect("as raw rows");
            assert_eq!(proved, root);
            // The same query again, of what the first one proved.
            let again = |_| Some(all.clone());
            let chained =
                GroveDb::verify_query_with_chained_path_queries(&proof, &all, vec![again], v);
            let (proved, of_each) = chained.expect("two queries of one proof");
            assert_eq!((proved, of_each.len()), (root, 2));
        }
    }
}
