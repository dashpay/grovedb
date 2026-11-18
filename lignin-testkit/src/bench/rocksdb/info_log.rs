//! Reads the events RocksDB writes to its info log.
//!
//! RocksDB records every table file it writes, and every flush and
//! compaction, as an `EVENT_LOG_v1` line of JSON in the `LOG` file of the
//! database directory. Those lines give the background writes that GroveDB
//! cannot see: a `table_file_creation` event carries the size of the file
//! written, and a `compaction_finished` event the bytes its compaction
//! wrote. Whatever a table file holds that no compaction wrote came from a
//! flush.

use serde::{Deserialize, Serialize};
use serde_json::Value;

const EVENT_MARKER: &str = "EVENT_LOG_v1 ";

/// Totals over the events read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTotals {
    /// Table files written, by flushes and compactions.
    pub table_files: u64,
    /// Their bytes.
    pub table_file_bytes: u64,
    /// Flushes finished.
    pub flushes: u64,
    /// Compactions finished.
    pub compactions: u64,
    /// Bytes the finished compactions wrote.
    pub compaction_bytes: u64,
}

impl EventTotals {
    /// Bytes written by flushes: table files no compaction wrote.
    pub fn flush_bytes(&self) -> u64 {
        self.table_file_bytes.saturating_sub(self.compaction_bytes)
    }
}

/// How far the info log has been read.
#[derive(Clone, Debug, Default)]
pub struct InfoLogTail {
    offset: u64,
}

impl InfoLogTail {
    /// Where the next read starts.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads the complete lines of `data`, the file's bytes from
    /// [`InfoLogTail::offset`] on, into `totals`. A line not finished yet
    /// is left for the next call.
    pub fn read(&mut self, data: &[u8], totals: &mut EventTotals) {
        let Some(last_newline) = data.iter().rposition(|byte| *byte == b'\n') else {
            return;
        };
        for line in data[..last_newline].split(|byte| *byte == b'\n') {
            read_line(line, totals);
        }
        self.offset += last_newline as u64 + 1;
    }
}

fn read_line(line: &[u8], totals: &mut EventTotals) {
    let Ok(line) = std::str::from_utf8(line) else {
        return;
    };
    let Some(start) = line.find(EVENT_MARKER) else {
        return;
    };
    let Ok(event) = serde_json::from_str::<Value>(&line[start + EVENT_MARKER.len()..]) else {
        return;
    };
    let number = |field: &str| event.get(field).and_then(Value::as_u64).unwrap_or(0);
    match event.get("event").and_then(Value::as_str) {
        Some("table_file_creation") => {
            totals.table_files += 1;
            totals.table_file_bytes += number("file_size");
        }
        Some("flush_finished") => totals.flushes += 1,
        Some("compaction_finished") => {
            totals.compactions += 1;
            totals.compaction_bytes += number("total_output_size");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
2026/11/18-10:00:00.000001 7f00 [db/flush_job.cc:900] [default] [JOB 2] Flushing memtable\n\
2026/11/18-10:00:00.000002 7f00 EVENT_LOG_v1 {\"time_micros\": 1, \"cf_name\": \"default\", \"job\": 2, \"event\": \"table_file_creation\", \"file_number\": 9, \"file_size\": 1000}\n\
2026/11/18-10:00:00.000003 7f00 EVENT_LOG_v1 {\"time_micros\": 2, \"job\": 2, \"event\": \"flush_finished\", \"output_compression\": \"Snappy\"}\n\
2026/11/18-10:00:00.000004 7f00 EVENT_LOG_v1 {\"time_micros\": 3, \"job\": 3, \"event\": \"table_file_creation\", \"file_number\": 10, \"file_size\": 700}\n\
2026/11/18-10:00:00.000005 7f00 EVENT_LOG_v1 {\"time_micros\": 4, \"job\": 3, \"event\": \"compaction_finished\", \"total_output_size\": 700}\n\
2026/11/18-10:00:00.000006 7f00 EVENT_LOG_v1 {not json}\n\
2026/11/18-10:00:00.000007 7f00 EVENT_LOG_v1 {\"event\": \"table_file_deletion\", \"file_number\": 9}\n";

    #[test]
    fn table_files_flushes_and_compactions_are_counted() {
        let mut totals = EventTotals::default();
        let mut tail = InfoLogTail::default();
        tail.read(LOG.as_bytes(), &mut totals);
        assert_eq!(tail.offset(), LOG.len() as u64);
        assert_eq!(
            totals,
            EventTotals {
                table_files: 2,
                table_file_bytes: 1700,
                flushes: 1,
                compactions: 1,
                compaction_bytes: 700,
            }
        );
        assert_eq!(totals.flush_bytes(), 1000);
    }

    #[test]
    fn an_unfinished_line_is_left_for_later() {
        let mut totals = EventTotals::default();
        let mut tail = InfoLogTail::default();
        let cut = LOG.find("\"file_size\": 1000").unwrap();
        tail.read(&LOG.as_bytes()[..cut], &mut totals);
        assert_eq!(totals.table_files, 0);
        assert_eq!(tail.offset(), LOG[..cut].rfind('\n').unwrap() as u64 + 1);
        let offset = tail.offset() as usize;
        tail.read(&LOG.as_bytes()[offset..], &mut totals);
        assert_eq!(totals.table_file_bytes, 1700);
        tail.read(&[0xFF, 0xFE, b'\n'], &mut totals);
        tail.read(b"no newline", &mut totals);
        assert_eq!(totals.table_files, 2);
        assert_eq!(tail.offset(), LOG.len() as u64 + 3);
    }
}
