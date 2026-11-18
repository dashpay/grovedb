#!/usr/bin/env python3
"""Enumerate every use of the grovedb-storage API from the rest of the workspace.

The search is done by the compiler, not by grep. `run` exports a commit into a
scratch directory, marks every public function, method, trait method, trait,
type and associated type of `grovedb-storage` `#[deprecated]`, and runs
`cargo check --all-features` (once `--lib --bins`, once `--all-targets`) with
`--force-warn deprecated` on every workspace package that depends on
`grovedb-storage`. Each deprecation warning whose primary span lies outside
`storage/` is one use. Calls through a generic `S: StorageContext`, calls on
concrete types, calls inside macros and calls under `#[allow(deprecated)]` are
all reported.

    python3 -I docs/lignindb/storage-surface-search.py run --rev <commit> \
        --workdir /work/scratch/base04-search > sites.tsv
    python3 -I docs/lignindb/storage-surface-search.py check \
        --sites sites.tsv --doc docs/lignindb/storage-surface.md

`run` prints tab-separated lines in four sections:
    ## calls           <path>:<line> <count> <API item> <enclosing function> <lib|test>
    ## types           the same columns, for references to types, traits,
                       associated types, variants and fields, and for imports
                       of free functions
    ## macro-expanded  call sites whose primary span lies in a macro body
    ## default-probe-unchecked
                       compile units the Default probe could not check
                       because a crate they depend on failed in it
    ## uncompiled      .rs files of the scanned packages no target compiled
`<Type>::default` call sites come from a third step: the `Default` impls of
storage types are removed and each resulting compile error is one call (a
trait impl item cannot be marked deprecated).
<count> is the number of distinct uses (columns) of the item on the line. A
site is "lib" when the `--lib --bins` pass reports it, "test" when only the
`--all-targets` pass does.

`check` compares the `## calls` lines with the inventory tables of the
document (rows starting `| `path:line` | `function` | `item` | count |`) and
fails if a site is listed in one but not the other, or with a different count.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter

MARKER = "BASE04-STORAGE-SURFACE"
ATTR = '#[deprecated(note = "%s")]' % MARKER

CALLABLE_KINDS = {"method", "associated function", "function"}

# ---------------------------------------------------------------------------
# A small Rust lexer: enough to find braces that are code, not text.
# ---------------------------------------------------------------------------


def code_mask(src):
    """Return a copy of `src` with comments and literals replaced by spaces.

    Newlines are kept so line numbers stay valid.
    """
    out = list(src)
    i, n = 0, len(src)

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif (c in "rb") and re.match(r'(br|r)#*"', src[i:i + 300]) and not (
            i > 0 and (src[i - 1].isalnum() or src[i - 1] == "_")
        ):
            m = re.match(r'(br|r)(#*)"', src[i:])
            hashes = m.group(2)
            start = i + m.end()
            end = src.find('"' + hashes, start)
            end = n if end < 0 else end + 1 + len(hashes)
            blank(i, end)
            i = end
        elif c == '"' or (c == "b" and src.startswith('b"', i) and not (
            i > 0 and (src[i - 1].isalnum() or src[i - 1] == "_")
        )):
            j = i + (2 if c == "b" else 1)
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            blank(i, j + 1)
            i = j + 1
        elif c == "'":
            m = re.match(r"'(\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'])'", src[i:])
            if m:
                blank(i, i + m.end())
                i += m.end()
            else:
                i += 1
        else:
            i += 1
    return "".join(out)


ITEM_PREFIX = re.compile(
    r"^(?:#!?\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:unsafe\s+)?"
)


def classify_header(header):
    h = " ".join(header.split())
    h = h[ITEM_PREFIX.match(h).end():]
    if re.match(r"impl\b", h):
        return ("impl_trait" if re.search(r"\bfor\b", re.sub(r"\bwhere\b.*", "", h))
                else "impl"), impl_self_type(h)
    m = re.match(r"(?:auto\s+)?trait\s+(\w+)", h)
    if m:
        return "trait", m.group(1)
    m = re.match(r"mod\s+(\w+)", h)
    if m:
        return "mod", m.group(1)
    m = re.search(r"\bfn\s+(\w+)", h)
    if m:
        return "fn", m.group(1)
    return "other", None


def impl_self_type(h):
    body = h[h.index("impl") + 4:]
    body = re.sub(r"\bwhere\b.*", "", body)
    if " for " in " %s " % body:
        body = body.split(" for ", 1)[1]
    else:
        body = strip_generics_prefix(body)
    name = re.search(r"[A-Za-z_]\w*", body)
    return name.group(0) if name else None


def strip_generics_prefix(s):
    s = s.strip()
    if not s.startswith("<"):
        return s
    depth = 0
    for k, ch in enumerate(s):
        if ch == "<":
            depth += 1
        elif ch == ">":
            depth -= 1
            if depth == 0:
                return s[k + 1:]
    return s


def block_stacks(src):
    """For each line (1-based), the stack of enclosing blocks at its start.

    A block is (kind, name), kind one of impl_trait, impl, trait, mod, fn,
    other.
    """
    mask = code_mask(src)
    stacks = [None]
    stack = []
    header_start = 0
    stacks.append(())
    nesting = 0
    for i, ch in enumerate(mask):
        if ch in "([":
            nesting += 1
        elif ch in ")]":
            nesting = max(nesting - 1, 0)
        elif ch == "{":
            stack.append((classify_header(mask[header_start:i]), nesting))
            nesting = 0
            header_start = i + 1
        elif ch == "}":
            if stack:
                nesting = stack.pop()[1]
            header_start = i + 1
        elif ch == ";" and nesting == 0:
            header_start = i + 1
        elif ch == "\n":
            stacks.append(tuple(block for block, _ in stack))
    return stacks


def enclosing_function(stack):
    fn_positions = [k for k, (kind, _) in enumerate(stack) if kind == "fn"]
    if not fn_positions:
        return "(item level)"
    outer = fn_positions[0]
    owners = [name for kind, name in stack[:outer] if kind in ("impl", "impl_trait", "trait") and name]
    fn = stack[fn_positions[-1]][1]
    return "%s::%s" % (owners[-1], fn) if owners else fn


# ---------------------------------------------------------------------------
# Marking the storage API deprecated
# ---------------------------------------------------------------------------

PUB_ITEM = re.compile(
    r"^(\s*)pub\s+(?:(?:const|unsafe|async|extern\s+\"C\")\s+)*"
    r"(fn|struct|trait|type|enum|static|const)\b"
)
TRAIT_MEMBER = re.compile(r"^(\s*)(?:(?:const|unsafe|async)\s+)*(fn|type)\b")


def annotate_file(path):
    with open(path) as f:
        src = f.read()
    lines = src.split("\n")
    stacks = block_stacks(src)
    out = []
    added = 0
    for idx, line in enumerate(lines, start=1):
        stack = stacks[idx] if idx < len(stacks) else ()
        inner = stack[-1][0] if stack else "mod"
        indent = None
        if inner == "trait":
            m = TRAIT_MEMBER.match(line)
            if m:
                indent = m.group(1)
        elif inner == "impl":
            m = PUB_ITEM.match(line)
            if m and m.group(2) in ("fn", "const"):
                indent = m.group(1)
        elif inner == "mod" and all(kind == "mod" for kind, _ in stack):
            m = PUB_ITEM.match(line)
            if m:
                indent = m.group(1)
        if indent is not None:
            out.append(indent + ATTR)
            added += 1
        out.append(line)
    with open(path, "w") as f:
        f.write("\n".join(out))
    return added


def annotate_storage(root):
    total = 0
    for dirpath, _, files in os.walk(os.path.join(root, "storage", "src")):
        for name in sorted(files):
            if not name.endswith(".rs") or name == "tests.rs":
                continue
            total += annotate_file(os.path.join(dirpath, name))
    return total


# ---------------------------------------------------------------------------
# run
# ---------------------------------------------------------------------------


def storage_dependents(root):
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=root
        )
    )
    deps = []
    for pkg in meta["packages"]:
        if pkg["name"] == "grovedb-storage":
            continue
        if any(d["name"] == "grovedb-storage" for d in pkg["dependencies"]):
            deps.append((pkg["name"], os.path.dirname(pkg["manifest_path"])))
    return sorted(deps)


def short_item(path):
    parts = re.sub(r"::<[^>]*>", "", path).split("::")
    first_upper = next((k for k, p in enumerate(parts) if p[:1].isupper()), len(parts) - 1)
    return "::".join(parts[first_upper:])


DEPRECATED_MSG = re.compile(r"use of deprecated (.+?) `([^`]+)`: " + MARKER)


def cargo_check(src, target, packages, target_args):
    """Run cargo check with JSON output. Returns (success, messages, built units)."""
    cmd = ["cargo", "check", "--message-format=json", "--all-features"] + target_args
    for name, _ in packages:
        cmd += ["-p", name]
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = target
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " --force-warn deprecated").strip()
    env.setdefault("CARGO_BUILD_JOBS", "8")
    proc = subprocess.run(cmd, cwd=src, env=env, stdout=subprocess.PIPE)
    messages, units = [], set()
    for line in proc.stdout.decode().splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        messages.append(msg)
        if msg.get("reason") == "compiler-artifact" and msg["package_id"].startswith("path+"):
            units.add(unit_name(msg) + (" (test)" if msg["profile"]["test"] else ""))
    return proc.returncode == 0, messages, units


def unit_name(msg):
    t = msg["target"]
    return "%s %s %s" % (msg["package_id"].split("#")[0].rsplit("/", 1)[-1],
                         "/".join(t["kind"]), t["name"])


def check_pass(src, target, packages, target_args):
    """Run one cargo check and collect the marked uses outside storage/.

    Returns (calls, types, macro_sites, units). calls and types map
    (path:line, item, enclosing function) to the number of distinct uses
    (columns) on that line. A file compiled into several targets reports each
    use once per target; keying on the column counts it once.
    """
    ok, messages, units = cargo_check(src, target, packages, target_args)
    if not ok:
        sys.stderr.write("\n".join(m["message"]["rendered"] for m in messages
                                    if m.get("reason") == "compiler-message"
                                    and m["message"]["level"] == "error"))
        sys.exit("cargo check failed")
    uses = {}
    macro_sites = set()
    stack_cache = {}
    for msg in messages:
        if msg.get("reason") != "compiler-message":
            continue
        m = msg["message"]
        hit = DEPRECATED_MSG.search(m.get("message", ""))
        if not hit:
            continue
        kind, item = hit.group(1), short_item(hit.group(2))
        for span in m["spans"]:
            if not span["is_primary"]:
                continue
            fname = os.path.relpath(os.path.join(src, span["file_name"]), src)
            if fname.startswith("storage/") or fname.startswith(".."):
                continue
            ln = span["line_start"]
            if fname not in stack_cache:
                with open(os.path.join(src, fname)) as f:
                    stack_cache[fname] = block_stacks(f.read())
            stacks = stack_cache[fname]
            func = enclosing_function(stacks[ln] if ln < len(stacks) else ())
            loc = "%s:%d" % (fname, ln)
            is_import = kind == "function" and func == "(item level)"
            uses[(loc, span["column_start"], item, func)] = (
                kind in CALLABLE_KINDS and not is_import
            )
            exp = span.get("expansion")
            if exp:
                inv = exp["span"]
                macro_sites.add((loc, item, exp.get("macro_decl_name", "?"),
                                 "%s:%d" % (inv["file_name"], inv["line_start"])))
    calls, types = Counter(), Counter()
    for (loc, _col, item, func), is_call in uses.items():
        (calls if is_call else types)[(loc, item, func)] += 1
    return calls, types, macro_sites, units


DEFAULT_IMPL = re.compile(r"^impl Default for (\w+) \{\n(?:    .*\n|\n)*?\}\n", re.M)
UNSATISFIED_DEFAULT = re.compile(r"the trait bound `(?:[\w:]+::)?(\w+): (?:std::default::)?Default` is not satisfied")
NO_DEFAULT_FN = re.compile(r"no function or associated item named `default` found for struct `(\w+)`")


def default_probe(src, target, packages):
    """Find calls of the `Default` impls of storage types.

    A trait impl item cannot carry `#[deprecated]`, so `Default::default()`
    building a storage type is invisible to the main passes. This removes
    every `impl Default for <Type>` from storage/src and collects the
    resulting errors. Each error is one call of `<Type>::default`. Returns
    (calls, units) per pass as for check_pass; targets that depend on a crate
    which fails to compile are not checked and are reported by the caller.
    """
    removed = []
    for dirpath, _, files in os.walk(os.path.join(src, "storage", "src")):
        for name in files:
            if not name.endswith(".rs"):
                continue
            path = os.path.join(dirpath, name)
            with open(path) as f:
                text = f.read()
            new_text = DEFAULT_IMPL.sub(lambda m: removed.append(m.group(1)) or "", text)
            if new_text != text:
                with open(path, "w") as f:
                    f.write(new_text)
    results = []
    failed = set()
    stack_cache = {}
    for target_args in (["--lib", "--bins"], ["--all-targets"]):
        _ok, messages, units = cargo_check(src, target, packages, ["--keep-going"] + target_args)
        uses = set()
        for msg in messages:
            if msg.get("reason") != "compiler-message" or msg["message"]["level"] != "error":
                continue
            failed.add(unit_name(msg))
            m = msg["message"]
            hit = UNSATISFIED_DEFAULT.search(m["message"]) or NO_DEFAULT_FN.search(m["message"])
            if not hit or hit.group(1) not in removed:
                sys.exit("unexpected error in the Default probe: %s" % m["rendered"])
            for span in m["spans"]:
                if not span["is_primary"]:
                    continue
                fname = os.path.relpath(os.path.join(src, span["file_name"]), src)
                ln = span["line_start"]
                if fname not in stack_cache:
                    with open(os.path.join(src, fname)) as f:
                        stack_cache[fname] = block_stacks(f.read())
                stacks = stack_cache[fname]
                func = enclosing_function(stacks[ln] if ln < len(stacks) else ())
                uses.add(("%s:%d" % (fname, ln), span["column_start"],
                          "%s::default" % hit.group(1), func))
        calls = Counter()
        for loc, _col, item, func in uses:
            calls[(loc, item, func)] += 1
        results.append((calls, units))
    return removed, failed, results


def run(args):
    root = os.path.abspath(args.workdir)
    src = os.path.join(root, "src")
    target = os.path.join(root, "target")
    repo = subprocess.check_output(["git", "rev-parse", "--show-toplevel"]).decode().strip()
    rev = subprocess.check_output(["git", "rev-parse", args.rev], cwd=repo).decode().strip()
    if os.path.exists(src):
        subprocess.check_call(["rm", "-rf", src])
    os.makedirs(src)
    archive = subprocess.Popen(["git", "archive", rev], cwd=repo, stdout=subprocess.PIPE)
    subprocess.check_call(["tar", "-x", "-C", src], stdin=archive.stdout)
    archive.wait()
    added = annotate_storage(src)
    packages = storage_dependents(src)
    print("# rev %s, %d storage items marked, packages: %s"
          % (rev, added, " ".join(p for p, _ in packages)), file=sys.stderr)
    lib_calls, lib_types, _, _ = check_pass(src, target, packages, ["--lib", "--bins"])
    calls, types, macro_sites, all_units = check_pass(src, target, packages, ["--all-targets"])
    removed, failed, ((lib_defaults, _), (defaults, probe_units)) = default_probe(
        src, target, packages)
    if set(lib_defaults) - set(defaults):
        sys.exit("Default probe: lib sites missing from the all-targets pass")
    lib_calls.update(lib_defaults)
    calls.update(defaults)
    for lib, all_ in ((lib_calls, calls), (lib_types, types)):
        missing = set(lib) - set(all_)
        if missing:
            sys.exit("sites seen in the lib pass but not the all-targets pass: %s" % sorted(missing))

    compiled = set()
    for dirpath, _, files in os.walk(target):
        for name in files:
            if name.endswith(".d"):
                with open(os.path.join(dirpath, name)) as f:
                    for tok in f.read().replace("\\\n", " ").split():
                        tok = tok.rstrip(":")
                        if tok.endswith(".rs"):
                            rel = os.path.relpath(os.path.join(src, tok), src)
                            if not rel.startswith(".."):
                                compiled.add(rel)

    def emit(counter, lib):
        def sort_key(k):
            p, l = k[0].rsplit(":", 1)
            return (p, int(l), k[1])
        for key in sorted(counter, key=sort_key):
            scope = "lib" if key in lib else "test"
            print("%s\t%d\t%s\t%s\t%s" % (key[0], counter[key], key[1], key[2], scope))

    print("## calls")
    emit(calls, lib_calls)
    print("## types")
    emit(types, lib_types)
    print("## macro-expanded")
    for site in sorted(macro_sites):
        print("\t".join(site))
    print("## default-probe-unchecked")
    print("# Default impls removed: %s" % " ".join(sorted(removed)))
    for unit in sorted(all_units - probe_units):
        if unit.replace(" (test)", "") not in failed:
            print(unit)
    print("## uncompiled")
    for _name, pdir in packages:
        for dirpath, _, files in os.walk(pdir):
            for f in sorted(files):
                if f.endswith(".rs"):
                    p = os.path.relpath(os.path.join(dirpath, f), src)
                    if p not in compiled:
                        print(p)
    print("# %d call sites (%d calls, %d in lib code), %d type-reference sites"
          % (len(calls), sum(calls.values()),
             sum(v for k, v in calls.items() if k in lib_calls), len(types)), file=sys.stderr)


# ---------------------------------------------------------------------------
# check
# ---------------------------------------------------------------------------

DOC_ROW = re.compile(r"^\|\s*`([^`]+:\d+)`\s*\|\s*`([^`]+)`\s*\|\s*`([^`]+)`\s*\|\s*(\d+)\s*\|")


def read_sites(path):
    sites = Counter()
    section = None
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            if line.startswith("## "):
                section = line[3:]
                continue
            if section != "calls" or not line or line.startswith("#"):
                continue
            loc, count, item = line.split("\t")[:3]
            sites[(loc, item)] += int(count)
    return sites


def read_doc(path):
    sites = Counter()
    with open(path) as f:
        for line in f:
            m = DOC_ROW.match(line)
            if m:
                loc, _func, item, count = m.groups()
                sites[(loc, item)] += int(count)
    return sites


def check(args):
    found, listed = read_sites(args.sites), read_doc(args.doc)
    missing = sorted(set(found) - set(listed))
    extra = sorted(set(listed) - set(found))
    wrong = sorted(k for k in set(found) & set(listed) if found[k] != listed[k])
    for k in missing:
        print("not in document: %s %s (x%d)" % (k[0], k[1], found[k]))
    for k in extra:
        print("not found by the search: %s %s" % k)
    for k in wrong:
        print("count differs: %s %s search=%d document=%d" % (k[0], k[1], found[k], listed[k]))
    print("search: %d sites, %d calls; document: %d sites, %d calls"
          % (len(found), sum(found.values()), len(listed), sum(listed.values())))
    if missing or extra or wrong:
        sys.exit(1)
    print("OK: the document lists exactly the call sites the search found")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--rev", default="HEAD")
    r.add_argument("--workdir", required=True)
    c = sub.add_parser("check")
    c.add_argument("--sites", required=True)
    c.add_argument("--doc", required=True)
    args = ap.parse_args()
    {"run": run, "check": check}[args.cmd](args)


if __name__ == "__main__":
    main()
