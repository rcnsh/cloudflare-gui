pub mod history;
pub mod session;
pub mod settings;
pub mod tails;

use std::time::Instant;

pub use session::{Resource, Session, SessionEvent};

/// Remote data with its loading status, so views can show spinners and
/// errors inline instead of blocking.
#[derive(Debug, Clone, Default)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded {
        value: T,
        at: Instant,
    },
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Loadable::Loaded { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading)
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Loadable::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// Whether a fetch should start: never loaded, or failed and being retried.
    pub fn needs_load(&self) -> bool {
        matches!(self, Loadable::NotLoaded | Loadable::Failed(_))
    }

    pub fn loaded(value: T) -> Self {
        Loadable::Loaded {
            value,
            at: Instant::now(),
        }
    }
}

/// Where the app keeps non-secret files (settings, query history).
pub fn data_dir() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudflare-gui")
}
