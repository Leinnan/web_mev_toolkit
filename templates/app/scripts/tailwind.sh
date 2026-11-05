#!/usr/bin/env bash
# Runs the pinned standalone Tailwind CLI, which needs no Node. It's downloaded to
# .tools/ on first use and checked against these digests from the release:
#
#   gh api repos/tailwindlabs/tailwindcss/releases/tags/v4.3.3 \
#     --jq '.assets[] | "\(.name) \(.digest)"'
#
# Set TAILWINDCSS_BIN to use a copy you already have. Its version must match, or
# the output differs and `just css-check` fails.
set -euo pipefail

version="4.3.3"

if [[ -n "${TAILWINDCSS_BIN:-}" ]]; then
    exec "$TAILWINDCSS_BIN" "$@"
fi

case "$(uname -s)-$(uname -m)" in
Linux-x86_64) asset="linux-x64" digest="dc61b3ac6b8c9ca874c0cc4c57b2409791a64c5540404ca5f5367360babc313a" ;;
Linux-aarch64) asset="linux-arm64" digest="55fd0b241214eff3de1e8ee4f22796662f2d2e7a49bcfca7477cfd0bac398195" ;;
Darwin-arm64) asset="macos-arm64" digest="cdf646702987a743464dff4d9c60fd4480d1c1e73dd819a9a67f1078815dce9d" ;;
Darwin-x86_64) asset="macos-x64" digest="7922e0953f2110c05976e3bf58f14e643d90427575e766b7d433f5f80cbee7e1" ;;
*)
    printf 'no Tailwind binary for %s-%s\n' "$(uname -s)" "$(uname -m)" >&2
    exit 1
    ;;
esac

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="$root/.tools/tailwindcss-$version"

if [[ ! -x "$binary" ]]; then
    mkdir -p "$root/.tools"
    download="$(mktemp "$root/.tools/download.XXXXXX")"
    trap 'rm -f "$download"' EXIT
    curl --fail --silent --show-error --location --output "$download" \
        "https://github.com/tailwindlabs/tailwindcss/releases/download/v$version/tailwindcss-$asset"
    if ! printf '%s  %s\n' "$digest" "$download" | shasum -a 256 --check --status; then
        printf 'the Tailwind download does not match its digest\n' >&2
        exit 1
    fi
    chmod +x "$download"
    mv "$download" "$binary"
    trap - EXIT
fi

exec "$binary" "$@"
