//! Typed client for the parts of the Cloudflare REST API this app uses.
//!
//! Every request runs on the shared tokio runtime (see `crate::runtime`), because
//! reqwest and tungstenite need tokio's reactor while GPUI has its own executor.

pub mod accounts;
pub mod d1;
pub mod kv;
pub mod r2;
pub mod workers;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode, header};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

pub const API_BASE: &str = "https://api.cloudflare.com/client/v4/";
const USER_AGENT: &str = concat!("cloudflare-gui/", env!("CARGO_PKG_VERSION"));

/// An API token that never prints itself.
#[derive(Clone)]
pub struct Token(Arc<str>);

impl Token {
    pub fn new(token: impl Into<String>) -> Self {
        Self(Arc::from(token.into().trim()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ApiMessage {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{}", describe_api_failure(*status, errors))]
    Api {
        status: StatusCode,
        errors: Vec<ApiMessage>,
    },
    #[error("rate limited by Cloudflare; try again shortly")]
    RateLimited,
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response from Cloudflare: {0}")]
    Decode(String),
}

impl ApiError {
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            ApiError::Api { status, .. } => Some(*status),
            ApiError::RateLimited => Some(StatusCode::TOO_MANY_REQUESTS),
            _ => None,
        }
    }

    pub fn is_auth(&self) -> bool {
        matches!(
            self.status(),
            Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        )
    }
}

fn describe_api_failure(status: StatusCode, errors: &[ApiMessage]) -> String {
    if errors.is_empty() {
        return format!("Cloudflare returned HTTP {}", status.as_u16());
    }
    let joined = errors
        .iter()
        .map(|e| {
            if e.code != 0 {
                format!("{} (code {})", e.message, e.code)
            } else {
                e.message.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("{joined} [HTTP {}]", status.as_u16())
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// The standard v4 response wrapper.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub errors: Vec<ApiMessage>,
    pub result: Option<T>,
    pub result_info: Option<ResultInfo>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ResultInfo {
    pub page: Option<f64>,
    pub per_page: Option<f64>,
    pub count: Option<f64>,
    pub total_count: Option<f64>,
    pub cursor: Option<String>,
    pub is_truncated: Option<bool>,
    #[serde(default)]
    pub delimited: Vec<String>,
}

impl ResultInfo {
    /// Cloudflare sends an empty string rather than omitting the cursor on the last page.
    pub fn next_cursor(&self) -> Option<String> {
        self.cursor.clone().filter(|c| !c.is_empty())
    }
}

/// Whether a request may be repeated after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Safe to repeat: GETs, DELETEs of ephemeral sessions, read-only SQL.
    Idempotent,
    /// Only repeat when Cloudflare refused it outright with 429. Used for
    /// anything that might change data, so a timeout never runs it twice.
    RateLimitOnly,
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    fn backoff(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(after) = retry_after {
            return after.min(self.max_delay);
        }
        let exp = self.base_delay.saturating_mul(1 << attempt.min(6));
        // Spread retries from parallel requests so they don't land together.
        let jitter_ms = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            % 250) as u64;
        (exp + Duration::from_millis(jitter_ms)).min(self.max_delay)
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    token: Token,
    base: Url,
    retry: RetryPolicy,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .finish()
    }
}

/// A request before it's sent: everything needed to rebuild it for a retry.
pub(crate) struct Request<'a> {
    method: Method,
    segments: Vec<&'a str>,
    query: Vec<(&'a str, String)>,
    headers: Vec<(&'static str, String)>,
    json: Option<serde_json::Value>,
    retry: Retry,
}

impl<'a> Request<'a> {
    pub(crate) fn get(segments: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            method: Method::GET,
            segments: segments.into_iter().collect(),
            query: Vec::new(),
            headers: Vec::new(),
            json: None,
            retry: Retry::Idempotent,
        }
    }

    pub(crate) fn post(
        segments: impl IntoIterator<Item = &'a str>,
        body: serde_json::Value,
    ) -> Self {
        Self {
            method: Method::POST,
            json: Some(body),
            retry: Retry::RateLimitOnly,
            ..Self::get(segments)
        }
    }

    pub(crate) fn delete(segments: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            method: Method::DELETE,
            retry: Retry::RateLimitOnly,
            ..Self::get(segments)
        }
    }

    pub(crate) fn query(mut self, key: &'a str, value: impl ToString) -> Self {
        self.query.push((key, value.to_string()));
        self
    }

    pub(crate) fn query_opt(self, key: &'a str, value: Option<impl ToString>) -> Self {
        match value {
            Some(v) => self.query(key, v),
            None => self,
        }
    }

    pub(crate) fn header(mut self, name: &'static str, value: impl ToString) -> Self {
        self.headers.push((name, value.to_string()));
        self
    }

    pub(crate) fn retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }
}

impl Client {
    pub fn new(token: Token) -> Self {
        Self::with_base(token, Url::parse(API_BASE).expect("valid base URL"))
    }

    pub fn with_base(token: Token, base: Url) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client builds with static config");
        Self {
            http,
            token,
            base,
            retry: RetryPolicy::default(),
        }
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn token(&self) -> &Token {
        &self.token
    }

    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Url {
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .expect("API base URL can have path segments");
            path.pop_if_empty();
            // `push` percent-encodes `/`, which KV and R2 keys routinely contain.
            for segment in segments {
                path.push(segment);
            }
        }
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
        }
        url
    }

