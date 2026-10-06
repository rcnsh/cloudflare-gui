//! Query history per D1 database, kept locally so it survives restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_PER_DATABASE: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub sql: String,
    /// Unix seconds.
    pub at: i64,
    pub ok: bool,
    pub duration_ms: Option<f64>,
    pub rows: Option<usize>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct QueryHistory {
    #[serde(default)]
    databases: HashMap<String, Vec<HistoryEntry>>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl QueryHistory {
    pub fn load() -> Self {
        Self::load_from(super::data_dir().join("history.json"))
    }

    fn load_from(path: PathBuf) -> Self {
        let mut history: QueryHistory = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        history.path = Some(path);
        history
    }

    /// Newest first.
    pub fn entries(&self, database_id: &str) -> &[HistoryEntry] {
        self.databases
            .get(database_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn record(&mut self, database_id: &str, entry: HistoryEntry) {
        let list = self.databases.entry(database_id.to_string()).or_default();
        // Re-running a query moves it to the top instead of duplicating it.
        list.retain(|e| e.sql.trim() != entry.sql.trim());
        list.insert(0, entry);
        list.truncate(MAX_PER_DATABASE);
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomic(path, &serde_json::to_vec(self).unwrap_or_default()) {
            log::warn!("couldn't save query history: {e}");
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str, at: i64) -> HistoryEntry {
        HistoryEntry {
            sql: sql.into(),
            at,
            ok: true,
            duration_ms: Some(1.0),
            rows: Some(1),
        }
    }

    #[test]
    fn records_newest_first_without_duplicates_and_persists() {
        let dir = std::env::temp_dir().join(format!("cfgui-history-{}", std::process::id()));
        let path = dir.join("history.json");
        let mut history = QueryHistory::load_from(path.clone());
        history.record("db1", entry("SELECT 1", 1));
        history.record("db1", entry("SELECT 2", 2));
        history.record("db1", entry("SELECT 1 ", 3));
        history.record("db2", entry("SELECT 9", 4));
        let sqls: Vec<_> = history
            .entries("db1")
            .iter()
            .map(|e| e.sql.as_str())
            .collect();
        assert_eq!(sqls, ["SELECT 1 ", "SELECT 2"]);

        let reloaded = QueryHistory::load_from(path);
        assert_eq!(reloaded.entries("db1").len(), 2);
        assert_eq!(reloaded.entries("db2")[0].sql, "SELECT 9");
        assert!(reloaded.entries("missing").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn caps_entries_per_database() {
        let mut history = QueryHistory::default();
        for i in 0..(MAX_PER_DATABASE + 10) {
            history.record("db", entry(&format!("SELECT {i}"), i as i64));
        }
        assert_eq!(history.entries("db").len(), MAX_PER_DATABASE);
        assert_eq!(
            history.entries("db")[0].sql,
            format!("SELECT {}", MAX_PER_DATABASE + 9)
        );
    }
}
