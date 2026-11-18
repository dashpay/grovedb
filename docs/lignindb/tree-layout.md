# LigninDB tree layout: the Merkle host model

This is the specification for task TREE-01 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
fixes how the nodes of Merk trees are clustered into pages:

- what a cell and a link hold (section 2)
- when a slot number may change (section 3)
- fragments, and what a write may move (section 4)
- where a new node goes, and where the root of a subtree goes (section 5)
- what a full page does, and which part of it leaves (section 6)
- what a page does that a delete has emptied (section 7)
- rotations and deletions (section 8)
- what moving a cell does to a value stored outside it (section 9)
- two Merks written by different threads in one page (section 10)
- dropping a subtree (section 11)

The shape of every tree is Merk's. Nothing here changes a key, a link, a
height or a hash, so nothing here can change a root hash, a proof or an
`OperationCost`. The rules decide only which page a node's bytes are in.

The rules were checked by simulation before any engine code exists. Section
13 says how Merk's node structure was observed, section 14 gives the numbers
for the reference workload and section 15 those for the adversarial ones.
Section 12 lists the parameters, section 16 says what later tasks take from
here, and section 17 lists the choices made here that the plan left open.

The simulator is `lignin-testkit::layout_sim`. Section 18 maps each rule to
its code and its tests, and gives the commands that reproduce every number.

It builds on [`format.md`](format.md) (page ids, section 3.1) and
[`value-log-format.md`](value-log-format.md) (the move rule O5).

## 1. Terms

- **Node**: one Merk node: a key, a value (the serialized element), the
  feature type with its aggregate data, the value hash, the key-value hash,
  and a link to each child.
- **Host, hosted Merk**: a tree element hosts the Merk that is its subtree.
  The node that stores the element is the **host** of that Merk. An indexed
  tree hosts several. A tree whose data is not in a Merk (MMR, bulk-append,
  dense, commitment, private-document) hosts none: its data is in the flat
  keyspace (TREE-07).
- **Grove tree**: every node of every Merk as one tree. The children of a
  node are its left and right child in its Merk and the roots of the Merks it
  hosts. The **parent** of a node is its Merk parent, or for the root of a
  Merk the host of that Merk. Only the root of the GroveDB root Merk has no
  parent. "Parent", "child" and "below" mean this tree unless a rule says
  "Merk parent".
- **Cell**: the bytes of one node in a page.
- **Capacity**: the bytes of a page that cells can use: the page size less
  the page header.
- **Fragment**: the cells of one page that are joined by links that stay
  inside the page (4.1). Its **root** is the one cell whose parent is not in
  the page.
- **Carry**: to move a cell from one page to another. The node does not
  change; its cell gets a new address.
- **Write**: the application of one Merk change set (SEAM-01, MERK-01). The
  simulation works on a state transition, which is a few writes in a row.

"Must" marks a requirement on every engine. Sections 5 to 8 are **policy**:
they decide how good a layout is, never whether it is valid (F5).

## 2. Cells and links

**C1. One node, one cell.** Every node of every Merk has exactly one cell, in
exactly one page. A cell holds:

| Part | Bytes | |
| --- | --- | --- |
| key | 1 to 255 | |
| feature type and aggregate data | as Merk encodes them | |
| value hash | 32 | |
| key-value hash | 32 | |
| left link, right link | each absent or as C2 | |
| a pointer to the root of each Merk the element hosts | C3 | none for an element that hosts no Merk, or an empty one |
| value | the value, or a value pointer | section 9 |

These are the hashes Merk stores today, one for one. The node's own hash is
not in its cell: as today it is in the link of its parent.

**C2. A link holds what Merk's link holds, with a cell address in place of
the child's key.**

| Part | Bytes |
| --- | --- |
| the child's address: page id and slot | 4 + 2 |
| the child's hash | 32 |
| the heights of the child's two subtrees | 2 |
| the child's aggregate data | as Merk encodes it |

Merk's link stores the child's key so that the child can be looked up. The
cell address replaces that lookup ("pointers, not lookups"). The key is still
in the child's own cell.

**C3. A pointer to a hosted root is a cell address and nothing else.** Merk
stores no hash of a hosted Merk's root beside the element: the host's value
hash already covers it. The root key and the tree type stay where they are
today, inside the value.

**C4. A link has the same form whether or not the child is in the same
page.** Every address is a page id and a slot. So retargeting a link never
changes the size of a cell, and a hash can be overwritten in place whatever
the link points at. A format that wants to save the four bytes of the page id
for a child in the same page may do so only if it keeps both properties
(FMT-02).

**C5. A cross-page link is one-way.** The cell that is pointed at does not
record who points at it, and neither does its page. The only stored addresses
of a cell are:

1. the link in its parent's cell (C2), or for the root of a Merk the pointer
   in its host's cell (C3)
2. for the root of a Merk, also the entry of that Merk in the subtree
   directory (TREE-05)

There is no sibling link and no parent pointer. This is what keeps a hash
update, a split and a merge from touching pages below the one they work on.
The price is rule F4: a cell can only be moved by a write that holds its
parent.

**C6. The sizes the simulation assumes.** FMT-02 fixes the real layout. Every
number in sections 14 and 15 is computed with these, and `CellCosts` holds
them so that the simulation can be run again when they change:

| | Bytes |
| --- | --- |
| page header | 32 |
| slot directory entry, per cell | 2 |
| cell header: flags, key length, value length | 4 |
| cell address in a link or a hosted-root pointer | 6 |
| value pointer, in place of an out-of-line value | 16 (value-log-format.md, section 4) |
| header of a value-log record | 18 (value-log-format.md, section 2) |

The key, the feature type, the aggregate data and the value are counted at
the length Merk's own record has for them. So a cell is

```text
2 + 4 + key + 64 + feature type and aggregates
  + 40 for each link + 6 for each hosted root
  + value, or 16
```

In the reference workload a document node with two children is 201 bytes and
a document leaf 121, with its 440-byte value out of line. A reference node is
about 185 to 265 bytes with its value in the cell.

## 3. Slots

A link names a cell by page id and slot (C2), so a slot number is a name that
other pages rely on.

**S1. A slot number names one cell from the commit that puts the cell into
the page to the commit that takes it out.** In between, nothing renumbers it:
not a change to the cell (a hash, a link, a value, its size), not a change to
another cell of the page, not a delta, not a consolidation, not a new page
image.

**S2. A slot number changes in exactly two cases**, and in both the cell
leaves its page:

