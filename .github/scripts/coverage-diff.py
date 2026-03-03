#!/usr/bin/env python3
"""Coverage diff analyzer for PRs.

Compares baseline LCOV coverage against PR coverage to detect whether
new #[test] functions actually cover previously-uncovered production lines.
Posts results as a markdown report.
"""

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


def parse_lcov(filepath):
    """Parse an LCOV file into {file: {line: hit_count}}.

    Normalizes paths to be repo-relative by stripping common CI workspace
    prefixes.
    """
    coverage = {}
    current_file = None

    try:
        with open(filepath) as f:
            for line in f:
                line = line.strip()
                if line.startswith("SF:"):
                    raw_path = line[3:]
                    current_file = normalize_path(raw_path)
                elif line.startswith("DA:") and current_file is not None:
                    parts = line[3:].split(",")
                    if len(parts) >= 2:
                        line_no = int(parts[0])
                        hit_count = int(parts[1])
                        if current_file not in coverage:
                            coverage[current_file] = {}
                        coverage[current_file][line_no] = hit_count
                elif line == "end_of_record":
                    current_file = None
    except FileNotFoundError:
        return {}

    return coverage


def normalize_path(path):
    """Strip CI workspace prefix to get repo-relative path."""
    # Common CI patterns: /home/runner/work/repo/repo/src/...
    # or /github/workspace/src/...
    markers = ["/home/runner/work/", "/github/workspace/"]
    for marker in markers:
        idx = path.find(marker)
        if idx != -1:
            remainder = path[idx + len(marker) :]
            # /home/runner/work/repo/repo/src -> strip two dirs
            if marker == "/home/runner/work/":
                parts = remainder.split("/", 2)
                if len(parts) > 2:
                    return parts[2]
            return remainder

    # If path is absolute but not a known CI prefix, try to make it relative
    # to the repo root by finding common crate directories
    for crate_dir in [
        "/grovedb/",
        "/merk/",
        "/storage/",
        "/costs/",
        "/path/",
        "/grovedb-version/",
        "/grovedb-epoch-based-storage-flags/",
        "/visualize/",
        "/node-grove/",
        "/grovedb-mmr/",
        "/grovedb-commitment-tree/",
    ]:
        idx = path.find(crate_dir)
        if idx != -1:
            return path[idx + 1 :]  # strip leading /

    return path


def is_production_file(filepath):
    """Return True if the file is production code (not test code)."""
    parts = filepath.replace("\\", "/").split("/")

    # Exclude test files
    if any(p == "tests" for p in parts):
        return False
    if filepath.endswith("_test.rs") or filepath.endswith("_tests.rs"):
        return False
    # Exclude benchmark files
    if any(p == "benches" for p in parts):
        return False

    return filepath.endswith(".rs")


