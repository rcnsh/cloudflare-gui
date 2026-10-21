pub mod history;
pub mod session;
pub mod settings;
pub mod tails;

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

    /// Whether a fetch should start: never loaded, or failed and being retried.
    pub fn needs_load(&self) -> bool {
        matches!(self, Loadable::NotLoaded | Loadable::Failed(_))
    }

    pub fn loaded(value: T) -> Self {
        Loadable::Loaded { value }
    }
}

/// Where the app keeps non-secret files (settings, query history).
pub fn data_dir() -> std::path::PathBuf {
    // The demo keeps its history and settings away from the real ones.
    #[cfg(feature = "devtools")]
    if crate::devtools::demo::active() {
        return std::env::temp_dir().join("cloudflare-gui-demo");
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudflare-gui")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unloaded_or_failed_lists_need_a_fetch() {
        assert!(Loadable::<u8>::NotLoaded.needs_load());
        assert!(Loadable::<u8>::Failed("boom".into()).needs_load());
        assert!(!Loadable::<u8>::Loading.needs_load());
        assert!(!Loadable::loaded(1u8).needs_load());
        assert_eq!(Loadable::loaded(1u8).value(), Some(&1));
        assert_eq!(Loadable::<u8>::Loading.value(), None);
        assert!(matches!(Loadable::<u8>::default(), Loadable::NotLoaded));
    }
}
