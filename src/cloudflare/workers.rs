//! Worker scripts and live tails.
//!
//! The tail protocol isn't in the OpenAPI spec; it mirrors what Wrangler does:
//! create a tail over REST, connect to the returned URL with the `trace-v1`
//! subprotocol, send `{"debug":false}`, ping every 10s, and delete the tail
//! when done so it doesn't linger on the account.

use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::{Client, Request, Result, Retry};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Script {
    /// The script name.
    pub id: String,
    pub modified_on: Option<String>,
    pub created_on: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TailSession {
    pub id: String,
    pub url: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TailLog {
    #[serde(default)]
    pub message: Vec<Value>,
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub timestamp: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TailException {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub message: Value,
    #[serde(default)]
    pub timestamp: i64,
    pub stack: Option<String>,
}

/// One Worker invocation as reported by the tail.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TailEvent {
    #[serde(default)]
    pub outcome: String,
    pub script_name: Option<String>,
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub exceptions: Vec<TailException>,
    #[serde(default)]
    pub logs: Vec<TailLog>,
    #[serde(default)]
    pub event_timestamp: i64,
    pub event: Option<Value>,
}

impl TailEvent {
    /// A one-line description of what triggered the invocation.
    pub fn trigger(&self) -> String {
        let Some(event) = &self.event else {
            return "unknown trigger".into();
        };
        if let Some(request) = event.get("request") {
            let method = request.get("method").and_then(Value::as_str).unwrap_or("?");
            let url = request.get("url").and_then(Value::as_str).unwrap_or("?");
            let status = event
                .get("response")
                .and_then(|r| r.get("status"))
                .and_then(Value::as_u64)
                .map(|s| format!(" → {s}"))
                .unwrap_or_default();
            return format!("{method} {url}{status}");
        }
        if let Some(cron) = event.get("cron").and_then(Value::as_str) {
            return format!("cron {cron}");
        }
        if let Some(queue) = event.get("queue").and_then(Value::as_str) {
            let size = event.get("batchSize").and_then(Value::as_u64).unwrap_or(0);
            return format!("queue {queue} ({size} messages)");
        }
        if let Some(from) = event.get("mailFrom").and_then(Value::as_str) {
            return format!("email from {from}");
        }
        if let Some(method) = event.get("rpcMethod").and_then(Value::as_str) {
            return format!("rpc {method}");
        }
        if let Some(message) = event.get("message").and_then(Value::as_str) {
            return message.to_string();
        }
        if event.get("scheduledTime").is_some() {
            return "alarm".into();
        }
        if event.get("consumedEvents").is_some() {
            return "tail events".into();
        }
        "event".into()
    }
}

/// Renders a console.log argument list the way a terminal would.
pub fn format_log_message(parts: &[Value]) -> String {
    parts
        .iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone)]
pub enum TailUpdate {
    Connected,
    Event(Box<TailEvent>),
    Reconnecting { attempt: usize, reason: String },
    Failed(String),
}

const PING_INTERVAL: Duration = Duration::from_secs(10);
const RECONNECT_BACKOFF: [u64; 5] = [1, 2, 4, 8, 16];

impl Client {
    pub async fn list_worker_scripts(&self, account_id: &str) -> Result<Vec<Script>> {
        let (mut scripts, _): (Vec<Script>, _) = self
            .json(Request::get(["accounts", account_id, "workers", "scripts"]))
            .await?;
        scripts.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(scripts)
    }

    /// Starts a tail session. This creates a short-lived resource on the account,
    /// so it must be paired with `delete_tail`.
    pub async fn create_tail(&self, account_id: &str, script: &str) -> Result<TailSession> {
        let request = Request::post(
            [
                "accounts", account_id, "workers", "scripts", script, "tails",
            ],
            json!({ "filters": [] }),
        );
        Ok(self.json(request).await?.0)
    }

    pub async fn delete_tail(&self, account_id: &str, script: &str, tail_id: &str) -> Result<()> {
        // Deleting twice is harmless, so retry like a read.
        let request = Request::delete([
            "accounts", account_id, "workers", "scripts", script, "tails", tail_id,
        ])
        .retry(Retry::Idempotent);
        self.send(request).await.map(|_| ())
    }
}

/// Streams tail events into `tx` until the receiver is dropped or reconnecting
/// gives up. Cancel by dropping or aborting the task running this future.
pub async fn stream_tail(url: String, tx: UnboundedSender<TailUpdate>) {
    let mut attempt = 0;
    loop {
        let reason = match stream_once(&url, &tx, &mut attempt).await {
            StreamEnd::ReceiverGone => return,
            StreamEnd::Closed => "the tail closed the connection".to_string(),
            StreamEnd::Error(e) => e,
        };
        if attempt >= RECONNECT_BACKOFF.len() {
            let _ = tx.unbounded_send(TailUpdate::Failed(reason));
            return;
        }
        let delay = RECONNECT_BACKOFF[attempt];
        attempt += 1;
        if tx
            .unbounded_send(TailUpdate::Reconnecting { attempt, reason })
            .is_err()
        {
            return;
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

enum StreamEnd {
    ReceiverGone,
    Closed,
    Error(String),
}

async fn stream_once(
    url: &str,
    tx: &UnboundedSender<TailUpdate>,
    attempt: &mut usize,
) -> StreamEnd {
    let mut request = match url.into_client_request() {
        Ok(r) => r,
        Err(e) => return StreamEnd::Error(e.to_string()),
    };
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static("trace-v1"),
    );
    let (socket, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(ok) => ok,
        Err(e) => return StreamEnd::Error(e.to_string()),
    };
    let (mut sink, mut stream) = socket.split();
    if let Err(e) = sink
        .send(Message::text(json!({ "debug": false }).to_string()))
        .await
    {
        return StreamEnd::Error(e.to_string());
    }
    *attempt = 0;
    if tx.unbounded_send(TailUpdate::Connected).is_err() {
        return StreamEnd::ReceiverGone;
    }

    const PING: &[u8] = b"cloudflare-gui tail ping";
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    let mut awaiting_pong = false;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if awaiting_pong {
                    return StreamEnd::Error("no reply to keep-alive ping".into());
                }
                awaiting_pong = true;
                if let Err(e) = sink.send(Message::Ping(PING.to_vec().into())).await {
                    return StreamEnd::Error(e.to_string());
                }
            }
            message = stream.next() => {
                let message = match message {
                    None => return StreamEnd::Closed,
                    Some(Err(e)) => return StreamEnd::Error(e.to_string()),
                    Some(Ok(m)) => m,
                };
                let payload = match message {
                    Message::Text(text) => text.as_bytes().to_vec(),
                    Message::Binary(bytes) => bytes.to_vec(),
                    Message::Pong(data) => {
                        if data.as_ref() == PING {
                            awaiting_pong = false;
                        }
                        continue;
                    }
                    Message::Close(_) => return StreamEnd::Closed,
                    _ => continue,
                };
                match parse_tail_event(&payload) {
                    Ok(event) => {
                        if tx.unbounded_send(TailUpdate::Event(Box::new(event))).is_err() {
                            return StreamEnd::ReceiverGone;
                        }
                    }
                    Err(e) => log::warn!("ignoring unparseable tail message: {e}"),
                }
            }
        }
    }
}

