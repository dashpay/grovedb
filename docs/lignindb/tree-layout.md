# LigninDB tree layout: the Merkle host model

This is the specification for task TREE-01 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
fixes how the nodes of Merk trees are clustered into pages:

- what a cell and a link hold (section 2)
- when a slot number may change (section 3)
- fragments, and what a write may move (section 4)
- where a new node goes, when it is placed, and where the root of a subtree
  goes (section 5)
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

It builds on [`format.md`](format.md) (page ids, section 3.1),
[`value-log-format.md`](value-log-format.md) (the move rule O5),
[`transactions.md`](transactions.md) (write scopes, kept locations) and
[`node-store.md`](node-store.md) (the change set Merk hands to a store).

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
- **Capacity**: the bytes of a page that cells can use: the page image, which
  is the page and at most 65,535 bytes (P10 of `flat-page.md`), less the
  page header.
- **Fragment**: the cells of one page that are joined by links that stay
  inside the page (4.1). Its **root** is the one cell whose parent is not in
  the page.
- **Carry**: to move a cell from one page to another. The node does not
  change; its cell gets a new address.
- **Write**: the application of the change sets of one GroveDB operation:
  one change set for each Merk the operation wrote (node-store.md), applied
  together when the operation's storage batch is applied. On existing
  GroveVersions those are the Merks on the way up to the GroveDB root. A
  Merk that commits outside a batch is a write of its own. A state
  transition of the simulation is one write.

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

So a link that crosses pages holds the page id and the slot of the child's
cell, the child's hash, its two heights and its aggregate data, and nothing
else: no key, no version, no mark of the Merk. The slot is in it because
slot numbers are stable names (S1).

**C3. A pointer to a hosted root is a cell address and nothing else.** Merk
stores no hash of a hosted Merk's root beside the element: the host's value
hash already covers it. The root key and the tree type stay where they are
today, inside the value. The pointer names the root cell in every committed
state; T3 says when it may be behind inside a transaction.

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
| header of a value-log record | 18, once for each part of a value (value-log-format.md, section 2 and V6) |

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

**C7. A cell is smaller than half the capacity of a page.** A key is at most
255 bytes and a value in the cell at most the inline threshold, so with the
proposed threshold the largest cell is about 700 bytes. FMT-02's page size
and TREE-08's threshold must keep a cell below half a page: section 6 relies
on it (V5), as a B-tree relies on a node holding two keys.

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
equal pages. The id of a page that a write empties (M3) is not used again
before that write has committed; how much later is for STORE-10 and
`transactions.md` to say. So within one write a page id names one page.

**S5. Child pointers need stable slots. The value log does not.** If slots
were positions, as in a flat page (P13 of `flat-page.md`), every insert into
a page would have to rewrite the links that point into it from other pages,
and by C5 nobody knows which pages those are. The value log names the owning
page and no slot (value-log-format.md, O1), so nothing in it depends on S1.

**S6. An address kept in memory is a hint.** S3 keeps every stored address
exact. A copy outside the pages is another matter: the location in a Merk
link, a node handle, a path cache. The cell can be carried or deleted while
the copy lives, by a write of the same transaction, and its slot can then be
given to another cell (S4), of the same Merk or of another, and with the
same key: co-located subtrees often hold the same keys. So:

- a holder checks a kept address before it follows it (PN8 of
  transactions.md), and finds the node by its key from the root of the Merk
  when the check fails
- comparing the key of the cell that is found is not a check
- a check by page version is safe, but it fails for every page the last
  write touched, and those are the pages a Merk that is kept open follows
  next. S1 and S2 allow a narrower one: an address into a page is good for
  as long as no cell has left that page since the address was read. One
  count for each page, of the cells that left it, is enough. TREE-02 chooses
  the mechanism

A holder that takes in what every write reports (P5) never fails the check
because of its own transaction.

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
nodes in its change sets and the cells it has carried. In a page where it
holds a cell it may read every cell. It may carry a cell only if it also
holds the cell that points at it, because that pointer must be rewritten (S3)
and C5 gives no other way to find it.

Every rule below keeps to this. The fragment a rule moves is always the one a
held cell is in, and its parent is held too: a write changes a hash in every
node from the one it touches up to the root, so the whole path is in its
change sets. (From iteration two a write can stop at a stop point below the
host of its Merk; section 10 says what holds then.)

**F5. Any placement is valid.** A reader follows addresses and never assumes
that two cells share a page, that a fragment has some size, or that a page
holds one Merk. What must hold is C1, C7, S1 to S4, F1, F3, and that a
page's cells fit in it. Sections 5 to 8 can therefore be changed, or tuned
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

**P5. A node is placed when its write is applied, not when it is prepared.**
Merk hands a store its nodes in steps (node-store.md): `prepare_node` during
the commit walk, children before parents; `put_node` after the walk, in key
order; then `commit`, which on a batch only stages the change set until the
change sets of all Merks of the operation are applied together. A page is
chosen in that last step, for three reasons.

- P1 needs the page of the parent. `prepare_node` is not told the parent,
  the parent is prepared after the child, and the parent may be new itself.
  The parent of a Merk's root is a cell of another Merk (P2), in another
  change set of the same operation.
- A node that takes the place of a deleted one goes to its page (R2), and
  the removals are named only after the walk.
- Section 6 looks at a page after everything the write does to it: what it
  adds, what it grows and what it deletes. Placing node by node would carry
  cells out of a page that a later node of the same write empties.