- the node is deleted
- the cell is carried to another page (sections 6 to 8), where it gets a slot
  of that page

**S3. The commit that carries a cell rewrites every stored address of it.**
By C5 that is one link or pointer, plus the directory entry for the root of a
Merk. Both are in cells the write holds (F4).

**S4. A freed slot number may be used again by a later commit.** No stored
address can still name it: S3 rewrote them in the commit that freed it, and a
snapshot of an earlier version reads the earlier version of both pages
(TXN-01). The lowest free number is taken first, so equal histories give
equal pages.

**S5. Child pointers need stable slots. The value log does not.** If slots
were positions, as in a flat page (P13 of `flat-page.md`), every insert into
a page would have to rewrite the links that point into it from other pages,
and by C5 nobody knows which pages those are. The value log names the owning
page and no slot (value-log-format.md, O1), so nothing in it depends on S1.

A slot number is 16 bits (format.md, draft 9 of the plan). A page of 32 KiB
holds a few hundred cells.

## 4. Fragments

### 4.1 What a fragment is

**F1. Every cell is in exactly one fragment.** Take the cells of one page and
the links and hosted-root pointers between them. A fragment is one connected
piece. It has exactly one cell whose parent is in another page, or that has
no parent: the **fragment root**. Every other cell of the fragment has its
parent in the fragment.

So a fragment is a piece of the grove tree: a node, and below it whatever is
reached without leaving the page. It may hold cells of several Merks, when a
hosted root is in the page of its host (section 5).

**F2. A page holds any number of fragments.** This is the fallback the plan
names for this task ("a page may hold more than one fragment"), and it is the
rule from the start, for three reasons:

- a subtree that is small sits in the page of its host (P2), beside whatever
  else that page holds
- a rotation changes links and moves nothing (R1), which can cut a fragment
  in two
- a page that takes in a fragment from a full neighbour is how fill gets
  above what halving alone gives (section 6.4)

**F3. The page records its fragment roots.** The fragment-root table of
FMT-02 lists the slots of the cells whose parent is not in the page. It is
derived from the cells and the links into the page; VER-07 checks it.

**F4. A write moves only what it holds.** A write holds the cells of the
nodes in its change set and the cells it has carried. In a page where it
holds a cell it may read every cell. It may carry a cell only if it also
holds the cell that points at it, because that pointer must be rewritten (S3)
and C5 gives no other way to find it.

Every rule below keeps to this. The fragment a rule moves is always the one a
held cell is in, and its parent is held too: a write changes a hash in every
node from the one it touches up to the root, so the whole path is in its
change set. (From iteration two a write stops at the root of its Merk;
section 10 says what then holds for the pointer in the host.)

**F5. Any placement is valid.** A reader follows addresses and never assumes
that two cells share a page, that a fragment has some size, or that a page
holds one Merk. What must hold is C1, S1 to S4, F1, F3, and that a page's
cells fit in it. Sections 5 to 8 can therefore be changed, or tuned
(PERF-08), without a format version, and a database written under one setting
is read under any other.

### 4.2 Sizes

Three sizes steer the rules. Section 12 gives their values.

- A fragment below the **small size** belongs in its parent's page.
- A fragment above the **largest size** is split.
- A page takes in a fragment from another page only up to the **seed
  limit**.

Why a fragment should be as large as the fill target allows: a hash update
writes every page on the path from the changed node to the root. A fragment
of *n* nodes that Merk keeps balanced puts about log2(*n*) of the path's
nodes into one page. Halving the largest fragment costs one more page for
every seven or so levels of tree.

## 5. Placement

**P1. A new node goes into the page of its parent.** For a node with a Merk
parent that is the page of the Merk parent.

**P2. The root of a Merk goes into the page of its host.** This is the
co-location of a subtree with its parent element: P1 with the parent of the
grove tree. The nodes added to that Merk afterwards follow P1, so a subtree
grows inside its host's page for as long as it fits there. It stops being
co-located only when section 6 moves a part of it out.

A subtree therefore never needs a size test to decide whether it is "small
enough" to sit with its host. A chain of 127 nested trees with one element
each is one fragment of 127 cells, and a write at the bottom of it dirties
the few pages that fragment is spread over (section 15).

**P3. The first node of a database starts the first page.** A node that
replaces a deleted node goes where the deleted one was (R2). A new node that
has no parent but has children goes into the page of one of them.

**P4. Placement never looks at the key.** Where a node goes depends on its
parent's page and on sizes. Two databases with the same history have the
same pages.

Without P2, with every Merk laid out on pages of its own, the first 300,000
transitions of the reference workload dirty 12.1 pages each against 6.9, and
the pages are 16% full against 74% (section 14.4).

## 6. A full page

A write first applies its change set by sections 5 and 8. That can leave a
page above its capacity for the moment, or a fragment above the largest size.
It then restores both, in this order.

### 6.1 A fragment above the largest size

**L1. Its root goes up.** If a fragment the write holds is above the largest
size, and its root has a parent, the root cell is carried to the page of its
parent. What hung below it in the page stays, and is now two or three
fragments of about half the size.

Nothing else moves. This is the split of a B-tree node read backwards: the
key in the middle goes up to the parent node, and the two halves are what is
left. Because Merk keeps its trees balanced, the root of a fragment is close
to the middle of it.

**L2. A small hosted Merk goes with its host.** If the cell that goes up
hosts a Merk whose root is in the same page and whose fragment is below the
small size, that fragment is carried with it. A larger one stays behind as a
fragment of its own.

**L3. The page the root arrives in is treated like any other.** If it is now
above its capacity or holds a fragment above the largest size, the same rules
apply there. The root of the grove has no parent: its fragment is not
limited by L1 and may fill its page.

### 6.2 A page above its capacity

Take the fragment of the overfull page that the write holds (F4).

**V1. If it is small, it goes to its parent's page**, when it is below the
small size and has a parent.

**V2. If it is at least half the capacity, it is split.** Its root goes up as
in L1 (or stays, if it has no parent). If one of the parts that are left is
still at least half the capacity and more than three quarters of them, that
part's root goes up too, and so on. Then the parts whose bytes together come
closest to what the page holds above half its capacity leave together.

**V3. Otherwise the whole fragment leaves.**

**V4. Where it goes.** A fragment that leaves by V2 or V3 goes to a **page
beside it**: a page that holds another child of the fragment that its parent
is in. Of those it takes the fullest one that, with the fragment added, stays
within the seed limit. If there is none, it starts a fresh page.

