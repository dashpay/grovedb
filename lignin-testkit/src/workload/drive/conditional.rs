//! Writes that depend on what is stored, made the way Drive makes them.

use std::collections::{BTreeMap, BTreeSet};

use grovedb::{Element, ElementFlags};

use super::{
    document_reference, document_type_trees, epoch_of_block, index_document,
    CONTRACT_DOCUMENTS_KEY, DOMAIN_CONDITIONAL, PRIMARY_KEY,
};
use crate::workload::{
    build::{self, batch, child, ends_block, in_block, path, read, storage_flags, value},
    op::{BatchOp, Fetch, Op, Path, QueryItem, Write},
    rng::Rng,
    Prepared, Stream,
};

/// A root tree of this workload's own, in place of Drive's
/// `DataContractDocuments`.
const ROOT: &[u8] = &[72];
const DOCUMENT_TYPE: &[u8] = b"profile";
const CITY: &[u8] = b"city";
const HANDLES: &[u8] = b"handle";
const DRAFTS: &[u8] = b"drafts";

/// Documents written, changed and removed through the operations that look
/// before they write.
///
/// A profile is a document in the primary key tree, with a reference in an
/// index by city, a handle that must be unique, and drafts in a tree per
/// owner. Each state transition does one of these, as Drive does them:
///
/// - **Create.** Drive asks whether the document and the city's index trees
///   are stored (`has_raw`, inside the block's transaction, so it sees the
///   transitions before it) and builds its batch from the answers. The
///   handle goes in through `insert_if_not_exists`, in both of its forms,
///   and is refused when another profile has it.
/// - **Update.** The document is written only if it changed: by Drive's
///   read of the stored element followed by a batch, or by
///   `insert_if_changed_value`. Half of the updates change nothing. One
///   that changes the document refreshes its index reference, whose hash
///   follows the document.
/// - **Delete.** The document's delete is built by
///   `delete_operation_for_delete_internal` and applied as a batch. Its
///   index reference is deleted up the tree: the trees of a city with no
///   profile left go with it, and the index tree itself stays. The handle
///   is deleted directly.
/// - **Draft.** An owner's tree of drafts is created if it is not there, a
///   draft is added or removed, and `delete_if_empty_tree` removes the tree
///   once it holds none, and leaves it when it still does.
///
/// After each block the checks are read against committed state: keys that
/// are stored and keys that are not, trees that are empty and trees that
/// are not, and one element through each of the `get` methods, with the
/// cache and without it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConditionalWrites {
    /// How many state transitions to apply.
    pub transitions: u64,
    /// How many cities the profiles are spread over.
    pub cities: u32,
    /// How many owners write drafts.
    pub owners: u32,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
}

impl Default for ConditionalWrites {
    fn default() -> Self {
        Self {
            transitions: 160,
            cities: 6,
            owners: 4,
            transitions_per_block: 8,
        }
    }
}

struct Profile {
    city: Vec<u8>,
    /// The handle the profile has, if it got one.
    handle: Option<Vec<u8>>,
    /// The document as it is stored.
    document: Element,
    /// The flags its index reference is stored with.
    reference_flags: Option<ElementFlags>,
}

struct State {
    rng: Rng,
    type_path: Path,
    primary_path: Path,
    index_path: Path,
    handles_path: Path,
    drafts_path: Path,
    profiles: BTreeMap<[u8; 32], Profile>,
    /// The handles taken, each with the profile that has it.
    handles: BTreeMap<Vec<u8>, [u8; 32]>,
    /// The drafts each owner keeps. An owner with a tree is in the map.
    drafts: BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
    cities: u32,
    owners: u32,
    block: u64,
    step: u64,
}

impl ConditionalWrites {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.cities > 0 && self.owners > 0, "a profile has a city");
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        let mut rng = Rng::derive(seed, DOMAIN_CONDITIONAL, 0);
        let contract = rng.id();
        let type_path = path(&[ROOT, &contract, CONTRACT_DOCUMENTS_KEY, DOCUMENT_TYPE]);
        let mut ops = document_type_trees(ROOT, &contract, DOCUMENT_TYPE, &[CITY, HANDLES, DRAFTS]);
        ops.push(Op::Commit);
        let mut state = State {
            rng,
            primary_path: child(&type_path, PRIMARY_KEY),
            index_path: child(&type_path, CITY),
            handles_path: child(&type_path, HANDLES),
            drafts_path: child(&type_path, DRAFTS),
            type_path,
            profiles: BTreeMap::new(),
            handles: BTreeMap::new(),
            drafts: BTreeMap::new(),
            cities: self.cities,
            owners: self.owners,
            block: 1,
            step: 0,
        };
        for done in 1..=self.transitions {
            state.transition(&mut ops);
            if ends_block(
                done,
                self.transitions,
                u64::from(self.transitions_per_block),
            ) {
                ops.push(Op::Commit);
                state.reads(&mut ops);
                state.block += 1;
            }
        }
        Prepared::stream(ops)
    }
}

