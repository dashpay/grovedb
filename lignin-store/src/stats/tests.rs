use std::time::Duration;

use super::*;

#[test]
fn counts_from_many_threads_add_up() {
    let stats = Stats::new();
    std::thread::scope(|scope| {
        for _ in 0..40 {
            scope.spawn(|| {
                for _ in 0..100 {
                    stats.count_hashes(3);
                    stats.count_keyed_lookups(1);
                }
                stats.count_cleaner_bytes(7);
            });
        }
    });
    assert_eq!(
        (stats.hashes(), stats.keyed_lookups(), stats.cleaner_bytes()),
        (12_000, 4_000, 280)
    );
}

#[test]
fn the_oldest_open_write_is_the_first_still_open() {
    let stats = Stats::new();
    assert_eq!(stats.open_writes(), (0, None));
    let first = stats.write_opened();
    std::thread::sleep(Duration::from_millis(5));
    let second = stats.write_opened();
    let (count, oldest) = stats.open_writes();
    assert_eq!(count, 2);
    assert!(oldest.expect("open") >= Duration::from_millis(5));
    drop(first);
    let (count, oldest) = stats.open_writes();
    assert_eq!(count, 1);
    assert!(oldest.expect("open") < Duration::from_millis(5));
    drop(second);
    assert_eq!(stats.open_writes(), (0, None));
}

#[test]
fn no_log_held_has_no_holder() {
    let watch = Watch {
        unwritten: 3,
        open_writes: 1,
        oldest_write: Some(Duration::from_secs(1)),
        ..Watch::default()
    };
    assert!(watch.log_held_by().is_empty());
    let watch = Watch {
        log_held: 10,
        ..Watch::default()
    };
    assert_eq!(watch.log_held_by(), vec![Holder::NextCheckpoint]);
}

#[test]
fn holders_come_worst_first() {
    let watch = Watch {
        log_held: 1 << 20,
        unwritten: 4,
        pending_value_bytes: 900,
        open_writes: 2,
        oldest_write: Some(Duration::from_secs(3)),
        blocked_pages: 1,
        failing_pages: 2,
        segments_not_durable: true,
        ..Watch::default()
    };
    assert_eq!(
        watch.log_held_by(),
        vec![
            Holder::SegmentsNotDurable,
            Holder::FailingPages { pages: 2 },
            Holder::BlockedPages { pages: 1 },
            Holder::OpenWrites {
                count: 2,
                oldest: Duration::from_secs(3)
            },
            Holder::PendingValues { bytes: 900 },
            Holder::Unwritten { pages: 4 },
        ]
    );
}

#[test]
fn every_holder_says_what_it_is() {
    let said: Vec<String> = [
        Holder::Unwritten { pages: 4 },
        Holder::PendingValues { bytes: 900 },
        Holder::OpenWrites {
            count: 2,
            oldest: Duration::from_secs(3),
        },
        Holder::BlockedPages { pages: 1 },
        Holder::FailingPages { pages: 2 },
        Holder::SegmentsNotDurable,
        Holder::NextCheckpoint,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        said,
        [
            "4 page versions to write back",
            "900 bytes of pending values",
            "2 open write transactions, the oldest open for 3s",
            "1 pages held back by write transactions",
            "2 pages that cannot be written",
            "a segment sync failed: no checkpoint until reopened",
            "the next table checkpoint",
        ]
    );
}

#[test]
fn a_reading_prints_as_a_short_report() {
    let reading = StoreStats {
        log_bytes: 100,
        log_groups: 2,
        node_records: 3,
        node_record_bytes: 40,
        other_records: 1,
        other_record_bytes: 5,
        flush_bytes: 70,
        cleaner_bytes: 8,
        page_reads: 9,
        cache_hits: 10,
        hashes: 11,
        keyed_lookups: 12,
        watch: Watch {
            log_held: 60,
            unwritten: 1,
            pending_value_bytes: 2,
            checkpoints_put_off: 3,
            checkpoints_failed: 2,
            room_waits: 4,
            room_stalls: 1,
            room_waited: Duration::from_millis(5),
            segment_syncs_failed: 0,
            ..Watch::default()
        },
        ..StoreStats::default()
    };
    assert_eq!(
        reading.to_string(),
        "log: 100 bytes in 2 groups; 3 node records (40 bytes), 1 other records (5 bytes)\n\
         written back: 70 bytes; cleaned: 8 bytes; pages read: 9, cache hits: 10; hashes: 11; \
         keyed lookups: 12\n\
         log held: 60 bytes by 2 bytes of pending values, 1 page versions to write back\n\
         checkpoints put off: 3, failed: 2; waits for room: 4 (1 stalled, 5ms in all); \
         segment syncs failed: 0"
    );
    let idle = StoreStats::default().to_string();
    assert!(idle.contains("log held: 0 bytes\n"), "{idle}");
}
