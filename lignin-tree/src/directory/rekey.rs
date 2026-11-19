//! Moving a Merk to another prefix, with every Merk below it.

use super::{
    error::DirectoryError,
    read::damaged,
    read::Children,
    record::{child_key, entry_key, Fields, Host, MerkId, Prefix},
    Directory,
};
use crate::flat::FlatPagesMut;

/// One step down from a moved Merk towards a Merk below it: the key of the
/// element that hosts the next Merk, and the position of that Merk among
/// the element's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step<'a> {
    /// The host's key.
    pub key: &'a [u8],
    /// The position among the host's Merks.
    pub position: u8,
}

/// A Merk whose children are being moved.
struct Level {
    children: Children,
    id: MerkId,
    /// The prefix the Merk has now.
    prefix: Prefix,
}

impl Directory {
    /// Moves the Merk under `from` to the prefix `to`, where `host` hosts
    /// it, and every Merk below it to the prefix `derive` gives: the
    /// re-keying of a move that keeps the cells (`tree-layout.md` D5).
    ///
    /// `derive` is given the steps from the moved Merk down to a Merk
    /// below it, and returns the prefix that Merk has under the new path.
    /// The directory knows no path and no hash, so it cannot make one.
    ///
    /// Every Merk keeps its id, its root and its tree type, and no cell is
    /// read. One entry is written for each Merk, with the records that
    /// lead to it. Returns how many Merks were moved.
    ///
    /// The move is refused, and nothing is written, if `to` has a live
    /// Merk, if `host` is not in a live Merk, has a Merk at the position,
    /// the moved one or another, or is below the moved Merk.
    ///
    /// A Merk below the moved one is refused only when the walk comes to
    /// it: with [`DirectoryError::Exists`] if `derive` gives a prefix that
    /// has a live Merk, and with [`DirectoryError::TooDeep`] if the move
    /// takes it more than 65,535 Merks deep. The move is then half made,
    /// and the writer's pages are given up, as after any other error.
    pub fn rekey<P, F>(
        &self,
        pages: &mut P,
        from: &Prefix,
        to: &Prefix,
        host: Host<'_>,
        mut derive: F,
    ) -> Result<u64, DirectoryError>
    where
        P: FlatPagesMut + ?Sized,
        F: FnMut(&[Step<'_>]) -> Prefix,
    {
        let entry = self.live(pages, from)?;
        let left = self.free(pages, to)?;
        let place = self.below(pages, &host, Some((from, entry.id())))?;
        self.host_is_free(pages, &host, &place)?;
        let fields = Fields {
            host: Some(host),
            lineage: &place.lineage,
            ..entry.fields()
        };
        let moved = fields.encode()?;

        if let Some(was) = entry.child_key() {
            self.tree.delete(pages, &was)?;
        }
        self.tree.delete(pages, &entry_key(from))?;
        self.displace(pages, to, left.as_ref())?;
        self.enter(pages, to, &moved, Some(&place.child))?;

        let mut count = 1;
        let mut lineage = place.lineage;
        lineage.extend_from_slice(&entry.id().encode());
        let mut keys: Vec<(Vec<u8>, u8)> = Vec::new();
        let mut levels = vec![Level {
            children: self.children(entry.id()),
            id: entry.id(),
            prefix: *to,
        }];
        while let Some(level) = levels.last_mut() {
            let Some(child) = level.children.next(pages)? else {
                levels.pop();
                keys.pop();
                lineage.truncate(lineage.len().saturating_sub(MerkId::ENCODED_LEN));
                continue;
            };
            let (parent, parent_prefix) = (level.id, level.prefix);
            keys.push((child.key.clone(), child.position));
            let steps: Vec<Step<'_>> = keys
                .iter()
                .map(|(key, position)| Step {
                    key,
                    position: *position,
                })
                .collect();
            let prefix = derive(&steps);
            let (key, position) = (child.key.as_slice(), child.position);

            let old = entry_key(&child.prefix);
            let nested = self.stored(pages, &child.prefix)?;
            let nested = nested.filter(|nested| nested.id() == child.id);
            let what = "a Merk in use hosts a Merk that has no entry";
            let nested = nested.ok_or_else(|| damaged(&old, what))?;
            self.tree.delete(pages, &old)?;
            let left = self.free(pages, &prefix)?;
            let host = Host {
                parent: &parent_prefix,
                key,
                position,
            };
            let fields = Fields {
                host: Some(host),
                lineage: &lineage,
                ..nested.fields()
            };
            let moved = fields.encode()?;
            self.displace(pages, &prefix, left.as_ref())?;
            let record = child_key(parent, position, key);
            self.enter(pages, &prefix, &moved, Some(&record))?;

            count += 1;
            lineage.extend_from_slice(&child.id.encode());
            levels.push(Level {
                children: self.children(child.id),
                id: child.id,
                prefix,
            });
        }
        Ok(count)
    }
}