**V5. Repeat until no page is above its capacity.** Each step takes bytes out
of the overfull page or sends one cell up, so it ends.

**V6. Ties are broken the same way everywhere**: fewer bytes moved first,
then the part that comes first in the order left, right, hosted; among pages
the lowest page id.

### 6.3 What this gives

A page fills, its fragments are cut at the largest size as they grow, and
when it is full the fragment that is being written to leaves for a
neighbour or a fresh page. Roots collect in the page above, which fills and
is treated the same way. The result is the shape of a B-tree whose nodes are
fragments, built without ever choosing a key: Merk's balancing does that.

### 6.4 Why the seed limit

Without V4's neighbours, a page that fills is in effect halved, and pages
that are halved when full are on average ln 2 = 69% full under random
inserts, just below the target of 70%. Uneven halves make it less: the
simulation measures 68% (section 14.3).

A fragment at half the capacity cannot join a page that is more than half
full. So the largest size is set to half the capacity: a page then holds two
to four fragments, one of them can leave on its own, and a page that is up to
the seed limit less that fragment can take it in. Fill rises to 75%.

It has two prices. The first is pages: a fragment of half the size puts one
level less of the tree into a page, and the reference workload dirties 7.0
pages per transition against 6.7. The second is carried cells. A page that
takes fragments in is fuller, so less of what a growing tree adds lands in
room that a fresh page brought, and more has to be made room for by carrying.
With a seed limit of 75% a growing tree carries 0.83 cells for each cell
inserted, against 0.68 without (section 14.3). Section 9 says why that number
matters.

## 7. A page a delete has emptied

**M1. A small fragment moves into its parent's page when there is room.**
After a write, every fragment it holds that is below the small size, and
whose parent is in another page, is carried to that page if it fits within
the capacity. This is what brings a subtree back to its host after a split
separated them, and what removes a fragment that deletes have shrunk to
nearly nothing.

**M2. A page below the merge threshold gives its fragment away.** When a
delete leaves a page below the merge threshold, the fragment the delete was
in goes to the fullest page beside it that stays within the seed limit
(V4). If there is none, it stays.

**M3. A page with no cell is freed.** Its id goes back to the allocator
(STORE-10).

The merge threshold is below half the seed limit, so two pages that qualify
fit into one, and a page that has just taken a fragment in is not the next to
give one away.

A tree that stays the same size under deletes and inserts shows what these
rules are for. Over four turnovers of every key, pages that are only ever
halved go from 68% full to 60% and are still falling. With the rules of this
section and of 6.2 the same run goes from 76% to 70% and levels off there; M2
alone accounts for two of those points (section 14.4).

## 8. Rotations and deletions

Merk reshapes a tree by rotation, and a write hands the result to storage as
changed links (SEAM-01). The tree layer does not see "a rotation"; it sees
which nodes have other children than before.

**R1. A rotation changes links and moves no cell.** Every node stays in its
page. A link whose two ends were in one page may now cross pages, and a
fragment may fall into two: the table of fragment roots changes (F3), and
that is all.

After a rotation across two pages, the path to some nodes passes through one
page more than before. The rules of sections 6 and 7 undo this over time
without a rule of their own: the node that rose is the root of a fragment,
and it goes up to the right page when that fragment is next split (L1) or
found small (M1).

The alternative was simulated: carry the node that rose into the page of the
node it rose above, which restores every path at once. It is worse. The
reference workload then dirties 7.7 pages per transition against 7.0, and a
tree of constant size under deletes and inserts 3.4 against 3.1 with less
fill (section 14.4): the carried nodes fill the pages above, which then split
more. So the simpler rule stands, and no rotation ever costs a value (section
9).

**R2. A node that takes the place of a deleted node takes its page.** When
Merk deletes a node with two children, a node from far below takes its
place in the tree. That node's cell is carried to the page the deleted cell
was in. If the replacement is a new node, it is placed there.

This is the one case where the reshaping of a tree carries a cell. Without
it the replacement would stay in a page near the leaves while standing where
the deleted node stood, which for a node near the top of a large tree is on
the path of most writes. In the churn run it carries a cell in 0.7% of the
transitions; without it that run dirties 3.2 pages per transition against
3.1.

## 9. Out-of-line values

A value above the inline threshold is in the value log, and its cell holds a
value pointer (TREE-08, value-log-format.md section 4). The value log record
names the page that owns the value and cannot be changed afterwards.

**X1. A carried cell takes its value with it.** This is rule O5 of
value-log-format.md, restated for this layout: whenever a rule of sections 6
to 8 carries a cell that holds a value pointer, the value is appended to the
value log again with the new page as its owner, in the same commit, and the
old record is dead.

**X2. What never costs a value.** A hash update, a link update, a rotation
(R1), a change of slot contents, a consolidation and a move of the page's
image inside the store leave the cell in its page, and the value where it
is.

**X3. What does.** L1 and L2 (one cell, with a small hosted Merk), V1 to V3
(a fragment), M1 and M2 (a fragment), R2 (one cell).

**X4. How often.** Section 14.5 gives the measured rates. In short: on a tree
that grows by random keys, 0.83 cells are carried for each cell inserted,
nearly all of them by V3. If the cells hold value pointers, that is 0.81
values appended again for each value written. On a tree that does not grow
the rate is about a thirtieth of that.

This is not a property of these rules. Any layout that keeps pages at a
given fill under random inserts must move content out of full pages into new
ones, and the fill decides how much. For pages that are halved when full it
is 0.5 / ln 2 = 0.72 cells per insert in theory, and 0.68 in the simulation.
A rule that moves less leaves its pages emptier. Section 17, item 9 says what
follows for the plan.

**X5. The rules never look at whether a value is out of line.** A split that
chose its half by the value bytes it would carry would be possible, and in
the reference workload it would change nothing: every node of the document
tree holds a value pointer.

## 10. Two Merks in one page, written by different threads

DEC-03 lets several threads write different Merks inside one transaction.
Iteration one writes on one thread; this section is used from iteration two.
P2 puts the cells of different Merks into one page, so the question is real.

The transaction layer is [`transactions.md`](transactions.md), section 9: a
write scope holds a claim on each Merk it writes (SC3), claims of open scopes
do not overlap, a scope works on private copies of the pages it changes, and
when two open scopes have changed the same page, the one that merges second
has its writes applied again on the merged page (LT4).

