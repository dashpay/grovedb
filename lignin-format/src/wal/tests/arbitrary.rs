//! Arbitrary bytes are an error or a log, and never a panic.

use super::*;
use crate::flat_page::FlatPage;

/// Goes through everything a scan of `log` gives, and checks that each item
/// is written again as the bytes it was read from.
fn walk(log: &[u8], limits: Limits) -> usize {
    let Ok(mut scan) = scan_with(log, limits) else {
        return 0;
    };
    let start = scan.start();
    let mut count = 0;
    loop {
        let at = scan.offset();
        let Some(item) = scan.next() else {
            break;
        };
        count += 1;
        if let LogItem::Commit(group) = item {
            for entry in group.entries() {
                match entry {
                    Entry::Tree(run) => assert_eq!(run.deltas().count(), run.deltas().len()),
                    Entry::Flat(run) => assert_eq!(run.changes().count(), run.changes().len()),
                    _ => {}
                }
            }
        }
        let bytes = &log[at as usize..scan.offset() as usize];
        assert_eq!(written_again(&item, &start, at, limits), bytes);
    }
    let tail = scan.tail();
    assert_eq!(tail.valid_len(), scan.offset());
    assert!(tail.valid_len() as usize <= log.len());
    assert_eq!(
        tail.failure().is_none(),
        tail.valid_len() as usize == log.len()
    );
    count
}

/// A record with a kind that exists and fields that are arbitrary, under a
/// checksum that verifies.
fn forged_record(rng: &mut SplitMix64) -> Vec<u8> {
    let len = (rng.next_u64() % 70) as usize;
    let mut payload = rng.bytes(len);
    payload.insert(0, 1 + (rng.next_u64() % 6) as u8);
    match payload[0] {
        // A part with flags, a version and a number that can be right.
        4 if payload.len() >= 14 => {
            payload[1] = (rng.next_u64() % 2) as u8;
            payload[2..10].copy_from_slice(&(1 + rng.next_u64() % 2).to_le_bytes());
            payload[10..14].copy_from_slice(&(rng.next_u64() as u32 % 2).to_le_bytes());
            for at in (14..payload.len()).step_by(5) {
                payload[at] = 1 + (rng.next_u64() % 6) as u8;
            }
        }
        5 if payload.len() >= 2 => payload[1] = 1 + (rng.next_u64() % 3) as u8,
        _ => {}
    }
    framed(&payload)
}

#[test]
fn arbitrary_bytes_are_an_error_or_a_log_and_never_a_panic() {
    let mut rng = SplitMix64::new(0xB10);
    let valid = [
        random_log(0xF0541, Limits::FORMAT, 14),
        random_log(0xF0542, SMALL, 14),
    ];
    let (mut refused, mut logs, mut found) = (0, 0, 0);
    for round in 0..45_000 {
        let built = &valid[round % 2];
        let log = match round % 3 {
            // Anything, behind the start of a file or not.
            0 => {
                let len = (rng.next_u64() % 200) as usize;
                let noise = rng.bytes(len);
                if rng.next_u64().is_multiple_of(4) {
                    noise
                } else {
                    [&built.log[..45], &noise[..]].concat()
                }
            }
            // Records whose checksums verify, with arbitrary fields.
            1 => {
                let mut log = built.log[..45].to_vec();
                for _ in 0..1 + rng.next_u64() % 3 {
                    log.extend_from_slice(&forged_record(&mut rng));
                }
                log
            }
            // A written log with a record changed under a checksum that
            // verifies, and with bytes changed under none.
            _ => {
                let mut log = built.log.clone();
                let record = (rng.next_u64() % built.record_ends.len() as u64) as usize;
                let from = built.record_start(built.record_ends[record] - 1) as usize;
                let to = built.record_ends[record] as usize;
                let mut payload = log[from + 8..to].to_vec();
                for _ in 0..1 + rng.next_u64() % 3 {
                    let at = (rng.next_u64() % payload.len() as u64) as usize;
                    payload[at] = match rng.next_u64() % 3 {
                        0 => rng.next_u64() as u8,
                        1 => payload[at].wrapping_add(1),
                        _ => 0,
                    };
                }
                log.splice(from..to, framed(&payload));
                if rng.next_u64().is_multiple_of(5) {
                    let at = (rng.next_u64() % log.len() as u64) as usize;
                    log[at] ^= 1 << (rng.next_u64() % 8);
                }
                log
            }
        };
        match scan_with(&log, built.limits) {
            Err(_) => refused += 1,
            Ok(_) => {
                logs += 1;
                found += walk(&log, built.limits);
            }
        }
        // A reader that is handed any bytes as a record gets a record or
        // an error.
        for at in [0, 20, 45] {
            if let Some(bytes) = log.get(at..) {
                let _ = Record::decode_with(bytes, V1, built.limits);
                let _ = record_len(bytes, V1);
                let _ = read_logged_value(bytes, V1);
                let _ = read_file_start(bytes);
            }
        }
    }
    println!("{refused} files refused, {logs} read, {found} items in them");
    assert!(
        refused > 3_000 && logs > 30_000 && found > 100_000,
        "{refused} {logs} {found}"
    );
}

