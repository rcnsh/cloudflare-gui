use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, Client, Request, Result, Retry};
use crate::sql;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Database {
    pub uuid: String,
    pub name: String,
    pub created_at: Option<String>,
    pub version: Option<String>,
    pub jurisdiction: Option<String>,
}

/// What the caller believes the SQL does. Checked again here so a bug in the UI
/// can't send a write without the user having enabled write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    Read,
    /// The user enabled write mode and confirmed this exact statement.
    Write,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct QueryMeta {
    pub duration: Option<f64>,
    pub changes: Option<f64>,
    pub rows_read: Option<f64>,
    pub rows_written: Option<f64>,
    pub last_row_id: Option<f64>,
    pub changed_db: Option<bool>,
    pub size_after: Option<f64>,
    pub served_by_region: Option<String>,
    pub served_by_colo: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct RawRows {
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub rows: Vec<Vec<Value>>,
}

/// One statement's result from the `/raw` endpoint, which keeps column order
/// and duplicate column names that the object-shaped `/query` endpoint loses.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct StatementResult {
    #[serde(default)]
    pub results: RawRows,
    #[serde(default)]
    pub meta: QueryMeta,
    #[serde(default = "yes")]
    pub success: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    /// `table` or `view`.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub decl_type: String,
    pub not_null: bool,
    pub primary_key: bool,
}

/// Quotes an identifier for SQLite so table names from the schema can be
/// interpolated safely.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Renders a cell for display. Blobs arrive as arrays of byte values.
pub fn display_value(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(items) if items.iter().all(|v| v.is_u64()) => {
            format!("<blob {} bytes>", items.len())
        }
        other => other.to_string(),
    }
}

impl Client {
    pub async fn list_d1_databases(&self, account_id: &str) -> Result<Vec<Database>> {
        let mut out = Vec::new();
        for page in 1.. {
            let (items, info): (Vec<Database>, _) = self
                .json(
                    Request::get(["accounts", account_id, "d1", "database"])
                        .query("page", page)
                        .query("per_page", 1000),
                )
                .await?;
            let fetched = items.len();
            out.extend(items);
            let total = info.and_then(|i| i.total_count).unwrap_or(0.0) as usize;
            if fetched == 0 || out.len() >= total {
                break;
            }
        }
        Ok(out)
    }

    pub async fn d1_query(
        &self,
        account_id: &str,
        database_id: &str,
        sql: &str,
        intent: QueryIntent,
    ) -> Result<Vec<StatementResult>> {
        let classification = sql::classify(sql);
        if classification.is_empty() {
            return Err(ApiError::Decode("nothing to run".into()));
        }
        let read_only = classification.is_read_only();
        if intent == QueryIntent::Read && !read_only {
            return Err(ApiError::Decode(
                "refusing to run a statement that may write without write mode".into(),
            ));
        }
        let retry = if read_only {
            Retry::Idempotent
        } else {
            Retry::RateLimitOnly
        };
        let request = Request::post(
            ["accounts", account_id, "d1", "database", database_id, "raw"],
            json!({ "sql": sql }),
        )
        .retry(retry);
        Ok(self.json(request).await?.0)
    }

    pub async fn d1_tables(&self, account_id: &str, database_id: &str) -> Result<Vec<TableInfo>> {
        // `_cf_*` tables are D1 internals that the API refuses to read.
        let sql = "SELECT name, type FROM sqlite_master \
                   WHERE type IN ('table', 'view') \
                   AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                   AND name NOT LIKE '\\_cf\\_%' ESCAPE '\\' \
                   ORDER BY name";
        let results = self
            .d1_query(account_id, database_id, sql, QueryIntent::Read)
            .await?;
        Ok(first_rows(&results)
            .iter()
            .filter_map(|row| {
                Some(TableInfo {
                    name: row.first()?.as_str()?.to_string(),
                    kind: row.get(1)?.as_str()?.to_string(),
                })
            })
            .collect())
    }

