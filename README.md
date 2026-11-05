# web_mev_toolkit

Crates shared by Rust web apps on axum 0.8, Tera 2 and sqlx 0.9 with SQLite.

| Crate | What it holds |
|---|---|
| `web_mev_core` | Framework-free helpers: tokens, `Clock`, env parsing, client IP, validators, bounded `PasswordHasher` |
| `web_mev_runtime` | Process plumbing: logging, fatal error reports, panic hook, shutdown signal, background workers |
| `web_mev_sqlite` | SQLite pool setup and tuning, write transactions, busy detection, readiness, stored secrets |
| `web_mev_axum` | The HTTP layer: error surfaces, extractors, security headers, access log, rate limits, health routes, flash, htmx helpers |
| `web_mev_test` | Test helpers, used as a dev-dependency: browser-like client, HTTP contract checks, test databases |

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