So placement runs over the whole write from the top down: a parent before
its children, a host before the Merk it hosts.

What follows for the location in a Merk link (node-store.md, "Following a
location"):

1. The address of a written node does not exist while `prepare_node` and
   `put_node` run, and on a batch it does not exist when `NodeStore::commit`
   returns. It exists when the write has been applied, and can only come
   back from there: from the application of the batch, which is the engine
   seam's (SEAM-04, TXN-08).
2. A write changes the address of two kinds of cell only: those it creates
   and those it carries. Every other cell it writes keeps its address (S1),
   whatever was written into it. On the reference workload a write writes 46
   nodes, creates 2 and carries 1.6.
3. Not every cell a write carries is a node of its change sets. A fragment
   travels with its root, and a small hosted Merk with its host (L2). So a
   Merk can hold a location that went stale without its having written the
   node (S6).
4. When a write has been applied, the tree layer knows every address it
   assigned. It can report them: subtree, key and address, for each node it
   created and each cell it carried.

A Merk that is kept open across writes has two ways to stay right. It can
take that report and correct the links it holds, and then it follows no link
by key. Or it can drop the location of what it wrote and find each such
node by key the first time it follows the link again, which costs one keyed
descent for each created or carried node it comes back to. PERF-03 asks for
no keyed lookup on the write path, which needs the first. MERK-01 and
PERF-03 choose; either is safe, because a kept location is checked before it
is used (S6).

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

L1 is applied once in a write, before 6.2. A fragment that V1 or M1 then
takes above the largest size, by less than the small size, is cut by the
next write that holds it.

### 6.2 A page above its capacity

Take the fragment of the overfull page that the write holds (F4).

**V1. If it is small, it goes to its parent's page**, when it is below the
small size and has a parent.

**V2. If it is at least half the capacity, it is split.** Its root goes up as
in L1 (or stays, if it has no parent). If one of the parts that are left is
still at least half the capacity and more than three quarters of them, that
part's root goes up too, and so on. Then the parts whose bytes together come
closest to what the page holds above half its capacity leave together. With
up to twelve parts every choice is compared. With more, the parts are taken
largest first, each only if it brings the sum closer.

**V3. Otherwise the whole fragment leaves.**

**V4. Where it goes.** A fragment that leaves by V2 or V3 goes to a **page
beside it**: a page that holds another child of the fragment that its parent
is in. Of those it takes the fullest one that, with the fragment added, stays
within the seed limit. If there is none, it starts a fresh page.

**V5. Repeat until no page is above its capacity.** Each step takes bytes out
of the overfull page or sends one cell up, so it ends. It needs C7: a
fragment of at least half the capacity then has more than one cell, so
cutting it always leaves a part that can go.

**V6. Ties are broken the same way everywhere.** Of two choices of parts
that are equally close, the one that moves fewer bytes is taken, then the
one whose last part comes earlier in the order the parts were cut off: left,
right, hosted, and the parts of a later cut after those of an earlier one.
Of two pages that are equally full, the one with the lower page id.

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
With a seed limit of 75% a growing tree carries 0.81 cells for each cell
inserted, against 0.67 without (section 14.3). Section 9 says why that number
matters.

## 7. A page a delete has emptied

**M1. A small fragment moves into its parent's page when there is room.**
After a write, every fragment it holds that is below the small size, and
whose parent is in another page, is carried to that page if it fits within
the capacity. This is what brings a subtree back to its host after a split
separated them, and what removes a fragment that deletes have shrunk to
nearly nothing.

**M2. A page below the merge threshold gives its fragments away.** When a
delete leaves a page below the merge threshold, each fragment of that page
that the write holds goes to the fullest page beside it that stays within
the seed limit (V4). A fragment for which there is none stays.

**M3. A page with no cell is freed.** Its id goes back to the allocator
(STORE-10).

As proposed, the merge threshold is below half the seed limit, so two pages
that qualify fit into one, and a page that has just taken a fragment in is
not the next to give one away. Any threshold up to the seed limit is valid.

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

What counts is where the cell is when the write ends. A cell that one write
carries twice has its value appended once, for the page it ends in, and a
cell that ends in the page it started in costs nothing. A cell that gets a
new value in the write that carries it costs nothing more than the new
value, which is written once with the final page as owner.

**X2. What never costs a value.** A hash update, a link update, a rotation
(R1), a change of slot contents, a consolidation and a move of the page's
image inside the store leave the cell in its page, and the value where it
is.

**X3. What does.** L1 and L2 (one cell, with a small hosted Merk), V1 to V3
(a fragment), M1 and M2 (a fragment), R2 (one cell).

**X4. How often.** Section 14.5 gives the measured rates. In short: on a tree
that grows by random keys, 0.81 cells are carried for each cell inserted,
nearly all of them by V3. If the cells hold value pointers, that is 0.80
values appended again for each value written. On a tree that does not grow
the rate is about a thirtieth of that.

This is not a property of these rules. Any layout that keeps pages at a
given fill under random inserts must move content out of full pages into new
ones, and the fill decides how much. For pages that are halved when full it
is 0.5 / ln 2 = 0.72 cells per insert in theory, and 0.67 in the simulation.
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

**T2. A scope changes only cells of Merks it holds a claim on, with one
exception: a small hosted fragment travels with its host.** When a rule
carries a cell of a Merk the scope holds, the small fragments of the Merks
hosted in that cell go with it (L2). They are carried, not otherwise
changed. The exception stops at a claim: a hosted fragment of a Merk that
another open scope has claimed is not carried. It stays where it is, as a
fragment whose parent is now in another page, and M1 brings it back later.

**T3. A scope that does not hold the host leaves the host's cell alone.**
From iteration two a write can stop below the host of the Merk it writes,
at a stop point, and its scope then has no claim on the Merk the host is
in. Two things follow.

1. *It does not carry the root.* For such a write the root of its Merk
   counts as a cell without a parent. L1, V1, M1 and M2 leave the root's
   fragment where it is, and V2 keeps the root and sends parts away, as for
   the root of the grove. This is F4 and no new rule: a cell is carried only
   by a write that holds the cell pointing at it. So S3 holds without
   exception.
2. *It cannot name a new root in the host.* When another node becomes the
   root of the Merk, or the Merk becomes empty or stops being empty, the
   pointer in the host's cell (C3) is behind until the host is written.
   GroveDB is in the same position one level up: the host's element holds
   the root key, and the write has put a pending mark on that element for
   finalize to clear (SC9 of transactions.md). The pointer follows the mark:

   - the directory entry of the Merk is written by the scope that holds the
     Merk, in the same write, and is exact at every moment
   - a pointer in a host whose element is marked pending is not followed.
     Whoever needs the root takes it from the directory entry
   - finalize rewrites the element and with it the pointer, and then removes
     the mark (GROVE-11). A commit refuses a transaction that has a mark
     left (CM1), so in every committed state every pointer is exact

   This is the one case in which a stored address may be behind, and it is
   bounded: inside an open transaction of iteration two, in a host that is
   marked pending.

On existing GroveVersions every write propagates to the root in one
operation. The host is in the write, no mark is ever set, and neither case
arises.

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

To settle with TXN-01 and TXN-09: T3 needs the directory entry of a Merk to
belong to that Merk's unit of exclusion (SC2) and the pending marks to be
visible to the tree layer, and LT4's second application needs the tree layer
to be able to run a change set against a page that another scope has
rearranged. Section 17, item 10.

## 11. Dropping a subtree

**D1. Dropping a Merk writes the page of its host and one directory entry,
and nothing else.** The host's cell loses its pointer (or is deleted), and
the directory entry of the dropped Merk goes. The cells of the dropped Merk
are not touched by the transaction, and neither are the cells and the
directory entries of the Merks hosted inside it (TREE-09: "unlink in O(1)").

**D2. The dropped cells are removed later.** Because of P2 a page can hold
cells of a dropped Merk beside cells that live on. So reclaiming is not only
freeing whole pages: the reclaimer walks the dropped Merk from its root by
its links, removes each cell, and frees a page when its last cell goes. On
the way it removes the directory entries of the Merks nested in it and
reports the value pointers the cells held (O6 of value-log-format.md). The
walk needs the root's address, which TREE-09 must keep from the moment of
the drop.

**D3. Reclaiming comes after the commit and does not merge.** Until the
reclaimer has come, the cells of a dropped Merk keep their room in their
pages, and a write that needs room there does not get theirs. A page that
reclaiming leaves nearly empty stays so until a write touches it (M1, M2).

The simulation does the same. The cells of a dropped Merk leave the layout
at once, keep their bytes in their pages to the end of the block, and are
then removed. A page the reclaimer has to write for that is counted on its
own, not as a page a transition dirtied (section 15).

**D4. Clearing a Merk is a drop that keeps the host.** The host's cell loses
its pointer, the directory entry is rewritten as that of an empty Merk, and
the cells go by D2. On RocksDB a clear deletes every record in the
transaction, and `Merk::clear` is a cursor and one `delete_node` for each
node (node-store.md). The engine does not have to work that way.

GroveDB already drops this way on RocksDB where it can: for a tree that
holds no tree, `drop_flat_subtree` removes the element, records the storage
prefix in the meta keyspace, and `flush_pending_prefix_drops` deletes the
records later, outside the transaction.

**D5. Moving a subtree moves no cell.** GroveDB's `Move` renames a key
inside its tree and keeps what is under it. On RocksDB it copies every
record under the old path to the storage prefix of the new path, for every
tree nested below as well. The layout needs none of that:

- the cell at the old key is deleted and the cell at the new key is a new
  node of the same Merk (P1, R2)
- the new cell gets the pointer to the root of the moved Merk (C3), and the
  cells of that Merk stay in their pages, which F5 allows. If the root's
  fragment is small, M1 brings it to the page of its new host
- what must be rewritten is the directory: its entries are found by path
  hash (TREE-05), and the path of the moved Merk and of every Merk nested in
  it has changed

So an engine can move a subtree for the price of two cells and one directory
entry for each Merk under the moved key. node-store.md already names the
move as an operation of the engine, "a change in the subtree directory, not
a copy", with the copy as RocksDB's way of doing it. The simulation observes
RocksDB, so it measures the copy (section 15, and section 17, item 19).

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
direct write or one batch, whole or in segments, that changes a Merk node. A
write that changes none, such as an aux record, is applied and not counted.
Reads are skipped. A flush of pending drops is applied, and the run stops if
it changed a Merk node.

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
its host is deleted, or its element stops being a tree or becomes another
kind of indexed tree. A Merk that is empty after the transition lost its
last node to a delete if it had one node. If it had more it is read as
cleared (D4): the records cannot tell a clear from a batch that deletes
every key, and the first is the usual one.

**Moves.** A moved subtree (D5) is seen as RocksDB sees it: the Merk under
the old key is dropped with everything nested in it, and under the new key
every node is new.

**The observation point.** Since SEAM-01, Merk hands every node it writes
to a `NodeStore` (node-store.md): one `put_node` for each node of the commit
walk, with a kind (`ValueWritten`, `StructureChanged`, `HashOnly`), and one
`delete_node` for each removal. That is what LigninDB will be handed, and a
store that records it would be the most direct place to observe. The
simulation cannot use it yet. GroveDB opens its Merks on the RocksDB context
type itself, and the adapter is an implementation for every storage context,
not a wrapper, so a run of the real GroveDB has no place for a second store
until it opens its Merks through an engine (SEAM-04). Every number here
comes from the diff of stored records described above. The two differ in
three ways:

- *A write that changes no byte.* A `put_node` whose record equals the
  stored one is a write for the store (ST5 of transactions.md; the root of a
  Merk whose batch changed nothing is one) and invisible to the diff. It
  changes no byte of a page, and no page is counted for it.
- *The kind.* The diff says what the bytes show: a value counts as written
  when its bytes differ. Merk claims the wider kind where it cannot prove
  the narrower one, so the tree element of every Merk on the path is
  `ValueWritten` where the diff sees a changed hash. MERK-01 and MERK-02 are
  to narrow that. The layout rules do not look at the kind. It decides only
  what section 14 counts as a value written.
- *The unit.* The diff is taken once for each GroveDB operation and shows
  what the operation changed in all. A store is handed one change set for
  each Merk, a child before its parent. The rules apply to the operation as
  a whole (P5), which is what the diff gives.

A `ChangeSet` of the simulator is already a change set in the store's sense:
the nodes written, with what changed in each, and the nodes removed. When
GroveDB can open its Merks on a recording store, the model can be fed from
it unchanged.

**What comes out.** A change set per transition: the nodes created and
changed, in the order parent before child and host before hosted; for each
its links before and after; the nodes deleted; the nodes that went with a
dropped Merk; and for each node that took the place of another, which one.
The page model sees nothing else.

**Checks on the observer itself.**

- `Observer::verify` reads every record under the storage prefix of every
  Merk the mirror knows and compares it with the mirror: the same keys, no
  more and no fewer, each record byte for byte the one the mirror read, and
  its children and their hashes the ones the mirror has. It also compares
  the Merks each element hosts, and the root each names, with the mirror,
  and the root of the root Merk with the one GroveDB states. So the mirror
  misses no Merk: every Merk is reached through an element of a Merk that
  was checked.
- When it runs. The tests run it after every block on every workload of
  BASE-01's suite. The large runs run it at every sample, after every flush
  of pending drops and at the end: on the reference workload that is every
  100,000 transitions. A comparison reads every record, so one for each of
  fifty thousand blocks of two million nodes is out of reach. Between two
  comparisons a miss could be put right by a later write to the same node
  before it is seen. What guards against that is the runs that compare at
  every block, on the same generators and the same code.
- On the reference workload it counts 46.5 node records and 20.5 KB per
  transition as RocksDB receives them, averaged over the load, and 49.3
  records in its last tenth. The plan states 58.8 records and 23.9 KB for
  this workload. The observer's count is the number of node records whose
  bytes differ after the transition, and the check above finds every record
  of that run as the mirror has it. Why the plan's figure is higher is not
  known (section 17, item 13).

**Limits.** Flat data of the append-only trees is not laid out: it is not in
a Merk. A tree inside a secondary Merk of an indexed tree could not be
followed: a secondary Merk has no path, so a tree under it would have no
storage prefix. GroveDB writes only reference rows into a secondary Merk,
and the observer stops the run if it meets a tree there.

## 14. Results on the reference workload

The workload is `Documents::reference()` with seed 1: seven setup writes,
then one million state transitions, each a batch that inserts one 400-byte
document into the document tree and one reference to it into the tree of its
owner's index, twenty to a block, ten thousand owners. At the end the grove
has 2,020,007 nodes in 20,007 Merks.

All numbers come from one deterministic run and are the same on every
machine. They were taken with GroveDB as it is on `feat/LigninDB` at
`922d0d3a`, after SEAM-01, on the latest GroveVersion. Section 18 gives the
commands.

### 14.1 The three targets

The layout is `Config::proposed()` (section 12).

| Target | Required | Measured |
| --- | --- | --- |
| nodes per page | at least 15 | **129.0** (2,020,007 nodes in 15,662 pages) |
| fill | at least 70% | **75.0%** at the end; between 72.1% and 78.0% at every 100,000 transitions |
| pages dirtied per transition | at most 8 | **7.04** over the whole load; **7.21** in its last tenth |

How each is counted:

- **Fill** is the bytes of all page headers and of the cells of all nodes
  that exist, over the bytes of all pages that hold a cell. Cells are sized
  by C6. What a dropped Merk left in a page counts as empty, and a page that
  holds nothing else still counts as a page until the reclaimer frees it.
- **A page is dirtied by a transition** when the transition changes a cell,
  a link or a hash in it. Every node on the path from a changed node to the
  root of the grove has a hash changed, so this counts the pages of the
  whole path, not only where cells were added. A page is counted once per
  transition. An entry of the subtree directory that has to be written
  (a Merk created or cleared, its root changed, its root cell carried, and
  one for a dropped subtree whatever is nested in it, D1) is counted as one
  more page. An entry is counted when a rule carries the root cell, also
  when a later rule of the same write brings the cell back. The 7.04 is
  6.90 tree pages and 0.14 directory entries.
- **The target is met as an average.** No rule bounds what one transition
  dirties. Half of the transitions dirty 7 pages or fewer, 99% dirty 10 or
  fewer, and the most is 107. Such peaks are directory entries: a fragment
  that leaves a page of the owner index takes the co-located subtrees of
  its owners with it, and each of those roots has its directory entry
  rewritten (S3). The peak grows with the page size (27 at 4 KiB, 55 at
  16 KiB, 213 at 64 KiB) while the tree pages one split writes do not.

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
| 4,096 | 14.2 | 66.6% | 13.85 | 9.48 | 1.53 |
| 8,192 | 30.2 | 70.5% | 10.86 | 7.18 | 1.55 |
| 16,384 | 63.1 | 73.5% | 9.14 | 5.60 | 1.59 |
| **32,768** | **129.0** | **75.0%** | **7.04** | **4.44** | **1.63** |
| 65,536 | 255.4 | 74.2% | 6.59 | 4.19 | 1.60 |

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
| halving only | 116.9 | 68.0% (62.0%) | 6.65 | 1.35 | 295 (64%) |
| largest fragment 100%, seed limit 75% | 117.7 | 68.5% (62.6%) | 6.66 | 1.47 | 317 (69%) |
| seed limit 67% | 123.4 | 71.7% (68.5%) | 7.10 | 1.42 | 320 (70%) |
| **proposed**: largest fragment 50%, seed limit 75% | **129.0** | **75.0% (72.1%)** | **7.04** | **1.63** | **368 (80%)** |
| seed limit 85%, merge threshold 40% | 134.6 | 78.3% (74.1%) | 7.19 | 1.97 | 459 (100%) |

The percentage in the last column is against the 459 bytes of out-of-line
values a transition writes in the first place (one document and its record
header).

- Halving only misses the fill target by two points, and by eight at its
  lowest sample.
- A seed limit alone does nothing: a fragment of half a page finds no page
  that can take it.
- With the largest fragment at half a page, fill follows the seed limit, and
  so do the carried cells. The seven points of fill of the proposed layout
  cost 21% more carried cells, and the next three cost as much again.

The proposed pair is the one that keeps every sample above 70%. A seed limit
of 67% would pass at the end of the load and carries 12% less; it is the
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
| no merge threshold (without M2) | 76.4% | 68.3% | 3.07 | 0.021 |
| a replacement stays where it was (without R2) | 76.4% | 68.7% | 3.16 | 0.026 |
| a rotation carries the node that rose (against R1) | 77.1% | 66.1% | 3.37 | 0.067 |
| halving only | 68.3% | 60.5% | 3.31 | 0.016 |

### 14.5 Cells carried, by cause

The proposed layout, the whole reference load, per transition (which inserts
two nodes). A cell counts when a transition leaves it in another page than
it found it in, and its value when the transition did not write that value
anyway (section 9):

| Cause | Rule | Cells carried | Of them with a value pointer | Value bytes appended again |
| --- | --- | --- | --- | --- |
| split | L1, L2, V2, V3 | 1.615 | 0.800 | 367.2 |
| merge | V1, M1, M2 | 0.011 | 0.003 | 1.2 |
| rotation | none (R1) | 0 | 0 | 0 |
| delete | R2 | 0 | 0 | 0 |
| all | | 1.626 | 0.803 | 368.4 |

- 0.81 cells are carried for each cell inserted, and 0.80 documents have
  their value appended again for each document written: 368 bytes on top of
  459.
- Of the split figure, 48,329 cells are fragment roots that went up one at a
  time (L1). The rest left in 26,744 fragments of about 60 cells (V3),
  11,079 of them to a page beside them and the others to a fresh page.
- The merge row is M1: 4,635 small fragments that rejoined their parent's
  page.

On the tree of constant size of 14.4, at the end: 0.024 cells per transition,
of which 0.007 by R2.

On `large-values` (section 15), where the values are 8 KB on average, 0.23
cells are carried per transition and 1,899 value bytes are appended again,
46% on top of the 4,152 that the workload writes.

## 15. Results on the adversarial workloads

`sweep_adversarial` runs BASE-01's adversarial generators, larger than their
defaults, on the proposed layout. A transition is one direct write or one
batch. "At its largest" is the sample with the most nodes; every one of these
workloads ends by deleting much of what it built.

| Workload | Size | Merk records per transition | Pages dirtied per transition: average | of them directory entries | 99% of transitions at most; the most | At its largest: nodes per page, fill | Fill at the end | Reclaimed after the blocks: cells, page writes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `deep-nesting` | 8 chains of 127 nested trees, 64 leaves each | 88.5 | 3.05 | 1.58 | 5; 195 | 148.5, 55.1% (6 pages) | 55.1% | 1,266, 8 |
| `tiny-subtrees` | 20,000 subtrees of 1 to 3 elements | 19.3 | 4.77 | 1.53 | 28; 80 | 185.9, 66.5% | 55.6% | 13,370, 1,687 |
| `delete-heavy` | four trees of 20,000 keys, filled and emptied four times | 16.3 | 2.46 | 0.00 | 10; 14 | 181.4, 79.3% | all but 9 nodes deleted | 2,448, 8 |
| `long-keys` | 20,000 keys of 255 bytes | 20.4 | 3.65 | 0.00 | 16; 20 | 61.2, 73.7% | 50.6% | none |
| `large-values` | 4,000 values of up to 16 KB, stored twice | 10.7 | 1.76 | 0.00 | 4; 4 | 154.5, 67.4% | 45.0% | none |
| `moves` | values, references and one tree of every type, moved back and forth 50 times | 15.2 | 3.84 | 2.84 | 12; 20 | 96.0, 37.4% (1 page) | 37.4% | 4,550, 500 |
| `index-rekey` | 20,000 documents indexed by 440 values, every value tree moved, then 2,000 more documents | 35.3 | 5.49 | 0.25 | 9; 99 | 101.8, 70.9% | 70.9% | 16,808, 11 |

- **Deep nesting.** A write at the bottom of a chain changes 127 Merks, and
  RocksDB is handed 89 node records per transition on average. They lie in
  1.5 tree pages: P2 puts a chain of single-node trees into one fragment.
  More than half of what is counted is directory entries, because the
  workload creates and drops trees throughout.
- **Tiny subtrees.** A subtree sits in the page of its host, so a page holds
  186 nodes of some 60 subtrees. A third of what is counted is directory
  entries: every transition creates a Merk.
- **Long keys.** A cell is about 400 bytes, a page holds 61, and a write
  still dirties under 4 pages.
- **Moves.** A move is measured as the copy RocksDB makes (D5). `moves` is
  a hundred nodes in one page: every transition writes that page, and the
  rest of what is counted is directory entries, one for each subtree a move
  drops and each Merk it creates. In `index-rekey` the 440 value trees, each
  with a tree of about 45 references inside, are moved in 55 batches of
  eight (`sweep_moves`). A batch writes 400 Merk records, drops 306 cells
  and creates 377, and dirties 10.3 tree pages and 22 directory entries, 40
  pages at most. The pages go from 74% full to 68%, because the copies do
  not land where the dropped cells were. An engine that re-hosts would
  write the two cells of each move and the same directory entries, and no
  cell of the moved trees.
- **The average of pages dirtied stays below 8 on all seven**, and nodes per
  page above 15. Single transitions go far above 8 on six of them. The
  largest are transitions that create or drop many Merks at once, each a
  directory entry: 195 for a chain of 127 nested trees, 80 in
  `tiny-subtrees`.
- **Fill is below 70% wherever much was deleted.** M2 moves a fragment only
  out of a page below the merge threshold of 35%. A workload that deletes a
  third of everything leaves its pages near half full, above that
  threshold, and they stay so until they are written again. During
  `delete-heavy`, fill moves between about 50% and 80% as its trees are
  emptied and refilled. Reclaiming a dropped subtree does not merge either
  (D3), which is what `tiny-subtrees` shows after a third of its subtrees
  are deleted (45% at the lowest sample).
- **The reclaimer writes pages too**, after the block that dropped the
  cells (D3). Where a dropped subtree had pages of its own they are freed
  whole: 11 page writes for the 16,808 cells of `index-rekey`. Where it
  shared its page with its neighbours the page is written again: 1,687 page
  writes for the 13,370 cells of `tiny-subtrees`. These writes are in none
  of the per-transition figures.
- **`large-values` is the workload for section 9**: 46% more value bytes
  than the workload itself writes.

`cargo test -p lignin-testkit --test layout_sim` also runs every workload of
BASE-01's suite, nineteen of them, at its default size through the proposed
layout and through a 4 KiB layout, recounting both from the mirror as it
goes. Among them are partial batches, flat drops with their flush, aux
records and the moves.

## 16. What the tasks that build on this take from it

| Task | Takes from here | Still its own to define |
| --- | --- | --- |
| FMT-02 Tree page codec | What a cell and a link hold (C1 to C4). A cell is smaller than half a page (C7). One-way links (C5). Slot numbers that are stable names, reused lowest first (S1 to S4). A table of fragment roots (F3). No Merk identity per cell (T1). A page is a record like any other: the first byte is its kind. | The byte layout, the page size, whether a link to a cell of the same page is stored shorter (C4), how the table is encoded. |
| FMT-03 Page delta records | A link update never changes a cell's size (C4), so it can be a fixed-size record like a hash update. A carried cell is a cell delete in one page and a cell insert in another. | The records. Whether the log can say "carried from" instead of repeating the cell. |
| TREE-02 Node handles | A node is named by page id and slot; the name is good until the cell is carried or deleted (S1, S2). A name kept in memory is a hint, and the key of the cell found does not prove it (S6). A view also gives the pointers to hosted roots (C3). | The handle API, and how a handle learns that its cell was carried: a count for each page of the cells that left it is enough (S6). |
| TREE-03 Fragment maintenance | Sections 5 to 8 as the rules to implement, on a change set as a whole. F4: move only what the write holds. X1: append the value again for every carried cell with a value pointer. The order in section 6. The parameters of section 12 as configuration, not constants. | In-page allocation. How the used bytes of a page beside a fragment are known without loading it (section 17, item 14). |
| TREE-05 Subtree directory | The entry of a Merk holds the address of its root cell and is rewritten when the root is carried or another node becomes the root (S3). Under parallel scopes it is the address to use while the host's element is marked pending (T3). A drop removes one entry and leaves the nested ones to the reclaimer (D1, D2). A move that keeps the cells re-keys every entry under the moved key (D5). | The entry. |
| TREE-06 Ordered cursor | No sibling links (C5): a cursor moves through its parent stack. Consecutive keys are in one page or in pages beside each other (V4). | The cursor. |
| TREE-08 Out-of-line values | A value pointer is 16 bytes in the cell. The threshold is a parameter; 256 bytes was simulated, and 512 puts the reference documents into their cells and costs 1.7 more dirtied pages (section 14.4). | The threshold policy. |
| TREE-09 Subtree drop | D1 to D4: the drop writes one page and one directory entry; reclaiming walks the dropped Merk because its cells can share pages with others, removes the directory entries nested in it and reports the value pointers. | Keeping the root's address, and the walk. |
| GROVE-03 Batch pipeline | D5: a moved subtree needs no cell to change its page. | Whether a move re-hosts or copies. |
| GROVE-05 Deletes and drops | D1 for a deleted tree, D4 for a cleared one. GroveDB's pending-drop records are the same idea at the level of storage prefixes. | The mapping. |
| TREE-10 Bulk loader | F5: any placement is valid, so a loader may fill pages to 90% with fragments of its choice. To leave a tree that later writes treat well, a loaded page should hold whole fragments of at most the largest size, and the roots above them should be together in the pages above. | The loader. |
| MERK-01 LigninNodeStore | A node is placed when its write is applied, not at `prepare_node` or `put_node` (P5), and the write is all change sets of the operation. The address of a written node comes back from there. The location in a `Link` is a page id and a slot, a hint that is checked before use (S6). | The mapping from `Link` to cell addresses, and how the addresses come back (P5). |
| PERF-03 Pointer write path | A write changes the address of the cells it creates and carries, and of no other (P5). Taking the tree layer's report of them keeps every kept link exact, so no link is followed by key. | The path cache. |
| SEAM-04, TXN-08 | The application of an operation's batch is where pages are chosen and where addresses come back from (P5). | The batch across subtrees. |
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
   FMT-02 sets the size. A page of 64 KiB is the largest that 16-bit offsets
   serve, with one byte of it unused (flat-page.md, P10), and would dirty
   6.6.
2. **"Pages dirtied per transition" is counted per transition, with hash
   updates, with directory entries.** The plan does not say how to count.
   This is the strictest reading. Counted once per block the figure is 4.44.
3. **The fallback is the rule.** A page holds several fragments from the
   start (F2). Without it the fill target is missed: 68.0% (14.3).
4. **The largest fragment is half a page and the seed limit 75%** (6.4).
   That meets the fill target at every sample and costs 0.4 pages dirtied
   and 21% more carried cells than halving alone. All of section 12 is
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
   layout appends 0.80 values again for every out-of-line value that the
   reference workload writes: 368 bytes per transition on top of 459. On
   `large-values` it is 46% on top. Nearly all of it is V3, a fragment
   leaving a full page; rotations cost nothing and deletes almost nothing
   (14.5).

   The layout can lower this only by giving up fill: halving alone carries
   17% less and misses the fill target, and no rule that keeps pages 70%
   full under random inserts carries much less than 0.7 cells per insert
   (X4). It is the price of clustering a growing tree into pages at all, as
   in any B-tree, where it is paid in rewritten cells. What O5 adds is that
   the value is rewritten with the cell, which the engine otherwise avoids.

   So the cost of O5 on a growing tree is about 0.8 extra value writes per
   value, and the dead records they leave are 44% of what the value log
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
    another open scope claims those", with a root whose host the scope does
    not hold staying where it is. Three things are needed from the
    transaction layer and are not yet stated in `transactions.md`: the
    directory entry of a Merk belongs to that Merk's unit, so the scope that
    holds the Merk can write it (T3); the tree layer can ask whether a
    host's element is marked pending (T3); and LT4's second application of a
    scope's writes goes through the tree layer, because the cells it wrote
    may have been carried by the scope that merged first.
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
    the host's cell, by-prefix opens would need another way to find it. A
    dropped subtree is counted as one entry (D1): the entries of the Merks
    nested in it are left to the reclaimer, which TREE-05's "the directory
    always agrees with the parent elements" has to allow for.
17. **The cell sizes of C6 are assumptions.** FMT-02 fixes the layout;
    `CellCosts` holds the six numbers and the simulation can be run again.
18. **The simulator reads Merk's records through `GroveDb::raw_storage`**,
    so `lignin-testkit` now depends on `grovedb` with the `unsafe-dump-load`
    feature, and on `grovedb-storage` and `grovedb-path` directly. Nothing
    in `grovedb`, `merk` or `storage` was changed. CI already builds with
    all features.
19. **A move can be a re-hosting** (D5): no cell changes its page, and one
    directory entry is rewritten for each Merk under the moved key. RocksDB
    copies every record, and that is what the simulation measures for
    `moves` and `index-rekey` (section 15). Which of the two GROVE-03
    builds is open. Re-hosting needs the directory to re-key the entries
    below a path, and under today's keying by path hash that is one write
    per nested Merk.
20. **A clear is a drop that keeps the host** (D4), and the simulation
    counts it so. A Merk emptied by a batch that deletes all of its several
    keys at once is counted the same way, because the records do not tell
    the two apart (section 13).
21. **A cell must be smaller than half a page** (C7). At the proposed sizes
    the largest cell is about 700 bytes of 32,736. The simulator refuses a
    run in which it does not hold.
22. **A node is placed when its write is applied** (P5), which answers the
    point node-store.md left open (its choice 10). The unit is the
    operation: the change sets of all its Merks together, because a root is
    placed beside a host that is in another change set. So a location
    cannot come back through `prepare_node`, and on a batch not through
    `NodeStore::commit`. The tree layer can report the addresses of what a
    write created and carried, and PERF-03's "no keyed lookup on the write
    path" needs that report to reach the Merks that are kept open.
23. **A location in memory is checked by more than the key** (S6). The
    proposal of one count for each page, of the cells that left it, is a
    recommendation to TREE-02 and FMT-02, not a rule of the layout.
24. **The numbers come from a diff of stored records, not from a recording
    `NodeStore`** (section 13). The diff sees no write that changes no byte,
    reports the narrowest kind where Merk reports a wider one, and takes an
    operation as a whole. None of the three changes a page that is counted.

## 18. Code, tests and reproduction

The simulator is `lignin-testkit::layout_sim`.

| Part | Code | Tests |
| --- | --- | --- |
| The mirror of the grove and a change set (section 13) | `tree::{Grove, ChangeSet, Event}` | Hand-built trees: the order of events; which node takes whose place after a single rotation, a double rotation, a delete with one child, a deleted root; dropping a Merk with Merks inside it. |
| Observation (section 13) | `observe::Observer` | `tests/layout_sim.rs`: every workload of BASE-01's suite, with the mirror compared against every stored record at the end of every block (`Observer::verify`); a write the observer did not see is found, in a record, in a tree's root and in the root of the root Merk; a Merk emptied by its last delete, a cleared Merk and a deleted tree with a tree inside are told apart. |
| Cells (C6) | `model::{CellCosts, Config::cell_bytes}` | Each part of a cell: links, a value above the threshold, a hosted root. A page of 64 KiB keeps one byte unused. Each configuration the model refuses. |
| Placement (section 5) | `Model::apply` | A node goes with its parent; a hosted root goes with its host, and not when co-location is off; a new node in the place of a deleted one goes to its page. |
| Largest size, overflow (section 6) | `Model::cap_fragments`, `settle_overflow`, `split`, `move_out`, `beside`, `choose` | The first split of a page sends about half of it away and keeps the top; a fragment root goes up to its parent's page; a fragment above the largest size is split where it is; a fragment that leaves goes to a page beside it when the seed limit allows, which raises fill; the choice of parts, its tie-breaks, and more than twelve parts; a cell of half a page is refused (C7). |
| Merge (section 7) | `Model::absorb_small`, `merge_underfull` | A small fragment rejoins its parent's page; a page below the threshold gives its fragment to a page beside it, and does not without the threshold. |
| Rotation, delete (section 8) | `Model::apply` | A rotation moves nothing; with `carry_on_rotation` the node that rose is carried; a replacement takes the deleted node's page, and does not without `carry_on_delete`. |
| Values (section 9) | `Model::carry`, `Model::count_moves`, `Stats::moves`, `Config::value_log_bytes` | A cell that ends a transition in another page is counted once, with its out-of-line value and the record header; not when it is back where it was, not when the transition created it, and without the value when the transition wrote the value anyway. A value longer than one record pays a header for each part. |
| Drop (section 11) | `Model::apply`, `Model::reclaim`, `Grove::drop_merk` | The nodes of a dropped Merk leave the layout without dirtying their pages; the host's page and one directory entry are written, whatever is nested in the Merk; the cells keep their room until the block ends; the reclaimer frees a page that held nothing else and writes one that did. |
| Counting (14.1) | `report::{Stats, Report}` | Pages per transition and per block; percentiles; the last tenth of a run; the printed report. |
| Bookkeeping | `Model::check` | Damage to each of the model's tables is reported: a node in no page, or in one after it is gone; a cell size; the cell count; a page's totals, dead cells included; the page count; the byte total; the free pages; a page above its capacity. A freed page is not handed out before the block ends. A run that ends without a commit ends its block. The integration tests recount every layout from the mirror, after every transition on the small documents workload and every sixteenth on the others. The large runs recount at every sample, and every run at its end. |

Unit tests and the small workloads:

```text
cargo test -p lignin-testkit --lib layout_sim
cargo test -p lignin-testkit --test layout_sim
```

The acceptance test of the task, section 14.1. It loads one million
documents into a RocksDB in a temporary directory (about 1 GB; set
`LAYOUT_SIM_DIR` to choose where) and takes 10 to 40 minutes:

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
LAYOUT_SIM_CONFIGS="32768,4096,8192,16384,65536,32768:100:0:0,32768:100:75:35,32768:50:67:35,32768:50:85:40,32768:50:75:35:256:32,32768:50:75:35:256:16:true:true,32768:50:75:35:512,65536:50:75:35:256:32" \
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
cargo test --release -p lignin-testkit --test layout_sim -- --ignored --nocapture sweep_moves
```

The runs are deterministic: the workload streams are pinned by BASE-01, and
the layout uses no clock, no random number and no hash-map order.
