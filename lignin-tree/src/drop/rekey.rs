//! The move of subtrees to other prefixes: what the transaction that
//! moves them writes to the directory and to the flat columns.

use std::collections::HashSet;

use super::{column::Flats, error::DropError};
use crate::{
    directory::{Directory, Dropped, Prefix, RekeyPlan},
    flat::FlatPagesMut,
};

/// Makes the moves that `plan` holds ([`Directory::plan_rekey`]): the
/// directory's records of every Merk that moves, and its flat records in
/// every column (`tree-layout.md` D5, D6).
///
/// After it every flat record that was under a prefix a Merk moved from
/// is under the prefix that Merk went to, with its key and its value, and
/// none is under a prefix that a Merk moved from and none went to. A Merk that leaves with
/// the move takes its flat records with it, as a drop does, and what it
/// left is returned for the reclaimer. Where a Merk goes to a prefix
/// under which a dropped Merk had left an entry, the flat records of that
/// entry go first, as for a Merk that is entered there ([`enter`](super::enter)).
///
/// **The moves are made together.** Every record is read as it is before
/// the call. The records of a Merk that goes to a prefix which another
/// Merk moves from are held in memory until every such prefix is empty;
/// those of every other Merk are written under the new prefix one by
/// one. The old ranges are taken out by the lazy range delete
/// ([`FlatDrops::unlink_prefix`](super::FlatDrops::unlink_prefix)).
///
/// So a move costs one put for each flat record of the Merks it re-keys,
/// with the value written again, and nothing for their cells. A
/// transaction that makes its pages again makes the call again.
///
/// The plan was made of `directory` as it is now. After an error the
/// caller rolls the operation's scope back.
pub fn rekey<F: FlatPagesMut + ?Sized>(
    directory: &mut Directory,
    flats: &mut Flats<'_, F>,
    plan: &RekeyPlan,
) -> Result<Vec<Dropped>, DropError> {
    let rekeyed = directory.rekey_pairs(flats.pages, plan)?;
    for prefix in plan.left().iter().chain(&rekeyed.displaced) {
        flats.unlink_subtree(prefix)?;
    }
    let sources: HashSet<&Prefix> = plan.moved().map(|(from, ..)| from).collect();
    let mut held = Vec::new();
    let mut value = Vec::new();
    for (from, to, _) in plan.moved() {
        let hold = sources.contains(to);
        for (index, column) in flats.columns.iter().enumerate() {
            let within = column.range(to);
            let mut cursor = column.tree.cursor_within(&column.range(from));
            cursor.seek_to_first(&*flats.pages)?;
            while let Some(key) = cursor.key() {
                let key = [&within, key].concat();
                value.clear();
                if cursor.value(&*flats.pages, &mut value)? {
                    match hold {
                        true => held.push((index, key, value.clone())),
                        false => column.tree.put(flats.pages, &key, &value)?,
                    }
                }
                cursor.next(&*flats.pages)?;
            }
        }
    }
    for (from, ..) in plan.moved() {
        flats.unlink_subtree(from)?;
    }
    for (column, key, value) in held {
        flats.columns[column].tree.put(flats.pages, &key, &value)?;
    }
    Ok(rekeyed.left)
}
