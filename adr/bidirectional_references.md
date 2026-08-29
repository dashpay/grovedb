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
    BidirectionalReference(BidirectionalReference),
    /// An ordinary value that can be targeted by bidirectional references —
    /// discriminant 26
    ItemWithBackwardsReferences(Vec<u8>, Vec<BackwardReference>, Option<ElementFlags>),
    /// Signed integer value that can be totaled in a sum tree and targeted
    /// by bidirectional references — discriminant 27
    SumItemWithBackwardsReferences(SumValue, Vec<BackwardReference>, Option<ElementFlags>),
}

pub struct BidirectionalReference {
    pub forward_reference_path: ReferencePathType,
    pub cascade_on_update: CascadeOnUpdate,
    pub max_hop: MaxReferenceHop,
    pub backward_references: Vec<BackwardReference>,
    pub flags: Option<ElementFlags>,
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

Additionally, a new flag was added to `InsertOptions` and `DeleteOptions`
called `propagate_backward_references` (`ClearOptions` support is deferred —
see the limitations below). Since propagation incurs a cost, starting with the
checks required to determine whether it should be performed, bidirectional references are
optional and must be explicitly enabled.

Even when a user inserts something unrelated to the bidirectional references feature,
a check must still be performed to determine whether the insertion overwrites an item
with backward references. If it does, this could trigger a cascade deletion or fail with
an error if cascade deletion is not allowed in the bidirectional references parameters.
However, propagation must be enabled from the start for this check to take place at all.
Fetching the previous item on every modification introduces additional overhead, which
would be unfair to applications that do not use this feature or for database sections that
do not require it. To address this, the flag was introduced.

## Versioning and scope

The whole feature activates with **`GROVE_V4`**: the three element variants
are rejected by earlier protocol versions (fail closed), `GROVE_V4` selects
`insert_on_transaction` v1 and `delete_internal_on_transaction` v2 — both
behaviour-preserving routers whose flag-less calls run the previous
version's body byte-for-byte.

Current limitations (fail closed, lift as needed):

- The three variants may not be wrapped in the aggregation wrappers
  (`NonCounted` / `NotSummed` / `NotCountedOrSummed`).
- `apply_batch` (and every batch entry point) rejects ops carrying them —
  the batch pipeline performs none of the backward-reference bookkeeping.
  Batch (or unflagged) ops that DELETE or OVERWRITE an existing
  backward-references participant are still admitted, exactly like any
  other unflagged write: consistency is forfeited at that point. A
  backward reference left dangling this way is tolerated — later flagged
  propagations and cascades skip it and lazily clear its slot — but
  `verify_grovedb` reports the affected references until the chain is
  rewritten through flagged operations.
- `clear_subtree` has no `propagate_backward_references` option yet; use
  `delete` with the flag for cascade-aware removal.
- Under the flag, insert supports items, references, and empty plain-Merk
  trees; delete supports plain Merk subtrees. The specialized data trees
  (commitment / MMR / bulk-append / dense / private document store) and
  indexed trees are rejected with the flag set — none of their contents
  can be targeted by bidirectional references, so insert/delete them
  without the flag.

## Rules

Next, we’ll go over the rules and limitations for using bidirectional references.

Note that for the rules to apply, the `propagate_backward_references` flag needs to be
set.

An 'Element with backward references' refers to `ItemWithBackwardsReferences`,
`SumItemWithBackwardsReferences`, and `BidirectionalReference`, as all these types contain
a list of backward references associated with them.

- __Only elements with backward references can be targets of bidirectional references.__
Trying to create a bidirectional reference to a regular item will result in an error. And
just like regular references, bidirectional references cannot point to subtrees.
- __A (Sum)Item with backward references can be referenced by up to 32 bidirectional
references.__ This limit exists due to implementation constraints and to ensure worst-case
costs remain predictable—without a limit, estimating these costs would not be possible.
- __A bidirectional reference can be referenced by another bidirectional reference, but
no more than 1.__ This limitation was introduced for the same reason as before: to keep
propagation costs predictable. By restricting chains to one reference per bidirectional
reference, we ensure that an item with up to 32 bidirectional references (each containing
no more than 10 links) can be traced without branching into more paths, allowing us to
predict and manage the worst-case update costs.
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

_Work in progress: Support for bidirectional references in `apply_batch` is
not yet implemented — every batch entry point rejects the three element
variants._

Bidirectional references are optional for each call to GroveDB's public API, and a flag is
used to enable their functionality for that specific call. Essentially, when the flag is
present, it modifies the regular execution process in two ways:

1. Modifications (both writes and deletions) will fetch the data being updated.
2. If the fetched item is an element with backward references, control is passed to the
   `bidirectional_references` module in the GroveDB root for post-processing. This occurs for
   bidirectional reference insertion regardless of whether the flag is set.

Quite a lot happens behind this "post-processing," and we'll go into the details shortly.

### On-element storage and two-layer hashing

The referrer list lives directly on the element: each of the three variants carries a
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
(`inner`) hash — the hash of the stripped serialization. Registering another referrer on a
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
with the required flag enabled can trigger updates across several subtrees simultaneously.

Thus, there are two ongoing propagations:

1. Backward references chain hash propagation / cascade deletion.
2. Regular hash propagation of subtrees.

It is possible that a reference propagation could impact a subtree that is also affected
by regular propagation from one of its descendants. This is difficult to predict. Since
these propagations happen at different steps, they can result in multiple Merk openings
causing issues. To manage this, caching becomes mandatory. This led to the introduction of
`MerkCache`, which has become a crucial component for handling bidirectional references.
