# Unsafe code in LigninDB: the audit

The rule is DEC-07 of the plan (dashpay/grovedb#1052): unsafe code is confined to the file layer (`lignin-io`) and the buffer pool (`lignin-store::cache`), which carry Miri and loom coverage, and every other LigninDB crate forbids it. This document is the audit of that rule (task VER-06): where the unsafe code is, what each piece of it rests on, which test reaches it, and what keeps all of that true after the audit.

A test reads this document and the sources and fails when they differ: `lignin-store/tests/unsafe_audit.rs`. The two tables between the `inventory` and `allocators` markers below are its input, so they are changed with the code, in the same commit.

## 1. Where the unsafe code is

| Place | Unsafe code | Lint |
| --- | --- | --- |
| `lignin-store/src/cache/frame.rs` | All of the buffer pool's: 38 blocks, 5 unsafe functions, 3 pairs of `unsafe impl Send` and `Sync`. | `lignin-store` has `#![deny(unsafe_code)]`, and `cache/mod.rs` has the one `#[allow(unsafe_code)]`, on `mod frame`. |
| `lignin-io` | None. The file layer is written against `std` alone. | `#![deny(unsafe_code)]`, so that a later module (the direct I/O path, IO-03) can allow it for itself. |
| `lignin-format`, `lignin-tree`, `lignindb`, `lignin-tools`, `lignin-testkit` and its `cost-model-storage` | None. | `#![forbid(unsafe_code)]` in each library. |
| `merk/src/store/lignin` | None. | Merk's own lints; the scan below covers the directory. |
| Nine test targets (section 5) | A counting allocator each: one `unsafe impl GlobalAlloc` that hands every call to `System`. | A test target is a crate of its own, which the library's lint does not reach. The scan below does. |

## 2. What keeps it so

`lignin-store/tests/unsafe_audit.rs` runs with the crate's tests, so in CI on every platform the LigninDB crates are tested on. It reads every `.rs` file under every workspace directory whose name begins with `lignin`, and under `merk/src/store/lignin`, with comments and string and character literals blanked, and checks:

1. **The keyword `unsafe` is in the files this document lists and in no other.** This holds for every target (libraries, binaries, tests, benches, build scripts), which a lint in a library does not.
2. **Every library forbids unsafe code**, the two of section 1 deny it, and the lint `unsafe_code` is named nowhere else but in the one `allow`, which is on `mod frame`.
3. **Every unsafe block and every `unsafe impl` has a comment with `SAFETY:` directly above its line**, and every `unsafe fn` that is not a trait's method has a `# Safety` section. `lignin-io` and `lignin-store` also deny `clippy::undocumented_unsafe_blocks`, so CI's clippy run says the same of their libraries.
4. **The unsafe code of `frame.rs` is, site for site and in order, the inventory of section 4.** A block that is added, removed or moved to another function fails the test until the inventory is changed, which is the moment to read section 4 again.
5. **Every test the inventory names exists**, has `#[test]`, and has no attribute that ignores it or leaves it out of a build.
6. **The places that hand out a way to reach a frame are as many as section 4.3 counts.** They are safe code that the unsafe blocks trust.
7. **Each counting allocator has the shape of section 5** and nothing more.
8. **Every file a crate compiles is one the scan read, or is held to the lint by the compiler**: a module with a `#[path]` names a file among those that were read, or is in a library that forbids unsafe code (the cost model's stand-in for `grovedb-storage` takes three of that crate's files this way), and no file is brought in with `include!`.

The scan reads source text. Code that a macro of another crate expands to (`thread_local!`, for one) is that crate's, and is not seen. The engine's crates depend on two crates outside the workspace, `crc32c` and `lz4_flex` (with its safe encoder and decoder), both in `lignin-format`.

What "a test that exercises it" means here: the named test executes the line of the unsafe block. That was measured once for this audit, test by test, with `-C instrument-coverage` (section 6), and every named test is deterministic and runs on one thread. The tests run under Miri as well (section 3), where executing the line is what lets Miri check it.

## 3. Miri

Miri is `cargo +nightly-2026-02-24 miri` (the newest toolchain the project's machines have with Miri). It interprets the tests and reports undefined behaviour: a read or write outside an allocation or after it was freed, a use of memory that was never written, a reference used against the borrow rules, a data race, a mismatched deallocation, and memory that is still allocated at the end.

| Run | Command | What it shows |
| --- | --- | --- |
| The buffer pool, Stacked Borrows | `cargo +nightly-2026-02-24 miri test -p lignin-store --lib cache::` | Every test of the pool, none ignored. |
| The buffer pool, Tree Borrows | the same with `MIRIFLAGS=-Zmiri-tree-borrows` | The same tests under the other model of the borrow rules. |
| The races, many schedules | `MIRIFLAGS=-Zmiri-many-seeds=0..64 cargo +nightly-2026-02-24 miri test -p lignin-store --lib cache::tests::races`, under both borrow models | Each race of `cache/tests/races.rs` under 64 of Miri's random schedules, with its data-race detector and its emulation of weak memory. |
| The buffer pool, stricter | the first with `MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-symbolic-alignment-check"` | No pointer is made from an integer, and no access relies on an alignment that an address only happens to have. |
| The pool measured at the allocator | `cargo +nightly-2026-02-24 miri test -p lignin-store --test cache_alloc` | The pool under a counting allocator, which is unsafe code of the tests (section 5). |
| The file layer | `MIRIFLAGS=-Zmiri-disable-isolation cargo +nightly-2026-02-24 miri test -p lignin-io`, under both borrow models | Every test of `lignin-io` that Miri can run. Isolation is off because the tests of the std backend use real temporary directories. |
| Another production platform | the first run and the file layer's with `--target aarch64-unknown-linux-gnu` | The same code as the other platform of DEC-06 compiles it. |

Miri ignores the tests of `lignin-io` that need what it does not emulate: hard links, inode numbers, `/dev/full`, a second filesystem and spawned processes. Each has `#[cfg_attr(miri, ignore = "...")]` with the reason, and they run in every normal test run. `lignin-io` has no unsafe code, so what Miri checks there is `std`'s use of the system and the crate's threads.

Results at the audited commit are in the task's report (the commands above, their test counts and their logs). The numbers of tests change as tests are added; what has to stay is "no error, none ignored" for the pool.

**What Miri does not show.** It runs the schedules it draws, not all of them, and it does not find an atomic order that is too weak unless a schedule it draws happens to show it. Rule A6 of `frame.rs` rests on `SeqCst` orders in `frame.rs` and `lanes.rs` (seven kinds of step, as STORE-04's second unit counted them). They are held by the loom models of `cache/tests/races.rs` (`RUSTFLAGS="--cfg loom" cargo test -p lignin-store --release --lib cache::tests::races`), where a weakened order fails a model. No test on real threads fails for one (STORE-04's planted faults), and a Miri run would only by the luck of its schedule. That is VER-05's ground, and section 4.4 says how far this audit went into it.

## 4. The buffer pool

### 4.1 The rules

The rules that make the unsafe code sound are stated once, at the top of `lignin-store/src/cache/frame.rs`, as the access rules A1 to A6, with the two arguments for an announced pin. They are not repeated here. In short: a frame is reached through its bucket's lock, through a pin, or by its owner (A1); a frame that is linked or pinned is not freed or given away (A2); ownership is exclusive and has a fixed list of beginnings (A3); content is written only with exclusive access (A4) and read only while it cannot be written (A5); and a pin is a count on the frame or an announcement in a lane (A6).

Every unsafe block names the rule it relies on. The types of the file stand for the ways of being allowed to reach a frame (`ChainRead`, `ChainWrite`, `Entry`, `Pinned`, `Frozen`, `Shared`, `Exclusive`, `OwnedFrame`), and the rest of the pool is safe code over them.

### 4.2 The inventory

"Rests on" is the rule the block names, or what else makes it sound. Test names are as `cargo test` prints them, without the leading `cache::`.

<!-- frame:start -->
| Site | Kind | Rests on | What it does, and why that is allowed | Exercised by |
| --- | --- | --- | --- | --- |
| `impl Send for Frame` | unsafe impl | `A4`, `A5` | A header is atomics, a lock and fields that never change. The content behind `bytes` and `attached` is reached through the four unsafe methods below only. | `tests::basic::the_handles_of_the_pool_can_go_to_other_threads`, `tests::threads::threads_read_what_was_committed` |
| `impl Sync for Frame` | unsafe impl | `A4`, `A5` | As above. | `tests::basic::the_handles_of_the_pool_can_go_to_other_threads`, `tests::threads::threads_read_what_was_committed` |
| `Frame::bytes` | unsafe fn | `A5` | The frame's bytes as a shared slice. The caller says that nothing writes them while the slice lives. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Frame::bytes` | block 1 | `A5` | `from_raw_parts` over `len` bytes that were zeroed when they were allocated and live as long as the frame. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Frame::bytes_mut` | unsafe fn | `A4` | The frame's bytes as an exclusive slice. The caller says that it alone reaches them. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Frame::bytes_mut` | block 1 | `A4` | `from_raw_parts_mut` over the same bytes. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Frame::attached` | unsafe fn | `A5` | The attached values, shared. As `bytes`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `Frame::attached` | block 1 | `A5` | A shared reference into the `UnsafeCell`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `Frame::attached_mut` | unsafe fn | `A4` | The attached values, to change. As `bytes_mut`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `Frame::attached_mut` | block 1 | `A4` | An exclusive reference into the `UnsafeCell`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `impl Send for FramePtr` | unsafe impl | `A1` | The pointer is an address. What it points to is used through `FramePtr::frame` only. | `tests::basic::the_handles_of_the_pool_can_go_to_other_threads`, `tests::threads::threads_read_what_was_committed` |
| `impl Sync for FramePtr` | unsafe impl | `A1` | As above. | `tests::basic::the_handles_of_the_pool_can_go_to_other_threads`, `tests::threads::threads_read_what_was_committed` |
| `FramePtr::frame` | unsafe fn | `A1` | Follows the pointer. The caller says which of the three ways of A1 it has, and for how long. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `FramePtr::frame` | block 1 | `A1`, `A2` | The pointer came from the arena's headers or from `Arena::allocate_overflow`, and the frame is still there. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `OwnedFrame::header` | block 1 | `A1` | The owner reaches its frame. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `OwnedFrame::bytes` | block 1 | `A5` | The owner reads, and the slice borrows the ownership. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `OwnedFrame::bytes_mut` | block 1 | `A4` | The owner writes, and the slice borrows the ownership mutably. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `OwnedFrame::attached` | block 1 | `A5` | As `OwnedFrame::bytes`. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `OwnedFrame::attached_mut` | block 1 | `A4` | As `OwnedFrame::bytes_mut`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `OwnedFrame::reset` | block 1 | `A4` | The owner clears the attached values of a frame it makes free. | `tests::basic::a_blank_beside_the_fixed_frames_is_given_back_when_dropped` |
| `Pinned::frame` | block 1 | `A1`, `A2` | The holder of a pin reaches the frame, for as long as it borrows the pin. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Pinned::unpin` | block 1 | `A1`, `A6` | Gives the pin back. After that only the header of an arena frame is read, which anyone may read: a counted pin on a frame beside the arena is refused before. | `tests::basic::a_hit_in_a_fixed_frame_is_pinned_without_a_count`, `tests::basic::a_frame_that_was_taken_out_under_a_hit_is_free_when_the_hit_is_dropped` |
| `Frozen::bytes` | block 1 | `A5` | A reader's bytes, with no latch: the frame was a page version when the pin became a `Frozen`, and a pinned page version has no writer. | `tests::basic::a_hit_in_a_fixed_frame_is_pinned_without_a_count` |
| `Frozen::attached` | block 1 | `A5` | As `Frozen::bytes`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `Shared::bytes` | block 1 | `A5` | A private copy's bytes under its shared latch, which keeps the exclusive latch out. | `tests::private::the_latch_lets_one_writer_or_many_readers_in` |
| `Shared::attached` | block 1 | `A5` | As `Shared::bytes`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `Exclusive::bytes` | block 1 | `A5` | A private copy's bytes under its exclusive latch. | `tests::private::the_latch_lets_one_writer_or_many_readers_in` |
| `Exclusive::bytes_mut` | block 1 | `A4` | The same, to write: the frame was private when the latch was taken, and stays so while it is held. | `tests::private::the_latch_lets_one_writer_or_many_readers_in` |
| `Exclusive::attached` | block 1 | `A5` | As `Exclusive::bytes`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `Exclusive::attached_mut` | block 1 | `A4` | As `Exclusive::bytes_mut`. | `tests::private::attached_values_travel_with_the_copy_and_the_commit` |
| `Entries::next` | block 1 | `A1`, `A2` | Follows a pointer of a chain that is borrowed under its lock, or by `Chain::take_all`, which alone can reach it. | `tests::basic::a_miss_is_loaded_and_then_hit`, `frame::tests::a_chain_gives_its_frames_up_to_the_arena_they_are_of` |
| `Links::unlink_from` | block 1 | `A1`, `A2` | Walks a chain to take a frame out, under the chain's lock for writing. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Links::unlink_from` | block 2 | `A1`, `A2` | The frame before the one that is taken out, which is linked in the same chain. | `tests::step::a_page_step_puts_a_new_frame_in_place_of_the_current_version` |
| `Chain::pin_unlocked` | block 1 | `A1` | The walk of a reader with no lock: follows a pointer that was just checked to lie among the arena's headers, which live as long as the arena. | `tests::basic::a_hit_in_a_fixed_frame_is_pinned_without_a_count`, `tests::basic::a_read_with_no_lock_takes_page_versions_only` |
| `ChainWrite::link` | block 1 | `A1` | The frame that was just linked, for as long as the chain stays borrowed. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `ChainWrite::retire` | block 1 | `A2` | The frame that was just unlinked under the lock. It is not used after it is marked retired, from when another thread can own it. | `tests::step::a_page_step_puts_a_new_frame_in_place_of_the_current_version`, `tests::private::a_copy_beside_the_fixed_frames_is_freed_when_it_is_discarded` |
| `allocate` | block 1 | the allocator's contract | `alloc_zeroed` with a layout whose size was just checked to be above zero. Zeroed, because a frame's bytes are handed out as a slice before anything is written to them. | `tests::basic::a_frame_can_be_read_before_anything_is_written_to_it`, `tests::basic::a_hit_in_a_frame_beside_the_fixed_ones_is_counted` |
| `impl Send for Arena` | unsafe impl | `A1`, `A2` | The arena owns the headers and the block, which are reached through frames only. | `tests::basic::the_handles_of_the_pool_can_go_to_other_threads`, `tests::threads::threads_read_what_was_committed` |
| `impl Sync for Arena` | unsafe impl | `A1`, `A2` | As above. | `tests::basic::the_handles_of_the_pool_can_go_to_other_threads`, `tests::threads::threads_read_what_was_committed` |
| `Arena::new` | block 1 | pointer arithmetic inside one allocation | The start of frame `index` in the block: `index * frame_len` is below the block's size, which was computed with a checked multiplication. | `tests::basic::a_miss_is_loaded_and_then_hit`, `tests::basic::every_page_size_of_a_store_is_served` |
| `Arena::bury` | block 1 | `A1` | Reads the header of an arena frame that was just retired. | `tests::basic::a_frame_that_was_taken_out_under_a_hit_is_free_when_the_hit_is_dropped` |
| `Arena::take_dead` | block 1 | `A1` | Walks the list of dead frames, which are arena frames, under that list's lock. | `tests::basic::a_dead_frame_whose_last_pin_went_is_taken_before_one_is_allocated` |
| `Arena::take_dead` | block 2 | `A1` | The dead frame before the one that is taken off the list. | `tests::basic::a_dead_frame_is_made_free_while_another_still_waits` |
| `Arena::frame` | block 1 | `A1`, `A2` | The header of an arena frame by its number, for the clock. | `tests::basic::a_frame_the_clock_could_not_take_keeps_its_mark` |
| `Arena::pointer` | block 1 | pointer arithmetic inside one allocation | The address of header `index`, which was just checked to be below the count. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Arena::free_overflow` | block 1 | `A3` | Takes a frame beside the arena back into the box it came out of. The frame is the arena's own and is beside it, both checked, and its owner gives it up. | `tests::basic::a_blank_beside_the_fixed_frames_is_given_back_when_dropped`, `tests::basic::a_pool_that_is_dropped_frees_what_is_beside_its_fixed_frames` |
| `Arena::free_overflow` | block 2 | the allocator's contract | Frees that frame's bytes with the layout they were allocated with. | `tests::basic::a_blank_beside_the_fixed_frames_is_given_back_when_dropped`, `tests::basic::a_pool_that_is_dropped_frees_what_is_beside_its_fixed_frames` |
| `Arena::drop` | block 1 | `A2` | Takes the headers back into the boxed slice they came out of. Nothing can follow a pointer to a header any more: every pin borrows the arena. | `tests::basic::a_miss_is_loaded_and_then_hit` |
| `Arena::drop` | block 2 | the allocator's contract | Frees the block with the layout it was allocated with, after the headers that pointed into it. | `tests::basic::a_miss_is_loaded_and_then_hit` |
<!-- frame:end -->

### 4.3 The safe code the unsafe blocks trust

An unsafe block is as sound as the claim of its caller. In `frame.rs` the claims are made by values of types that only this file can make, so the places that make them are part of the audit though none of them is unsafe code. The test counts them; a new one fails it.

<!-- trusted:start -->
| What is made | Places in `frame.rs` | Each one is allowed because |
| --- | --- | --- |
| `OwnedFrame(` | 8 | It is one of the beginnings of ownership that A3 lists, and says which in a comment: `Arena::new` and `Arena::allocate_overflow` (nothing has seen the frame), `ChainWrite::take_unpinned` (unlinked with no pin), `ChainWrite::retire` (a frame beside the arena, retired with no pin), `ChainWrite::unpin_linked` (the last pin of a retired frame), `Arena::bury` and `Arena::take_dead` (a retired arena frame with no pin, under the dead list's lock), `Chain::take_all` (the pool is going away). |
| `Pinned {` | 3 | `Entry::pin` counts under the chain's lock on a frame it has checked to be of the arena whose borrow the pin carries. `Pinned::again` counts under a pin. `Chain::pin_unlocked` makes an announcement a pin only when the bucket's count of changes was the same before and after. |
| `Frozen {` | 4 | Each is made of a frame that is a page version at that moment, under a pin or the chain's lock: `Pinned::into_frozen`, `Entry::freeze`, `Chain::pin_unlocked`. `Frozen::again` is a second pin on content that is frozen already. |
| `Exclusive::new(` | 2 | `Pinned::write` and `Pinned::try_write`, with the frame's latch held for writing. It gives nothing for a frame that is not a private copy. |
| `Shared::new(` | 2 | `Pinned::read` and `Pinned::try_read`, with the frame's latch held for reading. |
| `set_kind(` | 4 | What a frame is decides who may write it. It changes for an owned frame (`OwnedFrame::set_kind`, `OwnedFrame::reset`), under the exclusive latch of a private copy (`Exclusive::publish`), and when a chain retires a frame (`ChainWrite::retire`). The method itself is private to the file. |
<!-- trusted:end -->

Three things outside `frame.rs` carry the rules, and were read for this audit:

- **`cache/lanes.rs`** (safe code): `Lanes::announces` finds every announcement that was made before it looked. Rule A6 rests on it as on a lock.
- **`BufferPool::unpin`** (`cache/pool.rs`) is the one caller of `ChainWrite::unpin_linked`, and gives it the lock of the bucket that the frame's page is in. That is what lets a frame beside the arena leave in the same step as its last pin. Given another bucket's lock, the frame would stay in the table with no pin, against the pool's memory bound; no memory would be freed that anyone can reach.
- **Every chain of a pool is given that pool's arena**, at every call (`cache/pool.rs`, `cache/write.rs`, `cache/flush.rs`). `ChainWrite::link`, `Entry::pin`, `Arena::free_overflow` and, since this audit, `Chain::take_all` each check that the frame is of the arena they are given, so a mistake there is a panic and not a frame freed under a pin.

### 4.4 What was read, and what was found

Every unsafe block was read against the rule it names, together with the callers that make its claim true: all of `cache/frame.rs`, `lanes.rs`, `sync.rs`, `pool.rs`, `guard.rs`, `write.rs`, and the parts of `flush.rs` that reach a frame.

No unsound block was found. What was found and done:

1. **`Chain::take_all` took its caller's word for the arena.** It makes every frame of a chain owned on the ground that pins borrow the arena, which is borrowed mutably; that says nothing of a frame of another arena. The pool always passes its own, so nothing was wrong, but the function is safe to call and its soundness was the caller's. It now refuses a chain that holds a frame of another arena, before it unlinks anything (`frame::tests::a_chain_is_not_emptied_for_another_arena`).
2. **A frame's bytes are read before they are written**, by design: a `Blank` is a slice from its first moment. That is sound only because the block and every frame beside it are allocated zeroed. No test read a frame first, so a change to `alloc` would have passed every test and Miri run. `tests::basic::a_frame_can_be_read_before_anything_is_written_to_it` now does, for a fixed frame and for one beside them.
3. **`Send` and `Sync` of the pool's handles were stated by no test.** The three pairs of `unsafe impl` are what lets a pool and its handles cross threads, and a removed or weakened one shows only as a build error far from the cause. `tests::basic::the_handles_of_the_pool_can_go_to_other_threads` states it for each public type.
4. **One branch was reached by accident only**: taking a dead frame from behind another dead frame (`Arena::take_dead`, block 2) was executed by one deterministic test, which is about something else. `tests::basic::a_dead_frame_is_made_free_while_another_still_waits` is the test for it.
5. **The counting allocators of nine test targets had no safety comment.** Each has one now (section 5).
6. **Binaries, integration tests and build scripts are crates of their own**, which a library's `#![forbid(unsafe_code)]` does not reach. The scan of section 2 does.

What this audit did not do, and where it is done:

- **The `SeqCst` orders of A6.** The arguments in `frame.rs` ("why an announcement holds", "why what the announcer found is so") were read and followed step by step, and hold as written, on the orders the code has. That they are needed, each of them, is shown by the loom models and the planted faults of STORE-04's second unit (a weakened order fails a model). Exhaustive models and a ThreadSanitizer run are VER-05's.
- **References that are alive while a frame is freed.** A `&Frame` argument is live until its function returns. For a frame beside the arena, the functions that can be the last to touch it (`Frame::unpin`, `Frame::retire`) run under the lock of the frame's bucket, and whoever frees the frame needs that lock or is the same thread, so no frame is freed under a live reference. Miri's two borrow models check exactly this, and report nothing.

## 5. The counting allocators

Tests that promise "this path allocates nothing" measure it at the allocator. Each such test target has its own `CountingAllocator`: an `unsafe impl GlobalAlloc` of four methods, each of which counts in a thread-local cell and hands its arguments to the same method of `System`. They are test targets, outside their crates' libraries, which keep `#![forbid(unsafe_code)]` (or, for `lignin-store`, the one `allow` of section 1).

<!-- allocators:start -->
| Test target | A test that runs under the allocator |
| --- | --- |
| `lignin-format/tests/delta_alloc.rs` | `reading_a_run_allocates_nothing` |
| `lignin-format/tests/flat_page_alloc.rs` | `reading_a_leaf_does_not_allocate` |
| `lignin-format/tests/tree_page_alloc.rs` | `reading_a_tree_page_allocates_nothing` |
| `lignin-format/tests/wal_alloc.rs` | `reading_a_log_file_allocates_nothing` |
| `lignin-store/tests/cache_alloc.rs` | `the_read_path_allocates_nothing` |
| `lignin-store/tests/load_alloc.rs` | `a_load_with_grown_buffers_allocates_nothing` |
| `lignin-tree/tests/cursor_alloc.rs` | `a_cursor_that_has_grown_allocates_nothing` |
| `lignin-tree/tests/hash_alloc.rs` | `an_evaluation_allocates_for_its_lists_and_not_for_each_node` |
| `lignin-tree/tests/node_alloc.rs` | `reading_a_node_allocates_nothing` |
<!-- allocators:end -->

The safety of each is the same and is stated in the file: the memory is what `System` gives and takes back, under the contract the caller keeps, and counting allocates nothing and does not unwind.

## 6. How the tests were matched to the blocks

Once, for this audit: the tests of `lignin-store::cache` were built with `-C instrument-coverage`, each test was run alone, and the lines of `frame.rs` it executed were listed. Every unsafe block and unsafe function was executed by at least 2 tests and at most 87, before the tests of section 4.4 were added. The tests named in the inventory were chosen from those lists: deterministic, on one thread, and about the thing the block does.

CI's coverage run (`cargo llvm-cov`) reports `frame.rs` with every other file, so a block that loses its last test shows there as an uncovered line.

## 7. Adding unsafe code

The plan allows it in two places.

- **In the buffer pool**: in `cache/frame.rs`, under the access rules, with a `SAFETY:` comment that names the rule, a row in the inventory of section 4.2, and a test that executes it and runs under Miri. A new way to make an `OwnedFrame`, a `Pinned`, a `Frozen` or a latch changes section 4.3.
- **In the file layer** (the direct I/O path, IO-03): in a module of its own with `#[allow(unsafe_code)]` on it. The test of section 2 then has to be told of the file: a second inventory in this document, a second `allow` in the test's list, and Miri runs for whatever of it Miri can interpret. System calls it cannot interpret are the part that has to be argued in the safety comments and tested on the real system.

Anywhere else, the answer is no (DEC-07), and the test says so.