impl State {
    fn transition(&mut self, ops: &mut Vec<Op>) {
        self.step += 1;
        let roll = self.rng.below(10);
        let existing = self.any_profile();
        match (roll, existing) {
            (0..=3, _) | (_, None) => self.create(ops),
            (4..=5, Some(id)) => self.update(&id, ops),
            (6..=7, Some(id)) => self.delete(&id, ops),
            (_, Some(_)) => self.draft(ops),
        }
    }

    fn any_profile(&mut self) -> Option<[u8; 32]> {
        let drawn = self.rng.id();
        self.profiles
            .range(drawn..)
            .next()
            .or_else(|| self.profiles.iter().next())
            .map(|(id, _)| *id)
    }

    fn city(&mut self) -> Vec<u8> {
        format!("city-{:02}", self.rng.below(u64::from(self.cities))).into_bytes()
    }

    fn city_documents(&self, city: &[u8]) -> Path {
        child(&child(&self.index_path, city), PRIMARY_KEY)
    }

    fn city_is_indexed(&self, city: &[u8]) -> bool {
        self.profiles.values().any(|profile| profile.city == city)
    }

    fn create(&mut self, ops: &mut Vec<Op>) {
        let flags = storage_flags(epoch_of_block(self.block), None);
        let id = self.rng.id();
        let city = self.city();
        let document = Element::new_item_with_flags(value(&mut self.rng, 40, 120), flags.clone());
        let handle = format!("handle-{:02}", self.rng.below(48)).into_bytes();

        ops.push(in_block(read::has_raw(&self.primary_path, &id)));
        ops.push(in_block(read::has_raw(&self.index_path, &city)));
        let value_tree =
            (!self.city_is_indexed(&city)).then(|| Element::empty_tree_with_flags(flags.clone()));
        let mut create = vec![batch::insert_only(
            &self.primary_path,
            &id,
            document.clone(),
        )];
        index_document(
            &mut create,
            &self.index_path,
            &city,
            value_tree,
            &id,
            Element::new_reference_with_flags(document_reference(&id), flags.clone()),
        );
        ops.push(Op::Batch(create));

        // A handle another profile has is refused, and the profile is
        // stored without one.
        ops.push(Op::Write(Write::InsertIfNotExists {
            path: self.handles_path.clone(),
            key: handle.clone(),
            element: Element::new_item_with_flags(id.to_vec(), flags.clone()),
            return_existing: self.step.is_multiple_of(2),
        }));
        let handle = (!self.handles.contains_key(&handle)).then(|| {
            self.handles.insert(handle.clone(), id);
            handle
        });
        self.profiles.insert(
            id,
            Profile {
                city,
                handle,
                document,
                reference_flags: flags,
            },
        );
    }

    fn update(&mut self, id: &[u8; 32], ops: &mut Vec<Op>) {
        let flags = storage_flags(epoch_of_block(self.block), None);
        let unchanged = self.rng.chance(1, 2);
        let element = if unchanged {
            self.profiles[id].document.clone()
        } else {
            Element::new_item_with_flags(value(&mut self.rng, 40, 120), flags)
        };
        let profile = &self.profiles[id];
        let refresh = BatchOp::RefreshReference {
            path: self.city_documents(&profile.city),
            key: id.to_vec(),
            reference: document_reference(id),
            max_hop: None,
            flags: profile.reference_flags.clone(),
            non_counted: false,
            trusted: true,
        };
        if self.step.is_multiple_of(2) {
            ops.push(in_block(read::fetch(
                &self.primary_path,
                id,
                Fetch::RawOptional,
                true,
            )));
            if !unchanged {
                ops.push(Op::Batch(vec![
                    batch::replace(&self.primary_path, id, element.clone()),
                    refresh,
                ]));
            }
        } else {
            ops.push(Op::Write(Write::InsertIfChanged {
                path: self.primary_path.clone(),
                key: id.to_vec(),
                element: element.clone(),
            }));
            if !unchanged {
                ops.push(Op::Batch(vec![refresh]));
            }
        }
        self.profiles
            .get_mut(id)
            .expect("a profile to update")
            .document = element;
    }

