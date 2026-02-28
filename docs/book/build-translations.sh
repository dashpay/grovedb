#!/bin/bash
# Build the GroveDB book in English and all translated languages.
# Requires: mdbook, mdbook-mermaid
#
# Usage:
#   ./build-translations.sh          # Build all languages
#   ./build-translations.sh ru zh    # Build only specific languages

set -euo pipefail
cd "$(dirname "$0")"

LANGUAGES=(ru zh es fr pt ja ko ar de it tr vi id th pl cs)
SHARED_ASSETS=(mermaid.min.js mermaid-init.js lang-selector.js lang-selector.css)

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
  # Sync shared assets from English source to keep copies in sync
  for asset in "${SHARED_ASSETS[@]}"; do
    if [ -f "$asset" ]; then
      cp "$asset" "$dir/$asset"
    fi
  done
  echo "==> Building $lang translation"
  (cd "$dir" && mdbook build)
done

echo "==> Done. Outputs in output/ and output/{lang}/"
