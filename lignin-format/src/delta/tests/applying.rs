//! Applying a run, a chain and a consolidation: what each needs and what
//! each refuses.

use super::*;

/// A small page with cells in slots 1 to 6, in a frame, with its image.
fn small_page(id: u32, size: PageSize) -> (Vec<u8>, ModelPage) {
    let mut rng = Rng::new(0xA991 + u64::from(id));
    let mut model = ModelPage::new(id, 4);
    for raw in 1..=6 {
        model.cells.insert(raw, rng.cell(model.id, 100));
    }
    (frame_of(&model, size), model)
}

#[test]
fn a_run_is_applied_to_the_page_it_names() {
    let size = PageSize::MIN;
    let (mut frame, model) = small_page(7, size);
    let base = model.assemble();
    let hash = Op::Hash(1, HashField::Value, [9; 32]);
    let before = frame.clone();
    let refused = |found: u32, expected: u32| {
        Err(DeltaError::WrongPage {
            found: page_id(found),
            expected: page_id(expected),
        }
        .into())
    };

    // A run of another page than the frame holds.
    let other = run_of(page_id(8), None, std::slice::from_ref(&hash));
    let other = decode(&other);
    assert_eq!(other.apply(&mut frame, size, V1), refused(8, 7));
    assert!(frame == before);

    // In a chain, each run is compared with the page the chain is of, and
    // so is the base image.
    let own = run_of(page_id(7), None, std::slice::from_ref(&hash));
    let own = decode(&own);
    let creating = run_of(page_id(8), Some(0), &[]);
    let creating = decode(&creating);
    let mut image = Vec::new();
    for (page, chain, in_frame, from_base, changed) in [
        (7, vec![&other], refused(8, 7), refused(8, 7), false),
        // The frame holds another page than the chain is of. A frame says
        // so when the first run is applied, a base image at once.
        (8, vec![&other], refused(8, 7), refused(7, 8), false),
        (8, vec![&own], refused(7, 8), refused(7, 8), false),
        // A run that creates another page does not take the frame over.
        (7, vec![&creating], refused(8, 7), refused(8, 7), false),
        (7, vec![&own, &creating], refused(8, 7), refused(8, 7), true),
    ] {
        let mut frame = before.clone();
        assert_eq!(
            apply_chain(page_id(page), &mut frame, size, V1, chain.iter().copied()),
            in_frame
        );
        // Nothing of a run of another page was applied.
        assert_eq!(frame != before, changed);
        assert_eq!(
            consolidate(page_id(page), Some(&base), chain, V1, size, &mut image),
            from_base
        );
        assert!(image.is_empty());
    }
    // With no base image the first run says which page it creates, and the
    // chain is still the chain of one page.
    assert_eq!(
        consolidate(page_id(7), None, [&creating], V1, size, &mut image),
        refused(8, 7)
    );
    let hash_of_8 = run_of(page_id(8), None, std::slice::from_ref(&hash));
    assert_eq!(
        consolidate(
            page_id(8),
            None,
            [&creating, &own, &decode(&hash_of_8)],
            V1,
            size,
            &mut image
        ),
        refused(7, 8)
    );
    assert!(image.is_empty());

    own.apply(&mut frame, size, V1).expect("its page");
    assert!(frame != before);
    apply_chain(page_id(7), &mut frame, size, V1, [&own, &own]).expect("its page");
}

#[test]
fn a_run_that_creates_its_page_needs_no_page() {
    let size = PageSize::MIN;
    let cell = ModelCell::leaf(b"key", b"value");
    let bytes = run_of(page_id(9), Some(77), &[Op::Insert(2, cell.clone())]);
    let run = decode(&bytes);
    let mut expected = ModelPage::new(9, 77);
    expected.cells.insert(2, cell);

    // Whatever the frame held: nothing, noise, or another page.
    let (other, _) = small_page(8, size);
    let (same, _) = small_page(9, size);
    for mut frame in [
        vec![0; size.get()],
        Rng::new(1).bytes(size.get()),
        other,
        same,
    ] {
        run.apply(&mut frame, size, V1).expect("creates");
        assert_eq!(image_of(&frame), expected.assemble());
        let page = view(&frame);
        page.check(size).expect("a valid page");
        assert_eq!(page.departures(), 77);
    }

    // A frame is a buffer of the store's page size.
    let mut short = vec![0; size.get() - 1];
    assert_eq!(
        run.apply(&mut short, size, V1),
        Err(TreePageError::BufferTooSmall {
            len: size.get() - 1,
            needed: size.get(),
        }
        .into())
    );
    let hash = run_of(
        page_id(9),
        None,
        std::slice::from_ref(&Op::Hash(2, HashField::Value, [9; 32])),
    );
    assert_eq!(
        decode(&hash).apply(&mut short, size, V1),
        Err(TreePageError::BufferTooSmall {
            len: size.get() - 1,
            needed: size.get(),
        }
        .into())
    );

    // A run that does not create its page needs one: bytes that are no
    // page are refused before any change is made.
    let mut noise = vec![0; size.get()];
    assert_eq!(
        decode(&hash).apply(&mut noise, size, V1),
        Err(FormatError::UnknownRecordKind { code: 0 })
    );
    assert!(noise.iter().all(|&byte| byte == 0));
}

