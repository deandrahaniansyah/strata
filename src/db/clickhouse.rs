//! ClickHouse over its HTTP interface (JSONCompact output).

use super::*;
use crate::model::SslMode;
use serde_json::Value as Json;

pub struct ChDriver {
    http: reqwest::Client,
    base: String,
    user: String,
    password: String,
    database: String,
}

pub(crate) fn json_cell(v: &Json) -> Cell {
    match v {
        Json::Null => None,
        Json::String(s) => Some(s.clone()),
        Json::Bool(b) => Some(b.to_string()),
        Json::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

pub(crate) fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("Strata/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

impl ChDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let scheme = if cfg.ssl == SslMode::Disable { "http" } else { "https" };
        let mut d = Self {
            http: http_client()?,
            base: format!("{scheme}://{}:{}/", cfg.host, cfg.port),
            user: cfg.user.clone(),
            password: cfg.password.clone(),
            database: if cfg.database.is_empty() { "default".into() } else { cfg.database.clone() },
        };
        d.ping().await?;
        Ok(d)
    }

    async fn one(&self, sql: &str) -> Result<QueryResult> {
        let started = std::time::Instant::now();
        let resp = self
            .http
            .post(&self.base)
            .query(&[("database", self.database.as_str()), ("default_format", "JSONCompact")])
            .header("X-ClickHouse-User", &self.user)
            .header("X-ClickHouse-Key", &self.password)
            .body(sql.to_string())
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("{}", body.trim());
        }
        let mut r = QueryResult { sql: sql.to_string(), elapsed_ms: started.elapsed().as_millis() as u64, ..Default::default() };
        if body.trim().is_empty() {
            r.message = Some("OK".into());
            return Ok(r);
        }
        let json: Json = serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("{}", body.trim()))?;
        if let Some(meta) = json["meta"].as_array() {
            r.columns = meta.iter().map(|m| m["name"].as_str().unwrap_or("").to_string()).collect();
            r.types = meta.iter().map(|m| m["type"].as_str().unwrap_or("").to_string()).collect();
        }
        if let Some(rows) = json["data"].as_array() {
            r.rows = rows.iter().map(|row| row.as_array().map(|c| c.iter().map(json_cell).collect()).unwrap_or_default()).collect();
        }
        Ok(r)
    }
}

#[async_trait]
impl Driver for ChDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::ClickHouse
    }

    fn current_database(&self) -> String {
        self.database.clone()
    }

    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        let mut out = Vec::new();
        for stmt in statements(sql) {
            if let Some(db) = stmt.strip_prefix("USE ").or_else(|| stmt.strip_prefix("use ")) {
                self.database = db.trim().trim_matches(|c| c == '`' || c == '"').to_string();
                out.push(QueryResult::message(format!("Database changed to {}", self.database)));
                continue;
            }
            out.push(self.one(&stmt).await?);
        }
        Ok(out)
    }

    async fn switch_database(&mut self, db: &str) -> Result<()> {
        self.database = db.to_string();
        Ok(())
    }
}
