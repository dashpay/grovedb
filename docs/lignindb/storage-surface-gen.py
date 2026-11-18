#!/usr/bin/env python3
"""Build docs/lignindb/storage-surface.md from the search output.

Every column of the inventory tables except the site, function, item and count
comes from the rules in this file: class, seam, cost and note. The sources the
notes are derived from are read at the pinned commit with `git show`.

    python3 -I docs/lignindb/storage-surface-gen.py --rev <commit> \
        --sites sites.tsv --template docs/lignindb/storage-surface.template.md \
        > docs/lignindb/storage-surface.md

`storage-surface-search.py check` runs this and requires the document to be
byte-for-byte equal to its output.
"""
import argparse
import re
import subprocess
import sys
from collections import Counter, defaultdict, OrderedDict

ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
ap.add_argument("--rev", required=True)
ap.add_argument("--sites", required=True)
ap.add_argument("--template", required=True)
ARGS = ap.parse_args()

calls, types = [], []
section = None
for line in open(ARGS.sites):
    line = line.rstrip("\n")
    if line.startswith("## "):
        section = line[3:]
        continue
    if not line or line.startswith("#"):
        continue
    if section in ("calls", "types"):
        loc, cnt, item, fn, scope = line.split("\t")
        (calls if section == "calls" else types).append(
            dict(loc=loc, cnt=int(cnt), item=item, fn=fn, scope=scope))

src_cache = {}


def src_lines(path):
    if path not in src_cache:
        src_cache[path] = subprocess.run(
            ["git", "show", "%s:%s" % (ARGS.rev, path)], check=True,
            capture_output=True, text=True).stdout.split("\n")
    return src_cache[path]


def dropped(site):
    p, l = site["loc"].rsplit(":", 1)
    l = int(l)
    lines = src_lines(p)
    text = " ".join(x.strip() for x in lines[l - 1:l + 4])
    meth = site["item"].split("::")[-1]
    i = text.find(meth + "(")
    if i < 0:
        return False
    rest = text[i:]
    j = rest.find("(")
    depth, k = 0, j
    while k < len(rest):
        if rest[k] == "(":
            depth += 1
        elif rest[k] == ")":
            depth -= 1
            if depth == 0:
                break
        k += 1
    return bool(re.match(r"\s*\.(unwrap\(\)|value\b)", rest[k + 1:k + 40]))


def crate_of(path):
    return path.split("/")[0]


def lno(loc):
    return int(loc.rsplit(":", 1)[1])


def path_of(loc):
    return loc.rsplit(":", 1)[0]


APPEND_CRATES = {
    "grovedb-merkle-mountain-range",
    "grovedb-bulk-append-tree",
    "grovedb-dense-fixed-sized-merkle-tree",
    "grovedb-commitment-tree",
    "grovedb-private-document-store",
}

# --- where a grovedb context belongs -------------------------------------
FLAT_GROVEDB_FILES = (
    "grovedb/src/operations/bulk_append_tree.rs",
    "grovedb/src/operations/commitment_tree.rs",
    "grovedb/src/operations/dense_tree.rs",
    "grovedb/src/operations/mmr_tree.rs",
    "grovedb/src/operations/private_document_store.rs",
    "grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs",
    "grovedb/src/replication/non_merk_sync.rs",
)
FLAT_GROVEDB_LINES = {
    "grovedb/src/lib.rs": set(range(3300, 3800)),
    "grovedb/src/operations/proof/generate.rs": {4319, 4408, 4502, 4505, 4595},
    "grovedb/src/batch/mod.rs": {7795, 7803, 8653, 8661},
    "grovedb/src/operations/delete/clear_subtree/v0.rs": {73, 77},
    "grovedb/src/operations/delete/clear_subtree/v1.rs": {146, 150},
    "grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs": {207, 215},
    "grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs": {239, 247},
}
FLAT_TEST_FILES = (
    "grovedb/src/tests/commitment_tree_tests.rs",
    "grovedb/src/tests/flat_drop_tests.rs",
    "grovedb/src/tests/non_merk_integrity_audit_tests.rs",
    "grovedb/src/tests/non_merk_subtree_discovery_tests.rs",
    "grovedb/src/tests/private_document_store_tests.rs",
    "grovedb-commitment-tree/",
)
ESCAPE_ITER_LINES = {
    # raw record copy between namespaces
    "grovedb/src/batch/move_element.rs": {637, 638, 642, 643, 661, 674, 914, 915, 946},
    # node fetched and decoded by GroveDB through a pinned iterator
    "grovedb/src/operations/indexed_tree.rs": {3526, 3527, 3528, 3532},
}
# cursors over the aux column family (`raw_iter_aux`)
AUX_ITER_LINES = {
    "grovedb/src/operations/auxiliary.rs": {159, 162, 168, 172},
}
ESCAPE_KV_LINES = {
    "grovedb/src/operations/get/mod.rs": {607},
    "grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs": {394},
    "grovedb/src/batch/move_element.rs": {808, 815, 953, 960},
    "grovedb/src/lib.rs": {4686},
}


