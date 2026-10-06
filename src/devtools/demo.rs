//! An offline demo account for screenshots and trying the app without a
//! Cloudflare login. `CFGUI_DEMO=1` serves a fake REST API on localhost with
//! invented data: D1 is real SQLite in memory, so any SQL you type runs.
//!
//! In demo mode the Keychain and the settings directory are never touched, so
//! a real login on the same machine is safe.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use crate::cloudflare::accounts::Account;
use crate::runtime;

const ACCOUNT_ID: &str = "0000000000000000000000000000demo";

pub fn active() -> bool {
    std::env::var_os("CFGUI_DEMO").is_some()
}

/// Starts the fake API once and returns its base URL and the demo account,
/// or `None` when demo mode is off.
pub fn start() -> Option<(Url, Account)> {
    if !active() {
        return None;
    }
    static BASE: OnceLock<Option<Url>> = OnceLock::new();
    let base = BASE
        .get_or_init(|| {
            let listener = runtime::block_on(TcpListener::bind("127.0.0.1:0"))
                .map_err(|e| log::error!("demo server: {e}"))
                .ok()?;
            let addr = listener.local_addr().ok()?;
            drop(runtime::spawn(serve(listener)));
            log::info!("demo API at http://{addr}");
            Url::parse(&format!("http://{addr}/client/v4/")).ok()
        })
        .clone()?;
    Some((
        base,
        Account {
            id: ACCOUNT_ID.into(),
            name: "Acme Inc".into(),
            kind: None,
        },
    ))
}

async fn serve(listener: TcpListener) {
    while let Ok((socket, _)) = listener.accept().await {
        tokio::spawn(handle(socket));
    }
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: value.to_string().into_bytes(),
        }
    }

    fn ok(result: Value) -> Self {
        Self::json(200, envelope(result, Value::Null))
    }

    fn ok_paged(result: Value, info: Value) -> Self {
        Self::json(200, envelope(result, info))
    }

    fn not_found(what: &str) -> Self {
        Self::json(
            404,
            json!({ "success": false, "result": null, "messages": [],
                    "errors": [{ "code": 10007, "message": format!("{what} not found") }] }),
        )
    }

    fn raw(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type,
            body,
        }
    }
}

fn envelope(result: Value, info: Value) -> Value {
    let mut out = json!({ "success": true, "errors": [], "messages": [], "result": result });
    if !info.is_null() {
        out["result_info"] = info;
    }
    out
}

async fn handle(mut socket: TcpStream) {
    let Some((method, target, body)) = read_request(&mut socket).await else {
        return;
    };
    let response = match Url::parse(&format!("http://demo{target}")) {
        Ok(url) => route(&method, &url, &body),
        Err(_) => Response::not_found("route"),
    };
    let reason = if response.status < 300 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(&response.body).await;
    let _ = socket.shutdown().await;
}

