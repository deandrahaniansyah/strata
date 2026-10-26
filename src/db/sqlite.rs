//! SQLite through the bundled library. Calls run on the blocking pool.

use super::*;
use anyhow::Context;
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::ValueRef;
use rusqlite::{Batch, Connection, OpenFlags};
use std::sync::Mutex;

pub struct SqliteDriver {
    path: String,
    conn: Arc<Mutex<Connection>>,
}

impl SqliteDriver {
    pub async fn open(cfg: &ConnectionConfig) -> Result<Self> {
        let path = ssh::expand_home(cfg.path.trim());
        if path.is_empty() {
            anyhow::bail!("Choose a database file");
        }
        let read_only = cfg.read_only;
        let p = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let flags = if read_only {
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI
            } else {
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_URI
            };
            let c = Connection::open_with_flags(&p, flags).with_context(|| format!("Can't open {p}"))?;
            c.busy_timeout(std::time::Duration::from_secs(5))?;
            c.execute_batch("PRAGMA foreign_keys = ON")?;
            Ok(c)
        })
        .await??;
        Ok(Self { path, conn: Arc::new(Mutex::new(conn)) })
    }
}

pub(crate) fn value_text(v: ValueRef<'_>) -> Cell {
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).to_string()),
        ValueRef::Blob(b) => Some(match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => format!("x'{}'", hex::encode(b)),
        }),
    }
}

fn run_script(conn: &Connection, sql: &str) -> Result<Vec<QueryResult>> {
    let mut out = Vec::new();
    let mut batch = Batch::new(conn, sql);
    while let Some(mut stmt) = batch.next()? {
        let started = std::time::Instant::now();
        let text = stmt.expanded_sql().unwrap_or_default();
        let n = stmt.column_count();
        if n > 0 {
            let columns: Vec<String> = stmt.column_names().into_iter().map(str::to_string).collect();
            let types: Vec<String> = stmt.columns().map(|c| c.decl_type().unwrap_or("").to_lowercase()).collect();
            let mut rows_out = Vec::new();
            let mut rows = stmt.raw_query();
            while let Some(row) = rows.next()? {
                rows_out.push((0..n).map(|i| row.get_ref(i).map(value_text).unwrap_or(None)).collect());
            }
            out.push(QueryResult { columns, types, rows: rows_out, elapsed_ms: started.elapsed().as_millis() as u64, sql: text, ..Default::default() });
        } else {
            stmt.raw_execute()?;
            out.push(QueryResult { affected: Some(conn.changes()), elapsed_ms: started.elapsed().as_millis() as u64, sql: text, ..Default::default() });
        }
    }
    Ok(out)
}

#[async_trait]
impl Driver for SqliteDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Sqlite
    }

    fn current_database(&self) -> String {
        std::path::Path::new(&self.path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "main".into())
    }

    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
            run_script(&guard, &sql)
        })
        .await?
    }

    async fn switch_database(&mut self, _db: &str) -> Result<()> {
        Ok(())
    }

    fn canceller(&self) -> Option<Canceller> {
        let handle = self.conn.lock().ok()?.get_interrupt_handle();
        Some(Arc::new(move || handle.interrupt()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, data BLOB); INSERT INTO t(name, data) VALUES ('a', x'00ff'), (NULL, 'hi');").unwrap();
        c
    }

    #[test]
    fn runs_mixed_script_with_one_result_per_statement() {
        let c = mem();
        let res = run_script(&c, "UPDATE t SET name = 'b' WHERE id = 1; SELECT id, name, data FROM t ORDER BY id").unwrap();
        assert_eq!(res.len(), 2);
        assert_eq!(res[0].affected, Some(1));
        assert_eq!(res[1].columns, vec!["id", "name", "data"]);
        assert_eq!(res[1].rows[0], vec![Some("1".into()), Some("b".into()), Some("x'00ff'".into())]);
        assert_eq!(res[1].rows[1][1], None, "NULL stays None");
        assert_eq!(res[1].types[0], "integer");
    }

    #[test]
    fn catalog_queries_work_against_sqlite() {
        let c = mem();
        let t = TableRef { schema: "main".into(), name: "t".into() };
        let r = run_script(&c, &meta::columns_sql(Dialect::Sqlite, &t, "main")).unwrap().pop().unwrap();
        let cols = meta::parse_columns(Dialect::Sqlite, &r);
        assert_eq!(cols.len(), 3);
        assert!(cols[0].primary_key);
        assert!(cols[1].nullable);
        let objs = run_script(&c, &meta::objects_sql(Dialect::Sqlite, "main", "main")[0].0).unwrap().pop().unwrap();
        assert_eq!(objs.first_column(), vec!["t"]);
    }

    #[test]
    fn generated_edits_round_trip_through_sqlite() {
        let c = mem();
        let t = TableRef { schema: "main".into(), name: "t".into() };
        let stmts = [
            sqlgen::change_sql(Dialect::Sqlite, &t, &RowChange::Update { key: vec![("id".into(), Some("2".into()))], set: vec![("name".into(), EditValue::Text("it's".into()))] }),
            sqlgen::change_sql(Dialect::Sqlite, &t, &RowChange::Delete { key: vec![("id".into(), Some("1".into()))] }),
            sqlgen::change_sql(Dialect::Sqlite, &t, &RowChange::Insert { values: vec![("name".into(), EditValue::Null)] }),
        ];
        run_script(&c, &stmts.join(";\n")).unwrap();
        let r = run_script(&c, "SELECT id, name FROM t ORDER BY id").unwrap().pop().unwrap();
        assert_eq!(r.rows, vec![vec![Some("2".into()), Some("it's".into())], vec![Some("3".into()), None]]);
    }

    #[test]
    fn browse_with_filter_and_paging() {
        let c = mem();
        let t = TableRef { schema: "main".into(), name: "t".into() };
        let req = BrowseRequest {
            filters: vec![Filter { column: Some("name".into()), op: FilterOp::IsNotNull, ..Default::default() }],
            sort: vec![("id".into(), false)],
            limit: 10,
            offset: 0,
            ..Default::default()
        };
        let r = run_script(&c, &sqlgen::browse_sql(Dialect::Sqlite, &t, &req, &[])).unwrap().pop().unwrap();
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][1].as_deref(), Some("a"));
    }
}
