//! A recorded stream of GroveDB calls replayed on the two engines
//! (`lignin_testkit::diff::history`), with streams written by hand.

#![cfg(grovedb_engine_seam)]

use std::path::Path;

use lignin_testkit::{
    diff::history::{replay, report, Settings, Summary},
    golden::run::STACK_BYTES,
};

/// The options a batch of these streams is applied with: no validation, as
/// the recorded batches of a network are.
const OPTIONS: &str = "batch_apply_options=Some(BatchApplyOptions { \
                       validate_insertion_does_not_override: false, \
                       validate_insertion_does_not_override_tree: false, \
                       disable_operation_consistency_check: true, base_root_storage_is_free: true, \
                       batch_pause_height: None })";

/// A stream from its lines, each given without its first three fields. A
/// line that begins with `whole:` is written as the rest of it is.
fn stream(lines: &[String]) -> String {
    let mut text = "M\t0\t0\trecorder_open pid=1\n".to_string();
    for (number, line) in lines.iter().enumerate() {
        if let Some(whole) = line.strip_prefix("whole:") {
            text.push_str(whole);
            text.push('\n');
            continue;
        }
        let kind = if line.starts_with("request ") || line.starts_with("finalized ") {
            'M'
        } else {
            'C'
        };
        text.push_str(&format!("{kind}\t{}\t1\t{line}\n", number + 1));
    }
    text
}

fn mark_request(height: u64) -> String {
    format!("request block height={height} round=0 line=0")
}

fn mark_finalized(height: u64) -> String {
    format!("finalized height={height} round=0 state_app_hash=- grove_root=- chain_app_hash=-")
}

/// The first block: a tree `a`, a tree `a/b` with the item `a/b/c`, and
/// the item `a/d`, in one transaction.
fn first_block() -> Vec<String> {
    vec![
        mark_request(1),
        "tx_start\ttx=1\tkind=write\t|\tok".into(),
        "insert\tpath=\tkey=61\telement=020000\toptions=None\ttx=1\tgv=1\t|\tok\tv=()".into(),
        format!(
            "apply_batch\tops=3\top=/61|K62|InsertOrReplace|020000\top=/61/\
             62|K63|InsertOrReplace|000301020300\top=/61|K64|InsertOrReplace|00010900\t{OPTIONS}\ttx=1\tgv=1\t|\tok\tv=()"
        ),
        "tx_end\ttx=1\t|\tok".into(),
        "commit_transaction\ttx=1\t|\tok\tv=()".into(),
        "flush_pending_prefix_drops\tgv=1\t|\tok".into(),
        mark_finalized(1),
    ]
}

/// The summary of a replay of these lines, and what the replay and its
/// report wrote.
fn replayed(lines: &[String], keep_going: usize) -> (Summary, String) {
    replayed_with(lines, keep_going, |_| ())
}

/// The same, with settings a test changes.
fn replayed_with(
    lines: &[String],
    keep_going: usize,
    change: impl FnOnce(&mut Settings),
) -> (Summary, String) {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("stream.rec");
    std::fs::write(&path, stream(lines)).expect("the stream");
    let mut settings = Settings {
        stream: path,
        scratch: dir.path().to_path_buf(),
        keep_going,
        show_heights: vec![(0, u64::MAX)],
        ..Settings::default()
    };
    change(&mut settings);
    run_on_a_large_stack(move || {
        let mut out = Vec::new();
        let summary = replay(&settings, &mut out).expect("a replay");
        report(&summary, &mut out).expect("a report");
        (summary, String::from_utf8(out).expect("text"))
    })
}

fn run_on_a_large_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(body)
        .expect("a thread")
        .join()
        .expect("the replay does not panic")
}

#[test]
fn a_stream_is_answered_alike_and_the_store_is_checked() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        "get\tpath=/61/62\tkey=63\ttx=-\tgv=1\t|\tok".into(),
        "has_raw\tpath=/61\tkey=7a\ttx=-\tgv=1\t|\tok\tv=false".into(),
        "query_raw\tpath_query=range:/61\tallow_cache=true\tdecrease_limit_on_range_with_no_sub_elements=true\terror_if_intermediate_path_tree_not_present=true\tresult_type=QueryKeyElementPairResultType\ttx=-\tgv=1\t|\tok"
            .into(),
        "prove_query\tpath_query=range:/61/62\tprove_options=None\tgv=1\t|\tok".into(),
        "root_hash\ttx=-\tgv=1\t|\tok".into(),
        mark_finalized(2),
    ]);
    let (summary, report) = replayed(&lines, 1);
    assert!(summary.alike(), "{report}\n{summary:?}");
    assert_eq!(summary.blocks, 2);
    assert_eq!(summary.roots_alike, 2);
    assert_eq!(summary.replayed, 9, "{report}");
    assert!(summary.sound, "{}", summary.close);
    assert!(
        summary.not_replayed.is_empty(),
        "{:?}",
        summary.not_replayed
    );
    // The recorded value of the one read that names one is what the
    // reference answers.
    assert_eq!(summary.by_method["has_raw"].other_outcome, 0);
    assert!(report.contains("`prove_query`"), "{report}");
}