#[test]
fn a_refused_change_keeps_the_changes_before_it_and_makes_none_after_it() {
    let size = PageSize::MIN;
    let (mut frame, mut model) = small_page(7, size);
    let first = Op::Hash(1, HashField::Value, [9; 32]);
    let refused = Op::Remove(40);
    let last = Op::Hash(2, HashField::Value, [8; 32]);
    let bytes = run_of(model.id, None, &[first.clone(), refused, last]);

    assert_eq!(
        decode(&bytes).apply(&mut frame, size, V1),
        Err(TreePageError::SlotFree { slot: slot(40) }.into())
    );
    first.make(&mut model);
    assert_eq!(image_of(&frame), model.assemble());

    // In a chain, the runs after the refused one are not applied either.
    let (mut frame, mut model) = small_page(7, size);
    let good = run_of(model.id, None, std::slice::from_ref(&first));
    let bad = run_of(model.id, None, &[Op::Remove(40)]);
    let chain = [decode(&good), decode(&bad), decode(&good)];
    assert_eq!(
        apply_chain(model.id, &mut frame, size, V1, &chain),
        Err(TreePageError::SlotFree { slot: slot(40) }.into())
    );
    first.make(&mut model);
    assert_eq!(image_of(&frame), model.assemble());

    // A consolidation that is refused writes nothing.
    let (_, model) = small_page(7, size);
    let mut out = vec![1, 2, 3];
    assert_eq!(
        consolidate(
            model.id,
            Some(&model.assemble()),
            &chain,
            V1,
            size,
            &mut out
        ),
        Err(TreePageError::SlotFree { slot: slot(40) }.into())
    );
    assert_eq!(out, [1, 2, 3]);
}

