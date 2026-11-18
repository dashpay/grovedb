//! `ENOSPC` and `EIO`, injected by the simulated file system.
//!
//! A step that meets one fails with the injected error inside a
//! [`PageTableError`], and the directory keeps a whole checkpoint that its
//! manifest names: the one it had, or the new one where the failure came
//! once the new manifest was in place.

use std::collections::BTreeMap;

use lignin_io::sim::{Fault, FaultRule, OpKind};

use super::{
    crash::TABLE_LIFE,
    model::{check_recovered, Run, Step},
    *,
};
use crate::segment::tests::sim::{
    crash_states, failed_ops, sim_fs, traced, OnFailure, SOME_STATES,
};

/// How the disk fails, from one operation on.
#[derive(Clone, Copy, Debug)]
enum Disk {
    /// One operation fails. If it is an append, the file layer takes it back.
    FailsOnce(Fault),
    /// One append fails, and the file layer cannot take it back.
    TearsAnAppend(Fault),
    /// Every operation that needs space fails.
    IsFull,
}

impl Disk {
    fn fault(self) -> Fault {
        match self {
            Self::FailsOnce(fault) | Self::TearsAnAppend(fault) => fault,
            Self::IsFull => Fault::Enospc,
        }
    }

    fn rule(self, from_op: u64) -> FaultRule {
        let rule = FaultRule::new(self.fault()).from_op(from_op);
        match self {
            Self::FailsOnce(_) => rule.torn_append(false),
            Self::TearsAnAppend(_) => rule.on(OpKind::Append).torn_append(true),
            Self::IsFull => rule.forever().torn_append(false),
        }
    }
}

/// Runs `script` once for every operation of it that `disk` can fail, with
/// the disk failing from that operation on, and checks what the failure
/// leaves: after a crash, and after the process has ended. Returns how often
/// each kind of operation was failed, in a file of the checkpoints and in
/// any other.
fn fail_everywhere(seed: u64, disk: Disk, script: &[Step]) -> BTreeMap<(OpKind, bool), usize> {
    let mut failed_kinds = BTreeMap::new();
    let mut from_op = 1;
    loop {
        let fs = sim_fs(traced(seed, 512));
        fs.add_rule(disk.rule(from_op));
        let mut run = Run::new(&fs, seed);
        let result = run.script(script);
        let Some(failed) = failed_ops(&fs).into_iter().next() else {
            result.expect("the workload runs when nothing fails");
            break;
        };
        let context = || format!("seed {seed}, {disk:?}: {failed}");
        let _context = OnFailure(&context);
        let error = result.expect_err("the step that met the fault fails");
        assert_eq!(
            injected_op(&error),
            Some((disk.fault(), failed.op)),
            "{error}"
        );

        let image = fs.crash_image();
        for (plan, state) in crash_states(&image, &SOME_STATES).states {
            let context = || format!("{image:?}\n{plan:?}");
            let _context = OnFailure(&context);
            check_recovered(&state, &run.model);
        }

        run.end_session();
        fs.clear_rules();
        check_recovered(&fs, &run.model);

        let in_table = failed.path.starts_with("/db/table");
        *failed_kinds.entry((failed.kind, in_table)).or_insert(0) += 1;
        from_op = failed.op + 1;
    }
    failed_kinds
}

#[test]
fn a_fault_at_any_operation_fails_the_step_and_leaves_a_whole_checkpoint() {
    let every_kind = [
        OpKind::Open,
        OpKind::Rename,
        OpKind::RemoveFile,
        OpKind::CreateDir,
        OpKind::ReadDir,
        OpKind::SyncDir,
        OpKind::Read,
        OpKind::Write,
        OpKind::Append,
        OpKind::SyncData,
        OpKind::SyncAll,
        OpKind::Len,
    ];
    let of_a_checkpoint = [
        OpKind::Open,
        OpKind::RemoveFile,
        OpKind::CreateDir,
        OpKind::ReadDir,
        OpKind::SyncDir,
        OpKind::Read,
        OpKind::Append,
        OpKind::SyncData,
        OpKind::Len,
    ];
    let needs_space = [
        OpKind::Open,
        OpKind::CreateDir,
        OpKind::SyncDir,
        OpKind::Append,
        OpKind::SyncData,
    ];
    let cases: [(Disk, &[OpKind], &[OpKind]); 5] = [
        (Disk::FailsOnce(Fault::Eio), &every_kind, &of_a_checkpoint),
        (
            Disk::FailsOnce(Fault::Enospc),
            &every_kind[..0],
            &needs_space,
        ),
        (Disk::IsFull, &every_kind[..0], &needs_space),
        (
            Disk::TearsAnAppend(Fault::Eio),
            &[OpKind::Append],
            &[OpKind::Append],
        ),
        (
            Disk::TearsAnAppend(Fault::Enospc),
            &[OpKind::Append],
            &[OpKind::Append],
        ),
    ];
    for (disk, kinds, kinds_in_table) in cases {
        let failed = fail_everywhere(51, disk, &TABLE_LIFE);
        println!("{disk:?}: {failed:?}");
        let of = |in_table: bool| -> Vec<OpKind> {
            let mut kinds: Vec<OpKind> = failed
                .keys()
                .filter(|(_, table)| *table == in_table)
                .map(|(kind, _)| *kind)
                .collect();
            kinds.dedup();
            kinds
        };
        let mut all = of(false);
        all.extend(of(true));
        all.sort();
        all.dedup();
        if !kinds.is_empty() {
            assert_eq!(all, kinds);
        }
        assert_eq!(of(true), kinds_in_table);
    }
}

#[test]
fn a_session_goes_on_after_a_fault_in_any_operation() {
    let seed = 53;
    for disk in [
        Disk::FailsOnce(Fault::Eio),
        Disk::FailsOnce(Fault::Enospc),
        Disk::TearsAnAppend(Fault::Eio),
        Disk::TearsAnAppend(Fault::Enospc),
    ] {
        let (mut faults, mut went_on_in_session) = (0, 0);
        let mut from_op = 1;
        loop {
            let fs = sim_fs(traced(seed, 512));
            fs.add_rule(disk.rule(from_op));
            let mut run = Run::new(&fs, seed);
            let mut failures = 0;
            for step in TABLE_LIFE {
                let mut attempts = 0;
                while let Err(error) = run.take(step) {
                    let context =
                        || format!("{disk:?} from operation {from_op}: {step:?}: {error}");
                    let _context = OnFailure(&context);
                    assert!(injected_op(&error).is_some());
                    attempts += 1;
                    assert!(attempts < 3, "the step keeps failing");
                    failures += 1;
                    let in_session = run.settle(step, &error).expect("the session settles");
                    went_on_in_session += usize::from(in_session);
                }
            }
            let Some(failed) = failed_ops(&fs).into_iter().next() else {
                assert_eq!(failures, 0);
                break;
            };
            let context = || format!("{disk:?}: {failed}");
            let _context = OnFailure(&context);
            assert_eq!(failures, 1);
            run.check_session();
            let image = fs.crash_image();
            let durable = image.recover(&image.lose_all()).unwrap();
            assert!(!check_recovered(&durable, &run.model));
            faults += 1;
            from_op = failed.op + 1;
        }
        println!("{disk:?}: {faults} faults, {went_on_in_session} settled in the session");
        assert!(faults >= 20 && went_on_in_session >= 5, "{disk:?}");
    }
}
