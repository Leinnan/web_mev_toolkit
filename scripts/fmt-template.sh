#!/usr/bin/env bash
# Formats the Rust files of templates/app. They hold placeholders rustfmt can't
# parse, so this swaps them for identifiers of a typical length, formats a copy, and
# swaps them back.
set -euo pipefail

toolkit="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app="$toolkit/templates/app"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp -R "$app/src" "$app/tests" "$work/"
find "$work" -name '*.rs' -print0 |
    xargs -0 sed -i.bak -e 's/{{crate_name}}/mevcrate___/g' -e 's/{{project-name}}/mevproject_/g' -e 's/{{env_prefix}}/MEVPREFIX__/g'
find "$work" -name '*.bak' -delete
rustfmt --edition 2024 "$work/src/lib.rs" "$work/src/main.rs" "$work/tests/app.rs"
find "$work" -name '*.rs' -print0 |
    xargs -0 sed -i.bak -e 's/mevcrate___/{{crate_name}}/g' -e 's/mevproject_/{{project-name}}/g' -e 's/MEVPREFIX__/{{env_prefix}}/g'
find "$work" -name '*.bak' -delete
for directory in src tests; do
    (cd "$work/$directory" && find . -name '*.rs' -print0) | while IFS= read -r -d '' file; do
        cp "$work/$directory/$file" "$app/$directory/$file"
    done
done
