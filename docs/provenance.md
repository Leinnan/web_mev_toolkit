# Where each crate came from

Copies were compared on 2026-11-05. Projects keep moving, so diff again before adopting a crate in a project.

| Crate | Base | Merged in |
|---|---|---|
| `web_mev_core` | klucznik: `src/auth.rs` (`random_token`, `token_hash`, `secure_eq`, bounded hashing, `dummy_verify`, now an owned `PasswordHasher`), `src/keys.rs` (`sha256_hex`), `src/handlers/mod.rs` (`required_text`, `validate_slug`) | zbierak: `src/clock.rs`, `src/auth.rs` (`bearer`), `src/handlers.rs` (`slugify`), `src/config.rs` (`parse_u32`, `parse_origins`); oxi_feed: `src/config.rs` (`var`, `parse`, `parse_bool`, `parse_public_url`, `parse_proxies`, now methods on `Env`), `src/middleware.rs` (`client_ip`, now taking the raw header) |
| `web_mev_runtime` | oxi_feed: `src/main.rs` (`report_fatal`, panic hook), `src/lib.rs` (`init_logging`, signals, worker joining) | wihaister: `src/logging/mod.rs` (returned guard, daily file); new: `Workers`, `sleep_or_stop`, `load_dotenv` |
| `web_mev_sqlite` | oxi_feed: `src/db.rs` | zbierak: `src/db.rs` (`secure_delete`), `src/remote_config.rs` (busy detection); klucznik: `src/signing.rs` (`get_or_create_secret` race handling); oxi_feed: `src/lib.rs` (`instance_key`); new: `ready_for_writes` |
| `web_mev_axum` | zbierak: `src/error.rs`, `src/api_error.rs` (classification, now the `WebError` trait), `src/extractors.rs` (rejection mapping, now generic through `ErrorSurface`) | oxi_feed: `src/extractors.rs` (`read_body`), `src/flash.rs`, `src/middleware.rs` (`access_log`, `security_headers`), `src/web/mod.rs` (`hx_trigger`, now `HxTrigger`); klucznik: `src/limits.rs` (now with configurable paths), `src/lib.rs` (`no-store`); new: `Csp`, `TrustedProxies`, `ClientIp`, `health_routes`, `serve` |
| `web_mev_auth` | klucznik: `src/auth.rs` (sessions, throttle, `create_verified_session`, now `begin_verified`), `src/handlers/authentication.rs` (bootstrap, now `begin_bootstrap`) | zbierak: `src/audit.rs`, session ids and revocation from `src/handlers.rs`, `migrations/`; oxi_feed: `src/extractors.rs` (`CsrfForm`, login redirect), `src/auth.rs` (`authenticate`), `src/web/auth.rs` (double-submit cookie, now `AnonForm`); new: `SessionUser`, `session_sql!`, `verify_schema`, `Clock` in place of `unixepoch()` |
| `web_mev_templates` | oxi_feed: `src/assets.rs` (`html_templates`), `src/web/mod.rs` (`load_templates`, `utc`, `iso`, `ago`, `static_url`) | zbierak: `src/lib.rs` (`urlencode`, `relative_age`, required templates); new: `Templates` and its reload, whose only prior art was palumbus `actix_tera_hot_reload` |
| `web_mev_assets` | oxi_feed: `src/assets.rs`, `src/web/mod.rs` (`static_asset`, `accepted_encoding`) | zbierak: `src/handlers.rs` (extra MIME types); new: per-file compression off the runtime |
| `web_mev_i18n` | `bevy_mev_i18n` with Bevy, `format_printf`, `slug` and pseudo-locales removed, itself grown from that_day `crates/simple_csv_i18n` | that_day: `crates/simple_csv_i18n` (header errors), `src/web/mod.rs` (`safe_return_path`), `src/i18n.rs` (cookie lookup); new: `Accept-Language` negotiation |
| `web_mev_net` | oxi_feed: `src/net_policy.rs`, `src/feed/fetch.rs` (`fetch_limited`, now a `SafeClient` method) | |
| `web_mev_test` | oxi_feed: `tests/common/mod.rs` (`Client`, on-disk WAL database) | zbierak and klucznik: `tests/common/mod.rs` (in-memory pool); that_day: `src/routes/mod.rs` (`ConnectInfo` injection); zbierak: `tests/http_contract.rs` (now `contract`) |