def in_lines(table, loc):
    p = path_of(loc)
    return lno(loc) in table.get(p, ())


def is_flat(site):
    p = path_of(site["loc"])
    c = crate_of(p)
    if c in APPEND_CRATES:
        return True
    if p.startswith(FLAT_GROVEDB_FILES) or p.startswith(FLAT_TEST_FILES):
        return True
    return in_lines(FLAT_GROVEDB_LINES, site["loc"])


IMMEDIATE_LINES = {
    "grovedb/src/replication/non_merk_sync.rs": {676},
    "grovedb/src/replication/state_sync_session.rs": {891, 893},
}

NOTES = {
    "merk/src/tree/encoding.rs:73": "`TreeNode::get`: the node fetch behind `Fetch` (one record per node)",
    "merk/src/merk/mod.rs:1091": "`fetch_node`: parent node read during chunk restore (`Restorer`, `restore.rs:661`)",
    "merk/src/merk/mod.rs:424": "`Merk::commit`: change set of one commit, written as one batch",
    "merk/src/merk/mod.rs:465": "root-key pointer when the root moved (cost info only for standalone Merks)",
    "merk/src/merk/mod.rs:481": "root-key pointer removed when the tree became empty",
    "merk/src/merk/mod.rs:501": "every node the committer wrote, with child sizes and storage cost",
    "merk/src/merk/mod.rs:510": "deleted node keys, with removed-bytes cost info",
    "merk/src/merk/mod.rs:519": "aux puts carried in the same commit",
    "merk/src/merk/mod.rs:522": "aux deletes carried in the same commit",
    "merk/src/merk/mod.rs:535": "commit of the batch; GROVE versions with `merk_versions.batch.commit == 0` drop the cost it returns",
    "merk/src/merk/mod.rs:608": "`set_base_root_key`: writes the root-key pointer outside a commit, no cost info",
    "merk/src/merk/mod.rs:612": "`set_base_root_key(None)`: no cost info, so the batch commit estimates freed bytes from committed state",
    "merk/src/merk/mod.rs:628": "`load_base_root`: reads the root-key pointer on open",
    "merk/src/merk/clear.rs:19": "`Merk::clear`: walks every node record of the Merk",
    "merk/src/merk/clear.rs:25": "one delete per node, no cost info",
    "merk/src/merk/clear.rs:31": "commit of the clear batch",
    "merk/src/merk/restore.rs:384": "chunk restore writes restored nodes in one batch (immediate context in state sync)",
    "merk/src/merk/restore.rs:635": "batch commit; caller reads `.value` and drops the cost",
    "merk/src/merk/restore.rs:688": "rewrites a parent node's link record in place",
    "merk/src/merk/restore.rs:773": "height rewrite pass after restore",
    "merk/src/merk/restore.rs:951": "aggregate rewrite pass after restore",
    "merk/src/merk/meta.rs:18": "Merk meta cache, read-through",
    "merk/src/merk/get.rs:134": "`Merk::get_aux`",
    "merk/src/proofs/query/count_offset/pinned_view.rs:93": "point read of a stored node through a pinned iterator view",
    "merk/src/proofs/query/count_offset/pinned_view.rs:96": "cost of the key read is not billed (`.unwrap()`)",
    "merk/src/proofs/query/count_offset/pinned_view.rs:99": "cost of the value read is not billed; the caller charges one seek and the bytes itself",
    "merk/src/element/get.rs:386": "`Element::get_optional_from_storage_v0`: raw node record read, decoded as an element",
    "merk/src/element/get.rs:516": "`Element::get_optional_from_storage_v1`",
    "merk/src/merk/mod.rs:176": "`KVIterator`: ordered walk of node records",
    "merk/src/merk/mod.rs:561": "`is_empty_tree`: one seek",
    "merk/src/merk/mod.rs:569": "`is_empty_tree_except`",
    "merk/src/test_utils/temp_merk.rs:68": "test helper compiled into the lib by the `test_utils` feature",
    "grovedb-query/src/query_item/mod.rs:1109": "`QueryItem::seek_for_iter`: positions a node iterator for a query range",
    "grovedb-query/src/query_item/mod.rs:1236": "`iter_is_valid_for_type`",
    "grovedb-merkle-mountain-range/src/storage_adapter.rs:159": "`MmrStore`: MMR node by position",
    "grovedb-merkle-mountain-range/src/storage_adapter.rs:207": "`MmrStore::append`",
    "grovedb-bulk-append-tree/src/tree/append.rs:743": "cached MMR root record",
    "grovedb-bulk-append-tree/src/tree/append.rs:783": "persists the MMR root record; cost dropped (zero on a transactional context)",
    "grovedb-bulk-append-tree/src/tree/append.rs:809": "MMR root record; read cost dropped by the caller",
    "grovedb-commitment-tree/src/commitment_tree/mod.rs:228": "frontier record on open",
    "grovedb-commitment-tree/src/commitment_tree/mod.rs:644": "frontier record on save",
    "grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:484": "value at a position",
    "grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:535": "value at a position",
    "grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:599": "hash record",
    "grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:687": "hash record",
    "grovedb/src/lib.rs:374": "`GroveDb::open`",
    "grovedb/src/lib.rs:433": "`GroveDb::wipe`: one range tombstone per column family, outside any transaction",
    "grovedb/src/lib.rs:481": "SST ingest into a column family (`unsafe-dump-load`), outside any transaction",
    "grovedb/src/lib.rs:536": "Merk opened by a known prefix (indexed secondaries)",
    "grovedb/src/lib.rs:579": "state-sync restore: Merk on an immediate context",
    "grovedb/src/lib.rs:584": "state-sync restore: parent Merk on an immediate context",
    "grovedb/src/lib.rs:674": "root Merk",
    "grovedb/src/lib.rs:1923": "snapshot read transaction (RocksDB inherent method)",
    "grovedb/src/lib.rs:3364": "verify: flat store context; cost dropped",
    "grovedb/src/lib.rs:3500": "verify: flat store context; cost dropped",
    "grovedb/src/lib.rs:3647": "flat store state root; cost dropped",
    "grovedb/src/merk_cache.rs:66": "`batch: Default::default()`, found by the `Default` probe",
    "grovedb/src/operations/auxiliary.rs:59": "`Default::default()`, found by the `Default` probe",
    "grovedb/src/operations/auxiliary.rs:92": "`Default::default()`, found by the `Default` probe",
    "grovedb/src/util/visitor.rs:58": "`batch: Default::default()`, found by the `Default` probe",
    "grovedb/src/merk_cache.rs:157": "`MerkCache` opens each Merk once per batch",
    "grovedb/src/merk_cache.rs:453": "marks an operation boundary in the shared `StorageBatch`",
    "grovedb/src/operations/auxiliary.rs:63": "root-path context used for aux",
    "grovedb/src/operations/auxiliary.rs:96": "root-path context used for aux",
    "grovedb/src/operations/auxiliary.rs:127": "root-path context used for aux",
    "grovedb/src/operations/auxiliary.rs:155": "root-path context used for aux",
    "grovedb/src/operations/auxiliary.rs:158": "prefix scan of aux keys",
    "grovedb/src/operations/auxiliary.rs:258": "clears every Merk subtree found under a deleted path",
    "grovedb/src/operations/auxiliary.rs:283": "clears an indexed tree's secondary namespace",
    "grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs:271": "merges the traversal's batch over the main one",
    "grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs:394": "raw node-record delete during a recursive subtree teardown, bypassing Merk",
    "grovedb/src/operations/delete/flat_drop.rs:309": "pending-prefix-drop redo record",
    "grovedb/src/operations/delete/flat_drop.rs:312": "meta namespace of pending-prefix-drop records",
    "grovedb/src/operations/delete/flat_drop.rs:321": "redo record staged in the same commit as the parent delete",
    "grovedb/src/operations/delete/flat_drop.rs:410": "DB-level read of committed redo records, outside any transaction",
    "grovedb/src/operations/delete/flat_drop.rs:417": "range tombstones over each doomed prefix in all four column families, outside any transaction",
    "grovedb/src/operations/delete/flat_drop.rs:419": "DB-level delete of the redo record",
    "grovedb/src/operations/get/mod.rs:607": "`has_raw`: existence of a node record, read directly",
    "grovedb/src/operations/dense_tree.rs:236": "dense tree value read directly by GroveDB",
    "grovedb/src/operations/indexed_tree.rs:393": "indexed secondary Merk by derived prefix",
    "grovedb/src/operations/indexed_tree.rs:1483": "parent context whose iterator is retargeted to the secondary",
    "grovedb/src/operations/indexed_tree.rs:1514": "one pinned iterator reused across prefixes",
    "grovedb/src/operations/indexed_tree.rs:3526": "`snapshot_fetch_node`: node fetched and decoded by GroveDB through a pinned iterator",
    "grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs:229": "commitment frontier read; caller reads `.value` and drops the cost",
    "grovedb/src/operations/proof/generate.rs:2780": "scratch batch for payload reads",
    "grovedb/src/operations/proof/generate.rs:4505": "commitment frontier read; caller reads `.value` and drops the cost",
    "grovedb/src/element/local_offset_read.rs:70": "prefix of each path level for a retargeted view",
    "grovedb/src/element/local_offset_read.rs:71": "one pinned iterator walks several subtrees",
    "grovedb/src/element/local_offset_read.rs:186": "prefix of the host subtree for a retargeted view",
    "grovedb/src/element/local_offset_read.rs:190": "one pinned iterator walks several subtrees",
    "grovedb/src/batch/key_info.rs:85": "`Ord for KeyInfo`: orders batch keys by `max_length`, not a cost",
    "grovedb/src/batch/batch_structure.rs:72": "synthetic key for keyless ops, not a cost",
    "grovedb/src/batch/mod.rs:6979": "pending-prefix-drop redo record",
    "grovedb/src/batch/mod.rs:6982": "meta namespace of pending-prefix-drop records",
    "grovedb/src/batch/mod.rs:6989": "redo record staged in the batch",
    "grovedb/src/batch/mod.rs:7803": "clears a deleted flat tree's data namespace",
    "grovedb/src/batch/mod.rs:7884": "clears an indexed tree's secondary namespace",
    "grovedb/src/batch/mod.rs:8265": "partial batch: builds a RocksDB `WriteBatch` directly",
    "grovedb/src/batch/mod.rs:8661": "clears a deleted flat tree's data namespace",
    "grovedb/src/batch/mod.rs:8729": "clears an indexed tree's secondary namespace",
    "grovedb/src/batch/mod.rs:8772": "partial batch: appends to the RocksDB `WriteBatch`",
    "grovedb/src/batch/mod.rs:8782": "partial batch: commits the RocksDB `WriteBatch` into the transaction",
    "grovedb/src/batch/move_element.rs:637": "raw record copy between namespaces",
    "grovedb/src/batch/move_element.rs:808": "raw record moved to the destination namespace",
    "grovedb/src/batch/move_element.rs:815": "raw record removed from the source namespace",
    "grovedb/src/batch/move_element.rs:900": "source namespace by prefix",
    "grovedb/src/batch/move_element.rs:908": "destination namespace by prefix",
    "grovedb/src/batch/move_element.rs:914": "raw scan of the source namespace",
    "grovedb/src/batch/move_element.rs:946": "raw scan of the source namespace",
    "grovedb/src/batch/move_element.rs:953": "raw record moved to the destination namespace",
    "grovedb/src/batch/move_element.rs:960": "raw record removed from the source namespace",
    "grovedb/src/replication/non_merk_sync.rs:302": "flat store page served by chunk prefix",
    "grovedb/src/replication/non_merk_sync.rs:308": "commitment frontier; cost dropped",
    "grovedb/src/replication/non_merk_sync.rs:674": "flat store restore on an immediate context",
    "grovedb/src/replication/non_merk_sync.rs:676": "frontier written directly; cost dropped",
    "grovedb/src/replication/state_sync_session.rs:888": "incomplete-restore marker on the root path",
    "grovedb/src/replication/state_sync_session.rs:891": "incomplete-restore marker set",
    "grovedb/src/replication/state_sync_session.rs:893": "incomplete-restore marker cleared",
    "grovedb/src/replication/state_sync_session.rs:1066": "indexed secondary restored on an immediate context",
    "grovedb/src/replication/verify.rs:32": "walks every node record of a restored Merk",
    "grovedb/src/replication/verify.rs:236": "indexed secondary checked after restore",
    "grovedb/src/util.rs:31": "commits a transaction the call opened itself",
    "grovedb/src/util/compat.rs:45": "generic Merk open",
}

