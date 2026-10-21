//! R2 through the REST API. Listing and reading objects work with the same
//! API token as everything else, so no S3 access keys are needed.

use std::collections::HashMap;

use serde::Deserialize;

use super::{ApiError, Client, Page, Request, Result};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Bucket {
    pub name: String,
    pub creation_date: Option<String>,
    pub location: Option<String>,
    /// `default`, `eu`, `fedramp`... Non-default buckets need a header on every call.
    pub jurisdiction: Option<String>,
    pub storage_class: Option<String>,
}

impl Bucket {
    fn jurisdiction_header(&self) -> Option<&str> {
        self.jurisdiction.as_deref().filter(|j| *j != "default")
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct HttpMetadata {
    #[serde(rename = "contentType")]
    pub content_type: Option<String>,
    #[serde(rename = "cacheControl")]
    pub cache_control: Option<String>,
    #[serde(rename = "contentEncoding")]
    pub content_encoding: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Object {
    pub key: String,
    #[serde(default)]
    pub size: u64,
    pub last_modified: Option<String>,
    pub etag: Option<String>,
    #[serde(default)]
    pub http_metadata: HttpMetadata,
    #[serde(default)]
    pub custom_metadata: HashMap<String, String>,
    pub storage_class: Option<String>,
}

/// A directory-style listing: objects directly under the prefix plus the
/// "folders" (common prefixes) below it.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub page: Page<Object>,
    pub prefixes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BucketList {
    #[serde(default)]
    buckets: Vec<Bucket>,
}

pub const OBJECT_PAGE_SIZE: u32 = 1000;

impl Client {
    pub async fn list_r2_buckets(&self, account_id: &str) -> Result<Vec<Bucket>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let (list, info): (BucketList, _) = self
                .json(
                    Request::get(["accounts", account_id, "r2", "buckets"])
                        .query("per_page", 1000)
                        .query_opt("cursor", cursor.as_deref()),
                )
                .await?;
            let fetched = list.buckets.len();
            out.extend(list.buckets);
            cursor = info.and_then(|i| i.next_cursor());
            if cursor.is_none() || fetched == 0 {
                break;
            }
        }
        Ok(out)
    }

    pub async fn list_r2_objects(
        &self,
        account_id: &str,
        bucket: &Bucket,
        prefix: &str,
        cursor: Option<&str>,
    ) -> Result<Listing> {
        let mut request = Request::get([
            "accounts",
            account_id,
            "r2",
            "buckets",
            &bucket.name,
            "objects",
        ])
        .query("per_page", OBJECT_PAGE_SIZE)
        .query("delimiter", "/")
        .query_opt("prefix", Some(prefix).filter(|p| !p.is_empty()))
        .query_opt("cursor", cursor);
        if let Some(j) = bucket.jurisdiction_header() {
            request = request.header("cf-r2-jurisdiction", j);
        }
        let (items, info): (Vec<Object>, _) = self.json(request).await?;
        let info = info.unwrap_or_default();
        let truncated = info.is_truncated.unwrap_or(false);
        Ok(Listing {
            page: Page {
                items,
                cursor: info.next_cursor().filter(|_| truncated),
            },
            prefixes: info.delimited,
        })
    }

    /// Downloads at most `limit` bytes of an object. The REST endpoint has no
    /// range support, so the body is read in chunks and the connection dropped
    /// once the limit is reached; a huge object never downloads in full.
    pub async fn get_r2_object(
        &self,
        account_id: &str,
        bucket: &Bucket,
        key: &str,
        limit: usize,
    ) -> Result<ObjectBody> {
        let mut request = Request::get([
            "accounts",
            account_id,
            "r2",
            "buckets",
            &bucket.name,
            "objects",
            key,
        ]);
        if let Some(j) = bucket.jurisdiction_header() {
            request = request.header("cf-r2-jurisdiction", j);
        }
        let mut response = self.send(request).await?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mut bytes = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ApiError::Network(e.without_url().to_string()))?
        {
            let room = limit - bytes.len();
            if chunk.len() >= room {
                bytes.extend_from_slice(&chunk[..room]);
                // Anything left over, even an empty final read, means more
                // may follow; only an exact fit at the end is complete.
                truncated = chunk.len() > room || response.chunk().await.ok().flatten().is_some();
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(ObjectBody {
            bytes,
            content_type,
            truncated,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ObjectBody {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    /// The object is bigger than the limit and `bytes` is only the start.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::testing::{MockServer, Reply};

    #[tokio::test]
    async fn lists_buckets() {
        let server = MockServer::start(vec![Reply::fixture(200, "r2_buckets.json")]).await;
        let buckets = server.client().list_r2_buckets("acc").await.unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[1].jurisdiction.as_deref(), Some("eu"));
    }

    #[tokio::test]
    async fn lists_objects_with_prefixes_and_cursor() {
        let server = MockServer::start(vec![Reply::fixture(200, "r2_objects.json")]).await;
        let bucket = Bucket {
            name: "assets".into(),
            creation_date: None,
            location: None,
            jurisdiction: Some("eu".into()),
            storage_class: None,
        };
        let listing = server
            .client()
            .list_r2_objects("acc", &bucket, "images/", None)
            .await
            .unwrap();
        assert_eq!(listing.prefixes, ["images/thumbs/"]);
        assert_eq!(listing.page.items.len(), 2);
        assert_eq!(listing.page.items[0].size, 48213);
        assert_eq!(
            listing.page.items[0].http_metadata.content_type.as_deref(),
            Some("image/png")
        );
        assert_eq!(listing.page.cursor.as_deref(), Some("next-page-token"));
        assert_eq!(
            server.requests()[0].target,
            "/client/v4/accounts/acc/r2/buckets/assets/objects?per_page=1000&delimiter=%2F&prefix=images%2F"
        );
    }

    #[tokio::test]
    async fn object_keys_are_one_path_segment() {
        let server = MockServer::start(vec![Reply::status(200).body("hello")]).await;
        let bucket = Bucket {
            name: "b".into(),
            creation_date: None,
            location: None,
            jurisdiction: None,
            storage_class: None,
        };
        let body = server
            .client()
            .get_r2_object("acc", &bucket, "docs/read me.txt", 1024)
            .await
            .unwrap();
        assert_eq!(body.bytes, b"hello");
        assert!(!body.truncated);
        assert_eq!(
            server.requests()[0].target,
            "/client/v4/accounts/acc/r2/buckets/b/objects/docs%2Fread%20me.txt"
        );
    }

    #[tokio::test]
    async fn object_downloads_stop_at_the_limit() {
        let bucket = Bucket {
            name: "b".into(),
            creation_date: None,
            location: None,
            jurisdiction: None,
            storage_class: None,
        };
        let body = "x".repeat(10_000);
        let server = MockServer::start(vec![
            Reply::status(200).body(body.as_str()),
            Reply::status(200).body(body.as_str()),
        ])
        .await;
        let client = server.client();
        let cut = client
            .get_r2_object("acc", &bucket, "k", 100)
            .await
            .unwrap();
        assert_eq!(cut.bytes.len(), 100);
        assert!(cut.truncated);
        let exact = client
            .get_r2_object("acc", &bucket, "k", 10_000)
            .await
            .unwrap();
        assert_eq!(exact.bytes.len(), 10_000);
        assert!(!exact.truncated);
    }

    fn bucket(name: &str, jurisdiction: Option<&str>) -> Bucket {
        Bucket {
            name: name.into(),
            creation_date: None,
            location: None,
            jurisdiction: jurisdiction.map(Into::into),
            storage_class: None,
        }
    }

    fn envelope(result: serde_json::Value, info: serde_json::Value) -> Reply {
        Reply::status(200).body(
            serde_json::json!({ "success": true, "errors": [], "result": result, "result_info": info })
                .to_string(),
        )
    }

    #[tokio::test]
    async fn bucket_lists_follow_the_cursor() {
        let server = MockServer::start(vec![
            envelope(
                serde_json::json!({ "buckets": [{ "name": "a" }] }),
                serde_json::json!({ "cursor": "page-2" }),
            ),
            envelope(
                serde_json::json!({ "buckets": [{ "name": "b" }] }),
                serde_json::json!({ "cursor": "" }),
            ),
        ])
        .await;
        let buckets = server.client().list_r2_buckets("acc").await.unwrap();
        let names: Vec<_> = buckets.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        let seen = server.requests();
        assert_eq!(
            seen[0].target,
            "/client/v4/accounts/acc/r2/buckets?per_page=1000"
        );
        assert_eq!(
            seen[1].target,
            "/client/v4/accounts/acc/r2/buckets?per_page=1000&cursor=page-2"
        );
    }

    #[tokio::test]
    async fn only_non_default_jurisdictions_send_the_header() {
        let empty = || envelope(serde_json::json!([]), serde_json::json!({}));
        let server = MockServer::start(vec![empty(), empty(), empty(), Reply::status(200)]).await;
        let client = server.client();
        for b in [
            bucket("eu", Some("eu")),
            bucket("default", Some("default")),
            bucket("none", None),
        ] {
            client.list_r2_objects("acc", &b, "", None).await.unwrap();
        }
        client
            .get_r2_object("acc", &bucket("eu", Some("eu")), "k", 10)
            .await
            .unwrap();
        let headers: Vec<_> = server
            .requests()
            .iter()
            .map(|r| r.header("cf-r2-jurisdiction").map(str::to_string))
            .collect();
        assert_eq!(
            headers,
            [Some("eu".to_string()), None, None, Some("eu".to_string())]
        );
    }

    #[tokio::test]
    async fn root_listings_send_no_prefix_and_ignore_cursors_when_not_truncated() {
        let server = MockServer::start(vec![envelope(
            serde_json::json!([{ "key": "top.txt" }]),
            serde_json::json!({ "cursor": "stale", "is_truncated": false, "delimited": ["docs/"] }),
        )])
        .await;
        let listing = server
            .client()
            .list_r2_objects("acc", &bucket("b", None), "", Some("c1"))
            .await
            .unwrap();
        assert_eq!(listing.page.cursor, None);
        assert_eq!(listing.prefixes, ["docs/"]);
        assert_eq!(listing.page.items[0].size, 0, "size defaults to zero");
        assert_eq!(
            server.requests()[0].target,
            "/client/v4/accounts/acc/r2/buckets/b/objects?per_page=1000&delimiter=%2F&cursor=c1"
        );
    }

    #[tokio::test]
    async fn object_reads_report_the_content_type() {
        let server = MockServer::start(vec![
            Reply::status(200)
                .header("Content-Type", "text/csv")
                .body("a,b\n1,2\n"),
        ])
        .await;
        let body = server
            .client()
            .get_r2_object("acc", &bucket("b", None), "data.csv", 1024)
            .await
            .unwrap();
        assert_eq!(body.content_type.as_deref(), Some("text/csv"));
        assert_eq!(body.bytes, b"a,b\n1,2\n");
    }

    #[tokio::test]
    async fn missing_objects_are_api_errors() {
        let server = MockServer::start(vec![Reply::status(404)]).await;
        let err = server
            .client()
            .get_r2_object("acc", &bucket("b", None), "gone", 1024)
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(reqwest::StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn a_zero_limit_reads_nothing_but_knows_there_was_more() {
        let server = MockServer::start(vec![Reply::status(200).body("abc")]).await;
        let body = server
            .client()
            .get_r2_object("acc", &bucket("b", None), "k", 0)
            .await
            .unwrap();
        assert!(body.bytes.is_empty());
        assert!(body.truncated);
    }

    #[test]
    fn objects_decode_metadata() {
        let object: Object = serde_json::from_value(serde_json::json!({
            "key": "a.png",
            "size": 12,
            "http_metadata": { "contentType": "image/png", "cacheControl": "max-age=60" },
            "custom_metadata": { "owner": "ops" }
        }))
        .unwrap();
        assert_eq!(
            object.http_metadata.content_type.as_deref(),
            Some("image/png")
        );
        assert_eq!(
            object.http_metadata.cache_control.as_deref(),
            Some("max-age=60")
        );
        assert_eq!(object.custom_metadata["owner"], "ops");
    }
}
