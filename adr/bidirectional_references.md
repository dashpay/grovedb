# Bidirectional references

GroveDB has supported references since its first release; however, the consistency between
references and the data they refer to is only guaranteed at the moment they are inserted.
Subsequent updates to the data do not propagate to the references pointing to it, which
can lead to diverged hashes or references pointing to deleted items.

If the lack of consistency between references and data becomes a problem for a part of the
application using GroveDB, it can choose to use bidirectional references instead.

For this purpose, several new `Element` variants were introduced:

```rust
pub enum Element {
    ...
    /// A reference to an object by its path — discriminant 25
    BidirectionalReference(BidirectionalReference, Option<ElementFlags>),
    /// An ordinary value that can be targeted by bidirectional references —
    /// discriminant 26
    ItemWithBackwardsReferences(Vec<u8>, Vec<BackwardReference>, Option<ElementFlags>),
    /// Signed integer value that can be totaled in a sum tree and targeted
    /// by bidirectional references — discriminant 27
    SumItemWithBackwardsReferences(SumValue, Vec<BackwardReference>, Option<ElementFlags>),
    /// Item carrying an explicit sum value (like `ItemWithSumItem`) that can
    /// be targeted by bidirectional references — discriminant 28
    ItemWithSumItemWithBackwardsReferences(
        Vec<u8>,
        SumValue,
        Vec<BackwardReference>,
        Option<ElementFlags>,
    ),
}

pub struct BidirectionalReference {
    pub forward_reference_path: ReferencePathType,
    pub cascade_on_update: CascadeOnUpdate,
    pub max_hop: MaxReferenceHop,
    pub backward_references: Vec<BackwardReference>,
}

pub struct BackwardReference {
    /// Inverted path leading back to the referrer.
    pub inverted_reference: ReferencePathType,
    pub cascade_on_update: bool,
}
```

These items are counterparts of existing ones: items, sum items, and regular references.
A regular item with ordinary references does not propagate updates back to the reference
chain origin. When such behavior is required, a different type of element should be used.
Moreover, these types are incompatible, which will be discussed in the "Rules" section.

On `GROVE_V4`, ordinary inserts, replacements, deletes, and full batches
maintain backward references automatically, and every operation declares
what it displaces: `InsertOptions`, `DeleteOptions`, `ClearOptions` and each
`QualifiedGroveDbOp` carry a `DisplacedValue`, `MayBeParticipant` by default.
GroveDB reads the displaced value for the write anyway, so for a keyed
operation the declaration only decides what happens when that value takes
part in backward references: `MayBeParticipant` maintains the references,
`NotParticipant` refuses the operation before anything commits. Where nothing
reads the contents — a flat drop, a raw `clear_subtree`, the replacement of a
populated subtree, a live recursive delete — `NotParticipant` is trusted and
leaves any participant's registrations stale, exactly like the storage it
strands. There is no policy that skips maintenance on a value known to
participate. Inserting a bidirectional reference always registers its edge.

## Versioning and scope

The feature activates with **`GROVE_V4`**. Earlier protocol versions reject the
four variants and retain their historical execution and cost behavior. V4
selects `insert_on_transaction` v1, `delete_internal_on_transaction` v2, and
`apply_batch.backward_references_maintenance` 1;
the older implementations remain separate.

Current limitations:

- The four variants cannot be wrapped in `NonCounted`, `NotSummed`, or
  `NotCountedOrSummed`.
- Full batches support new targets, chains, propagation, and cascades by default.
  Partial batches cannot plan cross-segment reference maintenance. They reject
  family payloads, and their apply-time old-value observer also rejects plain
  writes or deletes that would displace an existing participant. Removing a
  subtree containing participants is refused before commit in either segment.
- Recursive batch deletion requires participant descendants to be explicitly
  accounted for by deletion operations. Use live `delete` for recursive
  reference maintenance. A subtree containing participants must be deleted
  before replacing its tree element.
- Ordinary specialized data trees and indexed trees continue to use their
  existing storage cleanup and indexed propagation. Live recursive reference
  maintenance still refuses a subtree containing both participants and
  specialized/indexed descendants. Remove those descendants first.