#[test]
fn a_write_with_no_transaction_beside_an_open_one_ends_its_commit_with_a_conflict_on_both() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        "tx_start\ttx=2\tkind=write\t|\tok".into(),
        "insert\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=2\tgv=1\t|\tok".into(),
        // GroveDB starts a transaction of its own for a call that is given
        // none: the recorder numbers it, and no call uses the number.
        "tx_start\ttx=3\tkind=write\t|\tok".into(),
        "tx_end\ttx=3\t|\tok".into(),
        "insert\tpath=/61\tkey=71\telement=00010200\toptions=None\ttx=-\tgv=1\t|\tok".into(),
        "tx_end\ttx=2\t|\tok".into(),
        "commit_transaction\ttx=2\t|\terr=StorageError(…)".into(),
        mark_finalized(2),
        mark_request(3),
        "has_raw\tpath=/61\tkey=70\ttx=-\tgv=1\t|\tok\tv=false".into(),
        "has_raw\tpath=/61\tkey=71\ttx=-\tgv=1\t|\tok\tv=true".into(),
        mark_finalized(3),
    ]);
    let (summary, report) = replayed(&lines, 1);
    assert!(summary.alike(), "{report}\n{summary:?}");
    let commits = &summary.by_method["commit_transaction"];
    assert_eq!(
        (commits.calls, commits.alike, commits.other_outcome),
        (2, 2, 0),
        "{report}"
    );
    assert!(report.contains("error Commit::Conflict"), "{report}");
    // What the transaction wrote is gone, and what was written beside it
    // stands, on both.
    assert_eq!(summary.by_method["has_raw"].other_outcome, 0, "{report}");
}

/// A difference of the engines, of the kind `docs/lignindb/engine.md`
/// states for a clear that is told to look for no tree ("A subtree leaves
/// with its element"): RocksDB keeps the records of a tree that a batch
/// removes with a plain delete while it holds an element, and a read that
/// goes by the tree's prefix and asks for no element on the way finds
/// them. LigninDB drops the subtree with its element. The replay has to
/// report the read.
#[test]
fn a_read_by_the_prefix_of_a_tree_removed_with_an_element_in_it_is_reported() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        "tx_start\ttx=2\tkind=write\t|\tok".into(),
        format!(
            "apply_batch\tops=1\top=/61|K62|DeleteDontCheckForBackwardsReferences\t{OPTIONS}\ttx=2\tgv=1\t|\tok\tv=()"
        ),
        "tx_end\ttx=2\t|\tok".into(),
        "commit_transaction\ttx=2\t|\tok\tv=()".into(),
        mark_finalized(2),
        mark_request(3),
        "get_raw_optional\tpath=/61\tkey=62\ttx=-\tgv=1\t|\tok\tv=None".into(),
        "has_raw\tpath=/61/62\tkey=63\ttx=-\tgv=1\t|\tok".into(),
        "has_raw\tpath=/61\tkey=64\ttx=-\tgv=1\t|\tok\tv=true".into(),
        mark_finalized(3),
    ]);
    let (summary, report) = replayed(&lines, 10);
    // The delete itself, its commit and the root hashes are alike.
    assert_eq!(summary.roots_alike, 3, "{report}");
    assert_eq!(summary.by_method["apply_batch"].alike, 2, "{report}");
    assert_eq!(summary.divergences.len(), 1, "{report}");
    assert!(summary.divergences[0].contains("`has_raw`"), "{report}");
    assert!(
        summary.divergences[0].contains("reference ok true"),
        "{report}"
    );
    assert!(
        summary.divergences[0].contains("candidate ok false"),
        "{report}"
    );
    assert!(!summary.alike());
    assert!(summary.sound, "{}", summary.close);
    assert!(report.contains("DIFFERS height 3"), "{report}");
}

#[test]
fn a_write_that_cannot_be_made_ends_the_replay() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        "ingest_subtree_sst\tcf_name=default\tpath=/61\ttx=-\t|\tok".into(),
        mark_finalized(2),
    ]);
    let (summary, report) = replayed(&lines, 1);
    assert!(!summary.alike(), "{report}");
    let why = summary.ended_early.expect("it ended early");
    assert!(
        why.contains("ingest_subtree_sst") && why.contains("is not made by this replay"),
        "{why}"
    );
    assert_eq!(summary.blocks, 1);
}

