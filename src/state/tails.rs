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
