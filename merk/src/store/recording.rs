//! A storage context that writes down every call made on it, on its batches
//! and on its cursors, and passes each call on to the context it wraps. It
//! can be told to fail one kind of call on the context itself.
//!
//! Every call that returns a cost returns one hash node call more than the
//! wrapped context charged, and a call that was told to fail returns
//! [`failed_call`]. No storage call charges a hash node call of its own, so a
//! cost that an adapter drops or counts twice shows in the total. The tape
//! adds up what was returned ([`Tape::take_cost`]). A put or a delete on a
//! batch returns no cost.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildKeyLength, ChildSumLength,
    ChildrenSizesWithIsSumTree, CostContext, CostResult, CostsExt, FeatureSumLength, OperationCost,
    TreeCostType,
};
use grovedb_storage::{Batch, Error, RawIterator, StorageContext};

/// The sizes a put was given: the kind and length of the feature data, then
/// the key length and aggregate length of each child link.
pub(crate) type Sizes = (
    Option<(&'static str, FeatureSumLength)>,
    Option<(ChildKeyLength, ChildSumLength)>,
    Option<(ChildKeyLength, ChildSumLength)>,
);

/// One call, with the arguments it was given.
#[derive(Debug, PartialEq)]
pub(crate) struct Call {
    pub(crate) name: &'static str,
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
    pub(crate) sizes: Option<Sizes>,
    pub(crate) cost: Option<KeyValueStorageCost>,
}

/// A call with no sizes and no cost information.
pub(crate) fn call(name: &'static str, key: &[u8], value: &[u8]) -> Call {
    Call {
        name,
        key: key.to_vec(),
        value: value.to_vec(),
        sizes: None,
        cost: None,
    }
}

impl Call {
    /// The name and key of the call, the value of a call that puts a root
    /// key or a side record, and whether cost information came with it.
    pub(crate) fn brief(&self) -> String {
        let mut brief = self.name.to_string();
        let shown_value = ["put_root", "put_aux", "put_meta"]
            .iter()
            .any(|put| self.name.ends_with(put));
        let value: &[u8] = if shown_value { &self.value } else { &[] };
        for bytes in [self.key.as_slice(), value]
            .into_iter()
            .filter(|bytes| !bytes.is_empty())
        {
            brief.push(' ');
            brief.push_str(&String::from_utf8_lossy(bytes));
        }
        if self.cost.is_some() {
            brief.push_str(" charged");
        }
        brief
    }
}

fn sizes(children_sizes: &ChildrenSizesWithIsSumTree) -> Option<Sizes> {
    children_sizes.as_ref().map(|(feature, left, right)| {
        let feature = feature.as_ref().map(|(kind, length)| {
            let kind = match kind {
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes => "varint as 8",
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes => "two varints as 16",
                TreeCostType::TreeFeatureUses16Bytes => "16",
                TreeCostType::TreeFeatureUsesVarIntCostAs8BytesAnd16Bytes => "varint as 8 and 16",
            };
            (kind, *length)
        });
        (feature, *left, *right)
    })
}

/// What every answered call costs on top of what the wrapped context charged.
pub(crate) fn one_call() -> OperationCost {
    OperationCost {
        hash_node_calls: 1,
        ..Default::default()
    }
}

/// What a call that was told to fail costs.
pub(crate) fn failed_call() -> OperationCost {
    OperationCost {
        seek_count: 7,
        storage_loaded_bytes: 11,
        hash_node_calls: 1,
        ..Default::default()
    }
}

/// The record of calls, shared by a context with its batches and cursors and
/// with the test that reads it.
#[derive(Clone, Default)]
pub(crate) struct Tape {
    calls: Rc<RefCell<Vec<Call>>>,
    cost: Rc<RefCell<OperationCost>>,
    failing: Rc<Cell<Option<&'static str>>>,
}

/// The calls [`Tape::fail`] can make fail: those of the context that return
/// a result.
const CAN_FAIL: [&str; 13] = [
    "put",
    "put_aux",
    "put_root",
    "put_meta",
    "delete",
    "delete_aux",
    "delete_root",
    "delete_meta",
    "get",
    "get_aux",
    "get_root",
    "get_meta",
    "commit_batch",
];

impl Tape {
    /// Wraps `inner` in a context that records on this tape.
    pub(crate) fn recording<S>(&self, inner: S) -> Recording<S> {
        Recording {
            inner,
            tape: self.clone(),
        }
    }

    /// The calls made since the last time they were taken.
    pub(crate) fn take(&self) -> Vec<Call> {
        self.calls.take()
    }

    /// [`Call::brief`] of the calls made since the last time they were
    /// taken.
    pub(crate) fn take_brief(&self) -> Vec<String> {
        self.take().iter().map(Call::brief).collect()
    }

    /// The costs the recorded calls returned since the last time they were
    /// taken, added up.
    pub(crate) fn take_cost(&self) -> OperationCost {
        self.cost.take()
    }

    /// Makes every later call of this name fail, or none.
    pub(crate) fn fail(&self, name: Option<&'static str>) {
        assert!(
            name.is_none_or(|name| CAN_FAIL.contains(&name)),
            "{name:?} cannot be made to fail"
        );
        self.failing.set(name);
    }

    fn record(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }

    fn returned<T>(&self, returned: CostContext<T>) -> CostContext<T> {
        *self.cost.borrow_mut() += returned.cost.clone();
        returned
    }
}

pub(crate) struct Recording<S> {
    inner: S,
    tape: Tape,
}

impl<S> Recording<S> {
    fn answer<T>(
        &self,
        call: Call,
        pass_on: impl FnOnce(&S) -> CostResult<T, Error>,
    ) -> CostResult<T, Error> {
        let name = call.name;
        self.tape.record(call);
        self.tape
            .returned(if self.tape.failing.get() == Some(name) {
                Err(Error::StorageError(format!("{name} failed"))).wrap_with_cost(failed_call())
            } else {
                pass_on(&self.inner).add_cost(one_call())
            })
    }
}

macro_rules! recorded_puts {
    ($($name:ident)*) => {$(
        fn $name<K: AsRef<[u8]>>(
            &self,
            key: K,
            value: &[u8],
            cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), Error> {
            let call = Call {
                cost: cost_info.clone(),
                ..call(stringify!($name), key.as_ref(), value)
            };
            self.answer(call, |inner| inner.$name(key, value, cost_info))
        }
    )*};
}

macro_rules! recorded_deletes {
    ($($name:ident)*) => {$(
        fn $name<K: AsRef<[u8]>>(
            &self,
            key: K,
            cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), Error> {
            let call = Call {
                cost: cost_info.clone(),
                ..call(stringify!($name), key.as_ref(), b"")
            };
            self.answer(call, |inner| inner.$name(key, cost_info))
        }
    )*};
}

macro_rules! recorded_gets {
    ($($name:ident)*) => {$(
        fn $name<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
            self.answer(call(stringify!($name), key.as_ref(), b""), |inner| {
                inner.$name(key)
            })
        }
    )*};
}