- Live participant maintenance below an indexed primary requires a full
  batch; the reference cache refuses that propagation before commit.
- `Delete` of a populated tree and subtree replacement inspect descendants
  when declared `MayBeParticipant`; those scans add reads and are charged in
  the V4 default cost tests. A batch `DeleteTree` is never pre-scanned:
  `DontCheckWithNoCleanup` declares that the batch's own deletes emptied the
  subtree, `Error` and `Skip` verify that at apply time, and `DeleteChildren`
  (with the defensive `Error`/`Skip` sweeps) checks the declaration on the
  cleanup walk it makes anyway, refusing a participant the batch does not
  explicitly delete. A delete-up-tree chain and a batch recursive removal
  therefore cost the plain removal under either declaration.
- Flat drop retains its O(1) contract. Standalone `drop_flat_subtree` takes
  the declaration as a required argument; it and batch `DropFlat` refuse
  `MayBeParticipant` before reading anything and trust `NotParticipant`. Use
  recursive delete when maintenance is required.
- `clear_subtree` under `MayBeParticipant` scans and refuses a subtree
  containing participants before making any mutation, including with a caller
  transaction; delete the participants through the normal API first.
  `NotParticipant` is trusted for a raw clear. A live recursive `delete`
  declared `NotParticipant` likewise trusts its contents: a clear runs one
  such delete per nested subtree inside the caller's transaction, where a
  refusal part-way through could not be undone.

Ordinary full batches keep their original operation set when neither stored
nor incoming values participate in references. Preparation retains the Merks
for execution, but does not apply the reference planner's conflict rules,
conditional-operation rewriting, or duplicate-position rejection. Reference
batches still require unambiguous positions even if ordinary consistency
checking is disabled.

Partial batches deliberately do not support reference planning across the
continuation callback. They reject new family payloads before each segment;
the old-value observer rejects displaced participants while applying a segment.
Subtree inspections run after the staged applies and before commit against the
transaction's original subtree contents. Cross-segment conflict checks prevent
those committed-state inspections from overlooking changes staged by the first
segment. A refusal discards the storage batch and preserves the caller's
transaction. These scans have real costs, pinned alongside the full-batch costs;
this API does not promise a no-scan recursive removal, only that `DeleteTree`
removals add no reads of their own: their contents are checked on the cleanup
walk.

## Rules

Next, we’ll go over the rules and limitations for using bidirectional references.

These rules always apply; `DisplacedValue::NotParticipant` is a checked claim, not an
opt-out.

An 'Element with backward references' refers to `ItemWithBackwardsReferences`,
`SumItemWithBackwardsReferences`, `ItemWithSumItemWithBackwardsReferences`, and
`BidirectionalReference`, as all these types contain a list of backward references
associated with them.

