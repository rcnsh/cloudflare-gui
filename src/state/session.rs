//! The signed-in session: API client, chosen account and cached resource lists.
//!
//! Resource lists are fetched once and kept until the user refreshes, so
//! browsing around the app doesn't re-hit the API.

use std::collections::HashMap;
use std::future::Future;

use gpui::{Context, EventEmitter};
use serde::Deserialize;

use super::Loadable;
use super::history::QueryHistory;
use crate::cloudflare::accounts::Account;
use crate::cloudflare::d1::{Database, QueryIntent, StatementResult, TableInfo};
use crate::cloudflare::kv::Namespace;
use crate::cloudflare::r2::Bucket;
use crate::cloudflare::workers::Script;
use crate::cloudflare::{self, Client};
use crate::runtime;

/// Anything that can be opened in a tab.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum Resource {
    D1Database {
        id: String,
        name: String,
    },
    D1Table {
        database_id: String,
        database_name: String,
        table: String,
    },
    KvNamespace {
        id: String,
        title: String,
    },
    R2Bucket(Bucket),
    Worker {
        name: String,
    },
}

impl Resource {
    pub fn label(&self) -> String {
        match self {
            Resource::D1Database { name, .. } => name.clone(),
            Resource::D1Table {
                database_name,
                table,
                ..
            } => format!("{database_name}.{table}"),
            Resource::KvNamespace { title, .. } => title.clone(),
            Resource::R2Bucket(bucket) => bucket.name.clone(),
            Resource::Worker { name } => name.clone(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Resource::D1Database { .. } => "D1 database",
            Resource::D1Table { .. } => "D1 table",
            Resource::KvNamespace { .. } => "KV namespace",
            Resource::R2Bucket(_) => "R2 bucket",
            Resource::Worker { .. } => "Worker",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    ResourcesChanged,
}

pub struct Session {
    pub client: Client,
    pub account: Account,
    pub d1: Loadable<Vec<Database>>,
    pub kv: Loadable<Vec<Namespace>>,
    pub r2: Loadable<Vec<Bucket>>,
    pub workers: Loadable<Vec<Script>>,
    pub tables: HashMap<String, Loadable<Vec<TableInfo>>>,
    pub history: QueryHistory,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(client: Client, account: Account) -> Self {
        Self {
            client,
            account,
            d1: Loadable::NotLoaded,
            kv: Loadable::NotLoaded,
            r2: Loadable::NotLoaded,
            workers: Loadable::NotLoaded,
            tables: HashMap::new(),
            history: QueryHistory::load(),
        }
    }

    pub fn account_id(&self) -> String {
        self.account.id.clone()
    }

    /// Loads each resource list unless it's already cached. `force` refetches.
    pub fn load_resources(&mut self, force: bool, cx: &mut Context<Self>) {
        let account = self.account_id();
        if force {
            self.tables.clear();
        }
        if force || self.d1.needs_load() {
            let (client, account) = (self.client.clone(), account.clone());
            self.fetch(cx, |s| &mut s.d1, async move {
                client.list_d1_databases(&account).await
            });
        }
        if force || self.kv.needs_load() {
            let (client, account) = (self.client.clone(), account.clone());
            self.fetch(cx, |s| &mut s.kv, async move {
                client.list_kv_namespaces(&account).await
            });
        }
        if force || self.r2.needs_load() {
            let (client, account) = (self.client.clone(), account.clone());
            self.fetch(cx, |s| &mut s.r2, async move {
                client.list_r2_buckets(&account).await
            });
        }
        if force || self.workers.needs_load() {
            let (client, account) = (self.client.clone(), account.clone());
            self.fetch(cx, |s| &mut s.workers, async move {
                client.list_worker_scripts(&account).await
            });
        }
    }

    pub fn load_tables(&mut self, database_id: &str, force: bool, cx: &mut Context<Self>) {
        let slot = self.tables.entry(database_id.to_string()).or_default();
        if !force && !slot.needs_load() {
            return;
        }
        *slot = Loadable::Loading;
        cx.emit(SessionEvent::ResourcesChanged);
        let (client, account, db) = (
            self.client.clone(),
            self.account_id(),
            database_id.to_string(),
        );
        let key = db.clone();
        let task = runtime::run(async move { client.d1_tables(&account, &db).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            let _ = this.update(cx, |this, cx| {
                this.tables.insert(key, result);
                cx.emit(SessionEvent::ResourcesChanged);
                cx.notify();
            });
        })
        .detach();
    }

    fn fetch<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        slot: fn(&mut Session) -> &mut Loadable<T>,
        future: impl Future<Output = cloudflare::Result<T>> + Send + 'static,
    ) {
        *slot(self) = Loadable::Loading;
        cx.emit(SessionEvent::ResourcesChanged);
        let task = runtime::run(future);
        cx.spawn(async move |this, cx| {
            let result = flatten(task.await);
            let _ = this.update(cx, |this, cx| {
                *slot(this) = result;
                cx.emit(SessionEvent::ResourcesChanged);
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs SQL on the network runtime. Errors come back as display strings
    /// because views only ever show them.
    pub fn query(
        &self,
        database_id: &str,
        sql: String,
        intent: QueryIntent,
    ) -> impl Future<Output = Result<Vec<StatementResult>, String>> + Send + 'static {
        let (client, account, db) = (
            self.client.clone(),
            self.account_id(),
            database_id.to_string(),
        );
        let task = runtime::run(async move { client.d1_query(&account, &db, &sql, intent).await });
        async move {
            match task.await {
                Ok(Ok(results)) => Ok(results),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
    }

    /// Every openable resource currently cached, for the command palette.
    pub fn all_resources(&self) -> Vec<Resource> {
        let mut out = Vec::new();
        for db in self.d1.value().into_iter().flatten() {
            out.push(Resource::D1Database {
                id: db.uuid.clone(),
                name: db.name.clone(),
            });
            if let Some(tables) = self.tables.get(&db.uuid).and_then(|t| t.value()) {
                out.extend(tables.iter().map(|t| Resource::D1Table {
                    database_id: db.uuid.clone(),
                    database_name: db.name.clone(),
                    table: t.name.clone(),
                }));
            }
        }
        out.extend(
            self.kv
                .value()
                .into_iter()
                .flatten()
                .map(|ns| Resource::KvNamespace {
                    id: ns.id.clone(),
                    title: ns.title.clone(),
                }),
        );
        out.extend(
            self.r2
                .value()
                .into_iter()
                .flatten()
                .cloned()
                .map(Resource::R2Bucket),
        );
        out.extend(
            self.workers
                .value()
                .into_iter()
                .flatten()
                .map(|s| Resource::Worker { name: s.id.clone() }),
        );
        out
    }
}

pub fn flatten<T>(result: anyhow::Result<cloudflare::Result<T>>) -> Loadable<T> {
    match result {
        Ok(Ok(value)) => Loadable::loaded(value),
        Ok(Err(e)) => Loadable::Failed(e.to_string()),
        Err(e) => Loadable::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::{ApiError, Token};

    fn session() -> Session {
        Session {
            client: Client::new(Token::new("t")),
            account: Account {
                id: "acc".into(),
                name: "Example".into(),
                kind: None,
            },
            d1: Loadable::NotLoaded,
            kv: Loadable::NotLoaded,
            r2: Loadable::NotLoaded,
            workers: Loadable::NotLoaded,
            tables: HashMap::new(),
            // Not `QueryHistory::load()`, which would read the real data directory.
            history: QueryHistory::default(),
        }
    }

    fn db(uuid: &str, name: &str) -> Database {
        Database {
            uuid: uuid.into(),
            name: name.into(),
            created_at: None,
            version: None,
            jurisdiction: None,
        }
    }

    #[test]
    fn nothing_is_listed_before_anything_loads() {
        let mut s = session();
        assert!(s.all_resources().is_empty());
        s.d1 = Loadable::Failed("nope".into());
        s.kv = Loadable::Loading;
        assert!(s.all_resources().is_empty());
    }

    #[test]
    fn all_resources_lists_kinds_in_order_with_loaded_tables_under_their_database() {
        let mut s = session();
        s.workers = Loadable::loaded(vec![Script {
            id: "api".into(),
            modified_on: None,
            created_on: None,
        }]);
        s.kv = Loadable::loaded(vec![Namespace {
            id: "ns1".into(),
            title: "CACHE".into(),
        }]);
        s.d1 = Loadable::loaded(vec![db("db1", "main"), db("db2", "logs")]);
        s.tables.insert(
            "db1".into(),
            Loadable::loaded(vec![TableInfo {
                name: "users".into(),
                kind: "table".into(),
            }]),
        );
        // A table list that's still loading adds nothing.
        s.tables.insert("db2".into(), Loadable::Loading);
        let labels: Vec<_> = s
            .all_resources()
            .iter()
            .map(|r| format!("{}: {}", r.kind(), r.label()))
            .collect();
        assert_eq!(
            labels,
            [
                "D1 database: main",
                "D1 table: main.users",
                "D1 database: logs",
                "KV namespace: CACHE",
                "Worker: api",
            ]
        );
    }

    #[test]
    fn resources_carry_what_a_tab_needs() {
        let bucket = Bucket {
            name: "assets".into(),
            creation_date: None,
            location: None,
            jurisdiction: Some("eu".into()),
            storage_class: None,
        };
        let mut s = session();
        s.r2 = Loadable::loaded(vec![bucket.clone()]);
        assert_eq!(s.all_resources(), [Resource::R2Bucket(bucket)]);
        assert_eq!(s.all_resources()[0].kind(), "R2 bucket");
        assert_eq!(s.account_id(), "acc");
    }

    #[test]
    fn resources_deserialize_from_action_json() {
        let r: Resource = serde_json::from_str(
            r#"{"D1Table":{"database_id":"d","database_name":"main","table":"users"}}"#,
        )
        .unwrap();
        assert_eq!(r.label(), "main.users");
        let r: Resource = serde_json::from_str(r#"{"Worker":{"name":"api"}}"#).unwrap();
        assert_eq!(r, Resource::Worker { name: "api".into() });
    }

    #[test]
    fn flatten_turns_every_failure_into_a_message() {
        assert!(matches!(flatten(Ok(Ok(3))), Loadable::Loaded { value: 3 }));
        match flatten::<u8>(Ok(Err(ApiError::RateLimited))) {
            Loadable::Failed(e) => assert_eq!(e, "rate limited by Cloudflare; try again shortly"),
            other => panic!("{other:?}"),
        }
        match flatten::<u8>(Err(anyhow::anyhow!("network task failed: cancelled"))) {
            Loadable::Failed(e) => assert!(e.contains("cancelled")),
            other => panic!("{other:?}"),
        }
    }
}
