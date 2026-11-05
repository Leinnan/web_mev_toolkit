# Using the toolkit from an app

The API is provisional. Only `templates/app` has used it so far, so expect changes when the first existing app adopts it. Apps pin revs, so a change only reaches an app when it moves its pin.

## Starting a new app

```bash
cargo generate --path templates/app --name my_app --define toolkit_rev="$(git rev-parse HEAD)"
```

The rev must be on GitHub for the generated app to build, unless you also pass `--define toolkit_url=file:///path/to/web_mev_toolkit`. Then run `cargo generate-lockfile` and commit the lockfile, because `just verify` and the Dockerfile build with `--locked`.

## Dependencies

List the crates at one rev:

```toml
web_mev_axum = { git = "https://github.com/Leinnan/web_mev_toolkit", rev = "..." }
web_mev_auth = { git = "https://github.com/Leinnan/web_mev_toolkit", rev = "..." }
```

Always use this exact URL. Cargo treats a different spelling as a different source and rejects the `git@github.com:` form.

Use one rev for every toolkit crate. Two revs compile two copies, and their types won't match. `web_mev_test` belongs in `[dev-dependencies]` only.

## Access while the repository is private

**On your machine**, in `~/.gitconfig` and `~/.cargo/config.toml`:

```ini
[url "git@github.com:Leinnan/web_mev_toolkit"]
    insteadOf = https://github.com/Leinnan/web_mev_toolkit
```

```toml
[net]
git-fetch-with-cli = true
```

**In CI**, the starter's `.github/actions/toolkit-auth` does the same with a `TOOLKIT_READ_TOKEN` secret, which needs read access to this repository and nothing else. **In Docker**, a private dependency also needs credentials in the build. The Dockerfile doesn't handle that, so make the repository public or add a BuildKit secret.

## Trying toolkit changes in an app

From the app's checkout:

```bash
cargo --config "$(~/Documents/UTILS/web_mev_toolkit/scripts/toolkit-dev-config.sh)" check
```

This rewrites `Cargo.lock`. Restore it before committing, then pin the new rev.

`clippy` and `nextest` are external subcommands, so cargo ignores a `--config` placed before them and silently builds the pinned rev. Put it after: `cargo clippy --config "$config"`, `cargo nextest run --config "$config"`.

## Wiring an app

`templates/app/src/` is the worked example. These are the parts that can't be guessed:

- **Errors.** Keep the app's own `AppError`. Implement `WebError` and `From<RequestError>`, keep a local `ApiError` wrapper, and implement `ErrorSurface` on the state. See `src/error.rs`.
- **Auth state.** Implement `FromRef<AppState>` for `SessionStore`, and `SessionUser` for the app's user. See `src/user.rs`.
- **Layers.** Put `CookieManagerLayer`, then the rate limit, access log and `security_headers`, on the outermost router, or the 404 fallback misses them.
- **Log filter.** Use `info,sqlx=warn`, not `my_app=info`, which hides the toolkit's access log and error reports.
- **Client addresses.** Pass real proxy addresses to `TrustedProxies`. With none, `X-Forwarded-For` is ignored and everyone behind a proxy shares one rate limit.
- **Order in `main`.** `load_dotenv`, then `init_logging`, or `RUST_LOG` from `.env` won't count.

## Adopting in an existing app

Checked on 2026-11-05 by reading the code. None of these has been tried. Move one crate at a time, in the order below, so each step can be tested and reverted alone.

| Order | Crates | What changes |
|---|---|---|
| 1 | `web_mev_core`, `web_mev_runtime`, `web_mev_sqlite` | Replace the local helpers. No schema change. |
| 2 | `web_mev_axum`, `web_mev_test` | Keep `AppError`, and add the traits. |
| 3 | `web_mev_templates`, `web_mev_assets`, `web_mev_net` | Replace local setup. Needs Tera 2. |
| 4 | `web_mev_auth` | One migration, and the behaviour changes below. |

