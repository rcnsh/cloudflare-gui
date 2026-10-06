# Cloudflare GUI

A fast, keyboard-driven native macOS console for Cloudflare's data products:
browse D1 tables and run SQL, explore KV namespaces and R2 buckets, and stream
live Worker logs. Built in Rust with [GPUI](https://gpui.rs) and
[gpui-component](https://gpui-kit.com).

It talks to the Cloudflare REST API directly with a single scoped API token
kept in your macOS login Keychain. It's **read-only by default**: anything that
could change data requires turning on write mode for that tab and confirming
the exact statement.

## Running

```sh
cargo run --release
```

On first launch, paste an API token, then pick an account.

## What it does

- **D1:** browse tables page by page and run SQL in an editor with
  highlighting. cmd-Enter runs the selection or the whole buffer. Row counts,
  timings and errors show inline, and each database keeps its query history.
- **KV:** search keys by prefix (paged with the API cursor as you scroll) and
  view values. JSON is pretty-printed, and metadata and expiry are shown.
- **R2:** browse buckets folder by folder, with sizes and modified times, and
  preview text, JSON and images. Previews read at most 8 MB of an object.
- **Worker logs:** press Start to stream a Worker's logs live. Filter by level
  or text, pause and clear, and click a line to see the full event.
- **Navigation:** every resource opens in its own tab. cmd-K jumps to any
  database, table, namespace, bucket or Worker.

## Keyboard

| Keys            | Action                                       |
| --------------- | -------------------------------------------- |
| cmd-K / cmd-P   | Command palette                              |
| cmd-R           | Refresh the resource lists                   |
| cmd-B           | Toggle the sidebar                           |
| cmd-T           | New SQL tab                                  |
| cmd-W           | Close tab                                    |
| cmd-Enter       | Run query (SQL tab), start/stop tail (logs)  |
| cmd-shift-W     | Toggle write mode (SQL tab)                  |
| cmd-] / cmd-[   | Next / previous page (table browser)         |
| cmd-F           | Focus the search or filter box               |
| Enter / ⌫ / cmd-↑ | Open folder, go to parent folder (R2)      |
| cmd-.           | Pause / resume logs                          |
| cmd-shift-K     | Clear logs                                   |
| cmd-shift-D     | Toggle dark mode                             |

## API token

Create a token at **dash.cloudflare.com → My Profile → API Tokens → Create
Token → Create Custom Token**. Scope it to the one account you want to browse.

### Minimum permissions (read-only use)

| Scope   | Permission             | Access | Used for                               |
| ------- | ---------------------- | ------ | -------------------------------------- |
| Account | Account Settings       | Read   | Listing accounts for the account picker |
| Account | D1                     | Read   | Listing databases, running read queries |
| Account | Workers KV Storage     | Read   | Namespaces, keys, values and metadata  |
| Account | Workers R2 Storage     | Read   | Buckets, object listings and previews  |
| Account | Workers Scripts        | Read   | Listing Workers for the sidebar        |
| Account | Workers Tail           | Read   | Live Worker logs                       |

R2 objects are listed and read through the REST API with this same token.
You don't need S3-style R2 access keys.

### Additional permissions for write mode (optional)

Only add these if you want to run mutating SQL. The app still asks for
confirmation every time.

| Scope   | Permission | Access |
| ------- | ---------- | ------ |
| Account | D1         | Edit   |

Starting a live tail creates a short-lived tail session on the Worker. It only
happens when you press Start. The app deletes the session when you stop, close
the tab or quit, the same way `wrangler tail` does when you exit.

## Where things are stored

- **API token:** login Keychain, service `dev.cloudflare-gui.api-token`.
- **Settings and query history:** `~/Library/Application Support/cloudflare-gui/`.
  Nothing secret is written there.

## Development

```sh
./script/check   # cargo fmt --check, clippy -D warnings, tests
```

See `CLAUDE.md` for the module layout and the GPUI upgrade procedure.
