//! Tail sessions that are open right now.
//!
//! A tail is a real resource on the account, so every one the app creates is
//! recorded here and deleted when its tab closes or the app quits. Tails also
//! expire on their own, which covers a crash.

use std::sync::Mutex;
use std::time::Duration;

use crate::cloudflare::Client;
use crate::runtime;

#[derive(Clone)]
pub struct OpenTail {
    pub client: Client,
    pub account_id: String,
    pub script: String,
    pub id: String,
}

static OPEN: Mutex<Vec<OpenTail>> = Mutex::new(Vec::new());

pub fn register(tail: OpenTail) {
    OPEN.lock().unwrap().push(tail);
}

fn take(id: &str) -> Option<OpenTail> {
    let mut open = OPEN.lock().unwrap();
    let ix = open.iter().position(|t| t.id == id)?;
    Some(open.remove(ix))
}

async fn delete(tail: OpenTail) {
    match tail
        .client
        .delete_tail(&tail.account_id, &tail.script, &tail.id)
        .await
    {
        Ok(()) => log::info!("deleted tail on {}", tail.script),
        Err(e) => log::warn!("couldn't delete tail on {}: {e}", tail.script),
    }
}

/// Deletes the tail in the background. Safe to call more than once.
pub fn close(id: &str) {
    if let Some(tail) = take(id) {
        drop(runtime::spawn(delete(tail)));
    }
}

/// Deletes every open tail before the process exits, giving up after
/// `timeout` so a dead network can't hang quitting.
pub fn close_all_blocking(timeout: Duration) {
    let tails: Vec<OpenTail> = std::mem::take(&mut *OPEN.lock().unwrap());
    if tails.is_empty() {
        return;
    }
    let all = futures::future::join_all(tails.into_iter().map(delete));
    runtime::block_on(async move {
        if tokio::time::timeout(timeout, all).await.is_err() {
            log::warn!("gave up deleting tails; they expire on their own");
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::cloudflare::testing::{MockServer, Reply};
    use crate::cloudflare::{Client, Token};

    fn tail(client: &Client, id: &str) -> OpenTail {
        OpenTail {
            client: client.clone(),
            account_id: "acc".into(),
            script: "api".into(),
            id: id.into(),
        }
    }

    // One test, because the registry is process-wide.
    #[test]
    fn quitting_deletes_every_open_tail_but_never_hangs() {
        let server = runtime::block_on(MockServer::start(vec![
            Reply::status(200).body(r#"{"success":true,"errors":[],"result":null}"#),
            Reply::status(200).body(r#"{"success":true,"errors":[],"result":null}"#),
        ]));
        let client = server.client();
        register(tail(&client, "t1"));
        register(tail(&client, "t2"));
        close_all_blocking(Duration::from_secs(5));
        let mut targets: Vec<_> = server
            .requests()
            .into_iter()
            .map(|r| format!("{} {}", r.method, r.target))
            .collect();
        targets.sort();
        assert_eq!(
            targets,
            [
                "DELETE /client/v4/accounts/acc/workers/scripts/api/tails/t1",
                "DELETE /client/v4/accounts/acc/workers/scripts/api/tails/t2",
            ]
        );
        assert!(OPEN.lock().unwrap().is_empty());
        // Nothing left, so this returns straight away and sends nothing.
        close_all_blocking(Duration::from_secs(5));
        close("unknown");
        assert_eq!(server.requests().len(), 2);

        // A server that accepts connections but never answers.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/client/v4/", silent.local_addr().unwrap());
        let stuck = Client::with_base(Token::new("t"), base.parse().unwrap());
        register(tail(&stuck, "t3"));
        let started = Instant::now();
        close_all_blocking(Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(take("t3").is_none(), "the tail was taken off the list");
    }
}