/// What a stream may have that is not a call this replay can make as it
/// was recorded. Each ends the replay where it stands: the line is not
/// counted as made, no call after it is made, and the replay does not come
/// out alike.
#[test]
fn a_line_that_cannot_be_made_as_it_was_recorded_ends_the_replay() {
    let batch = |fields: &str| format!("apply_batch\t{fields}\t{OPTIONS}\ttx=-\tgv=1\t|\tok");
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a line cut short",
            "whole:C\t9\t1".into(),
            "fewer than four fields",
        ),
        (
            "a call with no result",
            "insert\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=-\tgv=1".into(),
            "a call with no result",
        ),
        (
            "a transaction that is no number",
            "insert\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=7x\tgv=1\t|\tok".into(),
            "a transaction that is no number",
        ),
        (
            "a batch with fewer operations than it names",
            batch("ops=2\top=/61|K70|InsertOrReplace|00010100"),
            "names 2 operations and lists 1",
        ),
        (
            "an argument nothing reads",
            "insert\tpath=/61\tkey=70\telement=00010100\toptions=None\tnew_option=true\ttx=-\tgv=1\t|\tok".into(),
            "an argument `new_option` that this replay does not read",
        ),
        (
            "a method in none of the lists",
            "replace_everything\tpath=/61\ttx=-\tgv=1\t|\tok".into(),
            "is no method this replay knows",
        ),
        (
            "a method that changes a database and is not made",
            "commit_session\tsession=?\tgv=1\t|\tok".into(),
            "the write `commit_session` is not made",
        ),
        (
            "a call made inside a callback",
            "insert\tcb\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=-\tgv=1\t|\tok".into(),
            "inside a callback",
        ),
        (
            "a transaction on a method that takes none",
            "prove_query\tpath_query=range:/61\tprove_options=None\ttx=1\tgv=1\t|\tok".into(),
            "takes no transaction",
        ),
        (
            "a start of a transaction with an argument nothing reads",
            "tx_start\ttx=9\tkind=write\tnew_option=true\t|\tok".into(),
            "an argument `new_option` that this replay does not read",
        ),
        (
            "an end of a transaction with an argument nothing reads",
            "tx_end\ttx=9\tnew_option=true\t|\tok".into(),
            "an argument `new_option` that this replay does not read",
        ),
        (
            "a start of a transaction of no kind",
            "tx_start\ttx=9\t|\tok".into(),
            "neither `write` nor `snapshot`",
        ),
        (
            "a start of a transaction of a kind that is not known",
            "tx_start\ttx=9\tkind=other\t|\tok".into(),
            "neither `write` nor `snapshot`",
        ),
        (
            "a start of a transaction with no number",
            "tx_start\ttx=-\tkind=write\t|\tok".into(),
            "names no transaction",
        ),
        (
            "a call of a second thread",
            "whole:C\t9\t2\tinsert\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=-\tgv=1\t|\tok".into(),
            "two threads",
        ),
    ];
    for (what, line, why) in cases {
        let mut lines = first_block();
        lines.extend([
            mark_request(2),
            line,
            "has_raw\tpath=/61\tkey=70\ttx=-\tgv=1\t|\tok\tv=true".into(),
            mark_finalized(2),
        ]);
        let (summary, report) = replayed(&lines, 1);
        assert!(!summary.alike(), "{what}: {report}");
        let ended = summary
            .ended_early
            .unwrap_or_else(|| panic!("{what}: it went on\n{report}"));
        assert!(ended.contains(why), "{what}: {ended}");
        assert_eq!(summary.blocks, 1, "{what}");
        assert!(
            !summary.by_method.contains_key("has_raw"),
            "{what}: the call after it was made"
        );
        assert_eq!(
            summary.by_method.get("insert").map(|insert| insert.calls),
            Some(1),
            "{what}: the line was counted as made"
        );
        assert_eq!(summary.by_method["apply_batch"].calls, 1, "{what}");
    }

    let (summary, _) = replayed(&[mark_request(1), mark_finalized(1)], 1);
    let ended = summary.ended_early.clone().expect("it ended early");
    assert!(
        ended.contains("before any call named a grove version"),
        "{ended}"
    );
    assert!(!summary.alike());
    assert_eq!(summary.roots_alike, 0);
}

