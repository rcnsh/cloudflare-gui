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
}
