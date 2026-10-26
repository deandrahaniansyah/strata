//! MySQL / MariaDB via the text protocol, so multi-statement scripts work and values arrive as text.

use super::*;
use crate::model::SslMode;
use anyhow::Context;
use mysql_async::prelude::*;
use mysql_async::{Conn, OptsBuilder, Row, SslOpts, Value};

pub struct MyDriver {
    cfg: ConnectionConfig,
    conn: Conn,
    database: String,
}

impl MyDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let mut opts = OptsBuilder::default()
            .ip_or_hostname(cfg.host.clone())
            .tcp_port(cfg.port)
            .user(Some(cfg.user.clone()))
            .pass((!cfg.password.is_empty()).then(|| cfg.password.clone()))
            .db_name((!cfg.database.is_empty()).then(|| cfg.database.clone()))
            .prefer_socket(false)
            .client_found_rows(false);
        if cfg.ssl != SslMode::Disable {
            let accept_any = cfg.ssl != SslMode::VerifyFull;
            opts = opts.ssl_opts(SslOpts::default().with_danger_accept_invalid_certs(accept_any).with_danger_skip_domain_validation(accept_any));
        }
        let conn = Conn::new(opts).await.context("Can't connect to MySQL")?;
        let mut d = Self { cfg: cfg.clone(), conn, database: cfg.database.clone() };
        if d.database.is_empty() {
            if let Ok(r) = d.query_one("SELECT DATABASE()").await {
                d.database = r.cell(0, 0).unwrap_or("").to_string();
            }
        }
        Ok(d)
    }
}

pub(crate) fn value_to_text(v: &Value) -> Cell {
    match v {
        Value::NULL => None,
        Value::Bytes(b) => Some(match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => format!("0x{}", hex::encode(b)),
        }),
        Value::Int(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(f) => Some(f.to_string()),
        Value::Date(y, mo, d, h, mi, s, us) => Some(if *h == 0 && *mi == 0 && *s == 0 && *us == 0 {
            format!("{y:04}-{mo:02}-{d:02}")
        } else if *us == 0 {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        } else {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
        }),
        Value::Time(neg, days, h, mi, s, us) => {
            let hours = *days as u64 * 24 + *h as u64;
            let sign = if *neg { "-" } else { "" };
            Some(if *us == 0 { format!("{sign}{hours:02}:{mi:02}:{s:02}") } else { format!("{sign}{hours:02}:{mi:02}:{s:02}.{us:06}") })
        }
    }
}

#[async_trait]
impl Driver for MyDriver {
    fn kind(&self) -> DriverKind {
        self.cfg.driver
    }

    fn current_database(&self) -> String {
        self.database.clone()
    }

    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        let started = std::time::Instant::now();
        let mut out = Vec::new();
        let mut result = self.conn.query_iter(sql).await?;
        loop {
            let columns = result.columns();
            let rows: Vec<Row> = result.collect().await?;
            let affected = result.affected_rows();
            let mut r = QueryResult::default();
            if let Some(cols) = columns.filter(|c| !c.is_empty()) {
                r.columns = cols.iter().map(|c| c.name_str().to_string()).collect();
                r.types = cols.iter().map(|c| format!("{:?}", c.column_type()).trim_start_matches("MYSQL_TYPE_").to_lowercase()).collect();
                r.rows = rows.iter().map(|row| (0..row.len()).map(|i| row.as_ref(i).and_then(value_to_text)).collect()).collect();
            } else {
                r.affected = Some(affected);
            }
            out.push(r);
            if result.is_empty() {
                break;
            }
        }
        drop(result);
        let elapsed = started.elapsed().as_millis() as u64;
        let stmts = statements(sql);
        for (i, r) in out.iter_mut().enumerate() {
            r.elapsed_ms = elapsed;
            r.sql = stmts.get(i).cloned().unwrap_or_else(|| sql.to_string());
        }
        // A `USE db` inside a script changes our notion of the current database.
        if stmts.iter().any(|s| s.to_ascii_uppercase().starts_with("USE ")) {
            if let Ok(r) = self.conn.query_first::<Option<String>, _>("SELECT DATABASE()").await {
                self.database = r.flatten().unwrap_or_default();
            }
        }
        Ok(out)
    }

    async fn switch_database(&mut self, db: &str) -> Result<()> {
        self.conn.query_drop(format!("USE {}", sqlgen::quote_ident(Dialect::MySql, db))).await?;
        self.database = db.to_string();
        Ok(())
    }

    async fn close(&mut self) {
        let _ = self.conn.query_drop("DO 0").await;
    }
}