#[test]
fn a_call_made_with_a_stand_in_and_a_read_that_is_not_made_are_counted_apart() {
    let long = format!("#{}:300", "ab".repeat(16));
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        format!("put_aux\tkey=6b\tvalue={long}\tcost_info=None\ttx=-\t|\tok\tv=()"),
        "put_aux\tkey=6c\tvalue=0102\tcost_info=None\ttx=-\t|\tok\tv=()".into(),
        "get_aux\tkey=6b\ttx=-\t|\tok\tv=Some([1, 2, 3])".into(),
        "get_aux\tkey=6c\ttx=-\t|\tok\tv=Some([1, 2])".into(),
        "get_aux\tkey=6d\ttx=-\t|\tok\tv=Some([1, 2])".into(),
        "get_aux_by_key_prefix\tkey_prefix=6b\ttx=-\t|\tok\tv=[([107], [1, 2, 3])]".into(),
        "get_aux_by_key_prefix\tkey_prefix=6c\ttx=-\t|\tok\tv=[([108], [1, 2])]".into(),
        "delete_operation_for_delete_internal\tpath=/61\tkey=64\toptions=DeleteOptions { \
         allow_deleting_non_empty_trees: false, deleting_non_empty_trees_returns_error: true, \
         base_root_storage_is_free: true, validate_tree_at_path_exists: false, \
         backwards_references: DontCheck }\tis_known_to_be_subtree=None\t\
         current_batch_operations=?\ttx=-\tgv=1\t|\tok\tv=?"
            .into(),
        "root_key\tpath=/61\ttx=-\tgv=1\t|\tok".into(),
        mark_finalized(2),
    ]);
    let (summary, report) = replayed(&lines, 1);
    assert!(summary.alike(), "{report}\n{summary:?}");
    assert_eq!(summary.stand_ins, 2, "{report}");
    let puts = &summary.by_method["put_aux"];
    assert_eq!(
        (puts.calls, puts.alike, puts.stand_in),
        (2, 2, 1),
        "{report}"
    );
    assert_eq!(
        (puts.other_outcome, puts.outcome_not_held),
        (0, 0),
        "{report}"
    );
    let read = &summary.by_method["get_aux"];
    assert_eq!(
        (read.calls, read.outcome_not_held, read.other_outcome),
        (3, 1, 1),
        "the value under the key written with a stand-in is not held against the recording, \
         the value under the other key that was written is the recorded one, and the key \
         that was never written has none: {report}"
    );
    let by_prefix = &summary.by_method["get_aux_by_key_prefix"];
    assert_eq!(
        (
            by_prefix.calls,
            by_prefix.outcome_not_held,
            by_prefix.other_outcome
        ),
        (2, 1, 0),
        "of two reads by a prefix, the one that reads the stand-in is not held against the \
         recording and the other answers what was recorded: {report}"
    );
    let planned = &summary.by_method["delete_operation_for_delete_internal"];
    assert_eq!(
        (
            planned.stand_in,
            planned.outcome_not_held,
            planned.cost_not_held
        ),
        (1, 1, 1),
        "{report}"
    );
    assert_eq!(summary.not_replayed["root_key"].0, 1, "{report}");
    assert!(
        report.contains("2 with a stand-in for an argument"),
        "{report}"
    );
    assert!(report.contains("stand-in: put_aux x1"), "{report}");
    assert!(report.contains("not made: root_key x1"), "{report}");
}

/// A batch that fails on both engines before it comes to a callback the
/// recording has: the callback that was not asked for is noted and counted,
/// and the two engines are alike in it.
#[test]
fn the_recorded_callbacks_a_failed_batch_did_not_come_to_are_noted() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        format!(
            "apply_batch_with_element_flags_update\tops=1\top=/7a|K61|InsertOrReplace|00010100\t{OPTIONS}\ttx=-\tgv=1\tuf=4,0,N;-;0001;0002;1\t|\tok"
        ),
        mark_finalized(2),
    ]);
    let (summary, report) = replayed(&lines, 1);
    assert!(summary.alike(), "{report}");
    let batch = &summary.by_method["apply_batch_with_element_flags_update"];
    assert_eq!((batch.calls, batch.alike), (1, 1), "{report}");
    assert_eq!(
        batch.other_outcome, 1,
        "the recording has it succeed: {report}"
    );
    assert_eq!(summary.callbacks_asked_otherwise, 1, "{report}");
    assert!(report.contains("reference: error "), "{report}");
    assert!(
        report.contains(
            "1 flag merges, 0 removal splits and 0 add-ons of the recording node were not asked for"
        ),
        "{report}"
    );
}

