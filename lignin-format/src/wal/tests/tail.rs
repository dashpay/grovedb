//! The acceptance test of the task: a truncated or bit-flipped log decodes
//! to the longest valid prefix of whole groups.

use super::*;

/// Logs of random items. Half of them are written with records of at most
/// 200 bytes, so that their values and groups take several records each.
fn logs() -> Vec<Built> {
    vec![
        random_log(0xF0501, Limits::FORMAT, 12),
        random_log(0xF0502, SMALL, 12),
        random_log(0xF0503, Limits::FORMAT, 24),
        random_log(0xF0504, SMALL, 16),
        random_log(0xF0505, SMALL, 5),
        random_log(0xF0506, Limits::FORMAT, 1),
    ]
}

fn is_damage(error: FormatError) -> bool {
    matches!(
        error,
        FormatError::ChecksumMismatch { .. }
            | FormatError::RecordTooLong { .. }
            | FormatError::Truncated { .. }
    )
}

#[test]
fn a_truncated_or_bit_flipped_log_decodes_to_the_longest_valid_prefix_of_whole_groups() {
    let (mut groups, mut split_groups, mut split_values, mut others) = (0, 0, 0, 0);
    let (mut truncations, mut flips, mut holes) = (0u64, 0u64, 0u64);
    for built in logs() {
        let len = built.log.len() as u64;

        // The log as it was written gives every item, each as it was
        // written, and ends cleanly.
        let tail = built.check_prefix(&built.log, len);
        assert_eq!(tail.failure(), None);
        for item in scan_with(&built.log, built.limits).expect("a log") {
            let LogItem::Commit(group) = item else {
                others += 1;
                continue;
            };
            groups += 1;
            let records = built
                .record_ends
                .iter()
                .filter(|end| {
                    let at = built.start.offset_of(group.address()).expect("in the file");
                    at < **end && **end <= at + group.encoded_len() as u64
                })
                .count();
            let values: Vec<_> = group.values().collect();
            let parts = records
                - values
                    .iter()
                    .map(|(_, value)| value.bytes().map_or(1, |bytes| bytes.parts().count()))
                    .sum::<usize>();
            split_groups += usize::from(parts > 1);
            split_values += values
                .iter()
                .filter(|(_, value)| value.bytes().is_some_and(|bytes| bytes.parts().count() > 1))
                .count();
        }

        // Cut at every length.
        for cut in 0..len {
            let bytes = &built.log[..cut as usize];
            if cut < FIRST_ITEM_OFFSET {
                assert!(scan_with(bytes, built.limits).is_err(), "{cut}");
                continue;
            }
            truncations += 1;
            let tail = built.check_prefix(bytes, cut);
            let whole = cut == built.item_start(cut);
            match tail.failure() {
                None => assert!(whole, "cut at {cut}"),
                Some(failure) => {
                    assert!(!whole, "cut at {cut}");
                    assert_eq!(failure.offset, built.record_start(cut), "cut at {cut}");
                    let inside_a_record = failure.offset != cut;
                    assert_eq!(
                        matches!(failure.error, FormatError::Truncated { .. }),
                        inside_a_record,
                        "cut at {cut}: {failure:?}"
                    );
                    assert!(
                        inside_a_record || failure.error == wal_error(WalError::UnfinishedGroup),
                        "cut at {cut}: {failure:?}"
                    );
                }
            }
        }

        // Every single bit flipped.
        let mut bytes = built.log.clone();
        for at in 0..len {
            for bit in 0..8 {
                bytes[at as usize] ^= 1 << bit;
                if at < FIRST_ITEM_OFFSET {
                    assert!(scan_with(&bytes, built.limits).is_err(), "{at}");
                } else {
                    flips += 1;
                    let tail = built.check_prefix(&bytes, at);
                    let failure = tail.failure().expect("the flip is found");
                    assert_eq!(failure.offset, built.record_start(at), "bit {bit} of {at}");
                    assert!(is_damage(failure.error), "bit {bit} of {at}: {failure:?}");
                }
                bytes[at as usize] ^= 1 << bit;
            }
        }
        assert_eq!(bytes, built.log);

        // A range that reads as zeros: what a crash leaves of bytes that
        // were not synced, under a length that survived.
        for from in (FIRST_ITEM_OFFSET..len).step_by(13) {
            for hole in [1, 16, 64, 512, len] {
                let to = (from + hole).min(len);
                let range = from as usize..to as usize;
                let Some(first) = built.log[range.clone()].iter().position(|byte| *byte != 0)
                else {
                    continue;
                };
                bytes[range.clone()].fill(0);
                holes += 1;
                let tail = built.check_prefix(&bytes, from + first as u64);
                assert!(tail.failure().is_some(), "{from}..{to}");
                bytes[range.clone()].copy_from_slice(&built.log[range]);
            }
        }
    }
    println!(
        "{groups} groups ({split_groups} in several parts, {split_values} values in several \
         records), {others} other items; {truncations} cuts, {flips} flipped bits, {holes} holes"
    );
    assert!(groups > 30 && others > 10, "{groups} {others}");
    assert!(
        split_groups > 5 && split_values > 5,
        "{split_groups} {split_values}"
    );
}

