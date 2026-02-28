#!/bin/bash
# Build the GroveDB book in English and all translated languages.
# Requires: mdbook, mdbook-mermaid
#
# Usage:
#   ./build-translations.sh          # Build all languages
#   ./build-translations.sh ru zh    # Build only specific languages

set -euo pipefail
cd "$(dirname "$0")"

LANGUAGES=(ru zh es fr pt ja ko ar de it)

# If specific languages are passed, use those instead
if [ $# -gt 0 ]; then
  LANGUAGES=("$@")
fi

echo "==> Building English book"
mdbook build

for lang in "${LANGUAGES[@]}"; do
  dir="translations/$lang"
  if [ ! -f "$dir/book.toml" ]; then
    echo "WARN: $dir/book.toml not found, skipping $lang"
    continue
  fi
  echo "==> Building $lang translation"
  (cd "$dir" && mdbook build)
done

echo "==> Done. Outputs in output/ and output/{lang}/"
