use grovedb::GroveDb;
use std::thread;

fn main() {
    let primary = GroveDb::open("test.db").expect("should open");

    thread::spawn(|| {
        let secondary = GroveDb::open("test.db").expect("should open");
    });
}