**T1. A cell belongs to one Merk, a fragment may belong to several.** Which
Merk a cell belongs to follows from how it was reached; the page does not
record it.

**T2. A scope changes only cells of Merks it holds a claim on, with two
exceptions it can always make safely:**

1. *A hosted fragment travels with its host.* When a rule carries a cell of
   a Merk the scope holds, the small fragments of Merks hosted in that cell
   go with it (L2). They are carried, not otherwise changed.
2. *The pointer to a root is rewritten by whoever moves the root.* (S3)

**T3. Both exceptions stop at a claim of another open scope.**

1. A hosted fragment that belongs to a Merk another open scope has claimed
   is not carried. It stays where it is, as a fragment whose parent is now in
   another page. M1 brings it back later.
2. If the host's Merk is not held by the scope that moves the root, the
   pointer in the host is left stale, and the directory entry (TREE-05),
   which belongs to the moved Merk, is the address to use. The pointer is
   brought up to date when the host's cell is next written by a scope that
   holds it, at the latest by finalize, which rewrites every host whose
   subtree changed (GROVE-11). On existing GroveVersions every write
   propagates to the root, so the host is written by the same write and no
   pointer is ever stale.

**T4. The layout rules are a function of the pages and the change set.**
They use no clock, no thread identity and no state outside the pages a
write holds. So applying a scope's writes again on a merged page (LT4) is
well defined and gives a valid layout. It need not give the layout the scope
had on its private copy, and two interleavings need not give the same pages:
`transactions.md` promises equal logical content (its SC7), not equal bytes.

**T5. What sharing costs.** Two scopes that write Merks with cells in one
page each get a private copy of it and are reconciled at merge. With T2 and
T3 the reconciliation touches only the second scope's own cells. Sharing is
never visible to the caller (LT1): no scope fails or waits for another
because of it.

**T6. No rule keeps the Merks of different scopes apart.** `transactions.md`
offers that as an alternative ("TREE-01 may instead guarantee that Merks
written by different scopes never share a page"). It is not taken: a scope
cannot know which other Merks a later scope will claim, and keeping every
Merk on pages of its own doubles the pages dirtied (section 14.4).

Conflict to resolve with TXN-01 and TXN-09: T3.2 needs the directory to be
readable and writable by a scope for the Merks it holds, and LT4's second
application needs the tree layer to be able to run a change set against a
page that another scope has rearranged. Section 17, item 10.

## 11. Dropping a subtree

**D1. Dropping a Merk writes the page of its host and nothing else.** The
host's cell loses its pointer (or is deleted). The cells of the dropped Merk,
and of every Merk hosted inside it, are not touched by the transaction
(TREE-09: "unlink in O(1)").

**D2. The dropped cells are removed later.** Because of P2 a page can hold
cells of a dropped Merk beside cells that live on. So reclaiming is not only
freeing whole pages: the reclaimer walks the dropped Merk from its root by
its links, removes each cell, and frees a page when its last cell goes. The
walk needs the root's address, which TREE-09 must keep from the moment of
the drop.

**D3. Reclaiming does not merge.** A page that reclaiming leaves nearly
empty stays so until a write touches it (M1, M2). The simulation counts the
dropped cells as gone at once and the pages they leave as not dirtied.

## 12. Parameters

None of these is part of the on-disk format (F5).

| Parameter | Proposed | Meaning |
| --- | --- | --- |
| page size | 32,768 bytes | FMT-02 sets it. Sections 14.2 and 17 item 1 |
| inline threshold | 256 bytes | a longer value goes to the value log. TREE-08 sets it |
| largest fragment | 50% of the capacity | above it the root goes up (L1) |
| small fragment | 1/16 of the page size | below it a fragment belongs with its parent (L2, V1, M1) |
| seed limit | 75% of the capacity | a page takes a fragment in only up to this (V4, M2) |
| merge threshold | 35% of the capacity | a delete below it gives the fragment away (M2) |

Constraints between them, which `Model::new` checks:

- three small fragments fit in a largest one, so that half of a fragment that
  L1 has split is not small and M1 does not carry it straight back
- the merge threshold is at most the seed limit

`Config::proposed()` is this table.

## 13. How the simulation observes Merk

The simulation reimplements nothing of Merk. It runs the real GroveDB on
RocksDB and reads what Merk stored.

**Input.** A workload from `lignin-testkit::workload` (BASE-01), applied with
`workload::apply`, one RocksDB transaction per block. A **transition** is one
`Op::Write` or one `Op::Batch`. Reads are skipped.

**Observation.** After each transition, inside the open transaction, the
observer walks the grove from the root key of the root Merk. It reads node
records through GroveDB's storage (`GroveDb::raw_storage`, the escape hatch
behind the `unsafe-dump-load` feature) and decodes each with Merk's own
`TreeNode::decode_raw`. From a record it takes the links (child key, hash),
the value and value hash, and the lengths that section 2 counts.

**Only what changed is read.** A link stores the hash of the child it points
at, and that hash covers the child's whole subtree. A tree element's value
hash covers the root hash of the Merk it hosts. So if the mirror already has
a child under that key with that hash, the child and everything below it are
unchanged, and the walk stops there. The records the walk does read are
exactly the node records the transition changed, which are the ones Merk
handed to RocksDB.

**Deletions.** A node is deleted if it was a child of a changed node, or the
root, and the walk did not meet it again in its tree. A Merk is dropped if
its host is deleted or its element stops being a tree.

**What comes out.** A change set per transition: the nodes created and
changed, in the order parent before child and host before hosted; for each
its links before and after; the nodes deleted; the nodes that went with a
dropped Merk; and for each node that took the place of another, which one.
The page model sees nothing else.

**Checks on the observer itself.**

- The tests compare the mirror with a raw scan of every Merk's storage
  prefix after every block, on every workload of the BASE-01 suite: the same
  keys, no more and no fewer.
- On the reference workload it counts 46.5 node records and 20.5 KB per
  transition as RocksDB receives them, averaged over the load, and 49.3
  records in its last tenth. The plan states 58.8 records and 23.9 KB for
  this workload. The observer's count is the number of node records whose
  bytes differ after the transition, and the check above shows that it
  misses no node. Why the plan's figure is higher is not known (section 17,
  item 13).

**Limits.** Flat data of the append-only trees is not laid out: it is not in
a Merk. A tree element inside a secondary Merk of an indexed tree could not
be followed (it has no path); none of the workloads has one, and the observer
counts them.

## 14. Results on the reference workload