# --- classification -------------------------------------------------------
TXN = {"Storage::start_transaction", "Storage::commit_transaction", "Storage::rollback_transaction",
       "Tx::commit", "Tx::rollback", "Tx::set_savepoint", "Tx::rollback_to_savepoint",
       "Tx::snapshot_age", "Tx::is_snapshot_read", "RocksDbStorage::start_snapshot_read_transaction"}
LIFE = {"TempStorage::default", "RocksDbStorage::default_rocksdb_with_path", "RocksDbStorage::checkpoint_rocksdb_with_path",
        "Storage::create_checkpoint", "Storage::flush", "TempStorage::new", "TempStorage::crash"}
BATCH_GROVE = {"StorageBatch::new", "StorageBatch::default", "StorageBatch::len", "StorageBatch::is_empty",
               "StorageBatch::take_pending", "StorageBatch::merge", "StorageBatch::merge_overwriting",
               "StorageBatch::next_operation", "Storage::commit_multi_context_batch"}
WRITE_BATCH = {"RocksDbStorage::build_write_batch", "RocksDbStorage::continue_write_batch",
               "RocksDbStorage::commit_db_write_batch"}


def method(item):
    return item.split("::")[-1]


def iter_cost(m):
    if m in ("seek", "seek_to_first", "seek_to_last", "seek_for_prev", "next", "prev"):
        return "I-move"
    if m in ("key", "valid"):
        return "I-key"
    if m == "value":
        return "I-value"
    return "zero"


