//! LibSQL / Turso over the Hrana-over-HTTP v2 pipeline API. Speaks the SQLite dialect.

use super::clickhouse::http_client;
use super::*;
use serde_json::{json, Value as Json};

pub struct LibSqlDriver {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl LibSqlDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let mut base = cfg.host.trim().trim_end_matches('/').to_string();
        if let Some(rest) = base.strip_prefix("libsql://") {
            base = format!("https://{rest}");
        }
        if !base.starts_with("http") {
            base = format!("https://{base}");
        }
        let mut d = Self { http: http_client()?, url: format!("{base}/v2/pipeline"), token: cfg.password.clone() };
        d.ping().await?;
        Ok(d)
    }
}

fn hrana_value(v: &Json) -> Cell {
    match v["type"].as_str() {
        Some("null") | None => None,
        Some("blob") => Some(format!("base64:{}", v["base64"].as_str().unwrap_or(""))),
        _ => match &v["value"] {
            Json::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        },
    }
}

#[async_trait]
impl Driver for LibSqlDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::LibSql
    }

    fn current_database(&self) -> String {
        "main".into()
    }

    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        let stmts = statements(sql);
        let mut requests: Vec<Json> = stmts.iter().map(|s| json!({"type": "execute", "stmt": {"sql": s}})).collect();
        requests.push(json!({"type": "close"}));
        let started = std::time::Instant::now();
        let mut req = self.http.post(&self.url).json(&json!({ "requests": requests }));
        if !self.token.is_empty() {
            req = req.bearer_auth(&self.token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body: Json = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("{}", body["message"].as_str().unwrap_or(&body.to_string()));
        }
        let elapsed = started.elapsed().as_millis() as u64;
        let mut out = Vec::new();
        for (i, item) in body["results"].as_array().cloned().unwrap_or_default().iter().enumerate().take(stmts.len()) {
            if item["type"] == "error" {
                anyhow::bail!("{}", item["error"]["message"].as_str().unwrap_or("statement failed"));
            }
            let res = &item["response"]["result"];
            let mut r = QueryResult { sql: stmts[i].clone(), elapsed_ms: elapsed, ..Default::default() };
            let cols = res["cols"].as_array().cloned().unwrap_or_default();
            if cols.is_empty() {
                r.affected = res["affected_row_count"].as_u64();
            } else {
                r.columns = cols.iter().map(|c| c["name"].as_str().unwrap_or("").to_string()).collect();
                r.types = cols.iter().map(|c| c["decltype"].as_str().unwrap_or("").to_lowercase()).collect();
                r.rows = res["rows"].as_array().cloned().unwrap_or_default().iter().map(|row| row.as_array().map(|c| c.iter().map(hrana_value).collect()).unwrap_or_default()).collect();
            }
            out.push(r);
        }
        Ok(out)
    }

    async fn switch_database(&mut self, _db: &str) -> Result<()> {
        Ok(())
    }

    // Hrana requests are independent; BEGIN/COMMIT across requests would not hold a transaction.
    async fn run_in_transaction(&mut self, stmts: &[String]) -> Result<()> {
        let script = format!("BEGIN;\n{};\nCOMMIT", stmts.join(";\n"));
        self.execute(&script).await.map(|_| ())
    }
}