The workload is `Documents::reference()` with seed 1: seven setup writes,
then one million state transitions, each a batch that inserts one 400-byte
document into the document tree and one reference to it into the tree of its
owner's index, twenty to a block, ten thousand owners. At the end the grove
has 2,020,007 nodes in 20,007 Merks.

All numbers come from one deterministic run and are the same on every
machine. Section 18 gives the commands.

### 14.1 The three targets

The layout is `Config::proposed()` (section 12).

| Target | Required | Measured |
| --- | --- | --- |
| nodes per page | at least 15 | **129.0** (2,020,007 nodes in 15,662 pages) |
| fill | at least 70% | **75.0%** at the end; between 72.1% and 78.0% at every 100,000 transitions |
| pages dirtied per transition | at most 8 | **7.04** over the whole load; **7.21** in its last tenth |

How each is counted:

- **Fill** is the bytes of all page headers and cells over the bytes of all
  pages that hold a cell. Cells are sized by C6.
- **A page is dirtied by a transition** when the transition changes a cell,
  a link or a hash in it. Every node on the path from a changed node to the
  root of the grove has a hash changed, so this counts the pages of the
  whole path, not only where cells were added. A page is counted once per
  transition. An entry of the subtree directory that has to be written
  (a Merk created, dropped, its root changed or its root cell carried) is
  counted as one more page. The 7.04 is 6.90 tree pages and 0.14 directory
  entries.
- Half of the transitions dirty 7 pages or fewer, 99% dirty 10 or fewer. The
  most is 107. Such peaks are directory entries: a fragment that leaves a
  page of the owner index takes the co-located subtrees of its owners with
  it, and each of those roots has its directory entry rewritten (S3). The
  peak grows with the page size (39 at 4 KiB, 203 at 64 KiB in runs of a
  tenth of the size) while the tree pages one split writes do not.

Over the load:

| Transitions | Nodes | Pages | Nodes per page | Fill | Pages dirtied in the interval | Merk records in the interval |
| --- | --- | --- | --- | --- | --- | --- |
| 100,000 | 219,991 | 1,718 | 128.1 | 72.1% | 6.68 | 40.0 |
| 200,000 | 419,993 | 3,305 | 127.1 | 72.8% | 6.93 | 43.8 |
| 300,000 | 619,993 | 4,859 | 127.6 | 73.6% | 6.99 | 45.3 |
| 400,000 | 819,993 | 6,371 | 128.7 | 74.4% | 6.99 | 46.3 |
| 500,000 | 1,019,993 | 8,008 | 127.4 | 73.8% | 6.72 | 47.1 |
| 600,000 | 1,219,993 | 9,187 | 132.8 | 77.0% | 7.54 | 47.7 |
| 700,000 | 1,419,993 | 10,574 | 134.3 | 78.0% | 7.07 | 48.2 |
| 800,000 | 1,619,993 | 12,855 | 126.0 | 73.2% | 7.00 | 48.6 |
| 900,000 | 1,819,993 | 14,183 | 128.3 | 74.6% | 7.28 | 48.9 |
| 1,000,000 | 2,019,993 | 15,662 | 129.0 | 75.0% | 7.21 | 49.3 |

At full size a transition changes 49 node records, which RocksDB rewrites
whole, and they lie in 7.2 pages: 6.8 nodes of the path in each page.

**Counted once per block.** A page that twenty transitions of one block all
change is written back once. Counted that way the load dirties 4.44 pages per
transition. The target is met under either reading; the first is the one
used here.

### 14.2 Page size

Every other parameter as proposed, the whole reference load:

| Page size | Nodes per page | Fill | Pages dirtied per transition | Counted once per block | Cells carried per transition |
| --- | --- | --- | --- | --- | --- |
| 4,096 | 14.2 | 66.6% | 13.85 | 9.48 | 1.72 |
| 8,192 | 30.2 | 70.5% | 10.86 | 7.18 | 1.65 |
| 16,384 | 63.1 | 73.5% | 9.14 | 5.60 | 1.64 |
| **32,768** | **129.0** | **75.0%** | **7.04** | **4.44** | **1.66** |
| 65,536 | 255.3 | 74.2% | 6.59 | 4.19 | 1.62 |

The pages dirtied follow the page size and nothing else does. A page of 4 KiB
holds 14 nodes, which is four levels of a tree; the paths of one transition
are 49 nodes long. No rule can bring that under 8, and at 4 KiB the target of
15 nodes per page is missed as well. 32 KiB is the smallest size that meets
all three targets, with one page to spare. 64 KiB gains less than half a page
more as the table has it, and a whole page (5.99, at 75.6% fill) when its
small fragment is kept at the 2,048 bytes of the 32 KiB layout instead of
doubling with the page.

### 14.3 What the fallback costs

"Halving only" is the layout without the plan's fallback put to use: a
fragment may fill its page, a fragment that leaves always starts a fresh
page, and nothing is merged. The whole reference load at 32 KiB:

| Layout | Nodes per page | Fill, end (lowest sample) | Pages dirtied | Cells carried per transition | Value bytes appended again per transition |
| --- | --- | --- | --- | --- | --- |
| halving only | 116.9 | 68.0% (62.0%) | 6.65 | 1.36 | 296 (64%) |
| largest fragment 100%, seed limit 75% | 117.7 | 68.5% (62.6%) | 6.66 | 1.49 | 318 (69%) |
| seed limit 67% | 123.4 | 71.7% (68.5%) | 7.10 | 1.45 | 324 (71%) |
| **proposed**: largest fragment 50%, seed limit 75% | **129.0** | **75.0% (72.1%)** | **7.04** | **1.66** | **373 (81%)** |
| seed limit 85%, merge threshold 40% | 134.6 | 78.3% (74.1%) | 7.19 | 2.01 | 466 (102%) |

The percentage in the last column is against the 459 bytes of out-of-line
values a transition writes in the first place (one document and its record
header).

- Halving only misses the fill target by two points, and by eight at its
  lowest sample.
- A seed limit alone does nothing: a fragment of half a page finds no page
  that can take it.
- With the largest fragment at half a page, fill follows the seed limit, and
  so do the carried cells. The seven points of fill of the proposed layout
  cost 22% more carried cells, and the next three cost as much again.

The proposed pair is the one that keeps every sample above 70%. A seed limit
of 67% would pass at the end of the load and carries 13% less; it is the
setting to try first if PERF-08 finds the carried bytes too many.

### 14.4 The other rules, one at a time

