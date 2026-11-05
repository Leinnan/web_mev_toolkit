#!/usr/bin/env bash
# Writes a cargo config pointing a project's toolkit crates at this checkout,
# and prints its path, for trying toolkit changes before pinning a rev:
#
#   cargo --config "$(path/to/toolkit-dev-config.sh)" check
#
# `clippy` and `nextest` take it after the subcommand: before it, cargo ignores
# it and builds the pinned rev.
#
# Run it from the project's checkout. It patches exactly the toolkit crates the
# project names, plus the toolkit crates those depend on. An unused patch is
# recorded in Cargo.lock, and a crate left unpatched would arrive from both
# sources. Patching rewrites Cargo.lock, so restore that file before committing.
set -euo pipefail

toolkit="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
url="${TOOLKIT_GIT_URL:-https://github.com/Leinnan/web_mev_toolkit}"
project="$(pwd)"

if [[ ! -f "$project/Cargo.toml" ]]; then
    printf 'run this from a project checkout (no Cargo.toml in %s)\n' "$project" >&2
    exit 1
fi

crates="$(
    find "$project" -name Cargo.toml -not -path '*/target/*' -not -path '*/vendor/*' -not -path '*/.claude/*' -print0 |
        xargs -0 grep -hoE 'web_mev_[a-z0-9_]+' | grep -vx web_mev_toolkit | sort -u || true
)"
if [[ -z "$crates" ]]; then
    printf 'no web_mev_* crate in the manifests under %s\n' "$project" >&2
    exit 1
fi

# Toolkit crates reach each other by path, so follow those edges too.
while :; do
    next="$crates"
    while read -r crate; do
        if [[ ! -d "$toolkit/crates/$crate" ]]; then
            printf 'unknown toolkit crate %s\n' "$crate" >&2
            exit 1
        fi
        found="$(grep -hoE '^web_mev_[a-z0-9_]+' "$toolkit/crates/$crate/Cargo.toml" || true)"
        next="$(printf '%s\n%s\n' "$next" "$found" | grep -v '^$' | sort -u)"
    done <<<"$crates"
    if [[ "$next" == "$crates" ]]; then break; fi
    crates="$next"
done

output="$project/target/toolkit-dev.toml"
mkdir -p "$(dirname "$output")"
{
    printf '[patch."%s"]\n' "$url"
    while read -r crate; do
        printf '%s = { path = "%s/crates/%s" }\n' "$crate" "$toolkit" "$crate"
    done <<<"$crates"
} >"$output"
printf '%s\n' "$output"