async fn read_request(socket: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < length {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some((method, target, body))
}

fn route(method: &str, url: &Url, body: &[u8]) -> Response {
    let segments: Vec<String> = url
        .path_segments()
        .map(|s| {
            s.map(|p| {
                percent_encoding::percent_decode_str(p)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .collect()
        })
        .unwrap_or_default();
    let query: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
    // Drop the `client/v4` prefix.
    let parts: Vec<&str> = segments.iter().skip(2).map(String::as_str).collect();
    let data = data();
    match (method, parts.as_slice()) {
        ("GET", ["user", "tokens", "verify"]) => {
            Response::ok(json!({ "id": "demo", "status": "active" }))
        }
        ("GET", ["accounts"]) => Response::ok_paged(
            json!([{ "id": ACCOUNT_ID, "name": "Acme Inc", "type": "standard" }]),
            json!({ "page": 1, "per_page": 50, "count": 1, "total_count": 1 }),
        ),
        ("GET", ["accounts", _, "d1", "database"]) => {
            let dbs: Vec<Value> = data
                .databases
                .iter()
                .map(|db| {
                    json!({ "uuid": db.id, "name": db.name, "version": "production",
                            "created_at": "2026-03-14T09:26:53.000Z" })
                })
                .collect();
            let n = dbs.len();
            Response::ok_paged(
                json!(dbs),
                json!({ "page": 1, "per_page": 1000, "count": n, "total_count": n }),
            )
        }
        ("POST", ["accounts", _, "d1", "database", id, "raw"]) => {
            let Some(db) = data.databases.iter().find(|db| db.id == *id) else {
                return Response::not_found("database");
            };
            let sql = serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|v| v["sql"].as_str().map(str::to_string))
                .unwrap_or_default();
            run_sql(&db.conn.lock().unwrap(), &sql)
        }
        ("GET", ["accounts", _, "storage", "kv", "namespaces"]) => {
            let list: Vec<Value> = data
                .namespaces
                .iter()
                .map(|ns| json!({ "id": ns.id, "title": ns.title, "supports_url_encoding": true }))
                .collect();
            let n = list.len();
            Response::ok_paged(
                json!(list),
                json!({ "page": 1, "per_page": 100, "count": n, "total_count": n }),
            )
        }
        ("GET", ["accounts", _, "storage", "kv", "namespaces", ns, rest @ ..]) => {
            let Some(ns) = data.namespaces.iter().find(|n| n.id == *ns) else {
                return Response::not_found("namespace");
            };
            kv_route(ns, rest, &query)
        }
        ("GET", ["accounts", _, "r2", "buckets"]) => {
            let buckets: Vec<Value> = data
                .buckets
                .iter()
                .map(|b| {
                    json!({ "name": b.name, "creation_date": "2026-02-01T10:00:00.000Z",
                            "location": "WNAM", "storage_class": "Standard" })
                })
                .collect();
            Response::ok_paged(
                json!({ "buckets": buckets }),
                json!({ "cursor": "", "per_page": 1000 }),
            )
        }
        ("GET", ["accounts", _, "r2", "buckets", bucket, "objects"]) => {
            match data.buckets.iter().find(|b| b.name == *bucket) {
                Some(bucket) => list_objects(bucket, &query),
                None => Response::not_found("bucket"),
            }
        }
        ("GET", ["accounts", _, "r2", "buckets", bucket, "objects", key]) => {
            match data
                .buckets
                .iter()
                .find(|b| b.name == *bucket)
                .and_then(|b| b.objects.iter().find(|o| o.key == *key))
            {
                Some(o) => Response::raw(o.content_type, o.body.clone()),
                None => Response::not_found("object"),
            }
        }
        ("GET", ["accounts", _, "workers", "scripts"]) => {
            let scripts: Vec<Value> = WORKERS
                .iter()
                .map(|name| {
                    json!({ "id": name, "created_on": "2026-01-10T00:00:00.000000Z",
                            "modified_on": "2026-09-30T00:00:00.000000Z" })
                })
                .collect();
            Response::ok(json!(scripts))
        }
        ("POST", ["accounts", _, "workers", "scripts", _, "tails"]) => {
            match super::fake_tail_server() {
                Some(url) => Response::ok(json!({ "id": "demo-tail", "url": url,
                                                  "expires_at": "2099-01-01T00:00:00Z" })),
                None => Response::not_found("tail"),
            }
        }
        ("DELETE", ["accounts", _, "workers", "scripts", _, "tails", _]) => {
            Response::ok(Value::Null)
        }
        _ => {
            log::warn!("demo API has no route for {method} {}", url.path());
            Response::not_found("route")
        }
    }
}

fn kv_route(ns: &Namespace, rest: &[&str], query: &BTreeMap<String, String>) -> Response {
    match rest {
        ["keys"] => {
            let prefix = query.get("prefix").map(String::as_str).unwrap_or("");
            let matching: Vec<&KvEntry> = ns
                .entries
                .iter()
                .filter(|e| e.key.starts_with(prefix))
                .collect();
            let (page, cursor) = paginate(&matching, query, "limit", 1000);
            let keys: Vec<Value> = page
                .iter()
                .map(|e| {
                    let mut k = json!({ "name": e.key });
                    if let Some(exp) = e.expiration {
                        k["expiration"] = json!(exp);
                    }
                    if !e.metadata.is_null() {
                        k["metadata"] = e.metadata.clone();
                    }
                    k
                })
                .collect();
            Response::ok_paged(
                json!(keys),
                json!({ "count": keys.len(), "cursor": cursor }),
            )
        }
        ["values", key] => match ns.entries.iter().find(|e| e.key == *key) {
            Some(e) => Response::raw("application/octet-stream", e.value.clone().into_bytes()),
            None => Response::not_found("key"),
        },
        ["metadata", key] => match ns.entries.iter().find(|e| e.key == *key) {
            Some(e) => Response::ok(e.metadata.clone()),
            None => Response::not_found("key"),
        },
        _ => Response::not_found("route"),
    }
}

/// Offset cursors: good enough to exercise paging in the UI.
fn paginate<'a, T>(
    items: &'a [T],
    query: &BTreeMap<String, String>,
    limit_param: &str,
    default_limit: usize,
) -> (&'a [T], String) {
    let start: usize = query
        .get("cursor")
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let limit: usize = query
        .get(limit_param)
        .and_then(|l| l.parse().ok())
        .unwrap_or(default_limit);
    let start = start.min(items.len());
    let end = (start + limit).min(items.len());
    let cursor = if end < items.len() {
        end.to_string()
    } else {
        String::new()
    };
    (&items[start..end], cursor)
}

fn list_objects(bucket: &Bucket, query: &BTreeMap<String, String>) -> Response {
    let prefix = query.get("prefix").map(String::as_str).unwrap_or("");
    let mut folders: Vec<String> = Vec::new();
    let mut objects: Vec<&Object> = Vec::new();
    for o in bucket.objects.iter().filter(|o| o.key.starts_with(prefix)) {
        match o.key[prefix.len()..].find('/') {
            Some(ix) => {
                let folder = o.key[..prefix.len() + ix + 1].to_string();
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
            None => objects.push(o),
        }
    }
    let (page, cursor) = paginate(&objects, query, "per_page", 1000);
    let items: Vec<Value> = page
        .iter()
        .map(|o| {
            json!({ "key": o.key, "size": o.body.len(), "last_modified": o.modified,
                    "etag": format!("{:032x}", fnv(&o.body)),
                    "http_metadata": { "contentType": o.content_type },
                    "custom_metadata": o.metadata, "storage_class": "Standard" })
        })
        .collect();
    Response::ok_paged(
        json!(items),
        json!({ "cursor": cursor, "delimited": folders, "is_truncated": !cursor.is_empty(),
                "per_page": 1000 }),
    )
}

/// Runs each statement against SQLite and answers the way D1's `/raw` does.
fn run_sql(conn: &Connection, sql: &str) -> Response {
    let mut results = Vec::new();
    for statement in crate::sql::classify(sql).statements {
        let started = Instant::now();
        match run_statement(conn, &statement.text) {
            Ok(out) => {
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                results.push(json!({
                    "results": { "columns": out.columns, "rows": out.rows },
                    "success": true,
                    "meta": { "served_by": "demo", "duration": ms,
                              "timings": { "sql_duration_ms": ms },
                              "changes": out.changes, "last_row_id": conn.last_insert_rowid(),
                              "changed_db": out.changes > 0, "rows_read": out.rows_read,
                              "rows_written": out.changes, "size_after": 1_253_376 }
                }));
            }
            Err(e) => {
                return Response::json(
                    400,
                    json!({ "result": [], "success": false, "messages": [],
                            "errors": [{ "code": 7500, "message": format!("{e}: SQLITE_ERROR") }] }),
                );
            }
        }
    }
    Response::ok(json!(results))
}

struct StatementOutput {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    rows_read: u64,
    changes: u64,
}

fn run_statement(conn: &Connection, sql: &str) -> rusqlite::Result<StatementOutput> {
    let mut stmt = conn.prepare(sql)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let before = conn.total_changes();
    let mut rows = Vec::new();
    {
        let mut cursor = stmt.raw_query();
        while let Some(row) = cursor.next()? {
            let mut out = Vec::with_capacity(columns.len());
            for ix in 0..columns.len() {
                out.push(match row.get_ref(ix)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(i) => json!(i),
                    ValueRef::Real(f) => json!(f),
                    ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    ValueRef::Blob(b) => json!(b),
                });
            }
            rows.push(out);
        }
    }
    // Like D1, count rows scanned, not just rows returned.
    let scanned = stmt.get_status(rusqlite::StatementStatus::FullscanStep) as u64;
    let changes = conn.total_changes().saturating_sub(before);
    Ok(StatementOutput {
        rows_read: scanned.max(rows.len() as u64),
        columns,
        rows,
        changes,
    })
}

// --- The invented data -------------------------------------------------------

struct Database {
    id: &'static str,
    name: &'static str,
    conn: Mutex<Connection>,
}

struct KvEntry {
    key: String,
    value: String,
    expiration: Option<i64>,
    metadata: Value,
}

struct Namespace {
    id: &'static str,
    title: &'static str,
    entries: Vec<KvEntry>,
}

struct Object {
    key: String,
    body: Vec<u8>,
    content_type: &'static str,
    modified: String,
    metadata: Value,
}

struct Bucket {
    name: &'static str,
    objects: Vec<Object>,
}

struct Data {
    databases: Vec<Database>,
    namespaces: Vec<Namespace>,
    buckets: Vec<Bucket>,
}

const WORKERS: &[&str] = &[
    "api-gateway",
    "cron-reports",
    "image-resizer",
    "storefront",
    "webhooks",
];

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| Data {
        databases: vec![
            database(
                "6f1d2c3b-0000-4000-8000-00000000d001",
                "shop",
                SHOP_SCHEMA,
                seed_shop,
            ),
            database(
                "6f1d2c3b-0000-4000-8000-00000000d002",
                "analytics",
                ANALYTICS_SCHEMA,
                seed_analytics,
            ),
            database(
                "6f1d2c3b-0000-4000-8000-00000000d003",
                "auth",
                AUTH_SCHEMA,
                seed_auth,
            ),
        ],
        namespaces: namespaces(),
        buckets: buckets(),
    })
}