def classify(site):
    loc, item = site["loc"], site["item"]
    p = path_of(loc)
    c = crate_of(p)
    m = method(item)
    imm = in_lines(IMMEDIATE_LINES, loc)
    generic = c == "merk" or c in APPEND_CRATES
    flat = is_flat(site)
    test = site["scope"] == "test"

    def rw(kind):
        if kind == "R":
            return "R-tx / R-imm" if generic else ("R-imm" if imm else "R-tx")
        return "W-tx / W-imm" if generic else ("W-imm" if imm else "W-tx")

    if item in TXN:
        return "transaction", "Engine", "zero"
    if item in LIFE:
        return "lifecycle", "Engine", "zero"
    if item in BATCH_GROVE:
        cost = "B-commit" if item.endswith("commit_multi_context_batch") else "zero"
        return "batch", "Engine", cost
    if item in WRITE_BATCH:
        cost = "B-commit" if m != "build_write_batch" else "B-commit (staged)"
        return "escape hatch", "Engine", "B-commit"
    if item == "Storage::get_transactional_storage_context":
        return "path open", "Engine", "P-path"
    if item == "Storage::get_transactional_storage_context_by_subtree_prefix":
        return "by-prefix open", "Engine", "zero"
    if item == "Storage::get_immediate_storage_context":
        return "immediate context", "Engine", "P-path"
    if item == "Storage::get_immediate_storage_context_by_subtree_prefix":
        return "immediate context", "Engine", "zero"
    if item in ("Storage::get_storage_context_cost", "WorstKeyLength::max_length"):
        return "estimate", "Estimates", "EST"
    if item == "RocksDbStorage::build_prefix":
        return "prefix", "Engine", "P-path"
    if item == "RocksDbStorage::secondary_prefix_for":
        return "prefix", "Engine", "P-sec"
    if item == "pending_prefix_drops_namespace":
        return "prefix", "FlatStore", "zero"
    if item == "PendingPrefixDropRecord::encode":
        return "meta", "FlatStore", "zero"
    if item == "PrefixedRocksDbRawIterator::retarget":
        return "escape hatch", "Engine", "zero"
    if item == "PrefixedRocksDbTransactionContext::clear":
        return "range delete", ("FlatStore" if flat else "Engine"), "CLR"
    if item in ("RocksDbStorage::wipe", "RocksDbStorage::delete_prefix_ranges"):
        return "range delete", "Engine", "DB"
    if item in ("RocksDbStorage::pending_prefix_drop_records",
                "RocksDbStorage::remove_pending_prefix_drop_record"):
        return "escape hatch", "FlatStore", "DB"
    if item == "RocksDbStorage::ingest_subtree_sst":
        return "escape hatch", "Engine", "DB"
    if m in ("get_aux", "put_aux", "delete_aux", "raw_iter_aux"):
        if item.startswith("Batch::"):
            return "aux", "FlatStore", "B-part / B-imm"
        if m == "raw_iter_aux":
            return "aux", "FlatStore", "zero"
        return "aux", "FlatStore", rw("R" if m.startswith("get") else "W")
    if m in ("get_meta", "put_meta", "delete_meta"):
        return "meta", "FlatStore", rw("R" if m.startswith("get") else "W")
    if m in ("get_root", "put_root", "delete_root"):
        if item.startswith("Batch::"):
            return "roots", "NodeStore", "B-part / B-imm"
        return "roots", "NodeStore", rw("R" if m.startswith("get") else "W")
    if item in ("StorageContext::new_batch", "StorageContext::commit_batch"):
        return "batch", "NodeStore", "B-part / B-imm" if m == "commit_batch" else "zero"
    if item in ("Batch::put", "Batch::delete"):
        return "tree-node access", "NodeStore", "B-part / B-imm"
    if item in ("StorageContext::get", "StorageContext::put", "StorageContext::delete"):
        cost = rw("R" if m == "get" else "W")
        if in_lines(ESCAPE_KV_LINES, loc):
            return "escape hatch", "NodeStore", cost
        if flat:
            return "flat key-value", "FlatStore", cost
        if test and c == "grovedb":
            return "escape hatch", "NodeStore", cost
        return "tree-node access", "NodeStore", cost
    if item == "StorageContext::raw_iter" or item.startswith("RawIterator::"):
        cost = "zero" if m == "raw_iter" else iter_cost(m)
        if in_lines(ESCAPE_ITER_LINES, loc):
            return "escape hatch", "NodeStore", cost
        if flat or in_lines(AUX_ITER_LINES, loc):
            return "iterator", "FlatStore", cost
        return "iterator", "NodeStore", cost
    raise SystemExit("unclassified: %s %s" % (loc, item))


