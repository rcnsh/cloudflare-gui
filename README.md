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

Starting a live tail creates a short-lived tail session on the Worker. The app
deletes it when you close the tab, just as `wrangler tail` does when you exit.

## Where things are stored

- **API token:** login Keychain, service `dev.cloudflare-gui.api-token`.
- **Settings and query history:** `~/Library/Application Support/cloudflare-gui/`.
  Nothing secret is written there.

## Development

```sh
./script/check   # cargo fmt --check, clippy -D warnings, tests
```

See `CLAUDE.md` for the module layout and the GPUI upgrade procedure.