/// A tree that keeps flat records, as a recorded history has one: it is made,
/// two batches append to it, and its anchor, its count and a range of it
/// are read between them, with a transaction and with none. Every call is
/// made on both engines and answered alike. The two appends are operations
/// a caller recorded.
#[test]
fn the_appends_and_the_reads_of_a_commitment_tree_are_made() {
    const FIRST: [&str; 4] = [
        "c737cc07007de7faf11102921f72cc2aa78c764084b77b67bd5ba97bc5736133",
        "7f826385e888a51a3eb12cf9bd6ffc77d75af749987fa06db37c62e6d173f501",
        "e0c3cd0f3726ee12d3a869e642f81aec17a98eb55e54751f661d20460a513c8b",
        "9420ddef26bcbbc20c4f5208b61d62fe94461798de7889e59da0184e53d54a04affb67cfe60ca3a2af51dfb354341db2cd74f2d9a6fbd6a6912e97fcebd9c7c324a3243e45b8b68352eb89b4e168772bbc61f597fc483657b816e73ad90ffe1c8131d542a3769906b77c4e4221394d8fdff2b3bfb77db6a26f5ded339348437e40ab4844f4ddd90242a89e587d6caf6a68f18b9bc6263b17df8b21e24c528148c47f296c66c7c39fdd41b2e42573ae16a4c5ad453b18fe7a3b09a66904f4a9380ff057cf7235d4c0e6a8551a96927877b9a0dcf8b8e62836",
    ];
    const SECOND: [&str; 4] = [
        "c7c1f328c84c98d1ddca83b38f8ad3737736ab74c0b4f2abc32c19b43c26e424",
        "7a5c42e9ed99f9910ef447527928032c77502edfa1a1886927ccd59ffd98a93d",
        "063ffff21e739fef33715efe9bd234ea9f37c9aea287345c9e24b7173021b93e",
        "cdbd0fef8e22d27193e0bc568247ee4063afc4f9bdce84e0621ae801bac1b6a31d114e067b732283fdf55d8f119a39891463c9c9747535209b2614f86dba38b5724cf700bf69601fcb1c6ae14b8ee8b3dd5f04eeea66caa69ef70b9b0a63c013c520f5c2d183fa6ef9c0a8387a4c0882830da6edf4e5c6be49cc78948a9000875bab70baa78dd065740740a78fbccf3e64680918559906e7690e8b7a4a359cdbd51b6a875da93fc2c641617482a68f64339726c6ae5433a951ec56feea840e751e34925914a725bbf47da814e93a343e3f379f140beff509",
    ];
    let append = |fields: &[&str; 4], tx: u64| {
        format!(
            "apply_batch_with_element_flags_update\tops=1\top=/34/4d/80|-|CommitmentTreeInsert|{}\t{OPTIONS}\tcallbacks=storage-flags\ttx={tx}\tgv=3\t|\tok\tv=()",
            fields.join("|")
        )
    };
    let reads = |tx: &str, count: u64| {
        vec![
            format!("commitment_tree_anchor\tpath=/34/4d\tkey=80\ttx={tx}\tgv=3\t|\tok"),
            format!(
                "commitment_tree_count\tpath=/34/4d\tkey=80\ttx={tx}\tgv=3\t|\tok\tv={count}"
            ),
            format!(
                "commitment_tree_get_range\tpath=/34/4d\tkey=80\tstart=0\tlimit=2048\ttx={tx}\tgv=3\t|\tok"
            ),
        ]
    };
    let mut lines = vec![
        mark_request(1),
        "tx_start\ttx=1\tkind=write\t|\tok".into(),
        "insert_if_not_exists\tpath=\tkey=34\telement=04000000\ttx=1\tgv=3\t|\tok\tv=true".into(),
        "insert_if_not_exists\tpath=/34\tkey=4d\telement=04000000\ttx=1\tgv=3\t|\tok\tv=true"
            .into(),
        "insert_if_not_exists\tpath=/34/4d\tkey=80\telement=0b000b00\ttx=1\tgv=3\t|\tok\tv=true"
            .into(),
    ];
    lines.extend(reads("1", 0));
    lines.push(append(&FIRST, 1));
    lines.extend(reads("1", 1));
    lines.extend([
        "tx_end\ttx=1\t|\tok".into(),
        "commit_transaction\ttx=1\t|\tok\tv=()".into(),
        mark_finalized(1),
        mark_request(2),
    ]);
    lines.extend(reads("-", 1));
    lines.extend([
        "tx_start\ttx=2\tkind=write\t|\tok".into(),
        append(&SECOND, 2),
    ]);
    lines.extend(reads("2", 2));
    lines.extend([
        "tx_end\ttx=2\t|\tok".into(),
        "commit_transaction\ttx=2\t|\tok\tv=()".into(),
        mark_finalized(2),
        mark_request(3),
    ]);
    lines.extend(reads("-", 2));
    lines.push(mark_finalized(3));

    let (summary, report) = replayed(&lines, 1);
    assert!(summary.alike(), "{report}\n{summary:?}");
    assert!(summary.not_replayed.is_empty(), "{report}");
    assert_eq!(summary.roots_alike, 3, "{report}");
    let batches = &summary.by_method["apply_batch_with_element_flags_update"];
    assert_eq!(
        (batches.calls, batches.alike, batches.other_outcome),
        (2, 2, 0),
        "{report}"
    );
    for method in [
        "commitment_tree_anchor",
        "commitment_tree_count",
        "commitment_tree_get_range",
    ] {
        let read = &summary.by_method[method];
        assert_eq!((read.calls, read.alike), (5, 5), "{method}: {report}");
    }
    assert_eq!(
        summary.by_method["commitment_tree_count"].other_outcome, 0,
        "the count is what the stream says after each append: {report}"
    );
    assert!(summary.sound, "{}", summary.close);
}

