//! Elasticsearch / OpenSearch through the SQL endpoint. Indices are listed as tables; read-only.

use super::clickhouse::{http_client, json_cell};
use super::*;
use crate::model::SslMode;
use serde_json::{json, Value as Json};

pub struct EsDriver {
    http: reqwest::Client,
    base: String,
    user: String,
    password: String,
}

impl EsDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let scheme = if cfg.ssl == SslMode::Disable { "http" } else { "https" };
        let d = Self { http: http_client()?, base: format!("{scheme}://{}:{}", cfg.host, cfg.port), user: cfg.user.clone(), password: cfg.password.clone() };
        d.get("/").await?;
        Ok(d)
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.user.is_empty() { rb } else { rb.basic_auth(&self.user, Some(&self.password)) }
    }

    async fn get(&self, path: &str) -> Result<Json> {
        let resp = self.auth(self.http.get(format!("{}{path}", self.base))).send().await?;
        let status = resp.status();
        let body: Json = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("{}", body["error"]["reason"].as_str().unwrap_or(&body.to_string()));
        }
        Ok(body)
    }
}

#[async_trait]
impl Driver for EsDriver {
    async fn server_version(&mut self) -> Result<String> {
        let info = self.get("/").await?;
        info["version"]["number"].as_str().map(str::to_owned).ok_or_else(|| anyhow::anyhow!("The server returned no version"))
    }
    fn kind(&self) -> DriverKind {
        DriverKind::Elasticsearch
    }

    fn current_database(&self) -> String {
        "elasticsearch".into()
    }

    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        let mut out = Vec::new();
        for stmt in statements(sql) {
            let started = std::time::Instant::now();
            let resp = self.auth(self.http.post(format!("{}/_sql?format=json", self.base))).json(&json!({ "query": stmt })).send().await?;
            let status = resp.status();
            let body: Json = resp.json().await?;
            if !status.is_success() {
                anyhow::bail!("{}", body["error"]["reason"].as_str().unwrap_or(&body.to_string()));
            }
            let cols = body["columns"].as_array().cloned().unwrap_or_default();
            out.push(QueryResult {
                columns: cols.iter().map(|c| c["name"].as_str().unwrap_or("").to_string()).collect(),
                types: cols.iter().map(|c| c["type"].as_str().unwrap_or("").to_string()).collect(),
                rows: body["rows"].as_array().cloned().unwrap_or_default().iter().map(|r| r.as_array().map(|c| c.iter().map(json_cell).collect()).unwrap_or_default()).collect(),
                elapsed_ms: started.elapsed().as_millis() as u64,
                sql: stmt,
                ..Default::default()
            });
        }
        Ok(out)
    }

    async fn switch_database(&mut self, _db: &str) -> Result<()> {
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.get("/").await.map(|_| ())
    }

    async fn objects(&mut self, schema: &str) -> Result<Vec<DbObject>> {
        let body = self.get("/_cat/indices?format=json&h=index").await?;
        let mut v: Vec<DbObject> = body
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|i| i["index"].as_str().map(str::to_string))
            .filter(|n| !n.starts_with('.'))
            .map(|name| DbObject { parent: None, arguments: None, schema: schema.to_string(), name, kind: ObjectKind::Table })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    // Elasticsearch SQL has no OFFSET; paging past the first page is not supported here.
    async fn browse(&mut self, t: &TableRef, req: &BrowseRequest, columns: &[ColumnInfo]) -> Result<QueryResult> {
        let d = Dialect::Elastic;
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        let sql = format!(
            // unslop-ignore NO_SELECT_STAR -- The workbench bootstraps unknown result columns with a bounded LIMIT; later browsing projects known catalog columns explicitly.
            "SELECT * FROM {}{}{} LIMIT {}",
            sqlgen::quote_ident(d, &t.name),
            sqlgen::where_clause(d, req, columns),
            sqlgen::order_clause(d, req),
            req.limit
        );
        let mut r = self.query_one(&sql).await?;
        r.sql = sql;
        Ok(r)
    }

    async fn apply_changes(&mut self, _t: &TableRef, _c: &[RowChange]) -> Result<()> {
        anyhow::bail!("Elasticsearch connections are read-only in Strata")
    }
}
