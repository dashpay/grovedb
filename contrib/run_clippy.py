#!/usr/bin/env python3
"""Run cargo clippy on the workspace with strict settings.

Used by pre-commit as a pre-push hook and can also be run manually.
Uses a separate target directory to avoid invalidating the main build cache.
"""

import subprocess
import sys


def main() -> int:
    cmd = [
        "cargo", "clippy",
        "--workspace",
        "--all-features",
        "--all-targets",
        "--target-dir", "target/clippy",
        "--", "-D", "warnings", "-A", "mismatched_lifetime_syntaxes",
    ]

    print(f"Running: {' '.join(cmd)}")
    result = subprocess.run(cmd)
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
