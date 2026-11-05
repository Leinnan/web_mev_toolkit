# Using the toolkit from an app

## Dependencies

List the crates in the app's `[workspace.dependencies]`, all at one rev:

```toml
web_mev_axum = { git = "https://github.com/Leinnan/web_mev_toolkit", rev = "..." }
web_mev_auth = { git = "https://github.com/Leinnan/web_mev_toolkit", rev = "..." }
```

Always use this exact URL. Cargo treats a different spelling as a different source and rejects the `git@github.com:` form.

Use one rev for every toolkit crate. Two revs compile two copies, and their types won't match.

To try toolkit changes in an app before pinning a rev, run this from the app's checkout:

```bash
cargo --config "$(path/to/web_mev_toolkit/scripts/toolkit-dev-config.sh)" check
```
