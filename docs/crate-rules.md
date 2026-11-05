# Crate rules

`scripts/check-boundaries.sh` enforces what a dependency tree can prove. The rest is for review.

1. **Admission.** A crate or item needs two or more projects that have that code today and no app types, or a place on the list below. One project's code stays in that project until a second one wants it.
2. **Layers.**
   - `web_mev_templates` has no axum, so an app on another framework can use it.
   - `web_mev_axum` has no Tera and no sqlx, so an API-only app can use it.
   - `web_mev_core`, `web_mev_runtime`, `web_mev_sqlite`, `web_mev_i18n` and `web_mev_net` have no axum.
   - `web_mev_assets` only reaches axum through its `axum` feature.
3. **Features.** Heavy or optional parts stay behind features, off by default.
4. **No umbrella crate.** Apps depend on crates directly.
5. **Configuration comes from the app.** Cookie names, env prefixes, CSP sources, login paths and app names are passed in.
6. **Schema stays in the app.** Crates ship reference SQL and check for it. They never run migrations of their own, because one database has one version list.
7. **No panics in production code.** Clippy denies `unwrap`, `expect` and `panic`, and tests are exempt.
8. **Test helpers stay in dev-dependencies.** No crate has `web_mev_test` in its normal tree.
9. **Naming.** Crates are `web_mev_*`, and names carry no app branding.
10. **Not here:** app screens, `users` tables, config structs, domain jobs, and anything that needs an app type to make sense.

## Admitted by decision

These have fewer than two source projects today, and were included on purpose on 2026-11-05:

- `Clock`, session revocation, busy mapping (zbierak)
- `PasswordHasher`, verified sessions, rate limiter (klucznik)
- `CsrfForm`, `json_list`, htmx triggers, access-log redaction, `fetch_limited`, browser test client, asset compression (oxi_feed)
- Template hot reload (new code)
- All of `web_mev_i18n` (only that_day has translations)

## Parked

- **Uploads and thumbnails:** tied to app state, and different in each app. Sources: that_day `src/media.rs`, oxi_feed's `Upload` guard, kosync `src/cover.rs`.
- **Markdown and sanitizing:** zbierak `src/markdown.rs`, oxi_feed `src/feed/sanitize.rs`.
- **OpenAPI:** only zbierak serves docs (`src/openapi.rs`).
- **Metrics:** oxi_feed only (`src/metrics.rs`).
- **App-bound:** outbox delivery, retention, `require_project_role`, the tunnel's WebSocket bridge, the MCP registry.