/// Indexed trees as a recorded history has them on grove version 4: batches
/// make the three kinds and put their children, and the children leave by
/// the dedicated deletes, one of them after a flat drop of the tree under
/// it, with the drain after the commit. Every call is made on both engines
/// and answered alike, and each delete answers what the stream says.
#[test]
fn the_dedicated_deletes_of_an_indexed_tree_are_made() {
    let put = |path: &str, key: &str, element: &str| {
        format!("op={path}|K{key}|InsertOrReplaceDontCheckForBackwardsReferences|{element}")
    };
    let batch = |ops: &[String], tx: u64| {
        format!(
            "apply_batch_with_element_flags_update\tops={}\t{}\t{OPTIONS}\tcallbacks=storage-flags\ttx={tx}\tgv=4\t|\tok\tv=()",
            ops.len(),
            ops.join("\t")
        )
    };
    let delete = |method: &str, path: &str, key: &str, tx: &str, removed: bool| {
        format!("{method}\tpath={path}\titem_key={key}\ttx={tx}\tgv=4\t|\tok\tv={removed}")
    };
    let has = |path: &str, key: &str, there: bool| {
        format!("has_raw\tpath={path}\tkey={key}\ttx=-\tgv=4\t|\tok\tv={there}")
    };
    const BY_COUNT: &str = "delete_from_count_indexed_tree";
    const BY_SUM: &str = "delete_from_provable_sum_indexed_tree";
    const BY_COUNT_AND_SUM: &str = "delete_from_provable_count_provable_sum_indexed_tree";
    let lines = vec![
        mark_request(1),
        "tx_start\ttx=1\tkind=write\t|\tok".into(),
        "insert\tpath=\tkey=61\telement=020000\toptions=None\ttx=1\tgv=4\t|\tok\tv=()".into(),
        // An empty tree indexed by count, one by sum and one by both.
        batch(
            &[
                put("/61", "69", "1600000000"),
                put("/61", "73", "1500000000"),
                put("/61", "63", "1700000001010000"),
            ],
            1,
        ),
        // Two children of each: count trees, of which the second holds a
        // count tree, sum trees and sum items.
        batch(
            &[
                put("/61/69", "6b31", "06000000"),
                put("/61/69", "6b32", "06000000"),
                put("/61/69/6b32", "00", "06000000"),
                put("/61/73", "6b31", "04000000"),
                put("/61/73", "6b32", "04000000"),
                put("/61/63", "6b31", "030000"),
                put("/61/63", "6b32", "030000"),
            ],
            1,
        ),
        "tx_end\ttx=1\t|\tok".into(),
        "commit_transaction\ttx=1\t|\tok\tv=()".into(),
        "flush_pending_prefix_drops\tgv=4\t|\tok".into(),
        mark_finalized(1),
        mark_request(2),
        "tx_start\ttx=2\tkind=write\t|\tok".into(),
        delete(BY_COUNT, "/61/69", "6b31", "2", true),
        delete(BY_SUM, "/61/73", "6b31", "2", true),
        delete(BY_COUNT_AND_SUM, "/61/63", "6b31", "2", true),
        "reconcile_indexed_tree_secondaries\tpath=/61/69\ttx=2\tgv=4\t|\tok\tv=()".into(),
        "tx_end\ttx=2\t|\tok".into(),
        "commit_transaction\ttx=2\t|\tok\tv=()".into(),
        "flush_pending_prefix_drops\tgv=4\t|\tok".into(),
        mark_finalized(2),
        mark_request(3),
        has("/61/69", "6b31", false),
        has("/61/69", "6b32", true),
        has("/61/73", "6b31", false),
        has("/61/73", "6b32", true),
        has("/61/63", "6b31", false),
        has("/61/63", "6b32", true),
        // A child that is gone, with no transaction.
        delete(BY_COUNT, "/61/69", "6b31", "-", false),
        delete(BY_SUM, "/61/73", "6b31", "-", false),
        delete(BY_COUNT_AND_SUM, "/61/63", "6b31", "-", false),
        "reconcile_indexed_tree_secondaries\tpath=/61/73\ttx=-\tgv=4\t|\tok\tv=()".into(),
        "reconcile_indexed_tree_secondaries\tpath=/61/63\ttx=-\tgv=4\t|\tok\tv=()".into(),
        // The child that holds a tree: the tree under it is dropped flat,
        // then the child is deleted, and the drain follows the commit.
        "tx_start\ttx=3\tkind=write\t|\tok".into(),
        "drop_flat_subtree\tpath=/61/69/6b32\tkey=00\tbackwards_references=DontCheck\ttx=3\tgv=4\t|\tok\tv=()".into(),
        delete(BY_COUNT, "/61/69", "6b32", "3", true),
        "tx_end\ttx=3\t|\tok".into(),
        "commit_transaction\ttx=3\t|\tok\tv=()".into(),
        "flush_pending_prefix_drops\tgv=4\t|\tok".into(),
        mark_finalized(3),
        mark_request(4),
        has("/61/69", "6b32", false),
        has("/61", "69", true),
        mark_finalized(4),
    ];

    let (summary, report) = replayed(&lines, 1);
    assert!(summary.alike(), "{report}\n{summary:?}");
    assert!(summary.not_replayed.is_empty(), "{report}");
    assert_eq!(summary.roots_alike, 4, "{report}");
    for (method, calls) in [
        (BY_COUNT, 3),
        (BY_SUM, 2),
        (BY_COUNT_AND_SUM, 2),
        ("reconcile_indexed_tree_secondaries", 3),
        ("drop_flat_subtree", 1),
        ("has_raw", 8),
        ("apply_batch_with_element_flags_update", 2),
        ("commit_transaction", 3),
    ] {
        let made = &summary.by_method[method];
        assert_eq!(
            (made.calls, made.alike, made.other_outcome),
            (calls, calls, 0),
            "{method}: every call alike and answered as the stream says: {report}"
        );
    }
    assert!(summary.sound, "{}", summary.close);
}

