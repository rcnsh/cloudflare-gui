# Cloudflare GUI

Native macOS console for Cloudflare D1, KV, R2 and live Worker logs. Rust + GPUI + gpui-component.

## Build and run

```sh
cargo run              # debug (GPUI and text crates are built with opt-level 3 anyway)
cargo run --release
./script/check         # cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
RUST_LOG=cloudflare_gui=debug cargo run   # verbose logging (never logs the token)
```

If `cargo` isn't on PATH, it's at `~/.cargo/bin`. CI runs `script/check` on `macos-latest`
(`.github/workflows/check.yml`).

## Pinned GPUI versions

| Crate             | Version  | Notes                                                    |
| ----------------- | -------- | -------------------------------------------------------- |
| gpui-pre          | =0.3.8   | renamed to `gpui`; the macros emit `gpui::` paths, so keep the name |
| gpui-pre-platform | =0.3.8   | renamed to `gpui_platform`, features `font-kit`, `runtime_shaders` |
| gpui-component    | =0.7.1   | features `tree-sitter`, `tree-sitter-sql`                |
| gpui-kit-assets   | =0.7.1   | icon assets                                              |

`runtime_shaders` compiles the Metal shaders at launch. That way the build doesn't need Xcode's
Metal toolchain, either locally or on CI.

### Upgrading GPUI

gpui-component pins one exact gpui-pre snapshot, and gpui-pre's API changes between snapshots.
Always bump them together:

1. Pick the gpui-component release. Its exact requirement on `gpui-pre` comes from
   `https://crates.io/api/v1/crates/gpui-component/<ver>/dependencies`.
2. Set `gpui`, `gpui_platform`, `gpui-component` and `gpui-kit-assets` to those exact `=` versions.
3. Clone the matching gpui-kit tag into `~/dev/vendor/gpui-kit-<ver>`
   (`git clone --depth 1 --branch v<ver> https://github.com/longbridge/gpui-kit`) and read the
   changelog, the examples and the story app before fixing compile errors.
4. `./script/check`, then run the app and click through every view.

## Reference material

- gpui-kit source, examples and the story/gallery app: `~/dev/vendor/gpui-kit-0.7.1`
  (`examples/dock`, `crates/story/src/stories/*`, `crates/component/src/*`, `crates/base/src/dock`).
- gpui-pre source: `~/.cargo/registry/src/*/gpui-pre-0.3.8`.
- API cheat sheets written while building this: `~/dev/vendor/notes/*.md`.
- Cloudflare OpenAPI spec: `~/dev/vendor/cloudflare-api/openapi.json`, from
  `github.com/cloudflare/api-schemas`. Check endpoints against it before adding them.
- The Worker tail WebSocket protocol isn't in the spec. It follows Wrangler's
  `packages/wrangler/src/tail/` (subprotocol `trace-v1`, `{"debug":false}` on open, 10s pings).

## Rules

- **Read-only by default.** Any SQL that isn't a plain SELECT, PRAGMA getter or EXPLAIN goes through
  `sql::classify`, and must have write mode switched on for that tab and a confirmation dialog
  showing the exact statement and target database. `Client::d1_query` checks the classification again,
  so `QueryIntent::Read` can never send a write. The same rule applies to KV writes and R2 deletes.
- Tails are real resources. Only create one when the user presses Start. Register it in
  `state::tails` so it is deleted on stop, tab close or quit.
- Never retry anything that might write. Only `Retry::Idempotent` requests retry on 5xx or network errors.
- Be polite to the API: resource lists are cached in `state::Session` and refetched only on explicit refresh.
- Secrets live only in the Keychain (`keychain.rs`). `cloudflare::Token` redacts itself in Debug output.
  Never log tokens, put them in fixtures, or show them in screenshots.
- Test fixtures in `tests/fixtures/` must not contain real account IDs, emails or tokens.
- Comments explain why, not what. Plain-English, outcome-focused commit messages.

## Module layout

```
src/
  main.rs            app bootstrap: tokio runtime, assets, theme, window
  actions.rs         every GPUI action, keybindings and the menu bar
  keychain.rs        API token storage in the login Keychain
  runtime.rs         tokio runtime that runs network futures for GPUI (see the module docs)
  sql.rs             read/write classification of SQL (the safety gate), heavily tested
  cloudflare/        typed REST client, one file per product
    mod.rs           Client, envelope parsing, retries and backoff, errors
    accounts.rs d1.rs kv.rs r2.rs workers.rs (incl. the tail WebSocket)
    testing.rs       scripted local HTTP server for tests
  state/             GPUI entities and persisted state
    session.rs       client + account + cached resource lists
    history.rs       per-database query history
    settings.rs      non-secret preferences
    tails.rs         tail sessions that are open, so quitting can delete them
  devtools.rs        scripted UI driving and screenshots (feature `devtools` only)
  views/             one file per view
    app.rs           setup flow or workspace, title bar
    setup.rs         token entry, verification, account picker
    workspace.rs     dock layout, opens a tab per resource
    sidebar.rs       resource tree
    palette.rs       cmd-K command palette
    sql_editor.rs d1_table.rs results_table.rs     D1
    kv_browser.rs value_viewer.rs                  KV (the viewer is shared with R2)
    r2_browser.rs worker_tail.rs welcome.rs
tests/fixtures/      recorded API responses (sanitized)
```

## Driving the UI in development

`cargo build --features devtools` adds a script runner. `CFGUI_SCRIPT` is a `;`-separated list
of `wait <secs>`, `key <keystroke>`, `type <text>`, `click <x> <y>` (logical pixels),
`shot <file.png>` and `quit`. Screenshots are rendered by the app itself, so they don't need
screen-recording permission. Keep them in `screenshots/`, which is gitignored because the
account name contains an email address.

`CFGUI_FAKE_TAIL=1` points Worker tail tabs at a local WebSocket that sends synthetic events,
so the tail view can be exercised without creating a tail session on the account.

Unsigned debug builds get a new code signature on every build, so macOS may ask again for
Keychain access. The app shows "Reading the API token from the Keychain…" until someone answers.
