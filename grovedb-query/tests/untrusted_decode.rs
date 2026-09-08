//! Public query types retain their custom wire format and validation under untrusted decoding.
use bincode::{config::Config, error::DecodeError, BorrowDecodeUntrusted, DecodeUntrusted, Encode};
use grovedb_query::{
    AggregateFold, AggregateSumQuery, AxisQuery, AxisTraversal, IndexAxis, Query, QueryItem,
    ReadMode, SubqueryBranch, SumBudgetRead,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    fmt::Debug,
};

#[derive(Clone, Copy, Default)]
struct AllocationState {
    active: bool,
    fail: bool,
    largest: usize,
}

thread_local! {
    static ALLOCATIONS: Cell<AllocationState> = const { Cell::new(AllocationState {
        active: false, fail: false, largest: 0,
    }) };
}

fn record_allocation(size: usize) -> bool {
    ALLOCATIONS
        .try_with(|cell| {
            let mut state = cell.get();
            if !state.active {
                return false;
            }
            state.largest = state.largest.max(size);
            cell.set(state);
            state.fail
        })
        .unwrap_or(false)
}

struct ObservedAllocator;

// The observer only records sizes in allocation-free thread-local cells. All
// pointers and layouts are forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for ObservedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if record_allocation(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if record_allocation(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if record_allocation(size) {
            std::ptr::null_mut()
        } else {
            unsafe { System.realloc(ptr, layout, size) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

fn observe<T>(fail: bool, f: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ALLOCATIONS.with(|cell| cell.set(AllocationState::default()));
        }
    }
    ALLOCATIONS.with(|cell| {
        assert!(!cell.get().active);
        cell.set(AllocationState {
            active: true,
            fail,
            largest: 0,
        });
    });
    let reset = Reset;
    let result = f();
    let largest = ALLOCATIONS.with(|cell| cell.get().largest);
    drop(reset);
    (result, largest)
}

fn round_trip<
    T: Encode + DecodeUntrusted<()> + for<'de> BorrowDecodeUntrusted<'de, ()> + PartialEq + Debug,
    C: Config + Copy,
>(
    value: &T,
    config: C,
) {
    let bytes = bincode::encode_to_vec(value, config).unwrap();
    let (owned, used): (T, _) = bincode::decode_from_slice_untrusted(&bytes, config).unwrap();
    assert_eq!(&owned, value);
    assert_eq!(used, bytes.len());
    let (borrowed, used): (T, _) =
        bincode::borrow_decode_from_slice_untrusted(&bytes, config).unwrap();
    assert_eq!(&borrowed, value);
    assert_eq!(used, bytes.len());
    let streamed: T =
        bincode::decode_from_std_read_untrusted(&mut bytes.as_slice(), config).unwrap();
    assert_eq!(&streamed, value);
}

fn reject<T: DecodeUntrusted<()> + for<'de> BorrowDecodeUntrusted<'de, ()>>(bytes: &[u8]) {
    let config = bincode::config::standard();
    assert!(bincode::decode_from_slice_untrusted::<T, _>(bytes, config).is_err());
    assert!(bincode::borrow_decode_from_slice_untrusted::<T, _>(bytes, config).is_err());
    let mut reader = bytes;
    assert!(bincode::decode_from_std_read_untrusted::<T, _, _>(&mut reader, config).is_err());
}

#[test]
fn query_family_round_trips_with_existing_wire_formats() {
    let mut nested = Query::new_single_key(b"leaf".to_vec());
    nested.limit = Some(7);
    let mut query = Query::new_range_full();
    query.set_subquery_path(vec![b"child".to_vec()]);
    query.set_subquery(nested);
    query.add_conditional_subquery(
        QueryItem::Key(b"key".to_vec()),
        Some(vec![b"conditional".to_vec()]),
        Some(Query::new_single_key(b"result".to_vec())),
    );
    let standard = bincode::config::standard();
    round_trip(&query, standard);
    round_trip(
        &query,
        standard
            .with_big_endian()
            .with_fixed_int_encoding()
            .with_limit::<1048576>(),
    );
    round_trip(&query.default_subquery_branch, standard);
    round_trip(
        &AggregateSumQuery::new_single_key(b"item".to_vec(), 100),
        standard,
    );
    for mode in [
        ReadMode::Axis(AxisQuery::top_k(IndexAxis::Sum, 10, 5, true)),
        ReadMode::SumBudget(SumBudgetRead {
            sum_limit: 42,
            match_limit: Some(3),
        }),
    ] {
        round_trip(&mode, standard.with_fixed_int_encoding());
        let mut query = Query::new_range_full();
        query.read_mode = Some(Box::new(mode));
        round_trip(&query, standard);
        query.limit = Some(5);
        round_trip(&query, standard);
    }
    for traversal in [
        AxisTraversal::RankedPage { k: 3, offset: 7 },
        AxisTraversal::Bounded {
            lo: -8,
            hi: 20,
            limit: 5,
        },
        AxisTraversal::RankOfKey {
            key: b"key".to_vec(),
        },
        AxisTraversal::AggregateOverValueRange {
            lo: 0,
            hi: 30,
            fold: AggregateFold::Total,
        },
    ] {
        round_trip(
            &traversal,
            standard.with_big_endian().with_fixed_int_encoding(),
        );
    }
    round_trip(
        &SumBudgetRead {
            sum_limit: 100,
            match_limit: None,
        },
        standard,
    );
    round_trip(&AxisQuery::top_k(IndexAxis::Count, 2, 0, false), standard);
    let bytes = bincode::encode_to_vec(&query, standard).unwrap();
    let (context, _): (Query, _) =
        bincode::decode_from_slice_untrusted_with_context(&bytes, standard, 17u8).unwrap();
    assert_eq!(context, query);
}