/// A small deterministic generator, so every demo run shows the same data.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

fn fnv(bytes: &[u8]) -> u128 {
    bytes
        .iter()
        .fold(0x6c62_272e_07bb_0142_62b8_2175_6295_c58d, |h, b| {
            (h ^ *b as u128).wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b)
        })
}

const FIRST: &[&str] = &[
    "Ada",
    "Grace",
    "Alan",
    "Margaret",
    "Linus",
    "Barbara",
    "Dennis",
    "Frances",
    "Ken",
    "Radia",
    "Edsger",
    "Hedy",
    "Donald",
    "Katherine",
    "Tim",
    "Sophie",
    "Guido",
    "Annie",
    "Bjarne",
    "Mary",
];
const LAST: &[&str] = &[
    "Lovelace",
    "Hopper",
    "Turing",
    "Hamilton",
    "Torvalds",
    "Liskov",
    "Ritchie",
    "Allen",
    "Thompson",
    "Perlman",
    "Dijkstra",
    "Lamarr",
    "Knuth",
    "Johnson",
    "Berners-Lee",
    "Wilson",
    "van Rossum",
    "Easley",
    "Stroustrup",
    "Keller",
];
const CITIES: &[&str] = &[
    "London",
    "Lisbon",
    "Berlin",
    "Toronto",
    "Austin",
    "Sydney",
    "Singapore",
    "Tokyo",
    "Oslo",
    "Dublin",
];
const PRODUCTS: &[(&str, &str, i64)] = &[
    ("Porcelain teacup", "kitchen", 1800),
    ("Walnut cutting board", "kitchen", 4500),
    ("Linen apron", "kitchen", 3200),
    ("Ceramic pour-over", "coffee", 2900),
    ("Burr grinder", "coffee", 12900),
    ("Gooseneck kettle", "coffee", 7900),
    ("Wool throw", "home", 8900),
    ("Brass desk lamp", "home", 11900),
    ("Oak bookshelf", "home", 34900),
    ("Dotted notebook", "stationery", 1400),
    ("Fountain pen", "stationery", 6500),
    ("Desk organiser", "stationery", 2400),
];