`templates/app` follows `bevy_mev_toolkit`'s `templates/game` in shape. Its Dockerfile, compose file and `.dockerignore` come from oxi_feed, with `stop_grace_period` from that_day. Its CI's `cargo audit` comes from klucznik, and its setup token from klucznik's `bootstrap`, generated and logged when unset. The htmx file is the one all three apps vendor, 2.0.7, and Tailwind is pinned to the 4.3.3 that oxi_feed uses.

## Bugs fixed while porting

The source projects still have these:

- **zbierak `src/config.rs`:** an empty `ZBIERAK_SECRET_KEY` or `ZBIERAK_PUBLIC_URL` is parsed as a value, so the shipped `.env.example` should fail at startup. In `Env`, blank means unset.
- **that_day and kosync client IP:** the leftmost `X-Forwarded-For` value is trusted, and the client can write that one. `client_ip` takes the right-most untrusted address.
- **zbierak and oxi_feed `src/auth.rs`:** Argon2 runs on async workers, and an unknown user skips hashing, so timing reveals which accounts exist. `PasswordHasher` runs on the blocking pool behind a semaphore, and `verify_or_dummy` does the same work either way.
- **gastarbeiter `src/server/logging.rs` and palumbus `src/logging.rs`:** the log guard is dropped or leaked, so file logging stops or its last lines are lost. `init_logging` returns the guard.
- **zbierak, klucznik and oxi_feed `run()`:** logging starts before `dotenvy` runs, so `RUST_LOG` in `.env` is ignored. `load_dotenv` comes first in the documented order.
- **zbierak `src/error.rs`:** `Retry-After` is only set on the JSON surface. `WebError::retry_after` applies to both.
- **zbierak `src/error.rs`, klucznik and oxi_feed:** HTML error pages escape `<`, `>` and `&` but not quotes. `escape_html` also covers quotes.
- **klucznik `src/limits.rs`:** a poisoned lock refuses every request from then on, and `X-Forwarded-For` only works when the proxy overwrites it with one address. `RateLimiter` recovers from poisoning and uses `client_ip`.
- **zbierak and oxi_feed `src/lib.rs`:** subrouter layers miss the top-level fallback, so headers are added by hand there. `security_headers` goes on the outermost router, and a test covers the fallback.
- **zbierak and klucznik handlers:** the CSRF check is a manual call (34 and 19 of them), so a forgotten one still compiles. `CsrfForm` and `AnonForm` can't hand over the form without it.
- **zbierak `src/handlers.rs`:** the login CSRF cookie has no `Secure`. `issue_anon_csrf` sets it from config.
- **zbierak `src/handlers.rs`:** `X-CSRF-Token` is sent by `app.js` but ignored by the server. `CsrfForm` reads it.
- **zbierak and oxi_feed bootstrap:** the first visitor to reach an empty instance wins with no token (zbierak), and zbierak's form has no CSRF check. `AnonForm` covers the form. Whether to require a setup token stays the app's choice, as in klucznik.
- **oxi_feed `src/web/settings.rs`:** a password change isn't transactional, so the update, revocation and audit can partly apply. The starter does all three in one `write_tx`, and `revoke_all` takes the transaction.
- **zbierak and oxi_feed `src/net_policy.rs`:** the NAT64 check reads segments 6 and 7 instead of 0 and 1, so it never matches. The later 2000::/3 rule happens to block that range, but 6to4 (`2002::/16`) and Teredo (`2001::/32`) pass the policy and can tunnel to private IPv4 addresses. All three are blocked here.
- **zbierak `src/outbox.rs`:** the pinned client has no `.no_proxy()`, so a proxy from the environment would connect on its behalf and skip the policy. `SafeClient` always sets it.
- **palumbus `actix_tera_hot_reload`:** a busy lock silently loses a reload, a failed reload is discarded with no message, and handlers can return 500 during a swap. `Templates` builds the new set first, swaps it in only on success and logs failures, and readers keep the set they started with.
- **oxi_feed `src/web/mod.rs`:** any `?v=` value earns an immutable cache lifetime, so an old URL can pin new content, or a guessed one can pin it for a year. `Assets::respond` only trusts the file's current digest.
- **oxi_feed `src/assets.rs`:** the first request compresses every file at once on an async worker. `Assets` compresses each file once on first use, off the runtime.
- **zbierak bootstrap:** the first visitor to reach an empty instance gets the owner account. The starter always requires a setup token, generated and logged when none is set.
- **zbierak, klucznik and oxi_feed `init_logging`:** a default filter of `my_app=info` hides anything logged by a dependency under its own target. Once these apps use the toolkit, that would include its access log and error reports. The starter's default is `info,sqlx=warn`.