pub fn parse_tail_event(payload: &[u8]) -> serde_json::Result<TailEvent> {
    serde_json::from_slice(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::testing::{MockServer, Reply, fixture};

    #[tokio::test]
    async fn lists_scripts_sorted() {
        let server = MockServer::start(vec![Reply::fixture(200, "workers_scripts.json")]).await;
        let scripts = server.client().list_worker_scripts("acc").await.unwrap();
        let names: Vec<_> = scripts.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(names, ["api", "website"]);
    }

    #[tokio::test]
    async fn tail_lifecycle_requests() {
        let server = MockServer::start(vec![
            Reply::fixture(200, "tail_create.json"),
            Reply::status(200).body(r#"{"success":true,"errors":[],"messages":[],"result":null}"#),
        ])
        .await;
        let client = server.client();
        let session = client.create_tail("acc", "api").await.unwrap();
        assert_eq!(session.id, "03dc9f77817b488fb26c5861ec18f791");
        assert!(session.url.starts_with("wss://"));
        client.delete_tail("acc", "api", &session.id).await.unwrap();
        let seen = server.requests();
        assert_eq!(seen[0].method, "POST");
        assert_eq!(
            seen[0].target,
            "/client/v4/accounts/acc/workers/scripts/api/tails"
        );
        assert_eq!(seen[0].json(), json!({ "filters": [] }));
        assert_eq!(seen[1].method, "DELETE");
        assert_eq!(
            seen[1].target,
            "/client/v4/accounts/acc/workers/scripts/api/tails/03dc9f77817b488fb26c5861ec18f791"
        );
    }

    #[test]
    fn parses_request_event() {
        let event = parse_tail_event(fixture("tail_event_request.json").as_bytes()).unwrap();
        assert_eq!(event.outcome, "exception");
        assert_eq!(
            event.trigger(),
            "GET https://api.example.com/users?id=1 → 500"
        );
        assert_eq!(event.logs.len(), 2);
        assert_eq!(event.logs[1].level, "warn");
        assert_eq!(
            format_log_message(&event.logs[0].message),
            "loading user 1 {\"cached\":false}"
        );
        assert_eq!(event.exceptions[0].name, "TypeError");
    }

    #[test]
    fn parses_scheduled_and_queue_events() {
        let event = parse_tail_event(fixture("tail_event_scheduled.json").as_bytes()).unwrap();
        assert_eq!(event.trigger(), "cron */5 * * * *");
        assert_eq!(event.outcome, "ok");
        let queue = parse_tail_event(
            br#"{"outcome":"ok","logs":[],"exceptions":[],"eventTimestamp":1,"event":{"queue":"jobs","batchSize":4}}"#,
        )
        .unwrap();
        assert_eq!(queue.trigger(), "queue jobs (4 messages)");
        let none =
            parse_tail_event(br#"{"outcome":"ok","eventTimestamp":1,"event":null}"#).unwrap();
        assert_eq!(none.trigger(), "unknown trigger");
    }
}
