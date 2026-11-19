//! Making the pages what the plan says.
//!
//! Every change is a call of [`NodePageMut`](crate::node::NodePageMut), on
//! one page at a time. The changes are made in three passes, so that no
//! page is asked to hold more than it holds when the write is done:
//!
//! 1. each page gives up what leaves it: the cells of deleted nodes and of
//!    carried ones, and the parts of the cells that stay which get shorter
//! 2. the cells that arrive are put into their pages, a child before its
//!    parent, so that a parent is written with the addresses of its
//!    children
//! 3. the cells that stay are brought to what the write says, and the
//!    marks and the addresses in them that a carry made wrong are put right

use std::{collections::BTreeMap, ops::Range};

use lignin_format::{
    tree_page::{CellContent, CellValue, HashField, Side, TreeCell, TreeLink, TreePage},
    CellAddress, PageId, SlotId,
};
use lignin_store::load::FRAME_FORMAT;

use super::{
    error::FragmentError,
    pages::{used_bytes, Hints, PageMut, Pages, Values},
    plan::{Cell, Plan, Via},
    report::{Carried, Carries, Report},
    write::{HostedWrite, LinkWrite, NodeWrite, ValueWrite},
};
use crate::node::{ChangeSink, NodeError, NodeMut};

/// What is done to one page.
#[derive(Default)]
struct Work {
    /// The cells that are carried out of the page.
    out: Vec<usize>,
    /// The cells of deleted nodes.
    gone: Vec<usize>,
    /// The nodes of the write that stay in the page.
    kept: Vec<usize>,
    /// The addresses to put into cells the write does not write: a cell
    /// they point at was carried.
    fixes: Vec<(SlotId, Via, CellAddress)>,
    /// The cells that stay, with the fragment-root mark each has to have.
    marks: Vec<(SlotId, bool)>,
    /// The cells whose handle the report gives.
    wanted: Vec<usize>,
}

/// Where the cells of the plan are in the pages, as far as they are there.
type Places = [Option<CellAddress>];

fn origin(cell: &Cell) -> CellAddress {
    cell.origin.expect("a cell of the pages has an address")
}

fn node_mut<'p, 'a, S: ChangeSink>(
    page: &'p mut crate::node::NodePageMut<'a, S>,
    address: CellAddress,
) -> Result<NodeMut<'p, 'a, S>, NodeError> {
    page.node_mut(address.slot())
        .ok_or(NodeError::NoNode { address })
}

fn tree_link<'a>(link: &LinkWrite<'a>, child: CellAddress) -> TreeLink<'a> {
    TreeLink {
        child,
        hash: link.hash,
        heights: link.heights,
        child_key_len: link.child_key_len,
        aggregate: link.aggregate,
    }
}

