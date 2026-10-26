//! Redis. The keyspace is browsed as one table (key, type, ttl, value); the editor runs raw commands.

use super::*;
use crate::model::SslMode;
use ::redis::aio::MultiplexedConnection;
use ::redis::Value;
use serde_json::{json, Value as Json};

pub struct RedisDriver {
    cfg: ConnectionConfig,
    conn: MultiplexedConnection,
    db: u32,
}

const COLUMNS: [&str; 4] = ["key", "type", "ttl", "value"];

impl RedisDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let db = cfg.database.trim().trim_start_matches("db").parse::<u32>().unwrap_or(0);
        let conn = Self::open(cfg, db).await?;
        Ok(Self { cfg: cfg.clone(), conn, db })
    }

    async fn open(cfg: &ConnectionConfig, db: u32) -> Result<MultiplexedConnection> {
        use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
        let scheme = if cfg.ssl == SslMode::Disable { "redis" } else { "rediss" };
        let user = utf8_percent_encode(&cfg.user, NON_ALPHANUMERIC).to_string();
        let pass = utf8_percent_encode(&cfg.password, NON_ALPHANUMERIC).to_string();
        let auth = match (user.is_empty(), pass.is_empty()) {
            (true, true) => String::new(),
            (true, false) => format!(":{pass}@"),
            (false, true) => format!("{user}@"),
            (false, false) => format!("{user}:{pass}@"),
        };
        let insecure = if cfg.ssl == SslMode::Require || cfg.ssl == SslMode::Prefer { "#insecure" } else { "" };
        let url = format!("{scheme}://{auth}{}:{}/{db}{insecure}", cfg.host, cfg.port);
        let client = ::redis::Client::open(url)?;
        Ok(tokio::time::timeout(std::time::Duration::from_secs(15), client.get_multiplexed_async_connection()).await??)
    }

    async fn cmd(&mut self, args: &[String]) -> Result<Value> {
        let (name, rest) = args.split_first().ok_or_else(|| anyhow::anyhow!("empty command"))?;
        let mut c = ::redis::cmd(name);
        for a in rest {
            c.arg(a);
        }
        Ok(c.query_async::<Value>(&mut self.conn).await?)
    }

    async fn scan_keys(&mut self, pattern: &str, want: usize) -> Result<Vec<String>> {
        let mut cursor: u64 = 0;
        let mut keys = Vec::new();
        loop {
            let (next, batch): (u64, Vec<String>) = ::redis::cmd("SCAN").arg(cursor).arg("MATCH").arg(pattern).arg("COUNT").arg(1000).query_async(&mut self.conn).await?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 || keys.len() >= want {
                break;
            }
        }
        keys.sort();
        Ok(keys)
    }

    async fn describe_key(&mut self, key: &str) -> Result<Vec<Cell>> {
        let (kind, ttl): (String, i64) = ::redis::pipe().cmd("TYPE").arg(key).cmd("TTL").arg(key).query_async(&mut self.conn).await?;
        let value: Json = match kind.as_str() {
            "string" => {
                let v: Option<String> = ::redis::cmd("GET").arg(key).query_async(&mut self.conn).await?;
                return Ok(vec![Some(key.into()), Some(kind), Some(ttl.to_string()), v]);
            }
            "hash" => {
                let v: Vec<(String, String)> = ::redis::cmd("HGETALL").arg(key).query_async(&mut self.conn).await?;
                Json::Object(v.into_iter().map(|(k, v)| (k, Json::String(v))).collect())
            }
            "list" => json!(::redis::cmd("LRANGE").arg(key).arg(0).arg(499).query_async::<Vec<String>>(&mut self.conn).await?),
            "set" => json!(::redis::cmd("SMEMBERS").arg(key).query_async::<Vec<String>>(&mut self.conn).await?),
            "zset" => {
                let v: Vec<(String, f64)> = ::redis::cmd("ZRANGE").arg(key).arg(0).arg(499).arg("WITHSCORES").query_async(&mut self.conn).await?;
                Json::Array(v.into_iter().map(|(m, s)| json!([m, s])).collect())
            }
            "stream" => {
                let v: Value = ::redis::cmd("XRANGE").arg(key).arg("-").arg("+").arg("COUNT").arg(100).query_async(&mut self.conn).await?;
                value_json(&v)
            }
            _ => Json::Null,
        };
        Ok(vec![Some(key.into()), Some(kind), Some(ttl.to_string()), Some(value.to_string())])
    }

    fn pattern(req: &BrowseRequest) -> String {
        req.filters
            .iter()
            .filter(|f| f.enabled && !f.value.is_empty())
            .find(|f| f.column.as_deref().map(|c| c == "key").unwrap_or(true))
            .map(|f| match f.op {
                FilterOp::Eq => f.value.clone(),
                FilterOp::HasPrefix => format!("{}*", f.value),
                FilterOp::HasSuffix => format!("*{}", f.value),
                FilterOp::Like | FilterOp::ILike => f.value.replace('%', "*").replace('_', "?"),
                _ => format!("*{}*", f.value),
            })
            .unwrap_or_else(|| "*".into())
    }

    fn write_commands(change: &RowChange) -> Vec<Vec<String>> {
        let get = |vals: &[(String, EditValue)], col: &str| {
            vals.iter().find(|(c, _)| c == col).and_then(|(_, v)| match v {
                EditValue::Text(s) => Some(s.clone()),
                _ => None,
            })
        };
        let key_of = |k: &[(String, Cell)]| k.iter().find(|(c, _)| c == "key").and_then(|(_, v)| v.clone()).unwrap_or_default();
        match change {
            RowChange::Delete { key } => vec![vec!["DEL".into(), key_of(key)]],
            RowChange::Update { key, set } => {
                let mut k = key_of(key);
                let kind = key.iter().find(|(c, _)| c == "type").and_then(|(_, v)| v.clone()).unwrap_or_else(|| "string".into());
                let mut cmds = Vec::new();
                if let Some(new_key) = get(set, "key").filter(|n| *n != k) {
                    cmds.push(vec!["RENAME".into(), k.clone(), new_key.clone()]);
                    k = new_key;
                }
                if let Some(v) = get(set, "value") {
                    cmds.extend(value_commands(&k, &kind, &v, true));
                }
                if let Some(ttl) = set.iter().find(|(c, _)| c == "ttl").map(|(_, v)| v.clone()) {
                    match ttl {
                        EditValue::Text(t) if t.trim().parse::<i64>().map(|n| n > 0).unwrap_or(false) => cmds.push(vec!["EXPIRE".into(), k.clone(), t.trim().into()]),
                        _ => cmds.push(vec!["PERSIST".into(), k.clone()]),
                    }
                }
                cmds
            }
            RowChange::Insert { values } => {
                let k = get(values, "key").unwrap_or_default();
                let kind = get(values, "type").unwrap_or_else(|| "string".into());
                let mut cmds = value_commands(&k, &kind, &get(values, "value").unwrap_or_default(), false);
                if let Some(t) = get(values, "ttl").filter(|t| t.trim().parse::<i64>().map(|n| n > 0).unwrap_or(false)) {
                    cmds.push(vec!["EXPIRE".into(), k, t.trim().into()]);
                }
                cmds
            }
        }
    }
}

