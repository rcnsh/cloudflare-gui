//! A tiny scripted HTTP server so client tests exercise real requests, retries
//! and decoding against recorded fixtures without touching the network.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use super::{Client, RetryPolicy, Token};

pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn fixture(status: u16, name: &str) -> Self {
        Self::status(status)
            .header("Content-Type", "application/json")
            .body(fixture(name).into_bytes())
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub method: String,
    /// Path and query exactly as sent, still percent-encoded.
    pub target: String,
    pub authorization: Option<String>,
    pub body: String,
}

impl SeenRequest {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("request body is JSON")
    }
}

pub struct MockServer {
    base: Url,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
}

impl MockServer {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut socket).await;
                log.lock().unwrap().push(request);
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in &reply.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&reply.body).await;
                let _ = socket.shutdown().await;
            }
        });
        Self {
            base: Url::parse(&format!("http://{addr}/client/v4/")).unwrap(),
            seen,
        }
    }

    pub fn client(&self) -> Client {
        Client::with_base(Token::new("test-token"), self.base.clone()).with_retry_policy(
            RetryPolicy {
                max_attempts: 4,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
            },
        )
    }

    pub fn requests(&self) -> Vec<SeenRequest> {
        self.seen.lock().unwrap().clone()
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> SeenRequest {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.unwrap();
        if n == 0 {
            break buf.len();
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let target = request_line.next().unwrap_or_default().to_string();
    let mut content_length = 0;
    let mut authorization = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = v.trim().parse().unwrap_or(0),
                "authorization" => authorization = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = socket.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    SeenRequest {
        method,
        target,
        authorization,
        body: String::from_utf8_lossy(&body).to_string(),
    }
}