fn cell_value(value: ValueWrite<'_>) -> Option<CellValue<'_>> {
    match value {
        ValueWrite::Keep => None,
        ValueWrite::Inline(value) => Some(CellValue::Inline(value)),
        ValueWrite::Pending(pending) => Some(CellValue::Pending(pending)),
    }
}

/// The hosted-root pointers of a cell after the write. A root that has no
/// cell yet is `None`.
fn hosted_roots(
    cell: &Cell,
    write: Option<&NodeWrite<'_>>,
    stored: impl Iterator<Item = Option<CellAddress>>,
    places: &Places,
) -> Vec<Option<CellAddress>> {
    let mut roots = match write.map(|write| write.hosted) {
        Some(HostedWrite::Set(roots)) => vec![None; roots.len()],
        Some(HostedWrite::Keep) | None => stored.collect(),
    };
    for kid in &cell.kids {
        if let Via::Hosted(position) = kid.via {
            roots[usize::from(position)] = places[kid.cell];
        }
    }
    roots
}

/// Gives a cell that stays another value, and says what became of the one
/// it held.
fn put_value<S: ChangeSink, V: Values>(
    node: &mut NodeMut<'_, '_, S>,
    value: CellValue<'_>,
    values: &mut V,
) -> Result<(), NodeError> {
    let (page, old) = (node.node().address().page(), node.node().value());
    if old == value {
        return Ok(());
    }
    let released = old.place();
    node.set_value(value)?;
    if let Some(place) = released {
        values.released(place, page);
    }
    if let CellValue::Pending(pending) = value {
        values.placed(pending, page);
    }
    Ok(())
}

/// The first pass on a node of the write that stays: the parts of its cell
/// that get shorter.
fn shrink<V: Values>(
    page: &mut PageMut<'_, '_>,
    plan: &Plan<'_>,
    cell: usize,
    places: &Places,
    values: &mut V,
) -> Result<(), NodeError> {
    let entry = &plan.cells[cell];
    let write = &plan.write.nodes[entry.node.expect("a node of the write")];
    let mut node = node_mut(page, origin(entry))?;
    for (side, link) in Side::BOTH.into_iter().zip([write.left, write.right]) {
        let old = node.node().link(side);
        let old = old.map(|old| (old.child, old.aggregate.len()));
        match (old, link) {
            (Some(_), None) => node.set_link(side, None)?,
            (Some((child, len)), Some(link)) if link.aggregate.len() < len => {
                let kid = entry.kid(Via::Link(side));
                let child = kid.and_then(|kid| places[kid]).unwrap_or(child);
                node.set_link(side, Some(tree_link(&link, child)))?;
            }
            _ => {}
        }
    }
    if write.feature.len() < node.node().feature().len() {
        node.set_feature(write.feature)?;
    }
    if let HostedWrite::Set(roots) = write.hosted
        && roots.len() < node.node().hosted_roots().len()
    {
        let roots = hosted_roots(entry, Some(write), std::iter::empty(), places);
        node.set_hosted_roots(&roots)?;
    }
    if let Some(value) = cell_value(write.value)
        && value.stored_len() < node.node().value().stored_len()
    {
        put_value(&mut node, value, values)?;
    }
    Ok(())
}

/// The third pass on a node of the write that stays: every part of its
/// cell that is not what the write says is written, and no other.
fn sync<V: Values>(
    page: &mut PageMut<'_, '_>,
    plan: &Plan<'_>,
    cell: usize,
    places: &Places,
    values: &mut V,
) -> Result<(), NodeError> {
    let entry = &plan.cells[cell];
    let write = &plan.write.nodes[entry.node.expect("a node of the write")];
    let mut node = node_mut(page, origin(entry))?;
    if node.node().feature() != write.feature {
        node.set_feature(write.feature)?;
    }
    if node.node().value_hash() != write.value_hash {
        node.set_hash(HashField::Value, write.value_hash)?;
    }
    if node.node().kv_hash() != write.kv_hash {
        node.set_hash(HashField::KeyValue, write.kv_hash)?;
    }
    for (side, link) in Side::BOTH.into_iter().zip([write.left, write.right]) {
        let new = link.map(|link| {
            let kid = entry.kid(Via::Link(side));
            let child = kid.and_then(|kid| places[kid]);
            tree_link(&link, child.expect("the child of a written node has a cell"))
        });
        let (address, hash) = match (node.node().link(side), new) {
            (None, None) => continue,
            (Some(old), Some(new))
                if old.heights == new.heights
                    && old.child_key_len == new.child_key_len
                    && old.aggregate == new.aggregate =>
            {
                (
                    Some(new.child).filter(|child| *child != old.child),
                    Some(new.hash).filter(|hash| *hash != old.hash),
                )
            }
            _ => {
                node.set_link(side, new)?;
                continue;
            }
        };
        if let Some(child) = address {
            node.set_child(side, child)?;
        }
        if let Some(hash) = hash {
            node.set_hash(HashField::Child(side), hash)?;
        }
    }
    let stored: Vec<Option<CellAddress>> = node.node().hosted_roots().collect();
    let roots = hosted_roots(entry, Some(write), stored.iter().copied(), places);
    if roots.len() != stored.len() {
        node.set_hosted_roots(&roots)?;
    } else {
        for (position, root) in roots.iter().enumerate() {
            if stored[position] != *root {
                node.set_hosted_root(position as u8, *root)?;
            }
        }
    }
    if let Some(value) = cell_value(write.value) {
        put_value(&mut node, value, values)?;
    }
    Ok(())
}

/// The second pass: puts a cell into the page it goes to, a node the write
/// creates or a cell it carries. `old` is the cell as it was in the page it
/// left.
fn insert<V: Values>(
    page: &mut PageMut<'_, '_>,
    plan: &Plan<'_>,
    cell: usize,
    old: Option<&[u8]>,
    places: &Places,
    values: &mut V,
    carries: &mut Carries,
) -> Result<CellAddress, NodeError> {
    let entry = &plan.cells[cell];
    let to = page.id();
    let from = entry.origin.map(|address| address.page());
    let old = old
        .map(|bytes| TreeCell::decode(bytes, FRAME_FORMAT))
        .transpose()?;
    let write = entry.node.map(|node| &plan.write.nodes[node]);
    let child = |via: Via| {
        let kid = entry.kid(via).and_then(|kid| places[kid]);
        kid.expect("a child has its cell before its parent")
    };
    let link = |side: Side| match (write, old) {
        (Some(write), _) => {
            let link = match side {
                Side::Left => write.left,
                Side::Right => write.right,
            };
            link.map(|link| tree_link(&link, child(Via::Link(side))))
        }
        (None, old) => old?.link(side).map(|link| TreeLink {
            child: child(Via::Link(side)),
            ..link
        }),
    };
    let stored = old.into_iter().flat_map(|old| old.hosted_roots());
    let hosted = hosted_roots(entry, write, stored, places);
    let kept = old.map(|old| old.value());
    let set = write.and_then(|write| cell_value(write.value));
    let mut carried_len = None;
    let value = match (set, kept, from) {
        (Some(value), _, _) => value,
        (None, Some(CellValue::Inline(value)), _) => CellValue::Inline(value),
        (None, Some(kept), Some(from)) => {
            let place = kept.place().expect("a value that is not in its cell");
            let pending = values.carried(place, from, to);
            carried_len = Some(pending.value_len());
            CellValue::Pending(pending)
        }
        (None, _, _) => unreachable!("a node the write creates has a value"),
    };
    let (key, feature, value_hash, kv_hash) = match (write, old) {
        (Some(write), _) => (write.key, write.feature, write.value_hash, write.kv_hash),
        (None, Some(old)) => (old.key(), old.feature(), old.value_hash(), old.kv_hash()),
        (None, None) => unreachable!("a cell that arrives is written or carried"),
    };
    let content = CellContent {
        key,
        feature,
        value_hash,
        kv_hash,
        left: link(Side::Left),
        right: link(Side::Right),
        hosted_roots: &hosted,
        value,
        fragment_root: entry
            .parent
            .is_none_or(|parent| plan.cells[parent].page != entry.page),
    };
    let address = page.insert(&content)?.address();

    if let (Some(from), Some(_), Some(place)) = (from, set, kept.and_then(|kept| kept.place())) {
        values.released(place, from);
    }
    if let Some(CellValue::Pending(pending)) = set {
        values.placed(pending, to);
    }
    if from.is_some() {
        let moves = &mut carries[entry.cause.expect("a rule carried the cell")];
        moves.cells += 1;
        moves.cell_bytes += (content.encoded_len() + TreePage::SLOT_LEN) as u64;
        if let Some(len) = carried_len {
            moves.values += 1;
            moves.value_bytes += u64::from(len);
        }
    }
    Ok(address)
}

/// The cells that arrive, each child before its parent.
fn insert_order(plan: &Plan<'_>, inserts: &[usize]) -> Vec<usize> {
    const WAITING: u8 = 1;
    const SEEN: u8 = 2;
    let mut state = vec![0; plan.cells.len()];
    for &cell in inserts {
        state[cell] = WAITING;
    }
    let mut order = Vec::with_capacity(inserts.len());
    let mut stack = Vec::new();
    for &start in inserts {
        if state[start] != WAITING {
            continue;
        }
        state[start] = SEEN;
        stack.push((start, 0));
        while let Some((cell, next)) = stack.last_mut() {
            let kid = plan.cells[*cell].kids.get(*next).map(|kid| kid.cell);
            *next += 1;
            match kid {
                Some(kid) if state[kid] == WAITING => {
                    state[kid] = SEEN;
                    stack.push((kid, 0));
                }
                Some(_) => {}
                None => {
                    order.push(*cell);
                    stack.pop();
                }
            }
        }
    }
    order
}

/// Applies the plan of a write to the pages.
pub(super) fn apply<P: Pages, V: Values>(
    plan: &Plan<'_>,
    pages: &mut P,
    values: &mut V,
    hints: &mut Hints,
) -> Result<Report, FragmentError> {
    let cells = &plan.cells;
    let mut report = Report::default();
    let mut ids: Vec<Option<PageId>> = plan.pages.iter().map(|page| page.id).collect();
    for (page, entry) in plan.pages.iter().enumerate() {
        if entry.id.is_none() && entry.cells() > 0 {
            let id = pages.create()?;
            ids[page] = Some(id);
            report.created.push(id);
        }
    }

    let mut work: BTreeMap<PageId, Work> = BTreeMap::new();
    let mut inserts = Vec::new();
    let mut places: Vec<Option<CellAddress>> = cells
        .iter()
        .map(|cell| cell.origin.filter(|_| cell.stays()))
        .collect();
    for (index, cell) in cells.iter().enumerate() {
        match cell.origin {
            Some(address) if cell.removed => {
                work.entry(address.page()).or_default().gone.push(index);
            }
            Some(address) if !cell.stays() => {
                work.entry(address.page()).or_default().out.push(index);
                inserts.push(index);
            }
            Some(address) if cell.node.is_some() => {
                work.entry(address.page()).or_default().kept.push(index);
            }
            Some(_) => {}
            None => inserts.push(index),
        }
    }

    let mut copies = Vec::new();
    let mut ranges: Vec<Option<Range<usize>>> = vec![None; cells.len()];
    for (&id, page_work) in &work {
        pages.write(id, |page| {
            for &cell in &page_work.out {
                let address = origin(&cells[cell]);
                let view = page.view();
                let node = view
                    .node(address.slot())
                    .ok_or(NodeError::NoNode { address })?;
                let start = copies.len();
                copies.extend_from_slice(node.cell().as_bytes());
                ranges[cell] = Some(start..copies.len());
            }
            for &cell in &page_work.gone {
                let node = node_mut(page, origin(&cells[cell]))?;
                let place = node.node().value().place();
                node.remove()?;
                if let Some(place) = place {
                    values.released(place, id);
                }
            }
            for &cell in &page_work.out {
                node_mut(page, origin(&cells[cell]))?.remove()?;
            }
            for &cell in &page_work.kept {
                shrink(page, plan, cell, &places, values)?;
            }
            Ok(())
        })?;
        if !(page_work.out.is_empty() && page_work.gone.is_empty()) {
            report.left.push(id);
        }
    }

    let order = insert_order(plan, &inserts);
    let mut at = 0;
    while let Some(&first) = order.get(at) {
        let to = cells[first].page;
        let run = order[at..]
            .iter()
            .take_while(|cell| cells[**cell].page == to)
            .count();
        let id = ids[to].expect("a page that takes a cell has an id");
        pages.write(id, |page| {
            for &cell in &order[at..at + run] {
                let old = ranges[cell].clone().map(|range| &copies[range]);
                let carries = &mut report.carries;
                let address = insert(page, plan, cell, old, &places, values, carries)?;
                places[cell] = Some(address);
            }
            Ok(())
        })?;
        work.entry(id).or_default();
        at += run;
    }

    for &cell in &order {
        let entry = &cells[cell];
        let (Some(parent), Some(via)) = (entry.parent, entry.via) else {
            continue;
        };
        let parent = &cells[parent];
        if parent.stays() && parent.node.is_none() {
            let address = origin(parent);
            let to = places[cell].expect("the cell was put into its page");
            let page_work = work.entry(address.page()).or_default();
            page_work.fixes.push((address.slot(), via, to));
        }
    }
    for (index, cell) in cells.iter().enumerate() {
        if cell.removed {
            continue;
        }
        if cell.node.is_some() || !cell.stays() {
            let at = places[index].expect("the cell is in a page");
            work.entry(at.page()).or_default().wanted.push(index);
        } else {
            continue;
        }
        for kid in &cell.kids {
            let kid = &cells[kid.cell];
            if !kid.stays() {
                continue;
            }
            let address = origin(kid);
            if let Some(page_work) = work.get_mut(&address.page()) {
                let mark = kid.page != cell.page;
                page_work.marks.push((address.slot(), mark));
            }
        }
    }
    for &address in &plan.orphans {
        if let Some(page_work) = work.get_mut(&address.page()) {
            page_work.marks.push((address.slot(), true));
        }
    }

    for (&id, page_work) in &work {
        let idle = page_work.kept.is_empty()
            && page_work.fixes.is_empty()
            && page_work.marks.is_empty();
        if idle {
            continue;
        }
        pages.write(id, |page| {
            for &cell in &page_work.kept {
                sync(page, plan, cell, &places, values)?;
            }
            for &(slot, via, to) in &page_work.fixes {
                let mut node = node_mut(page, CellAddress::new(id, slot))?;
                match via {
                    Via::Link(side) => node.set_child(side, to)?,
                    Via::Hosted(position) => node.set_hosted_root(position, Some(to))?,
                }
            }
            for &(slot, mark) in &page_work.marks {
                let mut node = node_mut(page, CellAddress::new(id, slot))?;
                if node.node().is_fragment_root() != mark {
                    node.set_fragment_root(mark)?;
                }
            }
            Ok(())
        })?;
    }

    for entry in &plan.pages {
        if let Some(id) = entry.id.filter(|_| entry.touched && entry.cells() == 0) {
            pages.free(id)?;
            hints.forget(id);
            work.remove(&id);
            report.freed.push(id);
        }
    }
    let mut handles = vec![None; cells.len()];
    for (&id, page_work) in &work {
        let used = pages.read(id, |view| {
            for &cell in &page_work.wanted {
                let node = places[cell].and_then(|address| view.node(address.slot()));
                handles[cell] = node.map(|node| node.handle());
            }
            used_bytes(&view)
        })?;
        hints.note(id, used);
    }
    for (entry, id) in plan.pages.iter().zip(&ids) {
        if let Some(id) = id.filter(|id| work.contains_key(id)) {
            debug_assert_eq!(hints.used(id), Some(entry.used()), "{id} is as planned");
        }
    }

    let handle = |cell: usize| handles[cell].expect("the node is where it was put");
    report.nodes = plan.of_node.iter().map(|cell| handle(*cell)).collect();
    report.roots = plan
        .roots
        .iter()
        .map(|root| root.and_then(|cell| places[cell]))
        .collect();
    for &cell in &order {
        let entry = &cells[cell];
        let Some(from) = entry.origin else { continue };
        let hosted_by = match (entry.parent, entry.via) {
            (Some(host), Some(Via::Hosted(position))) => {
                places[host].map(|host| (host, position))
            }
            _ => None,
        };
        report.carried.push(Carried {
            from,
            to: handle(cell),
            cause: entry.cause.expect("a rule carried the cell"),
            node: entry.node,
            hosted_by,
        });
    }
    report.orphaned.clone_from(&plan.orphans);
    Ok(report)
}