pub fn preview(changes: &[RowChange]) -> Vec<String> {
    changes
        .iter()
        .flat_map(RedisDriver::write_commands)
        .map(|args| args.iter().map(|a| if a.contains(char::is_whitespace) || a.is_empty() { format!("{a:?}") } else { a.clone() }).collect::<Vec<_>>().join(" "))
        .collect()
}

/// Commands that make `key` hold `raw` (JSON for collection types).
fn value_commands(key: &str, kind: &str, raw: &str, replace: bool) -> Vec<Vec<String>> {
    let k = key.to_string();
    if kind == "string" {
        return vec![if replace { vec!["SET".into(), k, raw.into(), "KEEPTTL".into()] } else { vec!["SET".into(), k, raw.into()] }];
    }
    let parsed: Json = serde_json::from_str(raw).unwrap_or(Json::Null);
    let text = |v: &Json| match v {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut cmds = vec![vec!["DEL".to_string(), k.clone()]];
    match (kind, &parsed) {
        ("hash", Json::Object(map)) if !map.is_empty() => {
            let mut c = vec!["HSET".into(), k.clone()];
            for (f, v) in map {
                c.push(f.clone());
                c.push(text(v));
            }
            cmds.push(c);
        }
        ("list", Json::Array(items)) if !items.is_empty() => {
            cmds.push([vec!["RPUSH".into(), k.clone()], items.iter().map(text).collect()].concat());
        }
        ("set", Json::Array(items)) if !items.is_empty() => {
            cmds.push([vec!["SADD".into(), k.clone()], items.iter().map(text).collect()].concat());
        }
        ("zset", Json::Array(items)) if !items.is_empty() => {
            let mut c = vec!["ZADD".into(), k.clone()];
            for pair in items {
                if let Some([m, s]) = pair.as_array().map(|a| a.as_slice()).and_then(|a| <&[Json; 2]>::try_from(a).ok()) {
                    c.push(text(s));
                    c.push(text(m));
                }
            }
            cmds.push(c);
        }
        _ => {
            // Not valid JSON for the type: store as a plain string so nothing is lost.
            cmds = vec![vec!["SET".into(), k, raw.into()]];
        }
    }
    cmds
}

pub fn value_json(v: &Value) -> Json {
    match v {
        Value::Nil => Json::Null,
        Value::Int(i) => json!(i),
        Value::BulkString(b) => Json::String(String::from_utf8_lossy(b).to_string()),
        Value::SimpleString(s) => Json::String(s.clone()),
        Value::Okay => Json::String("OK".into()),
        Value::Array(items) | Value::Set(items) => Json::Array(items.iter().map(value_json).collect()),
        Value::Map(pairs) => Json::Object(pairs.iter().map(|(k, v)| (value_text(k).unwrap_or_default(), value_json(v))).collect()),
        Value::Double(d) => json!(d),
        Value::Boolean(b) => json!(b),
        other => Json::String(format!("{other:?}")),
    }
}

fn value_text(v: &Value) -> Cell {
    match value_json(v) {
        Json::Null => None,
        Json::String(s) => Some(s),
        other => Some(other.to_string()),
    }
}

/// Splits a command line honouring double/single quotes and backslash escapes.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(match n {
                        'n' => '\n',
                        't' => '\t',
                        o => o,
                    });
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[async_trait]
impl Driver for RedisDriver {
    async fn server_version(&mut self) -> Result<String> {
        let info: String = ::redis::cmd("INFO").arg("server").query_async(&mut self.conn).await?;
        info.lines().find_map(|line| line.strip_prefix("redis_version:")).map(|v| v.trim().to_string()).ok_or_else(|| anyhow::anyhow!("Redis returned no version"))
    }
    fn kind(&self) -> DriverKind {
        DriverKind::Redis
    }