impl<'db, S: StorageContext<'db>> StorageContext<'db> for Recording<S> {
    type Batch = RecordingBatch<S::Batch>;
    type RawIterator = RecordingCursor<S::RawIterator>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let call = Call {
            sizes: sizes(&children_sizes),
            cost: cost_info.clone(),
            ..call("put", key.as_ref(), value)
        };
        self.answer(call, |inner| {
            inner.put(key, value, children_sizes, cost_info)
        })
    }

    recorded_puts!(put_aux put_root put_meta);

    recorded_deletes!(delete delete_aux delete_root delete_meta);

    recorded_gets!(get get_aux get_root get_meta);

    fn new_batch(&self) -> Self::Batch {
        self.tape.record(call("new_batch", b"", b""));
        RecordingBatch {
            inner: self.inner.new_batch(),
            tape: self.tape.clone(),
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        self.answer(call("commit_batch", b"", b""), |inner| {
            inner.commit_batch(batch.inner)
        })
    }

    fn raw_iter(&self) -> Self::RawIterator {
        self.tape.record(call("raw_iter", b"", b""));
        RecordingCursor {
            inner: self.inner.raw_iter(),
            tape: self.tape.clone(),
        }
    }

    fn raw_iter_aux(&self) -> Self::RawIterator {
        self.tape.record(call("raw_iter_aux", b"", b""));
        RecordingCursor {
            inner: self.inner.raw_iter_aux(),
            tape: self.tape.clone(),
        }
    }
}

