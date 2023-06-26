use grovedb::Element;
use grovedb::GroveDb;

fn main() {
    // Specify a path and open GroveDB at the path as db
    let path = String::from("../tutorial-storage");
    let db = GroveDb::open(path).unwrap();

    // Define key-values for insertion
    let key1 = b"hello";
    let val1 = b"world";
    let key2 = b"grovedb";
    let val2 = b"rocks";

    // Insert key-value 1 into the root tree
    db.insert([], key1, Element::Item(val1.to_vec(), None), None, None)
        .unwrap()
        .expect("successful key1 insert");

    // Insert key-value 2 into the root tree
    db.insert([], key2, Element::Item(val2.to_vec(), None), None, None)
        .unwrap()
        .expect("successful key2 insert");

    // At this point the Items are fully inserted into the database.
    // No other steps are required.

    // To show that the Items are there, we will use the get()
    // function to get them from the RocksDB backing store.

    // Get value 1
    let result1 = db.get([], key1, None).unwrap();

    // Get value 2
    let result2 = db.get([], key2, None).unwrap();

    // Print the values to terminal
    println!("{:?}", result1);
    println!("{:?}", result2);
}
