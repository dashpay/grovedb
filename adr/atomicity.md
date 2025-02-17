# Addressing Atomicity

## Level 1: RocksDB Transactions

In GroveDB, almost no operation -- if any at all -- can be executed as a single atomic
operation in RocksDB, the underlying storage used by GroveDB. As long as parallel access
to GroveDB is allowed, there is no guarantee that data will remain consistent across the
multiple operations required at the RocksDB level. Partially, we address this issue using
RocksDB batches, which will be discussed in more detail in the next section. However,
these batches do not address data fetches that may occur while the final RocksDB batch is
being constructed. The data fetched at one step of the operation may be inconsistent with
the data fetched later, as background updates may have occurred in the meantime.

To demonstrate the problem, let’s consider a scenario where there is an only key `c`
under the subtree `[a,b]`. One actor updates this key with a new value while another actor
performs an insertion at a different location:

```
Actor 1:                                      Actor 2:
                                              - load subtree [a,b] with root -- c
- under subtree [a,b] key c insert value x,
  we're not going into much detail there as   - insert empty subtree into [a,b] under key d
  as it was done in one batch and we care     - under subtree [a,b,d] key e insert value y
  only about what happened to c               - under subtree [a,b] key d insert new root
                ...                             hash and root key of subtree [a,b,d]
                                              - compute root [a,b] hash as hash of joined
                                                hashes of c and d *WE HAVE OLD C*
                                              - under subtree [a] key b insert new root
                                                hash and root key of subtree [a,b]
                                              - under subtree [] key a insert new root
                                                hash and root key of subtree [a]
```

... and not to mention what will happen with the ancestors' hashes.

__Solution__: all operations shall be performed via RocksDB transactions.

While this is straightforward for modifications, queries and `get` operations also require
transactions. In general, they cannot be represented by a single RocksDB operation too.
Although `get` may be an exception when no references are involved, data still needs to be
loaded first, and isolation might be required. Therefore, transactions should be provided
from the start.

Since the first release transaction arguments are optional, now we internally start a
transaction if none is provided. To facilitate this, `crate::utils::TxRef` was introduced.

`TxRef` wraps a transactions provided from user if any, otherwise starts a new one. The
rest of the GroveDB internals are unaware of the transaction source and uses what `TxRef`
provied to them with `TxRef::as_ref` method.

In case the transaction was started internally it shall be commited internally as well,
for that purpose `TxRef::commit_local` is used, that will commit the transaction if it is
ineed "local" or is no-operation if the transaction is passed by user, leaving it to the
user to decide what to do with it.

## Level 2: RocksDB Batches

_Not to be confused with GroveDB batches!_

In general, if an operation fails, it doesn't necessarily mean that the entire transaction
should be aborted, unless previous operations were destructive. At least, this is not the
desired behavior in GroveDB, as it is used in Dash Platform: a transaction should live
for the duration of a block, with operations happening seamlessly -- even those that
may fail.

As stated before, an operation that changes the state of GroveDB consists of many
operations. However, we do not apply them directly to the provided transaction. Instead,
we aggregate them into a RocksDB batch, which is applied to the transaction all at once
at the end of the GroveDB operation. This approach allows for failure without aborting
the entire transaction, as it will only abort the batch, leaving the transaction state
untouched.

To apply the `StorageBatch` with these deferred operations onto a running transaction,
`Storage::commit_multi_context_match` is used, where the main implementation of `Storage`
in our case is `RocksDbStorage`.

## Level 3: GroveDB Batches

While RocksDB batches are an implementation detail, GroveDB batches are part of the public
API, on par with regular operations provided by GroveDB. When several updates to GroveDB
need to be performed atomically from a user perspective, without sacrificing a transaction
in case of failure, GroveDB batches are used.

The main takeaways are:

- Always a transaction, whether provided externally or not.
- Always one RocksDB batch applied for modifications.
- Calling `insert*/delete*` results in one RocksDB batch being applied.
- Applying a GroveDB batch full of `insert*/delete*` results in one RocksDB batch, likely
  just larger.
