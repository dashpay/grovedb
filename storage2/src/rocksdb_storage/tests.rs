use tempfile::TempDir;

use super::*;

struct TempStorage {
    dir: TempDir,
    storage: RocksDbStorage,
}

impl TempStorage {
    fn new() -> Self {
        let dir = TempDir::new().expect("cannot create tempir");
        let storage = RocksDbStorage::default_rocksdb_with_path(dir.path())
            .expect("cannot open RocksDB storage");
        TempStorage { dir, storage }
    }
}

mod no_transaction {
    #[test]
    fn test_aux_cf_methods() {}

    #[test]
    fn test_roots_cf_methods() {}

    #[test]
    fn test_meta_cf_methods() {}

    #[test]
    fn test_default_cf_methods() {}
}

mod transaction {
    #[test]
    fn test_aux_cf_methods() {}

    #[test]
    fn test_roots_cf_methods() {}

    #[test]
    fn test_meta_cf_methods() {}

    #[test]
    fn test_default_cf_methods() {}
}