#[test]
fn a_chain_with_no_base_must_create_its_page() {
    let size = PageSize::MIN;
    let hash = run_of(
        page_id(9),
        None,
        std::slice::from_ref(&Op::Hash(1, HashField::Value, [9; 32])),
    );
    let create = run_of(page_id(9), Some(3), &[]);
    let mut out = Vec::new();

    let none: [&DeltaRun<'_>; 0] = [];
    assert_eq!(
        consolidate(page_id(9), None, none, V1, size, &mut out),
        Err(DeltaError::MissingBase.into())
    );
    assert_eq!(
        consolidate(page_id(9), None, [&decode(&hash)], V1, size, &mut out),
        Err(DeltaError::MissingBase.into())
    );
    // A later run that creates the page does not stand in for the base of
    // the runs before it.
    assert_eq!(
        consolidate(
            page_id(9),
            None,
            [&decode(&hash), &decode(&create)],
            V1,
            size,
            &mut out
        ),
        Err(DeltaError::MissingBase.into())
    );
    assert!(out.is_empty());

    consolidate(page_id(9), None, [&decode(&create)], V1, size, &mut out).expect("creates");
    assert_eq!(out, ModelPage::new(9, 3).assemble());

    // A base image with no run is its own consolidation.
    let (_, model) = small_page(9, size);
    let base = model.assemble();
    let mut out = Vec::new();
    consolidate(page_id(9), Some(&base), none, V1, size, &mut out).expect("an image");
    assert_eq!(out, base);

    // A chain that creates its page again drops what the base held.
    let mut out = Vec::new();
    consolidate(
        page_id(9),
        Some(&base),
        [&decode(&create)],
        V1,
        size,
        &mut out,
    )
    .expect("creates");
    assert_eq!(out, ModelPage::new(9, 3).assemble());
}

#[test]
fn consolidation_takes_an_image_and_nothing_else() {
    let size = PageSize::MIN;
    let (mut frame, model) = small_page(7, size);
    let run = run_of(
        model.id,
        None,
        std::slice::from_ref(&Op::Hash(1, HashField::Value, [9; 32])),
    );
    let run = decode(&run);
    let mut out = vec![7; 4];
    let refused = |base: &[u8], size, out: &mut Vec<u8>| {
        let error =
            consolidate(page_id(7), Some(base), [&run], V1, size, out).expect_err("not an image");
        assert_eq!(*out, [7; 4]);
        error
    };

    // A page in a buffer that has unused bytes among its cells is not an
    // image.
    open(&mut frame, size).remove(slot(2)).expect("a cell");
    let page = view(&frame);
    assert!(page.unused_bytes() > 0);
    assert!(matches!(
        refused(page.as_bytes(), size, &mut out),
        FormatError::TreePage(TreePageError::NotCompact { .. })
    ));

    // Bytes that are cut short, and bytes that are no page.
    let image = model.assemble();
    assert_eq!(
        refused(&image[..image.len() - 1], size, &mut out),
        FormatError::Truncated {
            needed: image.len(),
            available: image.len() - 1,
        }
    );
    assert_eq!(
        refused(&[0; 64], size, &mut out),
        FormatError::UnknownRecordKind { code: 0 }
    );

    // An image that is longer than the store's pages.
    let (_, large) = full_page(0xB16, PageSize::DEFAULT);
    let large_image = large.assemble();
    let large_run = run_of(large.id, None, &[Op::FragmentRoot(1, true)]);
    let mut large_out = Vec::new();
    assert_eq!(
        consolidate(
            large.id,
            Some(&large_image),
            [&decode(&large_run)],
            V1,
            size,
            &mut large_out
        ),
        Err(TreePageError::PageTooLong {
            len: large_image.len(),
            capacity: size.get(),
        }
        .into())
    );
    assert!(large_out.is_empty());

    // The image itself is taken, and what follows it is ignored.
    let mut padded = image.clone();
    padded.extend([0xFF; 9]);
    let mut out = Vec::new();
    consolidate(model.id, Some(&padded), [&run], V1, size, &mut out).expect("an image");
    let mut expected = model.clone();
    Op::Hash(1, HashField::Value, [9; 32]).make(&mut expected);
    assert_eq!(out, expected.assemble());
}

/// A page that is full takes a change that needs no room and refuses one
/// that does, as its writer's page did.
#[test]
fn a_full_page_refuses_a_record_that_needs_room() {
    for size in SIZES {
        let (mut frame, model) = full_page(0xF011, size);
        let free = model.free_bytes(size);
        let cell = ModelCell::leaf(&[1; 200], &[2; 300]);
        assert!(free < cell.len() + 2);
        let raw = model.lowest_free_slot();
        let new_slots = usize::from(raw).saturating_sub(model.slot_count());
        let insert = run_of(model.id, None, &[Op::Insert(raw, cell.clone())]);
        let before = frame.clone();
        assert_eq!(
            decode(&insert).apply(&mut frame, size, V1),
            Err(TreePageError::PageFull {
                needed: cell.len() + 2 * new_slots,
                available: free,
            }
            .into())
        );
        assert!(frame == before);

        // Once a cell has left, the same record applies.
        let largest = model
            .cells
            .iter()
            .max_by_key(|(_, cell)| cell.len())
            .map(|(&raw, _)| raw)
            .expect("a cell");
        let remove = run_of(model.id, None, &[Op::Remove(largest)]);
        let mut expected = model.clone();
        let mut chain = vec![decode(&remove)];
        Op::Remove(largest).make(&mut expected);
        let fits = expected.free_bytes(size) >= cell.len() + 2 * new_slots;
        if fits && largest != raw {
            chain.push(decode(&insert));
            expected.cells.insert(raw, cell);
        }
        apply_chain(model.id, &mut frame, size, V1, &chain).expect("applies");
        assert_eq!(image_of(&frame), expected.assemble());
    }
}

#[test]
fn hosted_roots_are_the_same_list_given_or_read() {
    let given = [Some(address(8, 2)), None, Some(address(9, 1))];
    let list = RootList::from(given.as_slice());
    assert_eq!(list.len(), 3);
    assert!(!list.is_empty());
    assert_eq!(list.iter().len(), 3);
    assert_eq!(list.iter().collect::<Vec<_>>(), given);
    let none = RootList::from([].as_slice());
    assert!(none.is_empty());
    assert_eq!(none.iter().next(), None);

    let op = Op::HostedRoots(3, given.to_vec());
    let bytes = run_of(page_id(7), None, std::slice::from_ref(&op));
    let Some(Delta::HostedRoots {
        slot: read_slot,
        roots: read,
    }) = decode(&bytes).deltas().next()
    else {
        panic!("a list of hosted roots")
    };
    assert_eq!(read_slot, slot(3));
    assert_eq!(read, list);
    assert_eq!(list, read);
    assert_eq!(read.len(), 3);
    assert_eq!(read.iter().collect::<Vec<_>>(), given);
    assert_ne!(read, none);
    assert_ne!(read, RootList::from(&given[..2]));
    let other = [Some(address(8, 2)), None, None];
    assert_ne!(read, RootList::from(other.as_slice()));

    // Both give a cell the same pointers.
    let size = PageSize::MIN;
    let (frame, _) = small_page(7, size);
    let (mut as_given, mut as_read) = (frame.clone(), frame);
    Delta::HostedRoots {
        slot: slot(3),
        roots: list,
    }
    .apply(&mut open(&mut as_given, size))
    .expect("applies");
    Delta::HostedRoots {
        slot: slot(3),
        roots: read,
    }
    .apply(&mut open(&mut as_read, size))
    .expect("applies");
    assert!(as_given == as_read);
    let page = view(&as_read);
    let cell = page.cell(slot(3)).expect("a cell");
    assert_eq!(cell.hosted_roots().collect::<Vec<_>>(), given);

    // The longest list a cell takes, read from a record.
    let most = vec![Some(address(2, 2)); TreeCell::MAX_HOSTED_ROOTS];
    let bytes = run_of(page_id(7), None, &[Op::HostedRoots(1, most.clone())]);
    let (mut frame, _) = small_page(7, PageSize::DEFAULT);
    decode(&bytes)
        .apply(&mut frame, PageSize::DEFAULT, V1)
        .expect("applies");
    let page = view(&frame);
    let cell = page.cell(slot(1)).expect("a cell");
    assert_eq!(cell.hosted_roots().collect::<Vec<_>>(), most);
}

/// A change that was not read from a record, and could not be written as
/// one, is refused by the page as its writer's call would be.
#[test]
fn a_change_too_long_for_a_record_is_refused_by_the_page_too() {
    let size = PageSize::MIN;
    let (mut frame, _) = small_page(7, size);
    let before = frame.clone();
    let mut page = open(&mut frame, size);
    let slot = slot(1);

    let too_many = vec![None; TreeCell::MAX_HOSTED_ROOTS + 1];
    let roots = Delta::HostedRoots {
        slot,
        roots: too_many.as_slice().into(),
    };
    let refused = TreePageError::FieldTooLong {
        field: CellField::HostedRoots,
        len: 256,
        max: 255,
    };
    assert_eq!(roots.apply(&mut page), Err(refused.into()));
    assert_eq!(roots.encode_into(&mut Vec::new()), Err(refused.into()));

    let long = [0; 256];
    let feature = Delta::Feature {
        slot,
        feature: &long,
    };
    let refused = TreePageError::FieldTooLong {
        field: CellField::Feature,
        len: 256,
        max: 255,
    };
    assert_eq!(feature.apply(&mut page), Err(refused.into()));
    assert_eq!(feature.encode_into(&mut Vec::new()), Err(refused.into()));

    let link = Delta::Link {
        slot,
        side: Side::Left,
        link: Some(crate::tree_page::TreeLink {
            child: address(2, 2),
            hash: &[1; 32],
            heights: (0, 0),
            child_key_len: 0,
            aggregate: &long,
        }),
    };
    let refused = TreePageError::FieldTooLong {
        field: CellField::Aggregate,
        len: 256,
        max: 255,
    };
    assert_eq!(link.apply(&mut page), Err(refused.into()));
    assert_eq!(link.encode_into(&mut Vec::new()), Err(refused.into()));
    assert!(frame == before);
}

/// A carry, as the records it leaves in two pages. The cell leaves one
/// page and arrives in another, the link that points at it gets its new
/// address, and a value out of line gets a place of its own for the new
/// page: the old pointer does not go with the cell.
///
/// What the arriving cell holds until its value is placed is not this
/// format's to say. The test runs with two stand-ins for it: a pointer that
/// names no file a store has, and the value itself in the cell.
#[test]
fn a_carried_cell_is_a_remove_in_one_page_and_an_insert_in_another() {
    let size = PageSize::MIN;
    let old_place = pointer(12, 20, 300);
    let new_place = pointer(13, 64, 300);
    let pending = [
        ModelValue::OutOfLine(pointer(u32::MAX, 20, 300)),
        ModelValue::Inline(vec![0x77; 300]),
    ];
    for pending in pending {
        let carried = ModelCell {
            value: ModelValue::OutOfLine(old_place),
            ..ModelCell::leaf(b"child", b"")
        };
        let parent = ModelCell {
            left: Some(ModelLink {
                child: address(7, 2),
                hash: [3; 32],
                heights: (0, 0),
                child_key_len: 5,
                aggregate: Vec::new(),
            }),
            ..ModelCell::leaf(b"parent", b"p")
        };
        let mut from = ModelPage::new(7, 0);
        from.cells.insert(1, parent);
        from.cells.insert(2, carried.clone());
        let mut to = ModelPage::new(9, 4);
        to.cells.insert(1, ModelCell::leaf(b"other", b"o"));
        let (mut from_frame, mut to_frame) = (frame_of(&from, size), frame_of(&to, size));

        // The commit: 3 bytes for the cell that leaves and 9 for its new
        // address, in the page of its parent.
        let leaving = [Op::Remove(2), Op::Child(1, Side::Left, address(9, 2))];
        let leaving_run = run_of(from.id, None, &leaving);
        assert_eq!(leaving_run.len(), DeltaRun::HEADER_LEN + 3 + 9);
        // And the cell in its new page, with its value pending there.
        let arriving = ModelCell {
            value: pending.clone(),
            ..carried.clone()
        };
        let arriving_run = run_of(to.id, None, &[Op::Insert(2, arriving.clone())]);
        assert_eq!(
            arriving_run.len(),
            DeltaRun::HEADER_LEN + 5 + arriving.len()
        );
        // Write-back: the value was appended again for the new page, and
        // the cell is given its place.
        let placed = Op::Value(2, ModelValue::OutOfLine(new_place));
        let placing_run = run_of(to.id, None, std::slice::from_ref(&placed));
        assert_eq!(placing_run.len(), DeltaRun::HEADER_LEN + 19);

        decode(&leaving_run)
            .apply(&mut from_frame, size, V1)
            .expect("applies");
        decode(&arriving_run)
            .apply(&mut to_frame, size, V1)
            .expect("applies");

        // After the commit, before write-back: the cell is in its new
        // page with its value pending, as the insert gave it, and no cell
        // of either page holds the old pointer.
        let mut committed = to.clone();
        committed.cells.insert(2, arriving.clone());
        assert_eq!(image_of(&to_frame), committed.assemble());
        let arrived = view(&to_frame).cell(slot(2)).expect("the cell");
        assert_eq!(arrived.value(), pending.value());
        for frame in [&from_frame, &to_frame] {
            assert_eq!(view(frame).find_value_pointer(&old_place), None);
            assert_eq!(view(frame).find_value_pointer(&new_place), None);
        }

        decode(&placing_run)
            .apply(&mut to_frame, size, V1)
            .expect("applies");

        // The page the cell left: one departure, and the parent's link
        // names the cell where it is now.
        for op in &leaving {
            op.make(&mut from);
        }
        assert_eq!(from.departures, 1);
        assert_eq!(image_of(&from_frame), from.assemble());
        let parent = view(&from_frame).cell(slot(1)).expect("the parent");
        assert_eq!(
            parent.link(Side::Left).map(|link| link.child),
            Some(address(9, 2))
        );

        // The page it arrived in: the cell as it was, but for the place of
        // its value.
        to.cells.insert(
            2,
            ModelCell {
                value: ModelValue::OutOfLine(new_place),
                ..carried.clone()
            },
        );
        assert_eq!(image_of(&to_frame), to.assemble());
        let arrived = view(&to_frame).cell(slot(2)).expect("the cell");
        assert_eq!(arrived.key(), b"child");
        assert_eq!(arrived.value_len(), 300);
        assert_eq!(arrived.value_pointer(), Some(new_place));

        // No cell of either page holds the old pointer, or the stand-in.
        for frame in [&from_frame, &to_frame] {
            let page = view(frame);
            assert_eq!(page.find_value_pointer(&old_place), None);
            if let ModelValue::OutOfLine(stand_in) = &pending {
                assert_eq!(page.find_value_pointer(stand_in), None);
            }
        }
        assert_eq!(
            view(&to_frame).find_value_pointer(&new_place),
            Some(slot(2))
        );
    }
}
