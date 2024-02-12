use grovedb::GroveDb;
use tempfile::TempDir;

#[test]
fn test_replicating_aux_data_to_secondary_database() {
    let primary_dir = TempDir::new().expect("should create temp dir");

    let primary_grovedb = GroveDb::open(primary_dir.path()).expect("should open grovedb");

    // Store value in primary

    let key = b"key";
    let value = vec![1, 2, 3];

    primary_grovedb
        .put_aux(key, &value, None, None)
        .unwrap()
        .expect("should put value to primary");

    // Read value from primary

    let primary_value = primary_grovedb
        .get_aux(key, None)
        .unwrap()
        .expect("should get value from primary")
        .expect("value should exist on primary");

    assert_eq!(value, primary_value);

    // Open secondary

    let secondary_dir = TempDir::new().expect("should create temp dir");

    let secondary_grovedb = GroveDb::open_secondary(primary_dir.path(), secondary_dir.path())
        .expect("should open secondary");

    // Read value on secondary

    let secondary_value = secondary_grovedb
        .get_aux(key, None)
        .unwrap()
        .expect("should get value from secondary")
        .expect("value from primary should exist on secondary");

    assert_eq!(primary_value, secondary_value);

    // Update value on primary

    let primary_value2 = vec![4, 5, 6];

    primary_grovedb
        .put_aux(key, &primary_value2, None, None)
        .unwrap()
        .expect("should put value to primary");

    // Catch up secondary

    secondary_grovedb
        .try_to_catch_up_from_primary()
        .expect("should catch up");

    // Read updated value on secondary

    let secondary_value2 = secondary_grovedb
        .get_aux(key, None)
        .unwrap()
        .expect("should get value from secondary")
        .expect("value from primary should exist on secondary");

    assert_eq!(primary_value2, secondary_value2);
}
