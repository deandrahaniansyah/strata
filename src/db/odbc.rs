//! Vendor ODBC connections. A dedicated worker owns the environment, connection and cursors.
//! Credentials go through SQLConnect parameters, never through a persisted connection string.
use super::*;
use anyhow::{Context, ensure};
use odbc_api::{Environment, ConnectionOptions, Cursor};
use std::sync::mpsc::{sync_channel, SyncSender};
use tokio::sync::oneshot;

enum Request { Transaction(Vec<String>), Sql(String), Tables(String), Keys(TableRef), ForeignKeys(TableRef, bool), Columns(TableRef), AllColumns(String), Schemas, Catalogs, Version }
struct Command { request: Request, reply: oneshot::Sender<Result<Vec<QueryResult>>> }
pub struct OdbcDriver { cfg: ConnectionConfig, tx: SyncSender<Command> }
impl OdbcDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        ensure!(!cfg.odbc_dsn.trim().is_empty(), "Enter an ODBC data source name. Install and configure the vendor's ODBC driver first.");
        let (tx, rx) = sync_channel::<Command>(4);
        let (ready_tx, ready_rx) = oneshot::channel();
        let worker_cfg = cfg.clone();
        std::thread::Builder::new().name("strata-odbc".into()).spawn(move || {
            let environment = match Environment::new() { Ok(env) => env, Err(error) => { let _ = ready_tx.send(Err(anyhow::anyhow!("ODBC manager: {error}"))); return; } };
            let connection = match environment.connect(&worker_cfg.odbc_dsn, &worker_cfg.user, &worker_cfg.password, ConnectionOptions { login_timeout_sec: Some(15), ..Default::default() }) {
                Ok(conn) => conn,
                Err(error) => { let _ = ready_tx.send(Err(anyhow::anyhow!("Cannot open ODBC data source {}: {error}", worker_cfg.odbc_dsn))); return; }
            };
            let catalog = if worker_cfg.database.is_empty() { connection.current_catalog().unwrap_or_default() } else { worker_cfg.database.clone() };
            let _ = ready_tx.send(Ok(catalog.clone()));
            while let Ok(command) = rx.recv() {
                let transaction = matches!(&command.request, Request::Transaction(_));
                let result = serve(&connection, &worker_cfg, &catalog, command.request);
                let failed_transaction = transaction && result.is_err();
                let _ = command.reply.send(result);
                if failed_transaction { break; }
            }
        })?;
        let catalog = tokio::time::timeout(std::time::Duration::from_secs(20), ready_rx).await.context("ODBC login timed out")??;
        let mut cfg = cfg.clone(); cfg.database = catalog?;
        Ok(Self { cfg, tx })
    }
    async fn request(&self, request: Request) -> Result<Vec<QueryResult>> {
        let transaction = matches!(&request, Request::Transaction(_));
        let (reply, response) = oneshot::channel();
        self.tx.try_send(Command { request, reply }).map_err(|_| anyhow::anyhow!("ODBC connection is busy or closed"))?;
        tokio::time::timeout(std::time::Duration::from_secs(40), response).await.context(if transaction { "ODBC transaction timed out; its outcome may be unknown. Check the database before retrying" } else { "ODBC operation timed out; the driver may still be running the statement" })?.context("ODBC worker closed")?
    }
    async fn catalog(&self, request: Request) -> Result<QueryResult> { Ok(self.request(request).await?.pop().unwrap_or_default()) }
}
fn read_cursor(cursor: &mut impl Cursor) -> Result<QueryResult> {
    let columns = cursor.column_names()?.collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(columns.len() <= 1024, "This result has more than 1024 columns");
    let types = (1..=columns.len()).map(|i| cursor.col_data_type(i as u16).map(|t| format!("{t:?}"))).collect::<std::result::Result<Vec<_>, _>>()?;
    if columns.is_empty() { return Ok(QueryResult::message("Statement completed")); }
    let mut rows = Vec::new(); let mut bytes = 0usize; let mut truncated = false;
    while let Some(mut row) = cursor.next_row()? {
        if rows.len() >= 50_000 || bytes >= 128 * 1024 * 1024 { truncated = true; break; }
        let mut cells = Vec::with_capacity(columns.len());
        for i in 1..=columns.len() {
            let mut value = Vec::new();
            let present = row.get_text(i as u16, &mut value)?;
            ensure!(value.len() <= 8 * 1024 * 1024, "An ODBC cell exceeds the 8 MiB display limit. Select a smaller projection.");
            bytes += value.len();
            cells.push(if present { Some(String::from_utf8(value).context("ODBC returned non-UTF-8 text; configure the driver's UTF-8 encoding")?) } else { None });
        }
        rows.push(cells);
    }
    Ok(QueryResult { columns, types, rows, message: truncated.then(|| "Result limited to 50,000 rows or 128 MiB. Add a LIMIT or narrower projection.".into()), ..Default::default() })
}
fn serve(connection: &odbc_api::Connection<'_>, cfg: &ConnectionConfig, catalog: &str, request: Request) -> Result<Vec<QueryResult>> {
    let started = std::time::Instant::now();
    let mut statement = connection.preallocate()?;
    statement.set_query_timeout_sec(30)?;
    let result = match request {
        Request::Transaction(statements) => {
            connection.set_autocommit(false).context("This ODBC driver does not support atomic edits; use explicit queries instead")?;
            let work = (|| -> Result<()> {
                for sql in &statements {
                    ensure!(started.elapsed() < std::time::Duration::from_secs(30), "Transaction exceeded the 30-second deadline");
                    // Each statement comes from the reviewed row-edit plan.
                    let _ = connection.execute(sql, (), Some(30))?;
                }
                ensure!(started.elapsed() < std::time::Duration::from_secs(30), "Transaction exceeded the 30-second deadline");
                Ok(())
            })();
            if let Err(error) = work {
                connection.rollback().context("Rollback failed; connection closed. Check the database before retrying")?;
                return Err(error.context("Transaction rolled back; reconnect before retrying"));
            }
            connection.commit().context("Commit failed; its outcome may be unknown. Check the database before retrying")?;
            connection.set_autocommit(true).context("Commit succeeded but the connection could not be reset; do not repeat these changes")?;
            return Ok(vec![QueryResult::message("Transaction committed")]);
        }
        Request::Sql(sql) => {
            let mut results = Vec::new();
            // SQL text is authored by the workbench user; it is not an interpolated application query.
            if let Some(mut cursor) = statement.execute(&sql, ())? {
                loop {
                    let mut result = read_cursor(&mut cursor)?; result.sql = sql.clone(); result.elapsed_ms = started.elapsed().as_millis() as u64;
                    results.push(result);
                    match cursor.more_results()? { Some(next) => cursor = next, None => break }
                }
            }
            if results.is_empty() { results.push(QueryResult { sql, affected: statement.row_count()?.map(|n| n as u64), elapsed_ms: started.elapsed().as_millis() as u64, ..Default::default() }); }
            return Ok(results);
        }
        Request::Tables(schema) => read_cursor(&mut statement.tables_cursor(catalog, &pattern(&schema), "%", "TABLE,VIEW,MATERIALIZED VIEW")?)?,
        Request::Keys(table) => read_cursor(&mut statement.primary_keys_cursor((!catalog.is_empty()).then_some(catalog), (!table.schema.is_empty()).then_some(table.schema.as_str()), &table.name)?)?,
        Request::ForeignKeys(table, incoming) => {
            let cursor = if incoming { statement.foreign_keys_cursor(catalog, &table.schema, &table.name, "", "", "")? }
                else { statement.foreign_keys_cursor("", "", "", catalog, &table.schema, &table.name)? };
            read_cursor(&mut { cursor })?
        }
        Request::Columns(table) => read_cursor(&mut statement.columns_cursor(catalog, &pattern(&table.schema), &pattern(&table.name), "%")?)?,
        Request::AllColumns(schema) => read_cursor(&mut statement.columns_cursor(catalog, &pattern(&schema), "%", "%")?)?,
        Request::Schemas => read_cursor(&mut statement.tables_cursor("", "%", "", "")?)?,
        Request::Catalogs => read_cursor(&mut statement.tables_cursor("%", "", "", "")?)?,
        Request::Version => {
            let sql = match cfg.driver {
                DriverKind::Oracle => "SELECT version FROM product_component_version WHERE product LIKE 'Oracle%' FETCH FIRST 1 ROWS ONLY",
                DriverKind::Snowflake => "SELECT CURRENT_VERSION()",
                DriverKind::Cassandra => "SELECT release_version FROM system.local",
                DriverKind::DuckDb | DriverKind::Vertica => "SELECT version()",
                DriverKind::BigQuery | DriverKind::DynamoDb => return Ok(vec![QueryResult { columns: vec!["version".into()], rows: vec![vec![Some("Managed service; server version is not published".into())]], ..Default::default() }]),
                _ => anyhow::bail!("Not an ODBC driver"),
            };
            let mut cursor = statement.execute(sql, ())?.context("The server returned no version")?;
            read_cursor(&mut cursor)?
        }
    };
    Ok(vec![result])
}
fn pattern(value: &str) -> String { value.replace('\\', "\\\\").replace('_', "\\_").replace('%', "\\%") }
fn unique_column(result: &QueryResult, column: usize) -> Vec<String> {
    let mut values: Vec<_> = result.rows.iter().filter_map(|r| r.get(column).cloned().flatten()).filter(|v| !v.is_empty()).collect(); values.sort(); values.dedup(); values
}
fn odbc_relations(result: &QueryResult) -> Vec<Relation> {
    result.rows.iter().map(|row| {
        let cell = |i| row.get(i).cloned().flatten().unwrap_or_default();
        Relation { name: cell(11), schema: cell(5), table: cell(6), column: cell(7), ref_schema: cell(1), ref_table: cell(2), ref_column: cell(3), on_delete: match cell(10).as_str() { "0" => "CASCADE", "1" => "RESTRICT", "2" => "SET NULL", "4" => "SET DEFAULT", _ => "NO ACTION" }.into() }
    }).collect()
}
#[async_trait]
impl Driver for OdbcDriver {
    fn kind(&self) -> DriverKind { self.cfg.driver }
    fn current_database(&self) -> String { self.cfg.database.clone() }
    fn default_schema(&self) -> String {
        match self.cfg.driver { DriverKind::Oracle => self.cfg.user.to_uppercase(), DriverKind::Snowflake => "PUBLIC".into(), DriverKind::DuckDb => "main".into(), _ => "public".into() }
    }
    async fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> { self.request(Request::Sql(sql.into())).await }
    async fn run_in_transaction(&mut self, statements: &[String]) -> Result<()> { self.request(Request::Transaction(statements.to_vec())).await.map(|_| ()) }
    async fn switch_database(&mut self, db: &str) -> Result<()> { ensure!(db == self.cfg.database, "This ODBC DSN fixes the connection catalog. Create a DSN for the other database, then open another connection."); Ok(()) }
    async fn server_version(&mut self) -> Result<String> { self.catalog(Request::Version).await?.cell(0, 0).map(str::to_owned).context("Server returned no version") }
    async fn ping(&mut self) -> Result<()> { self.catalog(Request::Catalogs).await.map(|_| ()) }
    async fn databases(&mut self) -> Result<Vec<String>> { let mut values = unique_column(&self.catalog(Request::Catalogs).await?, 0); if values.is_empty() { values.push(self.current_database()); } Ok(values) }
    async fn schemas(&mut self) -> Result<Vec<String>> { let mut values = unique_column(&self.catalog(Request::Schemas).await?, 1); if values.is_empty() { values.push(String::new()); } Ok(values) }
    async fn objects(&mut self, schema: &str) -> Result<Vec<DbObject>> {
        let result = self.catalog(Request::Tables(schema.into())).await?;
        Ok(result.rows.iter().filter_map(|row| Some(DbObject { parent: None, arguments: None, schema: row.get(1).cloned().flatten().unwrap_or_default(), name: row.get(2)?.clone()?, kind: meta::kind_from_type(row.get(3).and_then(|v| v.as_deref())) })).collect())
    }
    async fn columns(&mut self, table: &TableRef) -> Result<Vec<ColumnInfo>> {
        let result = self.catalog(Request::Columns(table.clone())).await?;
        let primary_keys = match self.catalog(Request::Keys(table.clone())).await {
            Ok(keys) => unique_column(&keys, 3),
            // The data grid visibly warns before writes when no primary key is reported.
            Err(_) => Vec::new(),
        };
        Ok(result.rows.iter().filter_map(|row| Some(ColumnInfo { primary_key: row.get(3).and_then(|v| v.as_ref()).map(|name| primary_keys.contains(name)).unwrap_or(false), name: row.get(3)?.clone()?, data_type: row.get(5).cloned().flatten().unwrap_or_default(), nullable: row.get(10).and_then(|v| v.as_deref()) != Some("0"), default: row.get(12).cloned().flatten(), comment: row.get(11).cloned().flatten(), ..Default::default() })).collect())
    }
    async fn foreign_keys(&mut self, table: &TableRef) -> Result<Vec<ForeignKey>> {
        let result = self.catalog(Request::ForeignKeys(table.clone(), false)).await?;
        Ok(odbc_relations(&result).into_iter().map(|relation| ForeignKey { name: relation.name, column: relation.column, ref_schema: relation.ref_schema, ref_table: relation.ref_table, ref_column: relation.ref_column }).collect())
    }
    async fn referencing(&mut self, table: &TableRef) -> Result<Vec<Relation>> { Ok(odbc_relations(&self.catalog(Request::ForeignKeys(table.clone(), true)).await?)) }
    async fn ddl(&mut self, object: &DbObject) -> Result<String> {
        let d = self.dialect();
        let q = sqlgen::qualified(d, &TableRef { schema: object.schema.clone(), name: object.name.clone() });
        let name = sqlgen::lit(d, &object.name); let schema = sqlgen::lit(d, &object.schema);
        let sql = match self.cfg.driver {
            // unslop-ignore NO_SQL_INTERPOLATION -- DDL catalog arguments use lit; qualified names are quoted then encoded as SQL string literals; function names are fixed.
            DriverKind::Oracle => Some(format!("SELECT DBMS_METADATA.GET_DDL({}, {name}, {schema}) FROM dual", sqlgen::lit(d, if object.kind == ObjectKind::View { "VIEW" } else { "TABLE" }))),
            // unslop-ignore NO_SQL_INTERPOLATION -- DDL catalog arguments use lit; qualified names are quoted then encoded as SQL string literals; function names are fixed.
            DriverKind::Snowflake => Some(format!("SELECT GET_DDL({}, {})", sqlgen::lit(d, if object.kind == ObjectKind::View { "VIEW" } else { "TABLE" }), sqlgen::lit(d, &q))),
            // unslop-ignore NO_SQL_INTERPOLATION -- DDL catalog arguments use lit; qualified names are quoted then encoded as SQL string literals; function names are fixed.
            DriverKind::DuckDb => Some(format!("SELECT sql FROM {} WHERE schema_name={schema} AND {}={name}", if object.kind == ObjectKind::View { "duckdb_views()" } else { "duckdb_tables()" }, if object.kind == ObjectKind::View { "view_name" } else { "table_name" })),
            // unslop-ignore NO_SQL_INTERPOLATION -- DDL catalog arguments use lit; qualified names are quoted then encoded as SQL string literals; function names are fixed.
            DriverKind::Vertica => Some(format!("SELECT EXPORT_OBJECTS('', {})", sqlgen::lit(d, &q))),
            _ => None,
        };
        if let Some(sql) = sql {
            let result = self.query_one(&sql).await?;
            return result.cell(0,0).map(str::to_owned).context("The server returned no object definition");
        }
        anyhow::bail!("This ODBC driver does not expose object DDL. Use the vendor's catalog queries in the SQL editor.")
    }
    async fn all_columns(&mut self, schema: &str) -> Result<Vec<(String, String)>> {
        let result = self.catalog(Request::AllColumns(schema.into())).await?;
        Ok(result.rows.iter().filter_map(|row| Some((row.get(2)?.clone()?, row.get(3)?.clone()?))).collect())
    }
}