rows = []
for s in calls:
    cls, seam, cost = classify(s)
    note = NOTES.get(s["loc"], "")
    if s["scope"] == "lib" and dropped(s) and "dropped" not in note and "not billed" not in note:
        note = (note + "; " if note else "") + "cost dropped by the caller (`.unwrap()` / `.value`)"
    rows.append(dict(s, cls=cls, seam=seam, cost=cost, note=note))

CRATE_ORDER = ["merk", "grovedb-query", "grovedb-merkle-mountain-range", "grovedb-bulk-append-tree",
               "grovedb-dense-fixed-sized-merkle-tree", "grovedb-commitment-tree",
               "grovedb-private-document-store", "grovedb"]
CLASS_ORDER = ["tree-node access", "flat key-value", "aux", "meta", "roots", "iterator",
               "path open", "by-prefix open", "immediate context", "range delete", "escape hatch",
               "batch", "transaction", "lifecycle", "prefix", "estimate"]


def table(rs, with_note=True):
    out = []
    if with_note:
        out.append("| Site | Function | API item | Calls | Class | Seam | Cost | Note |")
        out.append("| --- | --- | --- | --- | --- | --- | --- | --- |")
    else:
        out.append("| Site | Function | API item | Calls | Class | Seam | Cost |")
        out.append("| --- | --- | --- | --- | --- | --- | --- |")
    for r in rs:
        cells = ["`%s`" % r["loc"], "`%s`" % r["fn"], "`%s`" % r["item"], str(r["cnt"]),
                 r["cls"], r["seam"], r["cost"]]
        if with_note:
            cells.append(r["note"])
        out.append("| " + " | ".join(cells) + " |")
    return "\n".join(out)


