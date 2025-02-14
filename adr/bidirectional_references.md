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
    /// A reference to an object by its path
    BidirectionalReference(BidirectionalReference),
    /// An ordinary value that has a backwards reference
    ItemWithBackwardsReferences(Vec<u8>, Option<ElementFlags>),
    /// Signed integer value that can be totaled in a sum tree and has a
    /// backwards reference
    SumItemWithBackwardsReferences(SumValue, Option<ElementFlags>),
}

pub struct BidirectionalReference {
    pub forward_reference_path: ReferencePathType,
    pub backward_reference_slot: SlotIdx,
    pub cascade_on_update: CascadeOnUpdate,
    pub max_hop: MaxReferenceHop,
    pub flags: Option<ElementFlags>,
}
```

These items are counterparts of existing ones: items, sum items, and regular references.
A regular item with ordinary references does not propagate updates back to the reference
chain origin. When such behavior is required, a different type of element should be used.
Moreover, these types are incompatible, which will be discussed in the "Rules" section.

Additionally, a new flag was added to `InsertOptions`, `DeleteOptions`, and `ClearOptions`
called `propagate_backward_references`. Since propagation incurs a cost, starting with the
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

# Rules

Next, we’ll go over the rules and limitations for using bidirectional references.

Note that for the rules to apply, the `propagate_backward_references` flag needs to be
set.

An 'Element with backward references' refers to ItemWithBackwardReferences,
SumItemWithBackwardReferences, and BidirectionalReference, as all these types contain a
list of backward references associated with them.

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