    /// Sends a request, retrying according to its policy, and returns the raw
    /// successful response. Error bodies are decoded into `ApiError::Api`.
    pub(crate) async fn send(&self, req: Request<'_>) -> Result<reqwest::Response> {
        let url = self.url(&req.segments, &req.query);
        let mut attempt = 0;
        loop {
            let mut builder = self
                .http
                .request(req.method.clone(), url.clone())
                .bearer_auth(self.token.expose());
            for (name, value) in &req.headers {
                builder = builder.header(*name, value);
            }
            if let Some(body) = &req.json {
                builder = builder.json(body);
            }
            let outcome = builder.send().await;
            attempt += 1;
            let can_retry = attempt < self.retry.max_attempts;

            let response = match outcome {
                Ok(r) => r,
                Err(err) => {
                    let safe = req.retry == Retry::Idempotent || err.is_connect();
                    if can_retry && safe {
                        log::warn!("{} {} failed ({err}); retrying", req.method, url.path());
                        tokio::time::sleep(self.retry.backoff(attempt, None)).await;
                        continue;
                    }
                    return Err(ApiError::Network(err.without_url().to_string()));
                }
            };

            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            let retryable = status == StatusCode::TOO_MANY_REQUESTS
                || (req.retry == Retry::Idempotent && status.is_server_error());
            if retryable && can_retry {
                let retry_after = response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let delay = self.retry.backoff(attempt, retry_after);
                log::warn!(
                    "{} {} returned {status}; retrying in {delay:?}",
                    req.method,
                    url.path()
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                return Err(ApiError::RateLimited);
            }
            let bytes = response.bytes().await.unwrap_or_default();
            let errors = serde_json::from_slice::<Envelope<serde_json::Value>>(&bytes)
                .map(|e| e.errors)
                .unwrap_or_default();
            return Err(ApiError::Api { status, errors });
        }
    }

    /// Sends a request and unwraps the standard `{ success, result }` envelope.
    pub(crate) async fn json<T: DeserializeOwned>(
        &self,
        req: Request<'_>,
    ) -> Result<(T, Option<ResultInfo>)> {
        let response = self.send(req).await?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ApiError::Network(e.without_url().to_string()))?;
        parse_envelope(status, &bytes)
    }

    pub(crate) async fn bytes(&self, req: Request<'_>) -> Result<(Vec<u8>, header::HeaderMap)> {
        let response = self.send(req).await?;
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ApiError::Network(e.without_url().to_string()))?;
        Ok((bytes.to_vec(), headers))
    }
}

pub fn parse_envelope<T: DeserializeOwned>(
    status: StatusCode,
    bytes: &[u8],
) -> Result<(T, Option<ResultInfo>)> {
    let envelope: Envelope<T> =
        serde_json::from_slice(bytes).map_err(|e| ApiError::Decode(e.to_string()))?;
    if !envelope.success {
        return Err(ApiError::Api {
            status,
            errors: envelope.errors,
        });
    }
    let result = envelope
        .result
        .ok_or_else(|| ApiError::Decode("response had no result".into()))?;
    Ok((result, envelope.result_info))
}

/// A page of results plus the cursor for the next one, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub cursor: Option<String>,
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use super::*;
    use testing::{MockServer, Reply};

    #[test]
    fn token_debug_is_redacted() {
        let token = Token::new("  super-secret-value \n");
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
        assert_eq!(token.expose(), "super-secret-value");
        let client = Client::new(token);
        assert!(!format!("{client:?}").contains("super-secret"));
    }

    #[test]
    fn path_segments_are_encoded() {
        let client = Client::new(Token::new("t"));
        let url = client.url(
            &[
                "accounts",
                "abc",
                "storage",
                "kv",
                "namespaces",
                "ns",
                "values",
                "user/42 ?#%",
            ],
            &[("prefix", "a b&c".to_string())],
        );
        assert_eq!(
            url.as_str(),
            "https://api.cloudflare.com/client/v4/accounts/abc/storage/kv/namespaces/ns/values/user%2F42%20%3F%23%25?prefix=a+b%26c"
        );
    }

    #[test]
    fn error_envelopes_become_api_errors() {
        let body = br#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"messages":[],"result":null}"#;
        let err = parse_envelope::<serde_json::Value>(StatusCode::FORBIDDEN, body).unwrap_err();
        assert!(err.is_auth());
        assert_eq!(
            err.to_string(),
            "Authentication error (code 10000) [HTTP 403]"
        );
    }

    #[test]
    fn empty_cursor_means_no_more_pages() {
        let info = ResultInfo {
            cursor: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(info.next_cursor(), None);
    }

    #[tokio::test]
    async fn retries_get_after_rate_limit_and_server_error() {
        let server = MockServer::start(vec![
            Reply::status(429).header("Retry-After", "0"),
            Reply::status(503),
            Reply::fixture(200, "accounts_list.json"),
        ])
        .await;
        let accounts = server.client().list_accounts().await.unwrap();
        assert_eq!(accounts.len(), 1);
        let seen = server.requests();
        assert_eq!(seen.len(), 3);
        assert!(
            seen.iter()
                .all(|r| r.authorization.as_deref() == Some("Bearer test-token"))
        );
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let server = MockServer::start(vec![
            Reply::status(500),
            Reply::status(500),
            Reply::status(500),
            Reply::status(500),
        ])
        .await;
        let err = server.client().list_accounts().await.unwrap_err();
        assert_eq!(err.status(), Some(StatusCode::INTERNAL_SERVER_ERROR));
        assert_eq!(server.requests().len(), 4);
    }

    #[tokio::test]
    async fn posts_are_not_retried_on_server_errors() {
        let server = MockServer::start(vec![
            Reply::status(502),
            Reply::fixture(200, "d1_raw_select.json"),
        ])
        .await;
        let err = server
            .client()
            .d1_query("acc", "db", "DELETE FROM t", d1::QueryIntent::Write)
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(StatusCode::BAD_GATEWAY));
        assert_eq!(
            server.requests().len(),
            1,
            "a write must never be sent twice"
        );
    }
}
