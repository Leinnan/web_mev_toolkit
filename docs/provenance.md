# Where each crate came from

Copies were compared on 2026-11-05. Projects keep moving, so diff again before adopting a crate in a project.

| Crate | Base | Merged in |
|---|---|---|
| `web_mev_core` | klucznik: `src/auth.rs` (`random_token`, `token_hash`, `secure_eq`, bounded hashing, `dummy_verify`, now an owned `PasswordHasher`), `src/keys.rs` (`sha256_hex`), `src/handlers/mod.rs` (`required_text`, `validate_slug`) | zbierak: `src/clock.rs`, `src/auth.rs` (`bearer`), `src/handlers.rs` (`slugify`), `src/config.rs` (`parse_u32`, `parse_origins`); oxi_feed: `src/config.rs` (`var`, `parse`, `parse_bool`, `parse_public_url`, `parse_proxies`, now methods on `Env`), `src/middleware.rs` (`client_ip`, now taking the raw header) |
| `web_mev_runtime` | oxi_feed: `src/main.rs` (`report_fatal`, panic hook), `src/lib.rs` (`init_logging`, signals, worker joining) | wihaister: `src/logging/mod.rs` (returned guard, daily file); new: `Workers`, `sleep_or_stop`, `load_dotenv` |
| `web_mev_sqlite` | oxi_feed: `src/db.rs` | zbierak: `src/db.rs` (`secure_delete`), `src/remote_config.rs` (busy detection); klucznik: `src/signing.rs` (`get_or_create_secret` race handling); oxi_feed: `src/lib.rs` (`instance_key`); new: `ready_for_writes` |
| `web_mev_axum` | zbierak: `src/error.rs`, `src/api_error.rs` (classification, now the `WebError` trait), `src/extractors.rs` (rejection mapping, now generic through `ErrorSurface`) | oxi_feed: `src/extractors.rs` (`read_body`), `src/flash.rs`, `src/middleware.rs` (`access_log`, `security_headers`), `src/web/mod.rs` (`hx_trigger`, now `HxTrigger`); klucznik: `src/limits.rs` (now with configurable paths), `src/lib.rs` (`no-store`); new: `Csp`, `TrustedProxies`, `ClientIp`, `health_routes`, `serve` |
| `web_mev_test` | oxi_feed: `tests/common/mod.rs` (`Client`, on-disk WAL database) | zbierak and klucznik: `tests/common/mod.rs` (in-memory pool); that_day: `src/routes/mod.rs` (`ConnectInfo` injection); zbierak: `tests/http_contract.rs` (now `contract`) |

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
