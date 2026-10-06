//! Small, non-secret preferences. The API token lives in the keychain only.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub account_id: Option<String>,
    pub account_name: Option<String>,
}

impl Settings {
    fn path() -> PathBuf {
        super::data_dir().join("settings.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn save(&self) {
        if let Err(e) = self.save_to(&Self::path()) {
            log::warn!("couldn't save settings: {e}");
        }
    }

    fn load_from(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_files() {
        let dir = std::env::temp_dir().join(format!("cfgui-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(Settings::load_from(&path), Settings::default());
        let settings = Settings {
            account_id: Some("abc".into()),
            account_name: Some("Example".into()),
        };
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), settings);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
