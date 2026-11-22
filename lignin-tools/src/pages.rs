//! `lignin pages DIR [PAGE...] [--cells]`: pages of the latest version,
//! decoded.
//!
//! The database is opened and each page is read as a reader reads it
//! ([`Operation::page`](lignindb::snapshot::Operation::page)): from the
//! buffer pool, or from its chain of blocks in the segments, at the latest
//! version. With no page named, every id up to the highest one in use is
//! read, and an id with no page at that version is counted and passed.

use std::io::Write;

use lignin_format::{
    flat_page::{FlatPage, FlatValue},
    tree_page::{CellValue, Side, TreePage},
    PageId,
};
use lignin_store::load::{LoadBuffers, LoadError, FRAME_FORMAT};

use crate::{args::Common, hex, open::open, Outcome, ToolError};

/// Writes the pages `pages`, or every page, of the database in
/// `common.dir` to `out`.
pub fn run(
    common: &Common,
    pages: &[PageId],
    cells: bool,
    out: &mut dyn Write,
) -> Result<Outcome, ToolError> {
    let db = open(common)?;
    let high = db.stats().ids.high_water.map_or(0, PageId::get);
    let scan = pages.is_empty();
    let every = (1..=high).filter_map(PageId::new);
    let pages: Box<dyn Iterator<Item = PageId>> = match scan {
        true => Box::new(every),
        false => Box::new(pages.iter().copied()),
    };
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    writeln!(
        out,
        "version {}, page size {} bytes",
        operation.at(),
        db.page_size().get()
    )?;
    let mut buffers = LoadBuffers::new();
    // An id of the scan with no page is counted; one that was named is
    // told, and the first such is the error of the run, after the rest.
    let (mut absent, mut missing) = (0u64, None);
    for page in pages {
        let frame = match operation.page(page, &mut buffers) {
            Ok(frame) => frame,
            Err(LoadError::NoPage { .. }) if scan => {
                absent += 1;
                continue;
            }
            Err(error @ LoadError::NoPage { .. }) => {
                writeln!(out, "page {page}: no page at this version")?;
                missing.get_or_insert(error);
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        write!(out, "page {page} (born at version {}): ", frame.born())?;
        show(&frame, cells, out)?;
    }
    if absent > 0 {
        writeln!(out, "{absent} ids have no page at this version")?;
    }
    drop(operation);
    drop(snapshot);
    db.close()?;
    match missing {
        Some(error) => Err(error.into()),
        None => Ok(Outcome::Done),
    }
}

/// Writes what the page image `bytes` holds to `out`.
fn show(bytes: &[u8], cells: bool, out: &mut dyn Write) -> Result<(), ToolError> {
    let format = |source| ToolError::Format {
        path: "the page".into(),
        offset: 0,
        source,
    };
    if bytes.first() == Some(&TreePage::KIND_CODE) {
        let page = TreePage::from_validated(bytes, FRAME_FORMAT).map_err(format)?;
        writeln!(
            out,
            "tree page, {} bytes used, {} cells in {} slots, {} unused bytes, {} cells departed",
            page.used_bytes(),
            page.cell_count(),
            page.slot_count(),
            page.unused_bytes(),
            page.departures()
        )?;
        if !cells {
            return Ok(());
        }
        for (slot, cell) in page.cells() {
            let value = match cell.value() {
                CellValue::Inline(value) => format!("{} bytes in the cell", value.len()),
                CellValue::OutOfLine(pointer) => format!(
                    "{} bytes in val/{} at {}",
                    pointer.value_len(),
                    pointer.file(),
                    pointer.offset()
                ),
                CellValue::Pending(pending) => {
                    format!("{} bytes pending", pending.value_len())
                }
            };
            write!(out, "  slot {slot} key {} value {value}", hex(cell.key()))?;
            for (side, name) in [(Side::Left, "left"), (Side::Right, "right")] {
                if let Some(link) = cell.link(side) {
                    let (left, right) = link.heights;
                    write!(out, "; {name} {} heights {left}/{right}", link.child)?;
                }
            }
            let hosted: Vec<String> = cell
                .hosted_roots()
                .map(|root| root.map_or("none".to_owned(), |root| root.to_string()))
                .collect();
            if !hosted.is_empty() {
                write!(out, "; hosts {}", hosted.join(" "))?;
            }
            if cell.is_fragment_root() {
                write!(out, "; fragment root")?;
            }
            writeln!(out)?;
        }
        return Ok(());
    }
    match FlatPage::from_validated(bytes, FRAME_FORMAT).map_err(format)? {
        FlatPage::Leaf(leaf) => {
            writeln!(
                out,
                "flat leaf, {} entries, prefix {}",
                leaf.len(),
                hex(leaf.prefix())
            )?;
            if cells {
                for (key, value) in leaf.iter() {
                    let value = match value {
                        FlatValue::Inline(value) => format!("{} bytes in the page", value.len()),
                        FlatValue::OutOfLine(_) if value.is_pending() => "pending".to_owned(),
                        FlatValue::OutOfLine(_) => "in the value log".to_owned(),
                    };
                    writeln!(out, "  key {} value {value}", hex(&key.to_vec()))?;
                }
            }
        }
        FlatPage::Branch(branch) => {
            writeln!(
                out,
                "flat branch, level {}, {} separators, prefix {}",
                branch.level(),
                branch.len(),
                hex(branch.prefix())
            )?;
            if cells {
                for index in 0..=branch.len() {
                    let child = branch.child(index).map_or(0, PageId::get);
                    match branch.separator(index) {
                        Some(key) => writeln!(out, "  child {child}, then {}", hex(&key.to_vec()))?,
                        None => writeln!(out, "  child {child}")?,
                    }
                }
            }
        }
    }
    Ok(())
}