    pub async fn d1_columns(
        &self,
        account_id: &str,
        database_id: &str,
        table: &str,
    ) -> Result<Vec<ColumnInfo>> {
        let sql = format!("PRAGMA table_info({})", quote_ident(table));
        let results = self
            .d1_query(account_id, database_id, &sql, QueryIntent::Read)
            .await?;
        // table_info columns: cid, name, type, notnull, dflt_value, pk
        Ok(first_rows(&results)
            .iter()
            .filter_map(|row| {
                Some(ColumnInfo {
                    name: row.get(1)?.as_str()?.to_string(),
                    decl_type: row.get(2).and_then(Value::as_str).unwrap_or("").to_string(),
                    not_null: row.get(3).and_then(Value::as_i64).unwrap_or(0) != 0,
                    primary_key: row.get(5).and_then(Value::as_i64).unwrap_or(0) != 0,
                })
            })
            .collect())
    }
}

fn first_rows(results: &[StatementResult]) -> &[Vec<Value>] {
    results
        .first()
        .map(|r| r.results.rows.as_slice())
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::testing::{MockServer, Reply};

    #[tokio::test]
    async fn lists_databases() {
        let server = MockServer::start(vec![Reply::fixture(200, "d1_list.json")]).await;
        let dbs = server.client().list_d1_databases("acc").await.unwrap();
        assert_eq!(dbs.len(), 2);
        assert_eq!(dbs[0].name, "example-db");
        assert_eq!(
            server.requests()[0].target,
            "/client/v4/accounts/acc/d1/database?page=1&per_page=1000"
        );
    }

    #[tokio::test]
    async fn raw_query_keeps_column_order() {
        let server = MockServer::start(vec![Reply::fixture(200, "d1_raw_select.json")]).await;
        let results = server
            .client()
            .d1_query(
                "acc",
                "db-1",
                "SELECT id, name, id FROM users",
                QueryIntent::Read,
            )
            .await
            .unwrap();
        let req = &server.requests()[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.target, "/client/v4/accounts/acc/d1/database/db-1/raw");
        assert_eq!(
            req.json(),
            serde_json::json!({ "sql": "SELECT id, name, id FROM users" })
        );
        let first = &results[0];
        assert_eq!(first.results.columns, ["id", "name", "id"]);
        assert_eq!(first.results.rows.len(), 3);
        assert_eq!(display_value(&first.results.rows[2][1]), "NULL");
        assert_eq!(first.meta.duration, Some(0.2081));
        assert_eq!(first.meta.rows_read, Some(3.0));
    }

    #[tokio::test]
    async fn read_intent_refuses_writes_without_a_request() {
        let server = MockServer::start(vec![]).await;
        let err = server
            .client()
            .d1_query("acc", "db", "SELECT 1; DROP TABLE users", QueryIntent::Read)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("write mode"));
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn sql_errors_surface_the_d1_message() {
        let server = MockServer::start(vec![Reply::fixture(400, "d1_error.json")]).await;
        let err = server
            .client()
            .d1_query("acc", "db", "SELECT * FROM nope", QueryIntent::Read)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no such table: nope"), "{err}");
    }

    #[tokio::test]
    async fn read_only_queries_retry_on_server_errors() {
        let server = MockServer::start(vec![
            Reply::status(503),
            Reply::fixture(200, "d1_raw_select.json"),
        ])
        .await;
        server
            .client()
            .d1_query("acc", "db", "SELECT 1", QueryIntent::Read)
            .await
            .unwrap();
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn tables_and_columns_parse() {
        let server = MockServer::start(vec![
            Reply::fixture(200, "d1_raw_tables.json"),
            Reply::fixture(200, "d1_raw_table_info.json"),
        ])
        .await;
        let client = server.client();
        let tables = client.d1_tables("acc", "db").await.unwrap();
        assert_eq!(
            tables,
            [
                TableInfo {
                    name: "posts".into(),
                    kind: "table".into()
                },
                TableInfo {
                    name: "user stats".into(),
                    kind: "view".into()
                },
            ]
        );
        let columns = client.d1_columns("acc", "db", "we\"ird").await.unwrap();
        assert_eq!(columns.len(), 3);
        assert!(columns[0].primary_key);
        assert_eq!(columns[1].decl_type, "TEXT");
        let pragma = server.requests()[1].json();
        assert_eq!(pragma["sql"], "PRAGMA table_info(\"we\"\"ird\")");
    }

    #[test]
    fn quoting_and_display() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(
            display_value(&serde_json::json!([1, 2, 3])),
            "<blob 3 bytes>"
        );
        assert_eq!(display_value(&serde_json::json!(1.5)), "1.5");
        assert_eq!(display_value(&serde_json::json!("x")), "x");
    }
}
