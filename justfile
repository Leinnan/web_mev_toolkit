_list:
    @just --list

# Format, lints and crate boundaries: run before every commit.
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    scripts/check-boundaries.sh

fix:
    cargo fmt --all
    cargo clippy --workspace --all-targets --all-features --fix --allow-dirty --allow-staged

test *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    # Agents get the quiet profile, which prints only failures.
    profile="default"
    if [[ -n "${CLAUDECODE:-}" ]]; then profile="agent"; fi
    if command -v cargo-nextest >/dev/null; then
        cargo nextest run --workspace --all-features --profile "$profile" --no-tests pass {{ARGS}}
    else
        cargo test --workspace --all-features {{ARGS}}
    fi

# Each crate alone with no default features, to catch feature assumptions.
check-alone:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob
    for manifest in crates/*/Cargo.toml; do
        crate="$(basename "$(dirname "$manifest")")"
        printf '== %s\n' "$crate"
        cargo check --package "$crate" --no-default-features
    done

# Generates templates/app and runs its checks against this checkout.
check-template:
    scripts/check-template.sh

# Everything CI runs.
ci: check check-alone test check-template

# A cargo config pointing a project at this checkout; run it from the project.
dev-config:
    @scripts/toolkit-dev-config.sh