def summary(rs):
    by = defaultdict(Counter)
    for r in rs:
        by[r["cls"]][crate_of(path_of(r["loc"]))] += r["cnt"]
    crates = [c for c in CRATE_ORDER if any(by[k][c] for k in by)]
    short = {"merk": "merk", "grovedb-query": "query", "grovedb-merkle-mountain-range": "MMR",
             "grovedb-bulk-append-tree": "bulk", "grovedb-dense-fixed-sized-merkle-tree": "dense",
             "grovedb-commitment-tree": "commitment", "grovedb-private-document-store": "private doc",
             "grovedb": "grovedb"}
    out = ["| Class | " + " | ".join(short[c] for c in crates) + " | Total |",
           "| --- | " + " | ".join("---" for _ in crates) + " | --- |"]
    tot = Counter()
    for k in CLASS_ORDER:
        if k not in by:
            continue
        vals = [by[k][c] for c in crates]
        for c in crates:
            tot[c] += by[k][c]
        out.append("| %s | %s | %d |" % (k, " | ".join(str(v) if v else "" for v in vals), sum(vals)))
    out.append("| **total** | %s | **%d** |" % (" | ".join(str(tot[c]) for c in crates), sum(tot.values())))
    return "\n".join(out)


def seam_summary(rs):
    by = defaultdict(Counter)
    for r in rs:
        by[r["seam"]][r["cls"]] += r["cnt"]
    out = ["| Seam | Calls | Classes |", "| --- | --- | --- |"]
    for seam in ["NodeStore", "FlatStore", "Engine", "Estimates"]:
        if seam in by:
            parts = ", ".join("%s %d" % (k, by[seam][k]) for k in CLASS_ORDER if by[seam][k])
            out.append("| %s | %d | %s |" % (seam, sum(by[seam].values()), parts))
    return "\n".join(out)


