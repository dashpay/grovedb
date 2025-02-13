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
```
