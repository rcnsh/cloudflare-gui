<div align="center">

# Cloudflare GUI

**A keyboard-driven native macOS console for Cloudflare D1, KV, R2 and live Worker logs.**

[![Check](https://github.com/rcnsh/cloudflare-gui/actions/workflows/check.yml/badge.svg)](https://github.com/rcnsh/cloudflare-gui/actions/workflows/check.yml)
[![Release](https://img.shields.io/github/v/release/rcnsh/cloudflare-gui?sort=semver)](https://github.com/rcnsh/cloudflare-gui/releases/latest)
![macOS 11+](https://img.shields.io/badge/macOS-11%2B-black?logo=apple)
![Rust](https://img.shields.io/badge/built_with-Rust_%2B_GPUI-orange?logo=rust)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

![The SQL editor running a query against a D1 database](docs/screenshots/sql.png)

</div>

I built this to be TablePlus for a Cloudflare account: browse D1 tables and run SQL, explore KV
namespaces and R2 buckets, and tail Worker logs in one native window. It's written in Rust with
[GPUI](https://gpui.rs) and talks to the [Cloudflare REST API](https://developers.cloudflare.com/api/)
directly with a single API token.

> Screenshots come from the built-in [demo mode](#demo), with invented data.

## Features

<table>
<tr>
<td width="50%" valign="top">

### D1
Browse any table page by page, or open an SQL tab with highlighting. **⌘↩** runs the
selection or the whole buffer. Row counts, timings, rows read and errors show inline, and every
database keeps its own query history.

</td>
<td width="50%" valign="top">

### Workers KV
Search keys by prefix. Keys load page by page as you scroll. Values open in a viewer that
pretty-prints JSON and shows metadata and expiry.

</td>
</tr>
<tr>
<td><img src="docs/screenshots/table.png" alt="Browsing the rows of a D1 table"></td>
<td><img src="docs/screenshots/kv.png" alt="A KV value pretty-printed as JSON"></td>
</tr>
<tr>
<td width="50%" valign="top">

### R2
Browse buckets folder by folder, with sizes and modified times. Preview images, JSON and text,
with custom metadata and ETags. Previews read at most 8 MB, so large objects never download in
full.

</td>
<td width="50%" valign="top">

### Worker logs
Stream a Worker's invocations as they happen. Filter by level or text, pause without losing
events, and click any line to see the full event.

</td>
</tr>
<tr>
<td><img src="docs/screenshots/r2.png" alt="Previewing an image in an R2 bucket"></td>
<td><img src="docs/screenshots/tail.png" alt="Live Worker logs with an exception highlighted"></td>
</tr>
</table>

Also:

- **⌘K** jumps to any database, table, namespace, bucket or Worker. It searches what's already
  loaded, so it never sends a request.
- Tabs in a dock layout. Every major action has a shortcut and a menu item.
- Resource lists are cached until you refresh. Rate limits and server errors are backed off and
  retried; anything that writes is never retried.

<p align="center"><img src="docs/screenshots/palette.png" width="80%" alt="The command palette"></p>

## Safety

The app is read-only unless you say otherwise.

- SQL is classified before it's sent. Anything that isn't a plain `SELECT`, `EXPLAIN` or
  read-only `PRAGMA` is blocked unless you turn on write mode for that tab, and then you confirm
  a dialog showing the exact statement and target database. The API client checks the
  classification again, so a UI bug can't send a write in read-only mode.
- The API token lives only in the macOS login Keychain. It's never written to disk, logged or
  shown.
- A live tail creates a short-lived tail session on the Worker. That only happens when you press
  **Start**, and the session is deleted when you stop, close the tab or quit.

## Quickstart

### Install

1. Download `Cloudflare-GUI-<version>-macos-universal.zip` from the
   [latest release](https://github.com/rcnsh/cloudflare-gui/releases/latest). It runs natively on
   Apple Silicon and Intel.
2. Unzip it and move **Cloudflare GUI.app** to `/Applications`.
3. The app isn't notarized, so the first launch is blocked. Open **System Settings → Privacy &
   Security** and click **Open Anyway**, or run:

   ```sh
   xattr -dr com.apple.quarantine "/Applications/Cloudflare GUI.app"
   ```

4. Paste an [API token](#api-token) and pick an account.

When macOS asks whether the app may use the token stored in your Keychain, click
**Always Allow**. Release builds are signed ad hoc, so macOS asks once more after each update.

### From source

Needs the Rust toolchain (edition 2024) and the Xcode Command Line Tools
(`xcode-select --install`). Full Xcode isn't required, because the Metal shaders compile at
launch.

```sh
git clone https://github.com/rcnsh/cloudflare-gui
cd cloudflare-gui
cargo run --release
```

### Demo

The development build has an offline demo account. It serves a fake Cloudflare API on localhost,
and D1 is a real in-memory SQLite database, so any SQL you type runs. Demo mode never reads or
writes your Keychain or settings.

```sh
CFGUI_DEMO=1 cargo run --features devtools
```

## Configuration

### API token

Create a token at **dash.cloudflare.com → My Profile → API Tokens → Create Token → Create Custom
Token**, scoped to the account you want to browse.

| Permission (Account)   | Access | Used for                                 |
| ---------------------- | ------ | ---------------------------------------- |
| Account Settings       | Read   | Listing accounts for the account picker  |
| D1                     | Read   | Listing databases, running read queries  |
| Workers KV Storage     | Read   | Namespaces, keys, values and metadata    |
| Workers R2 Storage     | Read   | Buckets, object listings and previews    |
| Workers Scripts        | Read   | Listing Workers                          |
| Workers Tail           | Read   | Live Worker logs                         |

R2 goes through the REST API with this same token, so you don't need S3 access keys. Add
**D1 · Edit** only if you want to run statements that write. The app still asks you to confirm
each one.

The app doesn't create any Cloudflare resources apart from tail sessions. If you want something
to browse, make it with Cloudflare's `cf` CLI:

```sh
cf d1 create --name <name>
cf kv namespaces create --title <title>
cf r2 buckets create --name <name>
```

### Environment variables

None are needed to run the app. These are read for development:

| Variable              | Read by                     | Effect                                                         |
| --------------------- | --------------------------- | -------------------------------------------------------------- |
| `RUST_LOG`            | `src/main.rs` (env_logger)  | Log filter. Default `cloudflare_gui=info,warn`. Never logs the token. |
| `CFGUI_DEMO`          | `src/devtools/demo.rs`      | Sign in to the offline demo account. Needs `--features devtools`. |
| `CFGUI_FAKE_TAIL`     | `src/devtools/mod.rs`       | Point tail tabs at a local fake WebSocket. Needs `devtools`.   |
| `CFGUI_SCRIPT`        | `src/devtools/mod.rs`       | Scripted UI steps for screenshots. Needs `devtools`.           |
| `CFGUI_SIGN_IDENTITY` | `script/run`                | Code-signing identity for debug builds. Default `cloudflare-gui (self-signed)`. |

### Files

- **API token:** login Keychain, service `dev.cloudflare-gui.api-token`.
- **Settings and query history:** `~/Library/Application Support/cloudflare-gui/`
  (`settings.json`, `history.json`). Nothing in there is secret. Demo mode uses a temp directory
  instead.

## Keyboard

| Keys              | Action                                        |
| ----------------- | --------------------------------------------- |
| ⌘K / ⌘P           | Command palette                               |
| ⌘R                | Refresh resource lists                        |
| ⌘B                | Show or hide the sidebar                      |
| ⌘T                | New SQL tab                                   |
| ⌘W                | Close tab                                     |
| ⌘↩                | Run query (SQL) · start or stop a tail (logs) |
| ⇧⌘W               | Toggle write mode (SQL)                       |
| ⌘] / ⌘[           | Next / previous page (table browser)          |
| ⌘F                | Focus search or filter                        |
| ↩ / ⌫ / ⌘↑        | Open folder / go to parent folder (R2)        |
| ⌘.                | Pause or resume logs                          |
| ⇧⌘K               | Clear logs                                    |
| ⇧⌘D               | Toggle dark mode                              |

## Deployment

This is a desktop app, so deployment means cutting a release.

1. Bump `version` in `Cargo.toml` and commit.
2. Push a matching tag:

   ```sh
   git tag v0.2.0 && git push origin v0.2.0
   ```

`.github/workflows/release.yml` checks the tag matches `Cargo.toml`, runs the tests, builds
`aarch64` and `x86_64` on separate runners, joins them with `lipo`, and wraps the result with
`script/bundle` into an ad-hoc-signed `Cloudflare GUI.app` plus a zip and `.sha256`. It then
publishes a GitHub release with generated notes. To rebuild an existing tag, run the workflow
manually with that tag.

To build the same bundle locally, for your own machine's architecture only (the zip is still
named `macos-universal`):

```sh
cargo build --release
script/bundle target/release/cloudflare-gui 0.1.0 dist
```

## Contributing

Issues and pull requests are welcome.

```sh
./script/check    # cargo fmt --check, clippy (with and without devtools), cargo test
```

CI runs the same script on `macos-latest`. Before opening a PR:

- Run `./script/check`.
- Keep the app read-only by default. Anything that can write goes through `sql::classify`,
  write mode and a confirmation dialog, and is never retried.
- Don't put real account IDs, emails or tokens in `tests/fixtures/`, logs or screenshots. Take
  screenshots in demo mode.
- Check new endpoints against the [Cloudflare OpenAPI schemas](https://github.com/cloudflare/api-schemas).

[ARCHITECTURE.md](ARCHITECTURE.md) covers the module layout and data flow. `CLAUDE.md` has the
pinned GPUI versions and how to upgrade them, the scripted UI runner, and the signing setup that
stops repeat Keychain prompts during development.

## License

[MIT](LICENSE)