lib = [r for r in rows if r["scope"] == "lib"]
tst = [r for r in rows if r["scope"] == "test"]

parts = OrderedDict()
parts["SUMMARY_LIB"] = summary(lib)
parts["SUMMARY_TEST"] = summary(tst)
parts["SEAMS_LIB"] = seam_summary(lib)
for c in CRATE_ORDER:
    rs = [r for r in lib if crate_of(path_of(r["loc"])) == c]
    parts["LIB_" + c] = table(rs) if rs else ""
for c in CRATE_ORDER:
    rs = [r for r in tst if crate_of(path_of(r["loc"])) == c]
    parts["TEST_" + c] = table(rs, with_note=False) if rs else ""

# type references: per item, files with counts
tref = defaultdict(Counter)
for t in types:
    tref[t["item"]][path_of(t["loc"])] += t["cnt"]
out = ["| Item | Uses | Files | Where (file: uses) |", "| --- | --- | --- | --- |"]
for item in sorted(tref, key=lambda k: -sum(tref[k].values())):
    files = tref[item]
    where = ", ".join("`%s`: %d" % (f, n) for f, n in sorted(files.items()))
    out.append("| `%s` | %d | %d | %s |" % (item, sum(files.values()), len(files), where))
parts["TYPES"] = "\n".join(out)
parts["COUNTS"] = "%d call sites, %d calls (%d sites / %d calls in library code, %d sites / %d calls only in test targets); %d type, trait and import references" % (
    len(rows), sum(r["cnt"] for r in rows), len(lib), sum(r["cnt"] for r in lib),
    len(tst), sum(r["cnt"] for r in tst), len(types))

parts["GROVEDB_NODE_ITER"] = str(sum(
    r["cnt"] for r in lib
    if crate_of(path_of(r["loc"])) == "grovedb" and r["cls"] == "iterator" and r["seam"] == "NodeStore"))

template = open(ARGS.template).read()
for k, v in parts.items():
    template = template.replace("{{%s}}" % k, v)
left = re.findall(r"\{\{[A-Z_a-z-]+\}\}", template)
if left:
    raise SystemExit("unfilled: %s" % left)
sys.stdout.write(template)
