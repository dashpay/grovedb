//! What the tests on the simulated file system share: a database directory
//! on it, the states a crash can leave, and what an error or the trace says
//! about a power loss or an injected fault.

use std::{collections::BTreeSet, io};

use lignin_io::{
    sim::{CrashImage, CrashPlan, Event, Fault, OpRecord, Outcome, SimConfig, SimError, SimVfs},
    OpenMode,
};

use super::*;

/// The database directory of every test on the simulated file system.
pub(super) const DB: &str = "/db";

pub(super) fn db_dir() -> &'static Path {
    Path::new(DB)
}

/// A configuration that records every operation, so that a test can ask the
/// trace which one a crash or a fault came at.
pub(super) fn traced(seed: u64, sector_size: u64) -> SimConfig {
    SimConfig {
        sector_size,
        trace_ops: true,
        ..SimConfig::new(seed)
    }
}

/// A simulated file system with an empty database directory whose name is
/// durable.
pub(super) fn sim_fs(config: SimConfig) -> SimVfs {
    let fs = SimVfs::new(config);
    fs.create_dir(db_dir()).unwrap();
    fs.sync_dir(Path::new("/")).unwrap();
    fs
}

pub(super) fn shared(fs: &SimVfs) -> Arc<dyn Vfs> {
    Arc::new(fs.clone())
}

fn io_error(error: &SegmentStoreError) -> Option<&io::Error> {
    match error {
        SegmentStoreError::Io { source, .. } | SegmentStoreError::NotDeleted { source, .. } => {
            Some(source)
        }
        _ => None,
    }
}

/// Whether `error` is the file layer's report that the power is off.
pub(super) fn is_power_loss(error: &SegmentStoreError) -> bool {
    io_error(error).is_some_and(SimError::is_power_loss)
}

/// The number of the operation whose injected failure `error` reports.
pub(super) fn injected_op(error: &SegmentStoreError) -> Option<(Fault, u64)> {
    match SimError::of(io_error(error)?)? {
        SimError::Injected { fault, op, .. } => Some((*fault, *op)),
        SimError::PowerLoss => None,
    }
}

fn op_records(fs: &SimVfs) -> impl Iterator<Item = OpRecord> {
    fs.trace().into_iter().filter_map(|event| match event {
        Event::Op(record) => Some(record),
        _ => None,
    })
}

/// The operation after which the power went off, on a file system that
/// records every operation.
pub(super) fn last_op_before_power_off(fs: &SimVfs) -> Option<OpRecord> {
    let after_op = fs.trace().into_iter().find_map(|event| match event {
        Event::PowerOff { after_op } => Some(after_op),
        _ => None,
    })?;
    op_records(fs).find(|record| record.op == after_op)
}

/// The operations a fault was injected into, in order.
pub(super) fn failed_ops(fs: &SimVfs) -> Vec<OpRecord> {
    op_records(fs)
        .filter(|record| matches!(record.outcome, Outcome::Injected { .. }))
        .collect()
}

/// Which of the states a crash can leave are checked.
#[derive(Clone, Copy, Debug)]
pub(super) struct Plans {
    /// Every plan of an image is taken if there are at most this many.
    pub(super) every_plan_limit: usize,
    /// Whether an image with more plans gets the plans of a file system that
    /// writes in order, with every tear of the first lost write, each change
    /// lost alone, and each change applied late.
    pub(super) in_order_and_one_off: bool,
    /// An image with more plans gets the two ends and this many plans drawn
    /// from the seed.
    pub(super) samples: u64,
}

/// The distinct states one crash can leave, each with a plan that gives it.
pub(super) struct CrashStates {
    pub(super) states: Vec<(CrashPlan, SimVfs)>,
    /// How many plans gave those states.
    pub(super) plans: usize,
    /// Whether the plans were all the plans the image has.
    pub(super) exhaustive: bool,
}

pub(super) fn crash_states(image: &CrashImage, plans: &Plans) -> CrashStates {
    let (plans, exhaustive) = match image.every_plan(plans.every_plan_limit) {
        Some(plans) => (plans, true),
        None => {
            let mut some = vec![image.lose_all(), image.keep_all()];
            if plans.in_order_and_one_off {
                some.extend(image.prefixes());
                some.extend(image.holes());
                some.extend(image.reorders());
            }
            some.extend((0..plans.samples).map(|draw| image.sample(draw)));
            (some, false)
        }
    };
    let mut seen = BTreeSet::new();
    let mut states = Vec::new();
    let count = plans.len();
    for plan in plans {
        let state = image.recover(&plan).expect("a plan of the image fits it");
        if seen.insert(state.fingerprint()) {
            states.push((plan, state));
        }
    }
    CrashStates {
        states,
        plans: count,
        exhaustive,
    }
}

pub(super) const EVERY_STATE: Plans = Plans {
    every_plan_limit: 200_000,
    in_order_and_one_off: true,
    samples: 0,
};

pub(super) const SOME_STATES: Plans = Plans {
    every_plan_limit: 2_000,
    in_order_and_one_off: true,
    samples: 24,
};

/// For the many crashes of a long workload whose images have many changes.
pub(super) const A_FEW_STATES: Plans = Plans {
    every_plan_limit: 2_000,
    in_order_and_one_off: false,
    samples: 8,
};

/// Says where a check failed, if it did.
pub(super) struct OnFailure<'a>(pub(super) &'a dyn Fn() -> String);

impl Drop for OnFailure<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", (self.0)());
        }
    }
}

/// The manifest of the database directory, or `None` if it has none.
pub(super) fn manifest_in(fs: &SimVfs) -> Option<Manifest> {
    match read_manifest(fs, db_dir()) {
        Ok(manifest) => Some(manifest),
        Err(SegmentStoreError::NoManifest { .. }) => None,
        Err(error) => panic!("MANIFEST is neither the old one nor the new one: {error}"),
    }
}

pub(super) fn has_temporary_file(fs: &SimVfs) -> bool {
    let temporary = db_dir().join(ManifestStore::TEMP_FILE_NAME);
    fs.open(&temporary, OpenMode::Read).is_ok()
}