const SHOP_SCHEMA: &str = "
CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL UNIQUE,
  city TEXT, created_at TEXT NOT NULL);
CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT NOT NULL, category TEXT NOT NULL,
  price_cents INTEGER NOT NULL, stock INTEGER NOT NULL);
CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customers(id),
  status TEXT NOT NULL, total_cents INTEGER NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE order_items (order_id INTEGER NOT NULL REFERENCES orders(id),
  product_id INTEGER NOT NULL REFERENCES products(id), quantity INTEGER NOT NULL);
CREATE VIEW revenue_by_category AS
  SELECT p.category, SUM(oi.quantity * p.price_cents) / 100.0 AS revenue
  FROM order_items oi JOIN products p ON p.id = oi.product_id GROUP BY p.category;
";

const ANALYTICS_SCHEMA: &str = "
CREATE TABLE page_views (id INTEGER PRIMARY KEY, path TEXT NOT NULL, country TEXT NOT NULL,
  duration_ms INTEGER NOT NULL, viewed_at TEXT NOT NULL);
CREATE TABLE events (id INTEGER PRIMARY KEY, name TEXT NOT NULL, properties TEXT, at TEXT NOT NULL);
";

const AUTH_SCHEMA: &str = "
CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, role TEXT NOT NULL,
  last_login TEXT);
CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id),
  expires_at TEXT NOT NULL);
";

fn database(id: &'static str, name: &'static str, schema: &str, seed: fn(&Connection)) -> Database {
    let conn = Connection::open_in_memory().expect("in-memory SQLite opens");
    conn.execute_batch(schema).expect("demo schema is valid");
    seed(&conn);
    Database {
        id,
        name,
        conn: Mutex::new(conn),
    }
}

fn date(rng: &mut Rng) -> String {
    format!(
        "2026-{:02}-{:02} {:02}:{:02}:{:02}",
        1 + rng.below(9),
        1 + rng.below(28),
        rng.below(24),
        rng.below(60),
        rng.below(60)
    )
}

/// A name and an email address. `n` keeps the address unique.
fn person(rng: &mut Rng, n: u64) -> (String, String) {
    let (first, last) = (*rng.pick(FIRST), *rng.pick(LAST));
    let email = format!(
        "{}.{}{n}@example.com",
        first.to_lowercase(),
        last.to_lowercase().replace([' ', '-'], ""),
    );
    (format!("{first} {last}"), email)
}

fn seed_shop(conn: &Connection) {
    let mut rng = Rng(0x5eed_0001);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 1..=480 {
        let (name, email) = person(&mut rng, id);
        let city = *rng.pick(CITIES);
        let created = date(&mut rng);
        tx.execute(
            "INSERT INTO customers VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id as i64, name, email, city, created],
        )
        .unwrap();
    }
    for (ix, (name, category, price)) in PRODUCTS.iter().enumerate() {
        tx.execute(
            "INSERT INTO products VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![ix as i64 + 1, name, category, price, rng.below(250) as i64],
        )
        .unwrap();
    }
    let statuses = [
        "paid", "paid", "paid", "shipped", "shipped", "refunded", "pending",
    ];
    for id in 1..=2400 {
        let items: Vec<(i64, i64)> = (0..1 + rng.below(3))
            .map(|_| {
                let product = 1 + rng.below(PRODUCTS.len() as u64) as i64;
                (product, 1 + rng.below(3) as i64)
            })
            .collect();
        let total: i64 = items
            .iter()
            .map(|(p, q)| PRODUCTS[*p as usize - 1].2 * q)
            .sum();
        let (status, created) = (*rng.pick(&statuses), date(&mut rng));
        tx.execute(
            "INSERT INTO orders VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, 1 + rng.below(480) as i64, status, total, created],
        )
        .unwrap();
        for (product, quantity) in items {
            tx.execute(
                "INSERT INTO order_items VALUES (?1, ?2, ?3)",
                rusqlite::params![id, product, quantity],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn seed_analytics(conn: &Connection) {
    let mut rng = Rng(0x5eed_0002);
    let tx = conn.unchecked_transaction().unwrap();
    let paths = [
        "/",
        "/shop",
        "/shop/kitchen",
        "/shop/coffee",
        "/cart",
        "/checkout",
        "/about",
    ];
    let countries = ["GB", "US", "DE", "PT", "CA", "AU", "SG", "JP"];
    for id in 1..=5000 {
        let (path, country, at) = (*rng.pick(&paths), *rng.pick(&countries), date(&mut rng));
        tx.execute(
            "INSERT INTO page_views VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, path, country, 200 + rng.below(9000) as i64, at],
        )
        .unwrap();
    }
    let names = [
        "signup",
        "add_to_cart",
        "checkout_started",
        "purchase",
        "newsletter",
    ];
    for id in 1..=1500 {
        let (name, at) = (*rng.pick(&names), date(&mut rng));
        let props = json!({ "source": rng.pick(&["ads", "organic", "email"]),
                            "variant": rng.pick(&["a", "b"]) });
        tx.execute(
            "INSERT INTO events VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, name, props.to_string(), at],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn seed_auth(conn: &Connection) {
    let mut rng = Rng(0x5eed_0003);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 1..=120 {
        let (_, email) = person(&mut rng, id);
        let role = *rng.pick(&["member", "member", "member", "admin"]);
        let login = date(&mut rng);
        tx.execute(
            "INSERT INTO users VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id as i64, email, role, login],
        )
        .unwrap();
    }
    for _ in 0..200 {
        let id = format!("sess_{:016x}", rng.next());
        tx.execute(
            "INSERT INTO sessions VALUES (?1, ?2, ?3)",
            rusqlite::params![id, 1 + rng.below(120) as i64, date(&mut rng)],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn namespaces() -> Vec<Namespace> {
    let now = chrono::Utc::now().timestamp();
    let mut rng = Rng(0x5eed_0004);
    let mut sessions = Vec::new();
    for n in 0..340 {
        let id = format!("{:016x}", rng.next());
        let (name, email) = person(&mut rng, n);
        sessions.push(KvEntry {
            key: format!("session:{id}"),
            value: json!({ "user": { "name": name, "email": email },
                           "cart": [rng.below(12) + 1, rng.below(12) + 1],
                           "csrf": format!("{:016x}", rng.next()) })
            .to_string(),
            expiration: Some(now + 3600 + rng.below(86_400 * 7) as i64),
            metadata: json!({ "country": rng.pick(&["GB", "US", "DE", "JP"]) }),
        });
    }
    sessions.sort_by(|a, b| a.key.cmp(&b.key));
    let flags = [
        (
            "checkout:new-flow",
            json!({ "enabled": true, "rollout": 0.25, "segments": ["beta", "staff"] }),
        ),
        (
            "checkout:apple-pay",
            json!({ "enabled": true, "rollout": 1.0 }),
        ),
        (
            "search:semantic",
            json!({ "enabled": false, "owner": "search-team" }),
        ),
        (
            "shop:holiday-banner",
            json!({ "enabled": true, "starts": "2026-12-01", "ends": "2026-12-27" }),
        ),
        (
            "shop:free-shipping",
            json!({ "currency": "GBP", "threshold": 50 }),
        ),
    ];
    let cache: Vec<KvEntry> = PRODUCTS
        .iter()
        .enumerate()
        .map(|(ix, (name, category, price))| KvEntry {
            key: format!("product:{}", ix + 1),
            value: json!({ "id": ix + 1, "name": name, "category": category,
                           "price": { "amount": price, "currency": "GBP" },
                           "images": [format!("products/{}.png", slug(name))] })
            .to_string(),
            expiration: Some(now + 600 + 60 * ix as i64),
            metadata: Value::Null,
        })
        .collect();
    vec![
        Namespace {
            id: "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d1",
            title: "CACHE",
            entries: cache,
        },
        Namespace {
            id: "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d2",
            title: "FEATURE_FLAGS",
            entries: flags
                .into_iter()
                .map(|(k, v)| KvEntry {
                    key: k.into(),
                    value: v.to_string(),
                    expiration: None,
                    metadata: json!({ "updated_by": "deploy-bot" }),
                })
                .collect(),
        },
        Namespace {
            id: "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d3",
            title: "SESSIONS",
            entries: sessions,
        },
    ]
}

fn slug(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

/// A soft studio-style shot of a coloured sphere, so image previews have
/// something to show.
fn product_image(seed: u64, w: u32, h: u32) -> Vec<u8> {
    let palette = [
        [0xf3, 0x80, 0x20],
        [0x2c, 0x7c, 0xf6],
        [0x16, 0xa3, 0x4a],
        [0xa8, 0x55, 0xf7],
        [0xe1, 0x1d, 0x48],
        [0x0e, 0x91, 0x9e],
    ];
    let base = palette[seed as usize % palette.len()];
    let (cx, cy, r) = (w as f32 * 0.5, h as f32 * 0.48, h as f32 * 0.3);
    let img =
        image::RgbImage::from_fn(w, h, |x, y| {
            let (x, y) = (x as f32, y as f32);
            let t = y / h as f32;
            let bg = [245.0 - 22.0 * t, 243.0 - 22.0 * t, 240.0 - 18.0 * t];
            // A soft shadow under the sphere.
            let sx = (x - cx) / (r * 1.1);
            let sy = (y - (cy + r * 1.02)) / (r * 0.18);
            let shadow = (1.0 - (sx * sx + sy * sy)).clamp(0.0, 1.0) * 0.22;
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            if d < r {
                // Light from the top left.
                let (nx, ny) = ((x - cx) / r, (y - cy) / r);
                let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
                let light = (-0.45 * nx - 0.55 * ny + 0.7 * nz).max(0.0);
                let spec = light.powi(24) * 0.6;
                image::Rgb(base.map(|c| {
                    (c as f32 * (0.35 + 0.75 * light) + 255.0 * spec).clamp(0.0, 255.0) as u8
                }))
            } else {
                image::Rgb(bg.map(|c| (c * (1.0 - shadow)) as u8))
            }
        });
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .expect("PNG encodes");
    out.into_inner()
}

fn buckets() -> Vec<Bucket> {
    let mut rng = Rng(0x5eed_0005);
    let mut images = Vec::new();
    for (ix, (name, _, _)) in PRODUCTS.iter().enumerate() {
        let slug = slug(name);
        images.push(Object {
            key: format!("products/{slug}.png"),
            body: product_image(ix as u64, 960, 720),
            content_type: "image/png",
            modified: format!("2026-09-{:02}T10:{:02}:00.000Z", 1 + ix, rng.below(60)),
            metadata: json!({ "uploaded-by": "cms", "alt": name }),
        });
        images.push(Object {
            key: format!("thumbnails/{slug}.png"),
            body: product_image(ix as u64, 240, 180),
            content_type: "image/png",
            modified: format!("2026-09-{:02}T10:{:02}:30.000Z", 1 + ix, rng.below(60)),
            metadata: json!({}),
        });
    }
    images.push(Object {
        key: "manifest.json".into(),
        body: serde_json::to_vec_pretty(&json!({ "version": 3, "images": PRODUCTS.len(),
                                                 "generated": "2026-09-30T08:00:00Z" }))
        .unwrap(),
        content_type: "application/json",
        modified: "2026-09-30T08:00:00.000Z".into(),
        metadata: json!({}),
    });
    let mut exports = Vec::new();
    for month in 1..=9 {
        let mut csv = String::from("order_id,customer_id,total_gbp,status\n");
        for _ in 0..200 {
            csv.push_str(&format!(
                "{},{},{:.2},{}\n",
                rng.below(2400) + 1,
                rng.below(480) + 1,
                rng.below(40_000) as f64 / 100.0,
                rng.pick(&["paid", "shipped", "refunded"])
            ));
        }
        exports.push(Object {
            key: format!("orders/2026-{month:02}.csv"),
            body: csv.into_bytes(),
            content_type: "text/csv",
            modified: format!("2026-{:02}-01T02:00:00.000Z", month + 1),
            metadata: json!({ "job": "monthly-export" }),
        });
    }
    exports.push(Object {
        key: "README.txt".into(),
        body: b"Monthly order exports, written by the cron-reports Worker.\n".to_vec(),
        content_type: "text/plain",
        modified: "2026-01-02T00:00:00.000Z".into(),
        metadata: json!({}),
    });
    vec![
        Bucket {
            name: "exports",
            objects: exports,
        },
        Bucket {
            name: "product-images",
            objects: images,
        },
    ]
}
