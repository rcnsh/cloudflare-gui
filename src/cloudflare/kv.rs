use serde::Deserialize;
use serde_json::Value;

use super::{ApiError, Client, Page, Request, Result};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Namespace {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Key {
    pub name: String,
    /// Seconds since the Unix epoch.
    pub expiration: Option<i64>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KvValue {
    pub bytes: Vec<u8>,
    pub metadata: Option<Value>,
    /// Seconds since the Unix epoch, from the key listing when available.
    pub expiration: Option<i64>,
}

/// The API's maximum, so a prefix search needs as few round trips as possible.
pub const KEY_PAGE_SIZE: u32 = 1000;

impl Client {
    pub async fn list_kv_namespaces(&self, account_id: &str) -> Result<Vec<Namespace>> {
        let mut out = Vec::new();
        for page in 1.. {
            let (items, info): (Vec<Namespace>, _) = self
                .json(
                    Request::get(["accounts", account_id, "storage", "kv", "namespaces"])
                        .query("page", page)
                        .query("per_page", 100),
                )
                .await?;
            let fetched = items.len();
            out.extend(items);
            let total = info.and_then(|i| i.total_count).unwrap_or(0.0) as usize;
            if fetched == 0 || out.len() >= total {
                break;
            }
        }
        out.sort_by_key(|n| n.title.to_lowercase());
        Ok(out)
    }

    pub async fn list_kv_keys(
        &self,
        account_id: &str,
        namespace_id: &str,
        prefix: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<Page<Key>> {
        let (items, info): (Vec<Key>, _) = self
            .json(
                Request::get([
                    "accounts",
                    account_id,
                    "storage",
                    "kv",
                    "namespaces",
                    namespace_id,
                    "keys",
                ])
                .query("limit", KEY_PAGE_SIZE)
                .query_opt("prefix", prefix.filter(|p| !p.is_empty()))
                .query_opt("cursor", cursor),
            )
            .await?;
        Ok(Page {
            items,
            cursor: info.and_then(|i| i.next_cursor()),
        })
    }

    /// Fetches the value and its metadata. `expiration` isn't returned by either
    /// endpoint, so callers pass through what the key listing said.
    pub async fn get_kv_value(
        &self,
        account_id: &str,
        namespace_id: &str,
        key: &str,
        expiration: Option<i64>,
    ) -> Result<KvValue> {
        let (bytes, _) = self
            .bytes(Request::get([
                "accounts",
                account_id,
                "storage",
                "kv",
                "namespaces",
                namespace_id,
                "values",
                key,
            ]))
            .await?;
        let metadata = match self
            .json::<Value>(Request::get([
                "accounts",
                account_id,
                "storage",
                "kv",
                "namespaces",
                namespace_id,
                "metadata",
                key,
            ]))
            .await
        {
            Ok((Value::Null, _)) => None,
            Ok((value, _)) => Some(value),
            // Keys without metadata can answer with an error envelope rather than null.
            Err(ApiError::Api { status, .. }) if status.is_client_error() => None,
            Err(ApiError::Decode(_)) => None,
            Err(err) => return Err(err),
        };
        Ok(KvValue {
            bytes,
            metadata,
            expiration,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::cloudflare::testing::{MockServer, Reply};

    #[tokio::test]
    async fn lists_namespaces_sorted() {
        let server = MockServer::start(vec![Reply::fixture(200, "kv_namespaces.json")]).await;
        let namespaces = server.client().list_kv_namespaces("acc").await.unwrap();
        let titles: Vec<_> = namespaces.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["CACHE", "sessions"]);
    }

    #[tokio::test]
    async fn key_pages_carry_cursor_and_prefix() {
        let server = MockServer::start(vec![
            Reply::fixture(200, "kv_keys_page1.json"),
            Reply::fixture(200, "kv_keys_page2.json"),
        ])
        .await;
        let client = server.client();
        let first = client
            .list_kv_keys("acc", "ns", Some("user:"), None)
            .await
            .unwrap();
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.items[0].expiration, Some(1_893_456_000));
        assert_eq!(
            first.items[1].metadata,
            Some(serde_json::json!({ "role": "admin" }))
        );
        let cursor = first.cursor.clone().expect("first page has a cursor");
        let second = client
            .list_kv_keys("acc", "ns", Some("user:"), Some(&cursor))
            .await
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.cursor, None);
        let seen = server.requests();
        assert_eq!(
            seen[0].target,
            "/client/v4/accounts/acc/storage/kv/namespaces/ns/keys?limit=1000&prefix=user%3A"
        );
        assert!(seen[1].target.ends_with(&format!("&cursor={cursor}")));
    }

    #[tokio::test]
    async fn value_fetch_encodes_slashes_and_reads_metadata() {
        let server = MockServer::start(vec![
            Reply::status(200).body(r#"{"theme":"dark","count":3}"#),
            Reply::fixture(200, "kv_metadata.json"),
        ])
        .await;
        let value = server
            .client()
            .get_kv_value("acc", "ns", "config/user 1", Some(10))
            .await
            .unwrap();
        assert_eq!(value.bytes, br#"{"theme":"dark","count":3}"#);
        assert_eq!(value.metadata, Some(serde_json::json!({ "version": 2 })));
        assert_eq!(value.expiration, Some(10));
        let seen = server.requests();
        assert_eq!(
            seen[0].target,
            "/client/v4/accounts/acc/storage/kv/namespaces/ns/values/config%2Fuser%201"
        );
        assert_eq!(
            seen[1].target,
            "/client/v4/accounts/acc/storage/kv/namespaces/ns/metadata/config%2Fuser%201"
        );
    }

    #[tokio::test]
    async fn missing_metadata_is_none() {
        let server = MockServer::start(vec![
            Reply::status(200).body("plain"),
            Reply::status(200).body(r#"{"success":true,"errors":[],"messages":[],"result":null}"#),
        ])
        .await;
        let value = server
            .client()
            .get_kv_value("acc", "ns", "k", None)
            .await
            .unwrap();
        assert_eq!(value.metadata, None);
    }
}