/// The proof of a query is held against the recorded one as the recorder
/// wrote it: the `Debug` text of its bytes, by a hash where it is long.
#[test]
fn a_proof_is_held_against_the_recorded_one() {
    let lines_with = |recorded: &str| {
        let mut lines = first_block();
        lines.extend([
            mark_request(2),
            format!("prove_query\tpath_query=range:/61/62\tprove_options=None\tgv=1\t|\tok\tv={recorded}"),
            mark_finalized(2),
        ]);
        lines
    };
    let (summary, report) = replayed(&lines_with("[1, 2, 3]"), 1);
    assert!(summary.alike(), "{report}");
    let proofs = &summary.by_method["prove_query"];
    assert_eq!((proofs.calls, proofs.other_outcome), (1, 1), "{report}");
    let (_, _, what) = proofs.first_other.as_ref().expect("the first");
    let written = what
        .split("(its value as the recorder writes one: ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .unwrap_or_else(|| panic!("{what}"));
    assert!(written.starts_with('#'), "a proof is long: {written}");

    let (summary, report) = replayed(&lines_with(written), 1);
    let proofs = &summary.by_method["prove_query"];
    assert_eq!(
        (proofs.calls, proofs.other_outcome, proofs.outcome_not_held),
        (1, 0, 0),
        "{report}"
    );
}

#[test]
fn the_root_hash_after_a_block_is_held_against_the_recorded_one() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        "insert\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=-\tgv=1\t|\tok".into(),
        mark_finalized(2),
    ]);
    let (summary, report) = replayed(&lines, 1);
    assert!(summary.alike(), "{report}");
    assert_eq!(
        (
            summary.roots_alike,
            summary.roots_not_recorded,
            summary.roots_as_recorded
        ),
        (2, 2, 0)
    );
    let root_at = |height: u64| {
        let line = report
            .lines()
            .find(|line| line.starts_with(&format!("height {height} finalized: ")))
            .expect("the line of the block");
        let hash = line
            .split("root hash reference ")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .expect("the hash");
        assert_eq!(hash.len(), 64, "{line}");
        hash.to_string()
    };
    let (first, second) = (root_at(1), root_at(2));
    assert_ne!(first, second);

    // The same stream with the first block's root recorded as it is and the
    // second block's recorded as the first's.
    let finalized = lines
        .iter()
        .filter(|line| line.starts_with("finalized "))
        .count();
    assert_eq!(finalized, 2);
    for line in lines
        .iter_mut()
        .filter(|line| line.starts_with("finalized "))
    {
        *line = line.replace("grove_root=-", &format!("grove_root={first}"));
    }
    let (summary, report) = replayed(&lines, 1);
    assert!(
        summary.alike(),
        "a root that is not the recorded one is no divergence: {report}"
    );
    assert_eq!(
        (
            summary.roots_alike,
            summary.roots_not_recorded,
            summary.roots_as_recorded
        ),
        (2, 0, 1),
        "{report}"
    );
    assert_eq!(summary.roots_not_as_recorded, vec![2]);
}