Each row changes one rule of the proposed layout.

The whole reference load:

| Variant | Fill | Pages dirtied per transition |
| --- | --- | --- |
| proposed | 75.0% | 7.04 |
| small fragment 1/32 of a page instead of 1/16 | 74.7% | 7.25 |
| a rotation carries the node that rose (against R1) | 75.5% | 7.70 |
| inline threshold 512 bytes: the documents are in their cells | 73.3% | 8.71 |

The first 300,000 transitions of it:

| Variant | Nodes per page | Fill | Pages dirtied per transition |
| --- | --- | --- | --- |
| proposed | 127.6 | 73.6% | 6.87 |
| small fragment 1/8 of a page | 122.0 | 70.3% | 8.31 |
| no co-location: every Merk on pages of its own (against P2) | 28.2 | 16.3% | 12.05 |

The small size matters more than its name suggests. A fragment below it
goes into its parent's page, and with the limit at an eighth of a page the
pages near the top of the owner index fill with the reference trees of the
owners they hold, when they should hold more owners.

A tree of constant size: 50,000 keys with 400-byte values, then 200,000
rounds that each delete one key and insert another (`sweep_churn`). Fill is
at the end, the pages dirtied are those of the last 50,000 transitions:

| Variant | Fill after the build | Fill at the end | Pages dirtied per transition | Cells carried per transition at the end |
| --- | --- | --- | --- | --- |
| proposed | 76.4% | 70.3% | 3.07 | 0.024 |
| no merge threshold (without M2) | 76.4% | 68.3% | 3.07 | 0.022 |
| a replacement stays where it was (without R2) | 76.4% | 68.7% | 3.16 | 0.026 |
| a rotation carries the node that rose (against R1) | 77.1% | 66.1% | 3.37 | 0.067 |
| halving only | 68.3% | 60.5% | 3.31 | 0.016 |

### 14.5 Cells carried, by cause

The proposed layout, the whole reference load, per transition (which inserts
two nodes):

| Cause | Rule | Cells carried | Of them with a value pointer | Value bytes appended again |
| --- | --- | --- | --- | --- |
| split | L1, L2, V2, V3 | 1.644 | 0.811 | 372.3 |
| merge | V1, M1, M2 | 0.011 | 0.003 | 1.2 |
| rotation | none (R1) | 0 | 0 | 0 |
| delete | R2 | 0 | 0 | 0 |
| all | | 1.655 | 0.814 | 373.5 |

- 0.83 cells are carried for each cell inserted, and 0.81 documents have
  their value appended again for each document written: 373 bytes on top of
  459.
- Of the split figure, 48,329 cells are fragment roots that went up one at a
  time (L1). The rest left in 26,744 fragments of 60 cells on average (V3),
  11,079 of them to a page beside them and the others to a fresh page.
- The merge row is M1: 4,635 small fragments that rejoined their parent's
  page.

On the tree of constant size of 14.4, at the end: 0.024 cells per transition,
of which 0.007 by R2.

On `large-values` (section 15), where the values are 8 KB on average, 0.23
cells are carried per transition and 1,916 value bytes are appended again,
46% on top of the 4,152 that the workload writes.

## 15. Results on the adversarial workloads

`sweep_adversarial` runs BASE-01's adversarial generators, larger than their
defaults, on the proposed layout. A transition is one direct write or one
batch. "At its largest" is the sample with the most nodes; every one of these
workloads ends by deleting much of what it built.

| Workload | Size | Merk records per transition | Pages dirtied per transition | of them directory entries | At its largest: nodes per page, fill | Fill at the end |
| --- | --- | --- | --- | --- | --- | --- |
| `deep-nesting` | 8 chains of 127 nested trees, 64 leaves each | 88.5 | 3.16 | 1.69 | 148.5, 55.1% (6 pages) | 55.1% |
| `tiny-subtrees` | 20,000 subtrees of 1 to 3 elements | 19.3 | 4.77 | 1.53 | 185.9, 66.5% | 55.7% |
| `delete-heavy` | four trees of 20,000 keys, filled and emptied four times | 16.3 | 2.46 | 0.00 | 181.4, 79.3% | all but 9 nodes deleted |
| `long-keys` | 20,000 keys of 255 bytes | 20.4 | 3.65 | 0.00 | 61.2, 73.7% | 50.6% |
| `large-values` | 4,000 values of up to 16 KB, stored twice | 10.7 | 1.76 | 0.00 | 154.5, 67.4% | 45.0% |

- **Deep nesting.** A write at the bottom of a chain changes 127 Merks, and
  RocksDB is handed 89 node records per transition on average. They lie in
  1.5 tree pages: P2 puts a chain of single-node trees into one fragment.
  More than half of what is counted is directory entries, because the
  workload creates and drops trees throughout.
- **Tiny subtrees.** A subtree sits in the page of its host, so a page holds
  186 nodes of some 60 subtrees. The pages dirtied are again half directory
  entries: every transition creates a Merk.
- **Long keys.** A cell is about 400 bytes, a page holds 61, and a write
  still dirties under 4 pages.
- **Pages dirtied stay below 8 on all five**, and nodes per page above 15.
- **Fill is below 70% wherever much was deleted.** M2 moves a fragment only
  out of a page below the merge threshold of 35%. A workload that deletes a
  third of everything leaves its pages near half full, above that
  threshold, and they stay so until they are written again. During
  `delete-heavy`, fill moves between about 50% and 80% as its trees are
  emptied and refilled. Reclaiming a dropped subtree does not merge either
  (D3), which is what `tiny-subtrees` shows after a third of its subtrees
  are deleted (45% at the lowest sample).
- **`large-values` is the workload for section 9**: 46% more value bytes
  than the workload itself writes.

`cargo test -p lignin-testkit --test layout_sim` also runs every workload of
BASE-01's suite at its default size through the proposed layout and through a
4 KiB layout, recounting both from the mirror as it goes.

## 16. What the tasks that build on this take from it