#[test]
fn every_query_item_variant_preserves_its_byte_tag() {
    let range = || Box::new(QueryItem::RangeFull(..));
    let values = [
        QueryItem::Key(vec![1]),
        QueryItem::Range(vec![1]..vec![2]),
        QueryItem::RangeInclusive(vec![1]..=vec![2]),
        QueryItem::RangeFull(..),
        QueryItem::RangeFrom(vec![1]..),
        QueryItem::RangeTo(..vec![2]),
        QueryItem::RangeToInclusive(..=vec![2]),
        QueryItem::RangeAfter(vec![1]..),
        QueryItem::RangeAfterTo(vec![1]..vec![2]),
        QueryItem::RangeAfterToInclusive(vec![1]..=vec![2]),
        QueryItem::AggregateCountOnRange(range()),
        QueryItem::AggregateSumOnRange(range()),
        QueryItem::AggregateCountAndSumOnRange(range()),
    ];
    for (tag, value) in values.iter().enumerate() {
        let config = bincode::config::standard().with_fixed_int_encoding();
        assert_eq!(bincode::encode_to_vec(value, config).unwrap()[0], tag as u8);
        round_trip(value, config);
    }
}

#[test]
fn manual_query_collections_do_not_allocate_from_headers() {
    let config = bincode::config::standard();
    let mut items = vec![1];
    items.extend(bincode::encode_to_vec(65536u64, config).unwrap());
    let mut branches = vec![1, 0, 0, 0, 1];
    branches.extend(bincode::encode_to_vec(1024u64, config).unwrap());
    for bytes in [items, branches] {
        let (ordinary, ordinary_allocation) = observe(false, || {
            bincode::decode_from_slice::<Query, _>(&bytes, config)
        });
        assert!(ordinary.is_err());
        assert!(
            ordinary_allocation >= 65536,
            "control should still eagerly reserve"
        );
        let (ordinary_borrowed, borrowed_allocation) = observe(false, || {
            bincode::borrow_decode_from_slice::<Query, _>(&bytes, config)
        });
        assert!(ordinary_borrowed.is_err());
        assert_eq!(borrowed_allocation, ordinary_allocation);
        let (owned, allocated) = observe(false, || {
            bincode::decode_from_slice_untrusted::<Query, _>(&bytes, config)
        });
        assert!(owned.is_err());
        assert!(allocated < 4096, "unbacked allocation: {allocated}");
        let (borrowed, allocated) = observe(false, || {
            bincode::borrow_decode_from_slice_untrusted::<Query, _>(&bytes, config)
        });
        assert!(borrowed.is_err());
        assert!(allocated < 4096, "unbacked allocation: {allocated}");
    }
}

#[test]
fn query_collections_honor_limits_and_report_reservation_failure() {
    let mut conditional = Query::new();
    conditional.add_conditional_subquery(QueryItem::RangeFull(..), None, None);
    for query in [Query::new_range_full(), conditional] {
        let bytes = bincode::encode_to_vec(&query, bincode::config::standard()).unwrap();
        let small = bincode::config::standard().with_limit::<32>();
        // Ordinary parsing retains its historical accounting; the new mode counts containers.
        assert!(bincode::decode_from_slice::<Query, _>(&bytes, small).is_ok());
        assert!(matches!(
            bincode::decode_from_slice_untrusted::<Query, _>(&bytes, small),
            Err(DecodeError::LimitExceeded)
        ));
        assert!(matches!(
            bincode::borrow_decode_from_slice_untrusted::<Query, _>(&bytes, small),
            Err(DecodeError::LimitExceeded)
        ));
        round_trip(&query, bincode::config::standard().with_limit::<4096>());
        let (result, _) = observe(true, || {
            bincode::decode_from_slice_untrusted::<Query, _>(&bytes, bincode::config::standard())
        });
        assert!(matches!(result, Err(DecodeError::LimitExceeded)));
        let (result, _) = observe(true, || {
            bincode::borrow_decode_from_slice_untrusted::<Query, _>(
                &bytes,
                bincode::config::standard(),
            )
        });
        assert!(matches!(result, Err(DecodeError::LimitExceeded)));
    }
}

#[test]
fn custom_validation_and_recursive_limits_remain_enforced() {
    for bytes in [&[0][..], &[3, 0], &[3, 128]] {
        reject::<Query>(bytes);
    }
    reject::<QueryItem>(&[13]);
    reject::<ReadMode>(&[2]);
    reject::<AxisTraversal>(&[9]);
    reject::<AxisQuery>(&[7, 0, 1, 0, 0]);
    let mut too_long = vec![2];
    too_long.extend(bincode::encode_to_vec(vec![0u8; 256], bincode::config::standard()).unwrap());
    reject::<AxisTraversal>(&too_long);
    // Fold tags are one byte even with fixed integer encoding.
    reject::<AxisTraversal>(&[3, 0, 0, 9]);
    for count in [65537u64, u64::MAX, 1u64 << 32] {
        let mut bytes = vec![1];
        bytes.extend(bincode::encode_to_vec(count, bincode::config::standard()).unwrap());
        reject::<Query>(&bytes);
    }
    let deep = [1, 0, 0, 1].repeat(100000);
    let aggregate_wrappers = [10, 11, 12].repeat(33334);
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            reject::<Query>(&deep);
            reject::<SubqueryBranch>(&deep[2..]);
            reject::<QueryItem>(&aggregate_wrappers);
        })
        .unwrap()
        .join()
        .unwrap();
    let mut valid_depth = Query::new_range_full();
    for _ in 0..64 {
        let mut parent = Query::new();
        parent.set_subquery(valid_depth);
        valid_depth = parent;
    }
    round_trip(&valid_depth, bincode::config::standard());
}