/// The databases a replay leaves after a block are gone on from by a later
/// replay of the same stream: it makes no call of the blocks before, checks
/// that both databases have the root hash the block was recorded with, and
/// comes to the root hashes a replay from the first block comes to.
/// Databases that are at the end of another block are refused.
#[test]
fn a_replay_goes_on_from_the_databases_another_left_after_a_block() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        "insert\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=-\tgv=1\t|\tok".into(),
        mark_finalized(2),
        mark_request(3),
        "insert\tpath=/61\tkey=71\telement=00010200\toptions=None\ttx=-\tgv=1\t|\tok".into(),
        "has_raw\tpath=/61\tkey=70\ttx=-\tgv=1\t|\tok\tv=true".into(),
        mark_finalized(3),
    ]);
    // The root hash after each block, from a replay of the whole stream.
    let (whole, report) = replayed(&lines, 1);
    assert!(whole.alike(), "{report}");
    let finalized = lines
        .iter_mut()
        .filter(|line| line.starts_with("finalized "));
    for (block, line) in finalized.enumerate() {
        let height = block + 1;
        let root = report
            .lines()
            .find(|line| line.starts_with(&format!("height {height} finalized: ")))
            .and_then(|line| line.split("root hash reference ").nth(1))
            .and_then(|rest| rest.split(',').next())
            .expect("the root hash of the block");
        *line = line.replace("grove_root=-", &format!("grove_root={root}"));
    }

    let kept = tempfile::tempdir().expect("a directory");
    let stores = kept.path().join("stores");
    let at = stores.clone();
    let (first, report) = replayed_with(&lines, 1, |settings| {
        settings.stores = Some(at);
        settings.stop_height = Some(1);
    });
    assert!(first.alike(), "{report}");
    assert_eq!((first.blocks, first.resumed_after), (1, None));
    assert!(stores.join("reference").exists() && stores.join("candidate").exists());

    // From the first block again, over databases that are there: refused.
    let at = stores.clone();
    let again = std::panic::catch_unwind(|| {
        replayed_with(&first_block(), 1, |settings| settings.stores = Some(at))
    });
    assert!(
        again.is_err(),
        "a replay from the first block took kept databases"
    );

    // After block 2, where the databases are at the end of block 1: refused,
    // with no call made.
    let at = stores.clone();
    let (wrong, report) = replayed_with(&lines, 1, |settings| {
        settings.stores = Some(at);
        settings.resume_after = Some(2);
    });
    assert!(!wrong.alike(), "{report}");
    let why = wrong.ended_early.expect("it ended early");
    assert!(why.contains("not at the end of height 2"), "{why}");
    assert_eq!((wrong.replayed, wrong.blocks), (0, 0), "{report}");

    // After block 1: the two blocks after it, with the root hashes of the
    // whole replay.
    let at = stores.clone();
    let (rest, report) = replayed_with(&lines, 1, |settings| {
        settings.stores = Some(at);
        settings.resume_after = Some(1);
    });
    assert!(rest.alike(), "{report}");
    assert_eq!(rest.resumed_after, Some(1), "{report}");
    assert_eq!(
        rest.before_the_resume, first.calls,
        "the calls of the first block, and the two steps of its transaction, are not made"
    );
    assert_eq!(
        (rest.blocks, rest.roots_alike, rest.roots_as_recorded),
        (2, 2, 2),
        "{report}"
    );
    assert_eq!(rest.replayed, whole.replayed - first.replayed, "{report}");
    assert_eq!(rest.by_method["has_raw"].other_outcome, 0, "{report}");
    assert!(rest.sound, "{}", rest.close);
    assert!(report.contains("went on after height 1"), "{report}");
}

#[test]
fn a_stream_that_ends_inside_a_block_says_what_was_left_open() {
    let mut lines = first_block();
    lines.extend([
        mark_request(2),
        "tx_start\ttx=2\tkind=write\t|\tok".into(),
        "insert\tpath=/61\tkey=70\telement=00010100\toptions=None\ttx=2\tgv=1\t|\tok".into(),
    ]);
    let (summary, text) = replayed(&lines, 1);
    assert_eq!(summary.blocks, 1);
    assert_eq!(
        (summary.calls_after_last_block, summary.open_at_end),
        (1, 1)
    );
    assert!(
        text.contains("after the last finalized block: 1 calls made, 1 transactions open"),
        "{text}"
    );
}

#[test]
fn the_binary_says_how_it_is_called() {
    let binary = Path::new(env!("CARGO_BIN_EXE_lignin-history"));
    let output = std::process::Command::new(binary)
        .output()
        .expect("it runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: lignin-history STREAM"));
}
