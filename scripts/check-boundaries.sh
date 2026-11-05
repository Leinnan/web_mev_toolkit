#!/usr/bin/env bash
# The rules of docs/crate-rules.md that a dependency tree can prove. Every
# `cargo tree` failure stops the script, so a failed fetch can never look clean.
set -euo pipefail

failed=0

# tree CRATE [cargo tree flags...]: the crate's dependency names, one per line.
tree() {
    local package="$1"
    shift
    cargo tree --package "$package" --edges normal --prefix none --format '{p}' "$@" |
        awk '{print $1}' | sort -u
}

# forbid LABEL NAMES_REGEX LIST
forbid() {
    local found
    found="$(printf '%s\n' "$3" | grep -E "^($2)$" || true)"
    if [[ -n "$found" ]]; then
        printf 'boundary violation in %s:\n%s\n' "$1" "$found" >&2
        failed=1
    fi
}

# Every crate in crates/ must be listed in this script, so a new one can't skip it.
known=""
for manifest in crates/*/Cargo.toml; do
    [[ -e "$manifest" ]] || continue
    crate="$(basename "$(dirname "$manifest")")"
    if ! grep -q "^# crate: $crate\$" "${BASH_SOURCE[0]}"; then
        printf 'boundary violation: %s is not listed in %s\n' "$crate" "${BASH_SOURCE[0]}" >&2
        failed=1
    fi
    known="$known $crate"
done

# Each crate's "must not depend on" list.
web_stack='axum|axum-core|hyper|tower-http'
# crate: web_mev_core
forbid web_mev_core "$web_stack|sqlx|tera|reqwest|tokio" "$(tree web_mev_core)"
forbid "web_mev_core (all features)" "$web_stack|sqlx|tera|reqwest" "$(tree web_mev_core --all-features)"
# crate: web_mev_runtime
forbid web_mev_runtime "$web_stack|sqlx|tera|reqwest" "$(tree web_mev_runtime --all-features)"
# crate: web_mev_sqlite
forbid web_mev_sqlite "$web_stack|tera|reqwest" "$(tree web_mev_sqlite --all-features)"
# crate: web_mev_test

# Positive control: the checks above see what they look for.
if ! tree web_mev_sqlite | grep -qx sqlx; then
    printf 'control failed: sqlx not found in the web_mev_sqlite tree\n' >&2
    failed=1
fi

# Nothing depends on the test crate outside dev-dependencies.
for crate in $known; do
    if [[ "$crate" != web_mev_test ]]; then
        forbid "$crate (all features)" 'web_mev_test' "$(tree "$crate" --all-features)"
    fi
done

if ((failed != 0)); then
    exit 1
fi
printf 'crate boundaries verified\n'
