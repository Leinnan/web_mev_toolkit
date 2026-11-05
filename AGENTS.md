# web_mev_toolkit

Crates shared by Rust web apps on axum 0.8, Tera 2 and sqlx 0.9 with SQLite, plus a `cargo generate` starter in `templates/app`. Read `docs/crate-rules.md` before adding a crate or a dependency, `docs/consuming.md` for how apps use them and `docs/provenance.md` for where the code came from.

## Commands

```bash
just check          # fmt, clippy -D warnings, crate boundaries
just test           # all tests, with nextest when installed
just check-alone    # each crate alone with no default features
just check-template # generates the starter and runs its checks
just ci             # everything CI runs
```

## Rules

- Run one cargo command at a time, and never `cargo clean`.
- Prefer `--workspace` over `-p` for tests: `-p` changes feature unification and forces rebuilds.
- Apps pin revs, so a breaking change here reaches an app only when it moves its pin. Add the note to `docs/consuming.md` when a change needs edits in apps.
- Add a new crate to the lists in `scripts/check-boundaries.sh` and the table in `README.md` when you add it to `crates/`, and record its sources in `docs/provenance.md`.
- Cookie names, env prefixes, CSP sources, paths and app names come from the app. Never hard-code one in a crate.
- A security fix gets a regression test, and a line under "Bugs fixed while porting" in `docs/provenance.md` when the source apps still have the bug.