- __Only elements with backward references can be targets of bidirectional references.__
Trying to create a bidirectional reference to a regular item will result in an error. And
just like regular references, bidirectional references cannot point to subtrees.
- __A (Sum)Item with backward references declares how many bidirectional references may
point at it__ (`BackwardReferences::max_incoming`, at most the protocol ceiling
`MAX_BACKWARD_REFERENCES` = 256; the convenience constructors declare 32). The declaration is
authenticated with the element — it sits in the inner (stripped) bytes every referrer commits
to — so every node enforces the same value. Registration past the declared capacity fails,
declaring a capacity above the ceiling fails at construction and at decode, and an update may
raise the capacity freely but may not lower it below the referrers already registered.
Declaring capacity allocates nothing: the list stores only actual registrations. The
declaration exists so worst-case costs stay predictable AND proportionate: a write of the
item carries its own capacity, so the cost estimator charges an item indexed four ways for
four chains instead of for the ceiling. The ceiling still bounds the work of operations that
cannot see the stored element they displace (a delete, a plain payload landing on a
registered element) and the node growth registrations can inflict on a target.
- __A bidirectional reference's declared `max_hop` must admit its own chain at
insertion.__ Public reads enforce the declared budget deterministically, so an edge whose
chain is already longer than its declaration would never resolve; the write path rejects
such dead edges instead of persisting them. (An edge can still fall out of budget later —
e.g. its target is overwritten into a plain reference behind a raw clear declared
`NotParticipant` — and
reads then return `ReferenceLimit`.)
- __Both ends of a bidirectional edge must sit at most 32 subtree levels deep__
(`MAX_BACKWARD_REFERENCES_GROVE_DEPTH`, enforced at registration). Every later derived
write — propagation rewrite, cascade deletion, registration cleanup — lands at one of the
edge's positions, and cost estimation charges up to this many ancestor updates per derived
foreign-subtree propagation; without the bound, a referrer parked arbitrarily deep would
make its propagation cost exceed any fixed estimate.
- __A bidirectional reference can be referenced by another bidirectional reference, but
no more than 1.__ This limitation was introduced for the same reason as before: to keep
propagation costs predictable. By restricting chains to one reference per bidirectional
reference, we ensure that an item with up to `max_incoming` bidirectional references (each
chain containing no more than 10 links) can be traced without branching into more paths —
at most `max_incoming × 10` affected nodes — allowing us to predict and manage the
worst-case update costs.
- __If an element with backward references is updated with another element with backward
references, hash propagation happens.__ All bidirectional references across all chains
shall update their hashes using the new one of the updated item. If the updated item is
a new bidirectional reference itself, it will follow the chain forward first to get the
value hash that will be used for propagation.
- __If an element can no longer be targeted (for example, updated to an item with no
backward references support or deleted entirely), a cascade deletion of bidirectional
references occurs.__ This requires the `cascade_on_update` setting for each affected
bidirectional reference. If this setting is not enabled, an error will be raised,
preventing the operation from completing successfully.

## Implementation

### Batching

`apply_batch` supports the whole family by default on V4. Its preparation
pass (`batch::backward_references`) exposes each existing value through
`Merk::observe_old_value`, then expands the operations using the shared
semantic planners in `bidirectional_references::semantics`.

The observer traverses the Merk and retains fetched nodes in that same
instance. Execution receives the prepared Merks, so it reuses the nodes for
replacement, deletion, and reclaimed-byte accounting. Preparing and applying
a cold ordinary node has the same total storage cost as applying it directly.
Observation is not itself a mutation and does not change the root hash.

Planning still precedes mutation: registration, cascade consent, cross-subtree
operations, and batch conflicts must be resolved before applying the atomic
batch. New references have no old value, so their payloads still trigger
registration. Reference traversal and recursive subtree inspection remain
separate work and incur their own costs; this is not a claim that maintenance
has no cost.

The expansion simulates one canonical sequential order over an overlay of
pending position states (pre-batch DB state plus the batch's staged
effects): first every non-reference op in user order, then every
`BidirectionalReference` op in topological order — targets before their
referrers. This makes references to targets created in the same batch
work regardless of op order, lets whole chains be created in one batch,
and validates the hop/component budgets against the prospective
post-batch state. Derived writes execute through the internal
`GroveOp::ReplaceBackwardReferenceFamilyMember` (rejected when supplied by
callers), which installs the fully-combined node value hash; user
reference ops are themselves converted into that form (an identical-edge
re-insert converts into nothing, mirroring the live no-op), and
registrations onto targets written in the same batch merge into the
target op's element.

Conflicts fail closed with specified errors: a reference inserted in the
same batch that deletes its target; a cascade deleting a position another
op touches; a propagation rewrite hitting a user delete; and
`RefreshReference` on a position holding any backward-reference participant.

Estimated costs (average and worst case) model the derived fan-out on
GROVE_V4+ by default, bounded by the budgets above (a written
item's DECLARED referrer capacity, the 256 ceiling for writes that cannot
see the element they displace, ≤10-hop chains, 1 referrer per reference);
pre-V4
estimation is preserved byte-for-byte for replay of historical admission
decisions.

The estimator cannot see stored state, so the bound for a write that may
displace a participant follows the op's own declaration: an op declared
`MayBeParticipant` charges the displaced-state fan-out and the delete probe,
an op declared `NotParticipant` charges the plain write alone, and ops that
themselves write a participant (family items, bidirectional references) are
charged from the op regardless. Declaring correctly is the caller's
responsibility; the apply path refuses a false `NotParticipant` claim rather
than running an unpriced cascade.