    fn current_database(&self) -> String {
        format!("db{}", self.db)
    }

    async fn execute(&mut self, script: &str) -> Result<Vec<QueryResult>> {
        let mut out = Vec::new();
        for line in script.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("--") && !l.starts_with('#')) {
            let started = std::time::Instant::now();
            let args = tokenize(line.trim_end_matches(';'));
            if args.first().map(|a| a.eq_ignore_ascii_case("SELECT")).unwrap_or(false) {
                let db = args.get(1).cloned().unwrap_or_default();
                self.switch_database(&db).await?;
                out.push(QueryResult::message(format!("Switched to db{}", self.db)));
                continue;
            }
            let value = self.cmd(&args).await?;
            let mut r = QueryResult { sql: line.to_string(), elapsed_ms: started.elapsed().as_millis() as u64, ..Default::default() };
            match &value {
                Value::Map(pairs) => {
                    r.columns = vec!["field".into(), "value".into()];
                    r.rows = pairs.iter().map(|(k, v)| vec![value_text(k), value_text(v)]).collect();
                }
                // RESP2 returns hashes as a flat [field, value, ...] list.
                Value::Array(items) if items.len() % 2 == 0 && args.first().map(|c| matches!(c.to_ascii_uppercase().as_str(), "HGETALL" | "CONFIG")).unwrap_or(false) => {
                    r.columns = vec!["field".into(), "value".into()];
                    r.rows = items.chunks(2).map(|kv| vec![value_text(&kv[0]), value_text(&kv[1])]).collect();
                }
                Value::Array(items) | Value::Set(items) => {
                    r.columns = vec!["#".into(), "value".into()];
                    r.rows = items.iter().enumerate().map(|(i, v)| vec![Some((i + 1).to_string()), value_text(v)]).collect();
                }
                other => {
                    r.columns = vec!["result".into()];
                    r.rows = vec![vec![value_text(other)]];
                }
            }
            out.push(r);
        }
        Ok(out)
    }

    async fn switch_database(&mut self, db: &str) -> Result<()> {
        let n = db.trim().trim_start_matches("db").parse::<u32>().map_err(|_| anyhow::anyhow!("Invalid Redis database '{db}'"))?;
        self.conn = Self::open(&self.cfg, n).await?;
        self.db = n;
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.cmd(&["PING".into()]).await.map(|_| ())
    }

    async fn databases(&mut self) -> Result<Vec<String>> {
        let count = match self.cmd(&["CONFIG".into(), "GET".into(), "databases".into()]).await {
            Ok(v) => value_json(&v).to_string().chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().unwrap_or(16),
            Err(_) => 16,
        };
        Ok((0..count.min(64)).map(|i| format!("db{i}")).collect())
    }

    async fn objects(&mut self, schema: &str) -> Result<Vec<DbObject>> {
        Ok(vec![DbObject { parent: None, arguments: None, schema: schema.into(), name: "keys".into(), kind: ObjectKind::Keys }])
    }

    async fn columns(&mut self, _t: &TableRef) -> Result<Vec<ColumnInfo>> {
        Ok(COLUMNS
            .iter()
            .map(|c| ColumnInfo { name: (*c).into(), data_type: "text".into(), primary_key: *c == "key" || *c == "type", nullable: *c != "key", ..Default::default() })
            .collect())
    }

    async fn ddl(&mut self, _obj: &DbObject) -> Result<String> {
        Ok(String::new())
    }

    async fn browse(&mut self, _t: &TableRef, req: &BrowseRequest, _c: &[ColumnInfo]) -> Result<QueryResult> {
        let started = std::time::Instant::now();
        let pattern = Self::pattern(req);
        let keys = self.scan_keys(&pattern, req.offset + req.limit).await?;
        let mut rows = Vec::new();
        for k in keys.iter().skip(req.offset).take(req.limit) {
            rows.push(self.describe_key(k).await?);
        }
        if let Some((col, asc)) = req.sort.first() {
            if let Some(i) = COLUMNS.iter().position(|c| c == col) {
                rows.sort_by(|a, b| {
                    let o = if i == 2 {
                        a[i].as_deref().and_then(|s| s.parse::<i64>().ok()).cmp(&b[i].as_deref().and_then(|s| s.parse::<i64>().ok()))
                    } else {
                        a[i].cmp(&b[i])
                    };
                    if *asc { o } else { o.reverse() }
                });
            }
        }
        Ok(QueryResult {
            columns: COLUMNS.iter().map(|c| c.to_string()).collect(),
            rows,
            elapsed_ms: started.elapsed().as_millis() as u64,
            sql: format!("SCAN 0 MATCH {pattern}"),
            ..Default::default()
        })
    }

    async fn count(&mut self, _t: &TableRef, req: &BrowseRequest, _c: &[ColumnInfo]) -> Result<u64> {
        let pattern = Self::pattern(req);
        if pattern == "*" {
            let n: u64 = ::redis::cmd("DBSIZE").query_async(&mut self.conn).await?;
            return Ok(n);
        }
        Ok(self.scan_keys(&pattern, usize::MAX).await?.len() as u64)
    }

    fn preview_changes(&self, _t: &TableRef, changes: &[RowChange]) -> Vec<String> {
        preview(changes)
    }

    async fn apply_changes(&mut self, _t: &TableRef, changes: &[RowChange]) -> Result<()> {
        let mut pipe = ::redis::pipe();
        pipe.atomic();
        for args in changes.iter().flat_map(Self::write_commands) {
            let (name, rest) = args.split_first().expect("non-empty command");
            let c = pipe.cmd(name);
            for a in rest {
                c.arg(a);
            }
        }
        pipe.query_async::<Value>(&mut self.conn).await?;
        Ok(())
    }

    async fn processes(&mut self) -> Result<QueryResult> {
        let v: String = ::redis::cmd("CLIENT").arg("LIST").query_async(&mut self.conn).await?;
        let mut r = QueryResult { columns: vec!["id".into(), "addr".into(), "name".into(), "db".into(), "age".into(), "idle".into(), "cmd".into()], ..Default::default() };
        for line in v.lines() {
            let fields: std::collections::HashMap<&str, &str> = line.split(' ').filter_map(|kv| kv.split_once('=')).collect();
            r.rows.push(r.columns.iter().map(|c| fields.get(c.as_str()).map(|s| s.to_string())).collect());
        }
        Ok(r)
    }

    async fn kill_process(&mut self, id: &str) -> Result<()> {
        self.cmd(&["CLIENT".into(), "KILL".into(), "ID".into(), id.into()]).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_quoted_arguments() {
        assert_eq!(tokenize(r#"SET "my key" 'a b' plain"#), vec!["SET", "my key", "a b", "plain"]);
        assert_eq!(tokenize(r#"SET k "line\nbreak" """#), vec!["SET", "k", "line\nbreak", ""]);
    }

    #[test]
    fn hash_update_rewrites_whole_value() {
        let c = RowChange::Update {
            key: vec![("key".into(), Some("user:1".into())), ("type".into(), Some("hash".into()))],
            set: vec![("value".into(), EditValue::Text(r#"{"name":"Ann","age":3}"#.into()))],
        };
        let cmds = RedisDriver::write_commands(&c);
        assert_eq!(cmds[0], vec!["DEL", "user:1"]);
        assert_eq!(cmds[1], vec!["HSET", "user:1", "name", "Ann", "age", "3"]);
    }

    #[test]
    fn rename_then_ttl_targets_new_key() {
        let c = RowChange::Update {
            key: vec![("key".into(), Some("a".into())), ("type".into(), Some("string".into()))],
            set: vec![("key".into(), EditValue::Text("b".into())), ("ttl".into(), EditValue::Text("60".into()))],
        };
        let cmds = RedisDriver::write_commands(&c);
        assert_eq!(cmds, vec![vec!["RENAME", "a", "b"], vec!["EXPIRE", "b", "60"]]);
    }

    #[test]
    fn invalid_collection_json_falls_back_to_string() {
        assert_eq!(value_commands("k", "list", "not json", false), vec![vec!["SET", "k", "not json"]]);
    }

    #[test]
    fn filter_becomes_scan_pattern() {
        let req = |op, v: &str| BrowseRequest { filters: vec![Filter { column: Some("key".into()), op, value: v.into(), ..Default::default() }], ..Default::default() };
        assert_eq!(RedisDriver::pattern(&req(FilterOp::HasPrefix, "user:")), "user:*");
        assert_eq!(RedisDriver::pattern(&req(FilterOp::Contains, "x")), "*x*");
        assert_eq!(RedisDriver::pattern(&BrowseRequest::default()), "*");
    }
}