#[test]
fn damage_never_gives_an_item_that_was_not_written() {
    let mut rng = SplitMix64::new(0xDA4A6E);
    let mut shorter = 0;
    for built in [
        random_log(0xF0511, Limits::FORMAT, 10),
        random_log(0xF0512, SMALL, 10),
    ] {
        let len = built.log.len();
        for _ in 0..4_000 {
            let mut bytes = built.log.clone();
            for _ in 0..1 + rng.next_u64() % 3 {
                let at = FIRST_ITEM_OFFSET as usize
                    + (rng.next_u64() % (len as u64 - FIRST_ITEM_OFFSET)) as usize;
                let to = (at + 1 + (rng.next_u64() % 40) as usize).min(len);
                match rng.next_u64() % 4 {
                    0 => bytes[at..to].fill(0),
                    1 => bytes[at..to].fill(0xFF),
                    2 => bytes[at..].fill(0),
                    _ => bytes[at..to].copy_from_slice(&rng.bytes(to - at)),
                }
            }
            if rng.next_u64().is_multiple_of(3) {
                bytes.truncate(FIRST_ITEM_OFFSET as usize + (rng.next_u64() as usize % len));
            }
            let common = bytes.len().min(len);
            let first = (0..common)
                .find(|at| bytes[*at] != built.log[*at])
                .unwrap_or(common);
            let tail = built.check_prefix(&bytes, first as u64);
            shorter += usize::from(tail.valid_len() < len as u64);
        }
    }
    assert!(shorter > 7_000, "{shorter}");
}

#[test]
fn the_log_goes_on_in_the_next_file_where_this_one_stops_being_valid() {
    let built = random_log(0xF0521, Limits::FORMAT, 8);
    let (_, tail) = items_with(&built.log, built.limits);
    assert_eq!(tail.valid_len(), built.log.len() as u64);

    let next = built
        .start
        .next(tail.valid_len(), tail.latest())
        .expect("a start");
    assert_eq!(next.base(), built.start.base() + tail.valid_len());
    assert_eq!(next.after(), tail.latest());
    assert_eq!(
        built
            .start
            .check_next(tail.valid_len(), tail.latest(), &next),
        Ok(())
    );

    // The file that was written behind the whole of this one does not
    // follow a shorter valid part of it: what a crash leaves when it loses
    // the end of one file and keeps the next.
    let cut = built.ends[built.ends.len() - 2];
    let (_, shorter) = items_with(&built.log[..cut as usize + 3], built.limits);
    assert_eq!(shorter.valid_len(), cut);
    assert_eq!(
        built
            .start
            .check_next(shorter.valid_len(), shorter.latest(), &next),
        Err(wal_error(WalError::NotTheNextFile {
            expected_base: built.start.base() + cut,
            found_base: next.base(),
            expected_after: shorter.latest(),
            found_after: next.after(),
        }))
    );

    // Nor does a file that states another commit before it.
    let other = LogStart::new(next.base(), version(tail.latest().get() + 1)).expect("a start");
    assert!(built
        .start
        .check_next(tail.valid_len(), tail.latest(), &other)
        .is_err());

    // The next file's first group has the version after the last one here.
    let mut log = new_log(2, &next);
    push_group(
        &mut log,
        &next,
        Limits::FORMAT,
        tail.latest().get() + 1,
        |_| {},
    );
    let (found, after) = items(&log);
    assert_eq!(found.len(), 1);
    assert_eq!(after.latest().get(), tail.latest().get() + 1);

    let mut log = new_log(2, &next);
    push_group(
        &mut log,
        &next,
        Limits::FORMAT,
        tail.latest().get() + 2,
        |_| {},
    );
    let (found, after) = items(&log);
    assert_eq!(found, []);
    assert_eq!(
        after.failure().map(|failure| failure.error),
        Some(wal_error(WalError::VersionGap {
            after: tail.latest(),
            found: version(tail.latest().get() + 2),
        }))
    );
}