| Task | Takes from here | Still its own to define |
| --- | --- | --- |
| FMT-02 Tree page codec | What a cell and a link hold (C1 to C4). One-way links (C5). Slot numbers that are stable names, reused lowest first (S1 to S4). A table of fragment roots (F3). No Merk identity per cell (T1). A page is a record like any other: the first byte is its kind. | The byte layout, the page size, whether a link to a cell of the same page is stored shorter (C4), how the table is encoded. |
| FMT-03 Page delta records | A link update never changes a cell's size (C4), so it can be a fixed-size record like a hash update. A carried cell is a cell delete in one page and a cell insert in another. | The records. Whether the log can say "carried from" instead of repeating the cell. |
| TREE-02 Node handles | A node is named by page id and slot; the name is good until the cell is carried or deleted (S1, S2). A view also gives the pointers to hosted roots (C3). | The handle API, and how a handle learns that its cell was carried. |
| TREE-03 Fragment maintenance | Sections 5 to 8 as the rules to implement, on a change set as a whole. F4: move only what the write holds. X1: append the value again for every carried cell with a value pointer. The order in section 6. The parameters of section 12 as configuration, not constants. | In-page allocation. How the used bytes of a page beside a fragment are known without loading it (section 17, item 14). |
| TREE-05 Subtree directory | The entry of a Merk holds the address of its root cell and is rewritten when the root is carried or another node becomes the root (S3). Under parallel scopes it is the address to trust while the host's pointer is stale (T3). | The entry. |
| TREE-06 Ordered cursor | No sibling links (C5): a cursor moves through its parent stack. Consecutive keys are in one page or in pages beside each other (V4). | The cursor. |
| TREE-08 Out-of-line values | A value pointer is 16 bytes in the cell. The threshold is a parameter; 256 bytes was simulated, and 512 puts the reference documents into their cells and costs 1.7 more dirtied pages (section 14.4). | The threshold policy. |
| TREE-09 Subtree drop | D1 to D3: the drop writes one page; reclaiming walks the dropped Merk because its cells can share pages with others. | Keeping the root's address, and the walk. |
| TREE-10 Bulk loader | F5: any placement is valid, so a loader may fill pages to 90% with fragments of its choice. To leave a tree that later writes treat well, a loaded page should hold whole fragments of at most the largest size, and the roots above them should be together in the pages above. | The loader. |
| MERK-01 LigninNodeStore | A change set is applied as a whole, because placement needs each node's parent, and the parent of a new node is in the same change set (P1). | The mapping from `Link` to cell addresses. |
| TXN-01, TXN-09 | Section 10: cells of several Merks share pages; a scope moves only cells it holds or that travel with them; rules are a pure function of pages and change set, so a second application is well defined. | Claims, private copies, reconciliation. |
| VER-07 Physical invariant checker | C1, S1, F1, F3, C5 and X1 as invariants: every node has one cell; every stored address names the cell it means; the fragment-root table equals what the links imply; no cell has two stored addresses besides the directory; every value pointer leads to a record owned by the cell's page. | The checker. The policy rules are not invariants (F5). |
| PERF-04, PERF-08 | Pages dirtied per transition and per block (section 14), cells carried by cause (14.5), and the parameters that trade fill against carried cells (6.4). | Tuning. |

## 17. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one before FMT-02 and TREE-03 build on it.

1. **The proposed page size is 32 KiB.** It is the smallest size at which
   the reference workload dirties at most 8 pages per transition (14.2). The
   targets of 15 nodes per page and 70% fill read like a page of 4 KiB; at
   that size a transition dirties 13.9 pages, or 9.5 counted once per block.
   FMT-02 sets the size. Sixteen-bit offsets allow up to 64 KiB, which would
   dirty 6.6.
2. **"Pages dirtied per transition" is counted per transition, with hash
   updates, with directory entries.** The plan does not say how to count.
   This is the strictest reading. Counted once per block the figure is 4.44.
3. **The fallback is the rule.** A page holds several fragments from the
   start (F2). Without it the fill target is missed: 68.0% (14.3).
4. **The largest fragment is half a page and the seed limit 75%** (6.4).
   That meets the fill target at every sample and costs 0.4 pages dirtied
   and 22% more carried cells than halving alone. All of section 12 is
   policy, not format (F5), so PERF-08 can move these without a format
   version.
5. **Co-location is placement, not a size class** (P2): the root of a Merk
   is placed as a child of its host, and the grove is laid out as one tree.
   There is no "small subtree" format and no flag.
6. **A rotation moves nothing** (R1), and a replacement after a delete
   moves one cell (R2). Both were measured against their alternative
   (14.4).
7. **Links are one-way and of one form** (C4, C5). No parent pointer, no
   sibling link, no back link from a page to the pages that point into it. A
   cell is moved only by a write that holds its parent (F4).
8. **Slot numbers are stable names, reused lowest first** (section 3).
   Tree pages therefore differ from flat pages, whose slots are positions.
9. **Carried cells and the value log's move rule.** The owner asked for this
   to be said plainly. With rule O5 of value-log-format.md, the proposed
   layout appends 0.81 values again for every out-of-line value that the
   reference workload writes: 373 bytes per transition on top of 459. On
   `large-values` it is 46% on top. Nearly all of it is V3, a fragment
   leaving a full page; rotations cost nothing and deletes almost nothing
   (14.5).

   The layout can lower this only by giving up fill: halving alone carries
   18% less and misses the fill target, and no rule that keeps pages 70%
   full under random inserts carries much less than 0.7 cells per insert
   (X4). It is the price of clustering a growing tree into pages at all, as
   in any B-tree, where it is paid in rewritten cells. What O5 adds is that
   the value is rewritten with the cell, which the engine otherwise avoids.

   So the cost of O5 on a growing tree is about 0.8 extra value writes per
   value, and the dead records they leave are 45% of what the value log
   receives, to be cleaned later. Whether that is acceptable is the owner's
   call; the alternatives listed in section 8, item 6 of value-log-format.md
   avoid it, each at the price it names there: a set of dead values for
   each file, the owner's key in every record, or a table from value id to
   location. On a tree that is updated and not grown the cost is small
   (0.024 cells per transition).
   The simulator reports the figure for any workload and layout
   (`Stats::moves`), so PERF-08 can measure it on real traces.
10. **Two Merks of different scopes may share a page** (section 10), and the
    rule that keeps this safe is "a scope moves only cells it holds a claim
    on, plus the small hosted fragments that travel with them, unless
    another open scope claims those". Two things are needed from the
    transaction layer and are not yet stated in `transactions.md`: a scope
    must be able to read and write the directory entry of a Merk it holds
    (T3.2), and LT4's second application of a scope's writes must go through
    the tree layer, because the cells it wrote may have been carried by the
    scope that merged first.
11. **The inline threshold is taken as 256 bytes.** TREE-08 owns it. It must
    stay below the size of the documents: at 512 bytes the reference
    workload dirties 8.7 pages (14.4).
