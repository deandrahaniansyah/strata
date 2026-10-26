//! Cloudflare D1 via the REST API. `host` holds the account id, `database` the database id,
//! `password` an API token. Speaks the SQLite dialect.

use super::clickhouse::{http_client, json_cell};
use super::*;
use serde_json::{json, Value as Json};

pub struct D1Driver {
    http: reqwest::Client,
    account: String,
    database: String,
    token: String,
}

impl D1Driver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        if cfg.host.trim().is_empty() || cfg.database.trim().is_empty() {
            anyhow::bail!("Account ID and Database ID are required");
        }
        let mut d = Self { http: http_client()?, account: cfg.host.trim().into(), database: cfg.database.trim().into(), token: cfg.password.clone() };
        d.ping().await?;
        Ok(d)
    }

    fn endpoint(&self) -> String {
        format!("https://api.cloudflare.com/client/v4/accounts/{}/d1/database/{}/query", self.account, self.database)
    }
}

#[async_trait]
impl Driver for D1Driver {
    fn kind(&self) -> DriverKind {
        DriverKind::CloudflareD1
    }

    fn current_database(&self) -> String {
        "main".into()
    }

    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        let started = std::time::Instant::now();
        let resp = self.http.post(self.endpoint()).bearer_auth(&self.token).json(&json!({ "sql": sql })).send().await?;
        let body: Json = resp.json().await?;
        if body["success"] != Json::Bool(true) {
            let msg = body["errors"].as_array().and_then(|e| e.first()).and_then(|e| e["message"].as_str()).unwrap_or("D1 request failed");
            anyhow::bail!("{msg}");
        }
        let stmts = statements(sql);
        let elapsed = started.elapsed().as_millis() as u64;
        let mut out = Vec::new();
        for (i, res) in body["result"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let rows = res["results"].as_array().cloned().unwrap_or_default();
            let mut r = QueryResult { sql: stmts.get(i).cloned().unwrap_or_default(), elapsed_ms: elapsed, ..Default::default() };
            if let Some(Json::Object(first)) = rows.first() {
                r.columns = first.keys().cloned().collect();
                r.rows = rows
                    .iter()
                    .map(|row| r.columns.iter().map(|c| json_cell(&row[c.as_str()])).collect())
                    .collect();
            } else if stmts.get(i).map(|s| is_read_only(s)).unwrap_or(false) {
                r.columns = vec!["(no rows)".into()];
            } else {
                r.affected = res["meta"]["changes"].as_u64();
            }
            out.push(r);
        }
        Ok(out)
    }

    async fn switch_database(&mut self, _db: &str) -> Result<()> {
        Ok(())
    }

    // D1 rejects explicit BEGIN/COMMIT; a multi-statement request already runs as one batch.
    async fn run_in_transaction(&mut self, stmts: &[String]) -> Result<()> {
        self.execute(&stmts.join(";\n")).await.map(|_| ())
    }
}