    fn delete(&mut self, id: &[u8; 32], ops: &mut Vec<Op>) {
        let profile = self.profiles.remove(id).expect("a profile to delete");
        ops.push(Op::Write(Write::DeleteBuilt {
            path: self.primary_path.clone(),
            key: id.to_vec(),
        }));
        ops.push(Op::Write(Write::DeleteUpTree {
            path: self.city_documents(&profile.city),
            key: id.to_vec(),
            stop_path_height: Some(self.type_path.len() as u16),
            built: self.step.is_multiple_of(2),
        }));
        if let Some(handle) = profile.handle {
            self.handles.remove(&handle);
            ops.push(build::delete(&self.handles_path, &handle));
        }
    }

    fn draft(&mut self, ops: &mut Vec<Op>) {
        let owner = format!("owner-{}", self.rng.below(u64::from(self.owners))).into_bytes();
        let owner_path = child(&self.drafts_path, &owner);
        ops.push(in_block(read::has_raw(&self.drafts_path, &owner)));
        if !self.drafts.contains_key(&owner) {
            ops.push(build::insert(
                &self.drafts_path,
                &owner,
                Element::empty_tree(),
            ));
        }
        let drafts = self.drafts.entry(owner.clone()).or_default();
        let oldest = drafts.iter().next().cloned();
        match oldest {
            Some(draft) if self.rng.chance(1, 2) => {
                drafts.remove(&draft);
                ops.push(build::delete(&owner_path, &draft));
            }
            _ => {
                let draft = self.step.to_be_bytes().to_vec();
                drafts.insert(draft.clone());
                ops.push(build::insert(
                    &owner_path,
                    &draft,
                    Element::new_item(value(&mut self.rng, 1, 30)),
                ));
            }
        }
        // The tree goes when the last draft has gone.
        ops.push(Op::Write(Write::DeleteIfEmptyTree {
            path: self.drafts_path.clone(),
            key: owner.clone(),
        }));
        if drafts.is_empty() {
            self.drafts.remove(&owner);
        }
    }

    fn reads(&mut self, ops: &mut Vec<Op>) {
        let city = self.city();
        ops.push(Op::Read(read::has_raw(&self.index_path, &city)));
        if self.city_is_indexed(&city) {
            ops.push(Op::Read(read::is_empty_tree(&self.city_documents(&city))));
            ops.push(build::query(build::select(
                &self.city_documents(&city),
                vec![QueryItem::RangeFull],
                Some(4),
            )));
        }
        ops.push(Op::Read(read::is_empty_tree(&self.index_path)));
        ops.push(Op::Read(read::is_empty_tree(&self.drafts_path)));
        let owner = format!("owner-{}", self.rng.below(u64::from(self.owners))).into_bytes();
        if self.drafts.contains_key(&owner) {
            ops.push(Op::Read(read::is_empty_tree(&child(
                &self.drafts_path,
                &owner,
            ))));
        }
        let absent = self.rng.id();
        ops.push(Op::Read(read::has_raw(&self.primary_path, &absent)));
        for cache in [true, false] {
            ops.push(Op::Read(read::fetch(
                &self.primary_path,
                &absent,
                Fetch::RawOptional,
                cache,
            )));
        }
        // A key under a tree that is not there is no element either.
        ops.push(Op::Read(read::fetch(
            &child(&self.index_path, b"nowhere"),
            PRIMARY_KEY,
            Fetch::RawOptional,
            self.block.is_multiple_of(2),
        )));
        let Some(id) = self.any_profile() else {
            return;
        };
        ops.push(Op::Read(read::has_raw(&self.primary_path, &id)));
        let profile = &self.profiles[&id];
        let reference_path = self.city_documents(&profile.city);
        let fetches = [Fetch::Resolved, Fetch::Raw, Fetch::RawOptional];
        for (cache, fetch) in [true, false, true].into_iter().zip(fetches) {
            let cache = cache != self.block.is_multiple_of(2);
            ops.push(Op::Read(read::fetch(&reference_path, &id, fetch, cache)));
            ops.push(Op::Read(read::fetch(
                &self.primary_path,
                &id,
                fetch,
                !cache,
            )));
        }
        if let Some(handle) = &profile.handle {
            ops.push(Op::Read(read::get(&self.handles_path, handle)));
        }
    }
}
