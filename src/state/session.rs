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
use crate::cloudflare::d1::{Database, TableInfo};
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
