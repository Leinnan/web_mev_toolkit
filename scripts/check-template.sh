#!/usr/bin/env bash
# Generates templates/app and runs its checks against this checkout, so the
# template can't rot. It needs no pushed commit: the generated app points at this
# repository by file URL, and the patch from toolkit-dev-config.sh replaces its code
# with the working tree, uncommitted changes included. Cargo still has to fetch the
# pinned rev, so the repository needs at least one commit.
#
# `just css-check` runs here too. It downloads the pinned Tailwind binary unless
# TAILWINDCSS_BIN names a copy you already have.
set -euo pipefail

toolkit="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Outside any `target` directory, which toolkit-dev-config.sh skips.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export CARGO_TARGET_DIR="$toolkit/target/template-check"

url="file://$toolkit"
rev="$(git -C "$toolkit" rev-parse HEAD)"

(
    cd "$work"
    cargo generate --path "$toolkit/templates/app" --name mev_starter --silent --vcs none \
        --define env_prefix=MEV_STARTER --define toolkit_url="$url" --define toolkit_rev="$rev"
)

# rustfmt wraps lines by length and sorts imports by name, so formatting must hold
# whatever the app is called. These only format, so they cost almost nothing.
for name in x zzz_a_rather_long_application_name_for_checking; do
    (
        cd "$work"
        cargo generate --path "$toolkit/templates/app" --name "$name" --silent --vcs none \
            --define env_prefix=A_RATHER_LONG_APPLICATION_PREFIX --define toolkit_url="$url" --define toolkit_rev="$rev"
        cd "$name"
        cargo fmt --all --check
    )
done

cd "$work/mev_starter"

# Nothing of the template syntax may survive into what a project gets.
if grep -rnE '\{\{ *(project-name|crate_name|env_prefix|toolkit_)|\{% *raw' \
    --exclude-dir=vendor --exclude-dir=target . >&2; then
    printf 'unrendered placeholders in the generated app\n' >&2
    exit 1
fi

config="$(TOOLKIT_GIT_URL="$url" "$toolkit/scripts/toolkit-dev-config.sh")"
cargo fmt --all --check
cargo clippy --config "$config" --all-targets --all-features -- -D warnings
cargo nextest run --config "$config" --all-features
scripts/tailwind.sh -i styles/input.css -o "$work/app.css" --minify
cmp -s "$work/app.css" src/static/app.css || {
    printf 'templates/app/src/static/app.css is stale: run `just css` in a generated app\n' >&2
    exit 1
}
# Test helpers stay out of what ships.
cargo --config "$config" tree -e normal --prefix none --format '{p}' |
    awk '{print $1}' | sort -u >"$work/release-tree"
if grep -qx web_mev_test "$work/release-tree"; then
    printf 'web_mev_test in the release tree\n' >&2
    exit 1
fi
printf 'template checks passed\n'
