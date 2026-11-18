//! The platform state, which Drive keeps outside the trees.

use std::collections::BTreeSet;

use super::DOMAIN_PLATFORM_STATE;
use crate::workload::{
    build::{self, value},
    op::Op,
    rng::Rng,
    Prepared, Stream,
};

const PROTOCOL_VERSION_KEY: &[u8] = b"protocol_version";
const STATE_KEY: &[u8] = b"saved_state";
const RECENT_STATE_KEY: &[u8] = b"saved_state_recent";
const MASTERNODES_PREFIX: &[u8] = b"saved_state/masternodes/";
const VALIDATOR_SETS_PREFIX: &[u8] = b"saved_state/validator_sets/";

/// The platform state in auxiliary storage.
///
/// Drive stores what it needs to restart in records no tree commits to: the
/// protocol version, the platform state, the part of it that changes with
/// every block, and one record per masternode and per validator set under a
/// shared key prefix. They are written in the block's transaction. A block
/// rewrites the small record, and now and then the large one, a few
/// masternode records and a rotated validator set. The reads are the ones
/// of a restart: each record by key and each collection by prefix.
///
/// Auxiliary records are not under a root tree: the keys are Drive's, and no
/// other workload writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformState {
    /// How many masternodes the list starts with.
    pub masternodes: u32,
    /// How many validator sets are kept.
    pub validator_sets: u32,
    /// How many blocks to write.
    pub blocks: u64,
    /// How many blocks pass between two rewrites of the whole state.
    pub blocks_per_rotation: u32,
}

impl Default for PlatformState {
    fn default() -> Self {
        Self {
            masternodes: 24,
            validator_sets: 4,
            blocks: 48,
            blocks_per_rotation: 6,
        }
    }
}

fn entry_key(prefix: &[u8], id: &[u8; 32]) -> Vec<u8> {
    [prefix, id].concat()
}

impl PlatformState {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.blocks_per_rotation > 0, "a rotation needs a block");
        let mut rng = Rng::derive(seed, DOMAIN_PLATFORM_STATE, 0);
        let mut read_rng = Rng::derive(seed, DOMAIN_PLATFORM_STATE, 1);
        let mut masternodes = BTreeSet::new();
        let mut validator_sets = Vec::new();
        let mut ops = vec![
            build::put_aux(PROTOCOL_VERSION_KEY, vec![12]),
            build::put_aux(STATE_KEY, value(&mut rng, 2_000, 4_000)),
        ];
        for _ in 0..self.masternodes {
            let id = rng.id();
            masternodes.insert(id);
            ops.push(build::put_aux(
                &entry_key(MASTERNODES_PREFIX, &id),
                value(&mut rng, 180, 260),
            ));
        }
        for _ in 0..self.validator_sets {
            let id = rng.id();
            validator_sets.push(id);
            ops.push(build::put_aux(
                &entry_key(VALIDATOR_SETS_PREFIX, &id),
                value(&mut rng, 1_200, 1_800),
            ));
        }
        ops.push(Op::Commit);
        ops.push(build::get_aux(PROTOCOL_VERSION_KEY));

        for block in 1..=self.blocks {
            ops.push(build::put_aux(RECENT_STATE_KEY, value(&mut rng, 90, 140)));
            if block % u64::from(self.blocks_per_rotation) == 0 {
                ops.push(build::put_aux(STATE_KEY, value(&mut rng, 2_000, 4_000)));
                if !validator_sets.is_empty() {
                    let oldest = validator_sets.remove(0);
                    ops.push(build::delete_aux(&entry_key(
                        VALIDATOR_SETS_PREFIX,
                        &oldest,
                    )));
                    let id = rng.id();
                    validator_sets.push(id);
                    ops.push(build::put_aux(
                        &entry_key(VALIDATOR_SETS_PREFIX, &id),
                        value(&mut rng, 1_200, 1_800),
                    ));
                }
                if block == u64::from(self.blocks_per_rotation) * 2 {
                    ops.push(build::put_aux(PROTOCOL_VERSION_KEY, vec![13]));
                }
            }
            if rng.chance(1, 2) {
                let roll = rng.below(4);
                let existing = masternodes
                    .range(rng.id()..)
                    .next()
                    .or_else(|| masternodes.iter().next())
                    .copied();
                match existing {
                    Some(id) if roll == 0 => {
                        masternodes.remove(&id);
                        ops.push(build::delete_aux(&entry_key(MASTERNODES_PREFIX, &id)));
                    }
                    Some(id) if roll < 3 => ops.push(build::put_aux(
                        &entry_key(MASTERNODES_PREFIX, &id),
                        value(&mut rng, 180, 260),
                    )),
                    _ => {
                        let id = rng.id();
                        masternodes.insert(id);
                        ops.push(build::put_aux(
                            &entry_key(MASTERNODES_PREFIX, &id),
                            value(&mut rng, 180, 260),
                        ));
                    }
                }
            }
            ops.push(Op::Commit);
            ops.push(match block % 6 {
                0 => build::get_aux(STATE_KEY),
                1 => build::get_aux(RECENT_STATE_KEY),
                2 => build::aux_by_prefix(MASTERNODES_PREFIX),
                3 => build::aux_by_prefix(VALIDATOR_SETS_PREFIX),
                4 => build::get_aux(&entry_key(MASTERNODES_PREFIX, &read_rng.id())),
                _ => build::get_aux(PROTOCOL_VERSION_KEY),
            });
        }
        Prepared::stream(ops)
    }
}
