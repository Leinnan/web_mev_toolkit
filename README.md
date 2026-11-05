# web_mev_toolkit

Crates shared by Rust web apps on axum 0.8, Tera 2 and sqlx 0.9 with SQLite.

| Crate | What it holds |
|---|---|

Read [docs/crate-rules.md](docs/crate-rules.md) before adding a crate or a dependency, [docs/consuming.md](docs/consuming.md) for how an app uses these crates, and [docs/provenance.md](docs/provenance.md) for where the code came from.

## Commands

```sh
just check          # fmt, clippy -D warnings, crate boundaries
just test           # all tests, with nextest when installed
just check-alone    # each crate alone with no default features
just check-template # generates the starter and runs its checks
just ci             # everything CI runs
```

## License

MIT OR Apache-2.0, at your option.
