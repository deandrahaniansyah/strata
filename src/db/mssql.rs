//! Microsoft SQL Server via TDS (tiberius).

use super::*;
use crate::model::SslMode;
use anyhow::Context;
use futures::TryStreamExt;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub struct MsDriver {
    cfg: ConnectionConfig,
    client: Client<Compat<TcpStream>>,
    database: String,
}

impl MsDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let client = Self::open(cfg, &cfg.database).await?;
        let mut d = Self { cfg: cfg.clone(), client, database: cfg.database.clone() };
        if let Ok(r) = d.query_one("SELECT DB_NAME()").await {
            d.database = r.cell(0, 0).unwrap_or("master").to_string();
        }
        Ok(d)
    }

    async fn open(cfg: &ConnectionConfig, db: &str) -> Result<Client<Compat<TcpStream>>> {
        let mut c = Config::new();
        c.host(&cfg.host);
        c.port(cfg.port);
        c.application_name("Strata");
        if !db.is_empty() {
            c.database(db);
        }
        c.authentication(AuthMethod::sql_server(&cfg.user, &cfg.password));
        c.encryption(match cfg.ssl {
            SslMode::Disable => EncryptionLevel::Off,
            SslMode::Prefer => EncryptionLevel::On,
            SslMode::Require | SslMode::VerifyFull => EncryptionLevel::Required,
        });
        if cfg.ssl != SslMode::VerifyFull {
            c.trust_cert();
        }
        let tcp = TcpStream::connect(c.get_addr()).await.context("Can't reach SQL Server")?;
        tcp.set_nodelay(true)?;
        Ok(Client::connect(c, tcp.compat_write()).await.context("Can't log in to SQL Server")?)
    }
}

fn cell_text(row: &tiberius::Row, i: usize, data: &ColumnData<'static>) -> Cell {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
    match data {
        ColumnData::U8(v) => v.map(|x| x.to_string()),
        ColumnData::I16(v) => v.map(|x| x.to_string()),
        ColumnData::I32(v) => v.map(|x| x.to_string()),
        ColumnData::I64(v) => v.map(|x| x.to_string()),
        ColumnData::F32(v) => v.map(|x| x.to_string()),
        ColumnData::F64(v) => v.map(|x| x.to_string()),
        ColumnData::Bit(v) => v.map(|x| if x { "1".into() } else { "0".into() }),
        ColumnData::String(v) => v.as_ref().map(|s| s.to_string()),
        ColumnData::Guid(v) => v.map(|g| g.to_string().to_uppercase()),
        ColumnData::Binary(v) => v.as_ref().map(|b| format!("0x{}", hex::encode_upper(b))),
        ColumnData::Numeric(v) => v.map(|n| n.to_string()),
        ColumnData::Xml(v) => v.as_ref().map(|x| x.to_string()),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            row.try_get::<NaiveDateTime, _>(i).ok().flatten().map(|d| d.to_string())
        }
        ColumnData::Date(_) => row.try_get::<NaiveDate, _>(i).ok().flatten().map(|d| d.to_string()),
        ColumnData::Time(_) => row.try_get::<NaiveTime, _>(i).ok().flatten().map(|d| d.to_string()),
        ColumnData::DateTimeOffset(_) => row.try_get::<DateTime<FixedOffset>, _>(i).ok().flatten().map(|d| d.to_rfc3339()),
        #[allow(unreachable_patterns)]
        other => Some(format!("{other:?}")),
    }
}

#[async_trait]
impl Driver for MsDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::SqlServer
    }

    fn current_database(&self) -> String {
        self.database.clone()
    }

    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        let started = std::time::Instant::now();
        let mut out: Vec<QueryResult> = Vec::new();
        {
            let mut stream = self.client.simple_query(sql).await?;
            while let Some(item) = stream.try_next().await? {
                match item {
                    QueryItem::Metadata(meta) => out.push(QueryResult {
                        columns: meta.columns().iter().map(|c| c.name().to_string()).collect(),
                        types: meta.columns().iter().map(|c| format!("{:?}", c.column_type()).to_lowercase()).collect(),
                        ..Default::default()
                    }),
                    QueryItem::Row(row) => {
                        let cells = row.cells().enumerate().map(|(i, (_, d))| cell_text(&row, i, d)).collect();
                        if let Some(r) = out.last_mut() {
                            r.rows.push(cells);
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            out.push(QueryResult { message: Some("Query executed".into()), ..Default::default() });
        }
        let elapsed = started.elapsed().as_millis() as u64;
        for r in &mut out {
            r.elapsed_ms = elapsed;
            r.sql = sql.to_string();
        }
        if sql.trim_start().to_ascii_uppercase().starts_with("USE ") {
            let rows = self.client.simple_query("SELECT DB_NAME()").await?.into_first_result().await?;
            if let Some(name) = rows.first().and_then(|r| r.get::<&str, _>(0)) {
                self.database = name.to_string();
            }
        }
        Ok(out)
    }

    async fn switch_database(&mut self, db: &str) -> Result<()> {
        self.client.simple_query(format!("USE {}", sqlgen::quote_ident(Dialect::SqlServer, db))).await?.into_results().await?;
        self.database = db.to_string();
        let _ = &self.cfg;
        Ok(())
    }
}