Live writes use the same preparation observer. Ordinary values retain the
existing parent and indexed-tree propagation; participating values reuse the
prepared Merk inside `MerkCache` for reference maintenance. All pending
changes share the caller's storage batch and commit only after validation.

### On-element storage and two-layer hashing

The referrer list lives directly on the element: each of the four variants carries a
`Vec<BackwardReference>`. Registering or removing a referrer rewrites the TARGET element's
bytes — but a naive design would then re-hash the target's value, changing the very hash
every existing referrer has committed to, and each registration would trigger a cascade of
propagations across all other referrers.

To avoid that, elements with backward references use a two-layer hash:

```text
inner_hash    = H(serialize(element with backward_references = []))   // the LOGICAL hash
backrefs_hash = H(serialize(backward_references))
node value_hash:
  (Sum)ItemWithBackwardsReferences  = combine(inner_hash, backrefs_hash)
  BidirectionalReference            = combine(combine(inner_hash, backrefs_hash), end_hash)
```

where `end_hash` for a bidirectional reference is the hash of what it transitively points
at, and `combine` is the existing two-input node-hash combinator.

Every reference in a chain (ordinary or bidirectional) commits to the target's *logical*
(`inner`) hash — the hash of the stripped serialization. This rule binds every write
path, including `apply_batch`: a batch-inserted plain reference that terminates on (or
chains through) a backward-references element commits the stripped hash, never the
node's combined hash. Registering another referrer on a
target changes only `backrefs_hash`, so the target's own node re-hashes (and propagates up
its subtree as usual), while every referrer that already points at it keeps its stored
node hash bit-for-bit. Cascaded hash propagation only happens when the *payload* — the
logical hash — actually changes.

Public reads (`get`, `get_raw`, query results, proved results) return the STRIPPED
element: the referrer list is internal bookkeeping and never crosses the API boundary.
Internal flows (propagation, cascade deletion, `verify_grovedb`) read the full element at
the merk level.

Since the referrer list is ordinary element data, state sync and chunk restore carry it
for free — no side-channel storage has to be reconstructed.

### Proofs

Because the node's `value_hash` is no longer `H(value_bytes)`, a plain `KVValueHash` proof
node cannot authenticate these elements. A dedicated proof node kind ships the stripped
payload plus the 32-byte referrer-list hash:

```text
Node::KVBackwardsReferencesValueHash(key, stripped_value_bytes, backrefs_hash)
```

The verifier RECOMPUTES `combine(H(stripped_value_bytes), backrefs_hash)` as the node's
value hash — the payload bytes are bound by the recomputation rather than trusted, and
tampering with either the payload or the referrer-list hash breaks the root-hash chain.
The verifier also rejects backward-references elements smuggled inside plain `KVValueHash`
/ `KVValueHashFeatureType` nodes, and the node kind itself is rejected in V0 proofs
(V0 is a frozen wire format). Bidirectional references resolve through the existing
`KVRefValueHash` mechanics with `combine(inner, backrefs)` as the carried self-hash.

One consequence: elements with backward references are rejected inside `Provable*`
aggregate trees (`ProvableCountTree`, `ProvableSumTree`, `ProvableCountSumTree`,
`ProvableCountProvableSumTree`) — their proof nodes carry aggregate data in shapes that
have no backward-references twin. Plain `SumTree` / `CountTree` parents work.

### Propagation

Previous read: [Merk cache](./merk_cache.md).

Deletion or an update of an element with backward references triggers a cascade hash
update or a deletion, both of which alter the state of affected subtrees, leading to
regular hash propagation to ancestor subtrees up to the GroveDB root. In short, operations
on V4 can trigger updates across
several subtrees simultaneously.

Thus, there are two ongoing propagations:

1. Backward references chain hash propagation / cascade deletion.
2. Regular hash propagation of subtrees.

It is possible that a reference propagation could impact a subtree that is also affected
by regular propagation from one of its descendants. This is difficult to predict. Since
these propagations happen at different steps, they can result in multiple Merk openings
causing issues. To manage this, caching becomes mandatory. This led to the introduction of
`MerkCache`, which has become a crucial component for handling bidirectional references.