12. **A delete merges only below 35% of a page** (M2), and reclaiming a
    dropped subtree does not merge (D3). Fill after mass deletion is
    therefore 45% to 56% on the adversarial workloads (section 15) until the
    pages are written again.
13. **The reference workload gives fewer node records than the plan
    states**: 46.5 per transition and 20.5 KB on average, 49.3 at full size,
    against 58.8 and 23.9 KB. BASE-03 has to reproduce the plan's figures
    within 10% and will meet the same gap. If the plan's workload has paths
    a fifth longer than BASE-01's stream, the pages dirtied on it would be
    about one higher than measured here, which is at the limit of 8.
14. **TREE-03 needs the used bytes of a page it has not loaded** to choose a
    page beside a fragment (V4, M2). The simulation assumes that this is
    known, for example from the page table. If it is not, V4 has to load the
    candidates, or be restricted to pages already in the buffer pool, which
    would make the layout depend on the cache.
15. **Reclaiming a dropped subtree is a walk, not a list of pages** (D2),
    because of P2. TREE-09's "all its pages are reclaimed" becomes "all its
    cells are removed, and a page is freed when it is empty".
16. **A directory entry holds the address of the root cell** and is counted
    as a page when it is written. It is 0.14 of the 7.04, in peaks: one
    split in the owner index rewrites the entries of every co-located
    subtree it carries (14.1). If TREE-05 kept the root's address only in
    the host's cell, by-prefix opens would need another way to find it.
17. **The cell sizes of C6 are assumptions.** FMT-02 fixes the layout;
    `CellCosts` holds the six numbers and the simulation can be run again.
18. **The simulator reads Merk's records through `GroveDb::raw_storage`**,
    so `lignin-testkit` now depends on `grovedb` with the `unsafe-dump-load`
    feature, and on `grovedb-storage` and `grovedb-path` directly. Nothing
    in `grovedb`, `merk` or `storage` was changed. CI already builds with
    all features.

## 18. Code, tests and reproduction

The simulator is `lignin-testkit::layout_sim`.

| Part | Code | Tests |
| --- | --- | --- |
| The mirror of the grove and a change set (section 13) | `tree::{Grove, ChangeSet, Event}` | Hand-built trees: the order of events; which node takes whose place after a single rotation, a double rotation, a delete with one child, a deleted root; dropping a Merk with Merks inside it. |
| Observation (section 13) | `observe::Observer` | `tests/layout_sim.rs`: every workload of BASE-01's suite, with the mirror compared against a raw scan of every Merk's storage prefix at the end of every block (`Observer::verify`). |
| Cells (C6) | `model::{CellCosts, Config::cell_bytes}` | Each part of a cell: links, a value above the threshold, a hosted root. |
| Placement (section 5) | `Model::apply` | A node goes with its parent; a hosted root goes with its host, and not when co-location is off; a new node in the place of a deleted one goes to its page. |
| Largest size, overflow (section 6) | `Model::cap_fragments`, `settle_overflow`, `split`, `move_out`, `beside` | A full page keeps the top and sends about half away; a fragment root goes up; a fragment above the largest size is split where it is; a fragment that leaves goes to a page beside it when the seed limit allows, which raises fill; the choice of parts and its tie-breaks. |
| Merge (section 7) | `Model::absorb_small`, `merge_underfull` | A small fragment rejoins its parent's page; a page below the threshold gives its fragment to a page beside it, and does not without the threshold. |
| Rotation, delete (section 8) | `Model::apply` | A rotation moves nothing; with `carry_on_rotation` the node that rose is carried; a replacement takes the deleted node's page, and does not without `carry_on_delete`. |
| Values (section 9) | `Model::carry`, `Stats::moves` | A carried cell with an out-of-line value is counted as one value appended again, with its record header. |
| Drop (section 11) | `Model::apply` | The nodes of a dropped Merk leave their pages without dirtying them; the host's page and one directory entry are written. |
| Counting (14.1) | `report::{Stats, Report}` | Pages per transition and per block; percentiles; the last tenth of a run; the printed report. |
| Bookkeeping | `Model::check` | Every kind of damage to the model's tables is reported. The integration tests recount every layout from the mirror, after every transition on the small documents workload and every sixteenth on the others. |

Unit tests and the small workloads:

```text
cargo test -p lignin-testkit --lib layout_sim
cargo test -p lignin-testkit --test layout_sim
```

The acceptance test of the task, section 14.1. It loads one million
documents into a RocksDB in a temporary directory (about 1 GB; set
`LAYOUT_SIM_DIR` to choose where) and takes 15 to 40 minutes:

```text
cargo test --release -p lignin-testkit --test layout_sim -- --ignored --nocapture the_reference_workload_meets_the_layout_targets
```

The tables of sections 14 and 15 come from the three sweeps in the same file.
A layout is given as
`page:largest fragment %:seed limit %:merge below %:inline:small divisor:colocate:rotation:delete`,
later fields optional, several separated by commas; all of them are laid out
in one pass over one run of GroveDB.

```text
# 14.2, 14.3 and the first table of 14.4
LAYOUT_SIM_DOCUMENTS=1000000 LAYOUT_SIM_OWNERS=10000 \
LAYOUT_SIM_CONFIGS="32768,4096,8192,16384,65536,32768:100:0:0,32768:100:75:35,32768:50:67:35,32768:50:85:40,32768:50:75:35:256:32,32768:50:75:35:256:16:true:true,32768:50:75:35:512" \
cargo test --release -p lignin-testkit --test layout_sim -- --ignored --nocapture sweep_documents

# the second table of 14.4
LAYOUT_SIM_DOCUMENTS=300000 LAYOUT_SIM_OWNERS=10000 \
LAYOUT_SIM_CONFIGS="32768,32768:50:75:35:256:8,32768:50:75:35:256:16:false" \
cargo test --release -p lignin-testkit --test layout_sim -- --ignored --nocapture sweep_documents

# the third table of 14.4
LAYOUT_SIM_CONFIGS="32768,32768:50:75:0,32768:50:75:35:256:16:true:false:false,32768:50:75:35:256:16:true:true:true,32768:100:0:0" \
cargo test --release -p lignin-testkit --test layout_sim -- --ignored --nocapture sweep_churn

# section 15
cargo test --release -p lignin-testkit --test layout_sim -- --ignored --nocapture sweep_adversarial
```

The runs are deterministic: the workload streams are pinned by BASE-01, and
the layout uses no clock, no random number and no hash-map order.