#[test]
fn an_arbitrary_flat_run_is_an_error_or_a_run_that_applies_or_is_refused() {
    let mut rng = SplitMix64::new(0xF1A7B10);
    let leaf = {
        let mut leaf = FlatLeafBuilder::new(page(5), b"", CAPACITY).unwrap();
        for key in 0..40u8 {
            leaf.push(&[key], FlatValue::Inline(&[key; 3])).unwrap();
        }
        leaf.finish()
    };
    let branch = {
        let mut branch = FlatBranchBuilder::new(page(5), b"", CAPACITY, 1, page(1)).unwrap();
        for key in 0..40u8 {
            branch.push(&[key], page(2 + u32::from(key))).unwrap();
        }
        branch.finish()
    };
    let (mut runs, mut applied) = (0, 0);
    for round in 0..40_000 {
        let bytes = match round % 3 {
            0 => {
                let len = (rng.next_u64() % 60) as usize;
                rng.bytes(len)
            }
            // Records of a kind that exists, with short keys and arbitrary
            // fields.
            1 => {
                let mut run = vec![5, 0, 0, 0];
                for _ in 0..1 + rng.next_u64() % 4 {
                    let kind = 2 + (rng.next_u64() % 6) as u8;
                    run.extend_from_slice(&[kind, 1, 0, (rng.next_u64() % 44) as u8]);
                    let len = (rng.next_u64() % 6) as usize;
                    run.extend_from_slice(&rng.bytes(len));
                }
                run
            }
            _ => {
                let mut run = random_flat_run(&mut rng, page(5), false, usize::MAX);
                let at = (rng.next_u64() % run.len() as u64) as usize;
                run[at] = rng.next_u64() as u8;
                run
            }
        };
        let Ok(run) = FlatRun::decode(&bytes, V1) else {
            continue;
        };
        runs += 1;
        // What was read is written the same.
        let mut again = Vec::new();
        let mut writer = FlatRunWriter::new(run.page(), &mut again);
        for change in run.changes() {
            writer.push(&change).expect("a change that was read");
        }
        writer.finish().expect("a run");
        assert_eq!(again, bytes);

        for base in [&leaf, &branch] {
            let mut out = Vec::new();
            if run.apply(Some(base), V1, CAPACITY, &mut out).is_ok() {
                applied += 1;
                let page = FlatPage::decode(&out, V1).expect("a page");
                assert_eq!(page.as_bytes().len(), out.len());
            } else {
                assert!(out.is_empty());
            }
        }
    }
    println!("{runs} runs, {applied} applied");
    assert!(runs > 8_000 && applied > 1_000, "{runs} {applied}");
}