/// The batch of a [`Recording`] context.
pub(crate) struct RecordingBatch<B> {
    inner: B,
    tape: Tape,
}

macro_rules! recorded_batch_puts {
    ($prefix:literal $($name:ident)*) => {$(
        fn $name<K: AsRef<[u8]>>(
            &mut self,
            key: K,
            value: &[u8],
            cost_info: Option<KeyValueStorageCost>,
        ) -> Result<(), grovedb_costs::error::Error> {
            self.tape.record(Call {
                cost: cost_info.clone(),
                ..call(concat!($prefix, stringify!($name)), key.as_ref(), value)
            });
            self.inner.$name(key, value, cost_info)
        }
    )*};
}

macro_rules! recorded_batch_deletes {
    ($prefix:literal $($name:ident)*) => {$(
        fn $name<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
            self.tape.record(Call {
                cost: cost_info.clone(),
                ..call(concat!($prefix, stringify!($name)), key.as_ref(), b"")
            });
            self.inner.$name(key, cost_info)
        }
    )*};
}

impl<B: Batch> Batch for RecordingBatch<B> {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        self.tape.record(Call {
            sizes: sizes(&children_sizes),
            cost: cost_info.clone(),
            ..call("batch.put", key.as_ref(), value)
        });
        self.inner.put(key, value, children_sizes, cost_info)
    }

    recorded_batch_puts!("batch." put_aux put_root);

    recorded_batch_deletes!("batch." delete delete_aux delete_root);
}

/// The cursor of a [`Recording`] context.
pub(crate) struct RecordingCursor<I> {
    inner: I,
    tape: Tape,
}

macro_rules! recorded_moves {
    ($prefix:literal $($name:ident)*) => {$(
        fn $name(&mut self) -> CostContext<()> {
            self.tape.record(call(concat!($prefix, stringify!($name)), b"", b""));
            self.tape.returned(self.inner.$name().add_cost(one_call()))
        }
    )*};
}

macro_rules! recorded_seeks {
    ($prefix:literal $($name:ident)*) => {$(
        fn $name<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
            self.tape.record(call(concat!($prefix, stringify!($name)), key.as_ref(), b""));
            self.tape.returned(self.inner.$name(key).add_cost(one_call()))
        }
    )*};
}

impl<I: RawIterator> RawIterator for RecordingCursor<I> {
    recorded_moves!("cursor." seek_to_first seek_to_last next prev);

    recorded_seeks!("cursor." seek seek_for_prev);

    fn value(&self) -> CostContext<Option<&[u8]>> {
        self.tape.record(call("cursor.value", b"", b""));
        self.tape.returned(self.inner.value().add_cost(one_call()))
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        self.tape.record(call("cursor.key", b"", b""));
        self.tape.returned(self.inner.key().add_cost(one_call()))
    }

    fn valid(&self) -> CostContext<bool> {
        self.tape.record(call("cursor.valid", b"", b""));
        self.tape.returned(self.inner.valid().add_cost(one_call()))
    }

    fn status(&self) -> Result<(), Error> {
        self.tape.record(call("cursor.status", b"", b""));
        self.inner.status()
    }
}