def detect_new_tests(base_ref):
    """Detect new #[test] functions added in this PR.

    Returns list of (file, function_name) tuples.
    """
    try:
        result = subprocess.run(
            [
                "git",
                "diff",
                f"origin/{base_ref}...HEAD",
                "--unified=0",
                "--",
                "*.rs",
            ],
            capture_output=True,
            text=True,
            check=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return []

    new_tests = []
    current_file = None
    saw_test_attr = False

    for line in result.stdout.splitlines():
        # Track current file
        if line.startswith("diff --git"):
            match = re.search(r"b/(.+)$", line)
            if match:
                current_file = match.group(1)
            saw_test_attr = False
            continue

        # Only look at added lines
        if not line.startswith("+") or line.startswith("+++"):
            if line.startswith("-") or line.startswith("@@"):
                continue
            # Context lines or other non-added lines reset test attr tracking
            # only if they contain actual code
            continue

        added = line[1:].strip()

        if "#[test]" in added or "#[tokio::test]" in added:
            saw_test_attr = True
            continue

        if saw_test_attr and added.startswith("fn "):
            fn_match = re.match(r"fn\s+(\w+)", added)
            if fn_match and current_file:
                new_tests.append((current_file, fn_match.group(1)))
            saw_test_attr = False
            continue

        # Reset if we see a non-empty, non-attribute line between #[test] and fn
        if saw_test_attr and added and not added.startswith("#[") and not added.startswith("//"):
            saw_test_attr = False

    return new_tests


def compute_coverage_diff(baseline, pr):
    """Compute coverage difference between baseline and PR.

    Returns:
        baseline_stats: (covered, total) for production files
        pr_stats: (covered, total) for production files
        newly_covered: {file: [line_numbers]} - lines covered in PR but not baseline
        newly_uncovered: {file: [line_numbers]} - lines covered in baseline but not PR
    """
    # Collect all production files from both
    all_files = set()
    for f in baseline:
        if is_production_file(f):
            all_files.add(f)
    for f in pr:
        if is_production_file(f):
            all_files.add(f)

    baseline_covered = 0
    baseline_total = 0
    pr_covered = 0
    pr_total = 0
    newly_covered = {}
    newly_uncovered = {}

    for f in sorted(all_files):
        base_lines = baseline.get(f, {})
        pr_lines = pr.get(f, {})
        all_lines = set(base_lines.keys()) | set(pr_lines.keys())

        for line_no in all_lines:
            base_hit = base_lines.get(line_no, 0)
            pr_hit = pr_lines.get(line_no, 0)

            if line_no in base_lines:
                baseline_total += 1
                if base_hit > 0:
                    baseline_covered += 1

            if line_no in pr_lines:
                pr_total += 1
                if pr_hit > 0:
                    pr_covered += 1

            # Newly covered: not covered in baseline (or absent), covered in PR
            if pr_hit > 0 and base_hit == 0:
                if f not in newly_covered:
                    newly_covered[f] = []
                newly_covered[f].append(line_no)

            # Newly uncovered: covered in baseline, not covered in PR
            if base_hit > 0 and pr_hit == 0 and line_no in pr_lines:
                if f not in newly_uncovered:
                    newly_uncovered[f] = []
                newly_uncovered[f].append(line_no)

    baseline_stats = (baseline_covered, baseline_total)
    pr_stats = (pr_covered, pr_total)

    return baseline_stats, pr_stats, newly_covered, newly_uncovered


def format_pct(covered, total):
    """Format coverage percentage."""
    if total == 0:
        return "N/A"
    return f"{covered / total * 100:.2f}%"


def generate_report(
    new_tests, baseline_stats, pr_stats, newly_covered, newly_uncovered, baseline_available
):
    """Generate markdown report."""
    lines = []
    lines.append("## Coverage Diff Report")
    lines.append("")

    if not baseline_available:
        lines.append(
            "> **Note:** No baseline coverage data available for comparison. "
            "This is expected on the first run or after cache expiry. "
            "Showing PR coverage summary only."
        )
        lines.append("")
        pr_covered, pr_total = pr_stats
        lines.append(f"**PR coverage:** {pr_covered}/{pr_total} lines ({format_pct(pr_covered, pr_total)})")
        lines.append("")
        if new_tests:
            lines.append(f"**New test functions detected:** {len(new_tests)}")
            lines.append("")
            _append_test_list(lines, new_tests)
        return "\n".join(lines)

    # Coverage summary
    base_covered, base_total = baseline_stats
    pr_covered, pr_total = pr_stats

    base_pct = base_covered / base_total * 100 if base_total > 0 else 0
    pr_pct = pr_covered / pr_total * 100 if pr_total > 0 else 0
    delta_pct = pr_pct - base_pct

    delta_sign = "+" if delta_pct >= 0 else ""
    delta_str = f"{delta_sign}{delta_pct:.2f}%"

    lines.append("| Metric | Baseline | PR | Delta |")
    lines.append("|--------|----------|-----|-------|")
    lines.append(
        f"| Production lines covered | {base_covered}/{base_total} "
        f"({format_pct(base_covered, base_total)}) | {pr_covered}/{pr_total} "
        f"({format_pct(pr_covered, pr_total)}) | {delta_str} |"
    )
    lines.append("")

    total_newly_covered = sum(len(v) for v in newly_covered.values())
    total_newly_uncovered = sum(len(v) for v in newly_uncovered.values())

    lines.append(f"**Newly covered production lines:** {total_newly_covered}")
    if total_newly_uncovered > 0:
        lines.append(f"**Newly uncovered production lines:** {total_newly_uncovered}")
    lines.append("")

    # New tests analysis
    if new_tests:
        lines.append(f"### New test functions ({len(new_tests)})")
        lines.append("")
        _append_test_list(lines, new_tests)
        lines.append("")

        if total_newly_covered == 0:
            lines.append(
                "> :warning: **Warning:** This PR adds new test functions but does not "
                "cover any previously-uncovered production lines. Consider whether these "
                "tests are exercising meaningful new code paths."
            )
        else:
            lines.append(
                f"> :white_check_mark: This PR adds new tests that cover "
                f"**{total_newly_covered}** previously-uncovered production line(s)."
            )
        lines.append("")

    # Per-file breakdown
    if newly_covered:
        lines.append("<details>")
        lines.append("<summary>Newly covered lines by file</summary>")
        lines.append("")
        for f in sorted(newly_covered.keys()):
            file_lines = newly_covered[f]
            ranges = _compress_line_ranges(file_lines)
            lines.append(f"- `{f}`: {ranges} ({len(file_lines)} lines)")
        lines.append("")
        lines.append("</details>")
        lines.append("")

    if newly_uncovered:
        lines.append("<details>")
        lines.append("<summary>Newly uncovered lines by file</summary>")
        lines.append("")
        for f in sorted(newly_uncovered.keys()):
            file_lines = newly_uncovered[f]
            ranges = _compress_line_ranges(file_lines)
            lines.append(f"- `{f}`: {ranges} ({len(file_lines)} lines)")
        lines.append("")
        lines.append("</details>")

    return "\n".join(lines)


def _append_test_list(lines, new_tests):
    """Append test function list, collapsing if >20."""
    if len(new_tests) > 20:
        lines.append("<details>")
        lines.append(f"<summary>{len(new_tests)} new test functions (click to expand)</summary>")
        lines.append("")

    for filepath, fn_name in new_tests:
        lines.append(f"- `{filepath}`: `{fn_name}`")

    if len(new_tests) > 20:
        lines.append("")
        lines.append("</details>")


def _compress_line_ranges(line_numbers):
    """Compress [1,2,3,5,7,8,9] into '1-3, 5, 7-9'."""
    if not line_numbers:
        return ""

    sorted_lines = sorted(line_numbers)
    ranges = []
    start = sorted_lines[0]
    end = start

    for n in sorted_lines[1:]:
        if n == end + 1:
            end = n
        else:
            ranges.append(f"{start}-{end}" if start != end else str(start))
            start = n
            end = n

    ranges.append(f"{start}-{end}" if start != end else str(start))
    return ", ".join(ranges)


def main():
    parser = argparse.ArgumentParser(description="Coverage diff analyzer")
    parser.add_argument("--baseline", required=True, help="Path to baseline LCOV file")
    parser.add_argument("--pr", required=True, help="Path to PR LCOV file")
    parser.add_argument("--base-ref", required=True, help="Base branch name")
    parser.add_argument(
        "--baseline-available",
        default="false",
        help="Whether baseline cache was found (true/false)",
    )
    parser.add_argument("--output", required=True, help="Output markdown file path")

    args = parser.parse_args()

    baseline_available = args.baseline_available.lower() == "true"

    # Parse coverage data
    pr_coverage = parse_lcov(args.pr)

    if baseline_available:
        baseline_coverage = parse_lcov(args.baseline)
    else:
        baseline_coverage = {}

    # Detect new tests
    new_tests = detect_new_tests(args.base_ref)

    # Compute diff
    if baseline_available:
        baseline_stats, pr_stats, newly_covered, newly_uncovered = compute_coverage_diff(
            baseline_coverage, pr_coverage
        )
    else:
        pr_prod_covered = 0
        pr_prod_total = 0
        for f, file_lines in pr_coverage.items():
            if is_production_file(f):
                for line_no, count in file_lines.items():
                    pr_prod_total += 1
                    if count > 0:
                        pr_prod_covered += 1
        pr_stats = (pr_prod_covered, pr_prod_total)
        baseline_stats = (0, 0)
        newly_covered = {}
        newly_uncovered = {}

    # Generate report
    report = generate_report(
        new_tests, baseline_stats, pr_stats, newly_covered, newly_uncovered, baseline_available
    )

    # Write output
    with open(args.output, "w") as f:
        f.write(report)

    print(report)
    print(f"\nReport written to {args.output}")


if __name__ == "__main__":
    main()
