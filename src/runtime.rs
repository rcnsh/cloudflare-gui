//! Bridges tokio-based networking into GPUI.
//!
//! reqwest and tungstenite need a tokio reactor, but GPUI drives its own
//! executor. All network futures are spawned on this runtime and the returned
//! handles are awaited from GPUI tasks; a `JoinHandle` is executor-agnostic.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;
use tokio::task::{AbortHandle, JoinHandle};

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("cloudflare-net")
            .enable_all()
            .build()
            .expect("tokio runtime starts")
    })
}

/// Runs `future` on the network runtime and resolves with its output.
/// Dropping the returned future (e.g. because the GPUI task owning it was
/// dropped when a tab closed) aborts the request instead of leaking it.
pub fn run<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
) -> impl Future<Output = anyhow::Result<T>> + Send + 'static {
    let handle = runtime().spawn(future);
    let guard = AbortOnDrop(handle.abort_handle());
    async move {
        let out = handle
            .await
            .map_err(|e| anyhow::anyhow!("network task failed: {e}"));
        drop(guard);
        out
    }
}

/// Spawns a long-running future (e.g. a tail stream) that the caller can abort.
pub fn spawn<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> JoinHandle<T> {
    runtime().spawn(future)
}

/// Blocks the calling thread on `future`. Only for the quit path, where there
/// is no executor left to await on; never call it from a tokio thread.
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    runtime().block_on(future)
}

/// Aborts a task when dropped, so a closed view stops its background work.
pub struct AbortOnDrop(pub AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