### Schema

Each app adds one ordinary migration of its own. `verify_schema` at startup then proves it worked, and a test runs these against populated tables and checks that existing sessions survive.

| App | Migration |
|---|---|
| klucznik, oxi_feed | `crates/web_mev_auth/sql/adopt_session_ids.sql`. The `DROP INDEX` lines cover both apps' index names. |
| oxi_feed | Also `crates/web_mev_auth/sql/adopt_audit_columns.sql`, and move 12 `auth::audit` calls to `audit::record`. |
| zbierak | None expected: its tables already have this shape. Run `verify_schema` to confirm. |
| that_day, kosync, lwa_webdav_server | Their auth models differ, so copy `REFERENCE_SQL` as new tables and move logins over. Sessions don't carry over. See below. |

### App by app

| App | Can adopt | Work and behaviour changes |
|---|---|---|
| klucznik | Everything but `web_mev_net`, which it has no use for | The best first app: smallest of the family. Replace 19 manual `check_csrf` calls with `CsrfForm`. `LoginPolicy::Status`, because its tests assert 401. Its rate limiter's `X-Forwarded-For` now takes the right-most untrusted address, so a proxy no longer has to overwrite the header. |
| oxi_feed | Everything | `MIN_PASSWORD` is 8, so use `PasswordHasher::new(4, 8..=1024)` or existing users hit a stricter rule on change. Argon2 moves off async workers, and unknown users now cost a hash. Pass `AccessLog::redact("/websub/", 1)`. Keep `Csp::strict().set("img-src", "* data:")` and its other sources. Its `net_policy` and `fetch` move to `SafeClient`, and the WebSub `Link` parsing stays in the app, reading `Fetched::headers`. |
| zbierak | Everything | The largest move. Replace 34 manual `check_csrf` calls with `CsrfForm`. An empty `ZBIERAK_SECRET_KEY` starts working. Bootstrap should gain a setup token, because any first visitor gets the owner account today. Its pinned webhook client moves to `SafeClient::http`, which adds `.no_proxy()`. |
| that_day | Core, runtime, sqlite, axum, assets, test | Tera 2 first: 18 templates, no custom filters, needs bulk loading. No user table, so `web_mev_auth` doesn't fit its role-based cookie. Move `simple_csv_i18n` to `web_mev_i18n`, whose headers are compatible, and pass counts as `n.unsigned_abs()`. Its client IP changes from leftmost to right-most untrusted. |
| kosync_rs | Core, runtime, sqlite, axum, assets, test; `web_mev_auth` after a login rework | Tera 2 is real work here: it has no macros, which leaves 3 macros, 21 imports and about 91 call sites across 16 templates. KOReader's protocol errors stay in the app. Replacing MD5 needs a password migration: KOReader sends the MD5 itself, so check that before planning it. |
| lwa_webdav_server | Everything but `web_mev_net` | Tera 2: 5 templates, trivial. Its hashed-token sessions are close, but `token_hash` is a raw BLOB rather than hex text, so it needs a migration of its own, which logs people out. Add `DefaultBodyLimit`, because browser uploads hit axum's 2 MB default today. |
| kuchcik | Core, runtime, axum, templates, assets, net, test | No database, so no sqlite or auth. Fix its XSS and GET logout first, and don't wait for adoption. Its `downloader` moves to `SafeClient`, which adds the SSRF guard, timeout and size limit it lacks. |
| gastarbeiter | Core, runtime, axum, test | No Tera. Its logging, shutdown, error type, bearer check and request log can all go. Its in-app health route should move outside auth. |
| palumbus | Core, runtime, i18n, net; templates after Tera 2 | On actix-web, so nothing with axum in it. Replaces its `actix_tera_hot_reload` crate and logging. A port to axum is feasible at about 2k lines. |

## Breaking changes

None yet. Add a note here when a change needs edits in apps.
