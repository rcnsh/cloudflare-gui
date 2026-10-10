# Architecture

A single-binary macOS app. GPUI draws the UI, a tokio runtime does the networking, and a small
typed client talks to the Cloudflare REST API. There's no backend of its own.

## Layout

```
src/
  main.rs            bootstrap: logging, assets, theme, keybindings, menus, main window, quit hook
  actions.rs         every GPUI action, keybinding and the menu bar
  keychain.rs        API token in the login Keychain
  runtime.rs         shared tokio runtime for network futures
  sql.rs             read/write classification of SQL (the safety gate)
  cloudflare/        typed REST client, one file per product
    mod.rs           Client, Token, request building, envelope parsing, retries and backoff
    accounts.rs      token verify, account list
    d1.rs            databases, raw queries, table list
    kv.rs            namespaces, paged keys, values and metadata
    r2.rs            buckets, object listings, size-capped object reads
    workers.rs       scripts, tail create/delete, tail WebSocket stream
    testing.rs       scripted local HTTP server used by the tests
  state/             GPUI entities and persisted state
    session.rs       client, chosen account, cached resource lists
    history.rs       per-database query history (history.json)
    settings.rs      non-secret preferences (settings.json)
    tails.rs         open tail sessions, so they can be deleted on close or quit
  views/             one file per view
    app.rs           setup flow or workspace
    setup.rs         token entry, verification, account picker
    workspace.rs     dock layout, one tab per opened resource
    sidebar.rs       resource tree
    palette.rs       ⌘K command palette
    sql_editor.rs d1_table.rs results_table.rs   D1
    kv_browser.rs value_viewer.rs                KV (viewer shared with R2)
    r2_browser.rs worker_tail.rs welcome.rs
  devtools/          only with --features devtools
    mod.rs           scripted UI driving, frame capture, fake tail server
    demo.rs          offline demo account: fake REST API, D1 on in-memory SQLite
tests/fixtures/      recorded API responses, sanitized
script/              check (CI), run (cargo runner, signs debug builds), bundle (release .app)
```

## Data flow

1. **Startup.** `main.rs` opens one window holding `views::app::AppView`. It loads the token from
   the Keychain off the UI thread. No token, or no account chosen, shows `views::setup`;
   otherwise the workspace.
2. **Sign-in.** `setup.rs` builds a `cloudflare::Client`, calls `user/tokens/verify` and
   `accounts`, and saves a newly entered token to the Keychain once it verifies.
3. **Browsing.** `state::Session` holds the client and account, and fetches the D1, KV, R2 and
   Workers lists once. The sidebar and the palette read from that cache; ⌘R forces a refetch.
4. **Opening a resource.** `views::workspace` adds a dock tab for it. Each view calls the client
   directly for its own data (table pages, SQL results, key pages, object listings, previews).
5. **Tails.** `worker_tail.rs` creates a tail over REST only when the user presses Start,
   registers it in `state::tails`, and streams events from the returned `wss://` URL. Stop, tab
   close and quit delete it.

Every network future runs on the tokio runtime in `runtime.rs`; GPUI tasks await the
`JoinHandle`. reqwest and tungstenite need tokio's reactor, and GPUI has its own executor.

## External services

- **Cloudflare REST API** at `https://api.cloudflare.com/client/v4/`, bearer token auth.
  Endpoints used: token verify, accounts, D1 databases and `raw` query, KV namespaces/keys/values/
  metadata, R2 buckets and objects, Workers scripts and tails.
- **Worker tail WebSocket.** Not in the OpenAPI spec; follows Wrangler's implementation
  (subprotocol `trace-v1`, `{"debug":false}` on open, a ping every 10 s).
- **macOS Keychain** via `keyring-core` and `apple-native-keyring-store`.

## Decisions

- **Read-only by default, checked twice.** `sql::classify` treats anything it isn't sure of as a
  write. The SQL editor needs write mode plus a confirmation dialog, and `Client::d1_query`
  refuses to send a write with `QueryIntent::Read`, so a UI bug can't bypass the gate.
- **Writes are never retried after they might have run.** Only `Retry::Idempotent` requests retry
  on 5xx or network errors. `Retry::RateLimitOnly` requests retry only on an outright 429 or a failed
  connection. Backoff honours `Retry-After`.
- **Cache lists, refetch on request.** Keeps the app light on the API and makes ⌘K instant.
- **Tails are real resources.** Created only on Start and tracked so they're deleted. Quit
  blocks for up to 3 s to delete them, because GPUI's quit hooks are too short for a request.
  Tails also expire server-side, which covers a crash.
- **Secrets only in the Keychain.** `cloudflare::Token` redacts itself in `Debug` output, and
  nothing in the data directory is secret.
- **REST for R2.** Object reads go through the REST API with the same token, so no S3 keys. The
  body is streamed and dropped after 8 MB, so large objects never download in full.
- **Pinned GPUI.** `gpui-pre`, `gpui-pre-platform`, `gpui-component` and `gpui-kit-assets` are
  pinned to exact versions and must be bumped together (see `CLAUDE.md`). `runtime_shaders`
  compiles Metal shaders at launch, so building needs only the Command Line Tools.
- **Demo mode instead of mocks in the UI.** `devtools::demo` serves a fake API on localhost and
  the real client talks to it via `Client::with_base`, so the whole app runs unchanged against
  invented data.

## Tests

`cargo test` covers the client against `cloudflare::testing`'s local server and the fixtures in
`tests/fixtures/` (envelope parsing, retries, pagination, the D1 write guard), plus `sql.rs`,
tail event parsing, history, settings and a few pure view helpers. There are no UI tests; `devtools` scripts are used for
manual checks and screenshots.
