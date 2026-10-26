//! MongoDB. Collections browse as tables (top-level fields become columns); the editor accepts
//! shell-style calls such as `db.users.find({age: {$gt: 30}}).sort({name: 1}).limit(20)`.

use super::*;
use bson::{doc, Bson, Document};
use futures::TryStreamExt;
use mongodb::Client;
use serde_json::Value as Json;

pub struct MongoDriver {
    client: Client,
    database: String,
}

impl MongoDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
        let uri = if cfg.host.starts_with("mongodb://") || cfg.host.starts_with("mongodb+srv://") {
            cfg.host.clone()
        } else {
            let auth = if cfg.user.is_empty() {
                String::new()
            } else {
                format!("{}:{}@", utf8_percent_encode(&cfg.user, NON_ALPHANUMERIC), utf8_percent_encode(&cfg.password, NON_ALPHANUMERIC))
            };
            let tls = if cfg.ssl != crate::model::SslMode::Disable { "&tls=true&tlsAllowInvalidCertificates=true" } else { "" };
            format!("mongodb://{auth}{}:{}/?directConnection=true&serverSelectionTimeoutMS=15000{tls}", cfg.host, cfg.port)
        };
        let client = Client::with_uri_str(&uri).await?;
        let database = if cfg.database.is_empty() {
            client.default_database().map(|d| d.name().to_string()).unwrap_or_else(|| "test".into())
        } else {
            cfg.database.clone()
        };
        client.database("admin").run_command(doc! { "ping": 1 }).await?;
        Ok(Self { client, database })
    }

    fn db(&self) -> mongodb::Database {
        self.client.database(&self.database)
    }
}

/// How a BSON value appears in a grid cell.
pub fn bson_cell(v: &Bson) -> Cell {
    match v {
        Bson::Null | Bson::Undefined => None,
        Bson::String(s) => Some(s.clone()),
        Bson::ObjectId(o) => Some(format!("ObjectId('{o}')")),
        Bson::Int32(i) => Some(i.to_string()),
        Bson::Int64(i) => Some(i.to_string()),
        Bson::Double(d) => Some(d.to_string()),
        Bson::Boolean(b) => Some(b.to_string()),
        Bson::DateTime(d) => Some(format!("ISODate('{}')", d.try_to_rfc3339_string().unwrap_or_default())),
        other => Some(other.clone().into_relaxed_extjson().to_string()),
    }
}

fn shell_to_json(src: &str) -> String {
    use std::sync::LazyLock;
    // unslop-ignore RUST_UNWRAP -- This regex is a fixed, validated source literal; no runtime input affects regex compilation.
    static OID: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"ObjectId\(\s*["']([0-9a-fA-F]{24})["']\s*\)"#).unwrap());
    // unslop-ignore RUST_UNWRAP -- This regex is a fixed, validated source literal; no runtime input affects regex compilation.
    static DATE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?:ISODate|new Date)\(\s*["']([^"']+)["']\s*\)"#).unwrap());
    let s = OID.replace_all(src, r#"{"$$oid":"$1"}"#);
    DATE.replace_all(&s, r#"{"$$date":"$1"}"#).to_string()
}

/// Parses user input (shell syntax, JSON5 or extended JSON) into BSON; bare text stays a string.
pub fn parse_value(text: &str) -> Bson {
    let converted = shell_to_json(text.trim());
    match json5::from_str::<Json>(&converted) {
        Ok(j) => Bson::try_from(j).unwrap_or_else(|_| Bson::String(text.to_string())),
        Err(_) => Bson::String(text.to_string()),
    }
}

fn parse_args(src: &str) -> Result<Vec<Bson>> {
    if src.trim().is_empty() {
        return Ok(vec![]);
    }
    let json: Json = json5::from_str(&format!("[{}]", shell_to_json(src))).map_err(|e| anyhow::anyhow!("Can't parse arguments: {e}"))?;
    Ok(json.as_array().cloned().unwrap_or_default().into_iter().map(|j| Bson::try_from(j).unwrap_or(Bson::Null)).collect())
}

fn arg_doc(args: &[Bson], i: usize) -> Document {
    match args.get(i) {
        Some(Bson::Document(d)) => d.clone(),
        _ => Document::new(),
    }
}

/// `name(args)` segments of a call chain; returns (name, raw args).
fn split_calls(mut s: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start().trim_start_matches('.');
        if s.trim().is_empty() || s.trim() == ";" {
            return Ok(out);
        }
        let open = s.find('(').ok_or_else(|| anyhow::anyhow!("Expected '(' in {s}"))?;
        let name = s[..open].trim().to_string();
        let mut depth = 0;
        let mut quote: Option<char> = None;
        let mut end = None;
        for (i, c) in s[open..].char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '(' | '{' | '[') => depth += 1,
                (None, ')' | '}' | ']') => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| anyhow::anyhow!("Unbalanced parentheses"))?;
        out.push((name, s[open + 1..end].to_string()));
        s = &s[end + 1..];
    }
}

fn docs_result(docs: Vec<Document>) -> QueryResult {
    let mut columns: Vec<String> = Vec::new();
    for d in &docs {
        for k in d.keys() {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    if let Some(pos) = columns.iter().position(|c| c == "_id") {
        let id = columns.remove(pos);
        columns.insert(0, id);
    }
    let rows = docs.iter().map(|d| columns.iter().map(|c| d.get(c).and_then(bson_cell)).collect()).collect();
    QueryResult { columns, rows, ..Default::default() }
}

/// Top-level fields of a document sample, typed by the BSON types seen. A field that is missing
/// or null in any sampled document is reported nullable (optional).
fn sampled_fields(docs: &[Document]) -> Vec<ColumnInfo> {
    let r = docs_result(docs.to_vec());
    r.columns
        .iter()
        .map(|c| {
            let mut types: Vec<&str> = Vec::new();
            let mut optional = false;
            for d in docs {
                match d.get(c) {
                    None | Some(Bson::Null) => optional = true,
                    Some(v) => {
                        let t = bson_type_name(v);
                        if !types.contains(&t) {
                            types.push(t);
                        }
                    }
                }
            }
            ColumnInfo { name: c.clone(), data_type: if types.is_empty() { "null".into() } else { types.join(" | ") }, nullable: optional, primary_key: c == "_id", ..Default::default() }
        })
        .collect()
}

fn bson_type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "javascript",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Decimal128(_) => "decimal",
        _ => "other",
    }
}

fn json_result(v: Bson) -> QueryResult {
    QueryResult { columns: vec!["result".into()], rows: vec![vec![Some(v.into_relaxed_extjson().to_string())]], ..Default::default() }
}

fn filter_doc(req: &BrowseRequest) -> Document {
    let mut parts: Vec<Bson> = Vec::new();
    for f in req.filters.iter().filter(|f| f.enabled) {
        let Some(col) = f.column.clone() else {
            if f.op == FilterOp::Raw {
                if let Bson::Document(d) = parse_value(&f.value) {
                    parts.push(Bson::Document(d));
                }
            }
            continue;
        };
        let v = parse_value(&f.value);
        let re = |p: String| Bson::Document(doc! { "$regex": p, "$options": "i" });
        let esc = regex::escape(&f.value);
        let cond = match f.op {
            FilterOp::Eq => v,
            FilterOp::Ne => Bson::Document(doc! { "$ne": v }),
            FilterOp::Lt => Bson::Document(doc! { "$lt": v }),
            FilterOp::Gt => Bson::Document(doc! { "$gt": v }),
            FilterOp::Le => Bson::Document(doc! { "$lte": v }),
            FilterOp::Ge => Bson::Document(doc! { "$gte": v }),
            FilterOp::In | FilterOp::NotIn => {
                let list: Vec<Bson> = f.value.split(',').map(|x| parse_value(x.trim())).collect();
                Bson::Document(if f.op == FilterOp::In { doc! { "$in": list } } else { doc! { "$nin": list } })
            }
            FilterOp::IsNull => Bson::Null,
            FilterOp::IsNotNull => Bson::Document(doc! { "$ne": Bson::Null }),
            FilterOp::Between => Bson::Document(doc! { "$gte": v, "$lte": parse_value(&f.value2) }),
            FilterOp::NotBetween => Bson::Document(doc! { "$not": { "$gte": v, "$lte": parse_value(&f.value2) } }),
            FilterOp::Contains | FilterOp::Like | FilterOp::ILike => re(esc),
            FilterOp::NotContains | FilterOp::NotLike => Bson::Document(doc! { "$not": { "$regex": esc, "$options": "i" } }),
            FilterOp::HasPrefix => re(format!("^{esc}")),
            FilterOp::HasSuffix => re(format!("{esc}$")),
            FilterOp::Raw => parse_value(&f.value),
        };
        parts.push(Bson::Document(doc! { col: cond }));
    }
    match parts.len() {
        0 => Document::new(),
        1 => parts.pop().and_then(|b| b.as_document().cloned()).unwrap_or_default(),
        _ => doc! { if req.match_any { "$or" } else { "$and" }: parts },
    }
}

fn key_filter(key: &[(String, Cell)]) -> Document {
    let mut d = Document::new();
    for (c, v) in key {
        d.insert(c.clone(), v.as_deref().map(parse_value).unwrap_or(Bson::Null));
    }
    d
}

fn edit_bson(v: &EditValue) -> Bson {
    match v {
        EditValue::Null | EditValue::Default => Bson::Null,
        EditValue::Text(t) => parse_value(t),
    }
}

pub fn preview(t: &TableRef, changes: &[RowChange]) -> Vec<String> {
    let j = |d: Document| Bson::Document(d).into_relaxed_extjson().to_string();
    changes
        .iter()
        .map(|c| match c {
            RowChange::Update { key, set } => {
                let mut s = Document::new();
                for (k, v) in set {
                    s.insert(k.clone(), edit_bson(v));
                }
                format!("db.{}.updateOne({}, {{\"$set\": {}}})", t.name, j(key_filter(key)), j(s))
            }
            RowChange::Insert { values } => {
                let mut d = Document::new();
                for (k, v) in values.iter().filter(|(_, v)| *v != EditValue::Default) {
                    d.insert(k.clone(), edit_bson(v));
                }
                format!("db.{}.insertOne({})", t.name, j(d))
            }
            RowChange::Delete { key } => format!("db.{}.deleteOne({})", t.name, j(key_filter(key))),
        })
        .collect()
}

#[async_trait]
impl Driver for MongoDriver {
    async fn server_version(&mut self) -> Result<String> {
        let result = self.client.database("admin").run_command(doc! { "buildInfo": 1 }).await?;
        Ok(result.get_str("version")?.to_owned())
    }
    fn kind(&self) -> DriverKind {
        DriverKind::MongoDb
    }

    fn current_database(&self) -> String {
        self.database.clone()
    }

    async fn ping(&mut self) -> Result<()> {
        self.db().run_command(doc! { "ping": 1 }).await.map(|_| ()).map_err(Into::into)
    }

    async fn execute(&mut self, script: &str) -> Result<Vec<QueryResult>> {
        let mut out = Vec::new();
        for stmt in statements(script) {
            let started = std::time::Instant::now();
            let mut r = self.run_one(stmt.trim()).await?;
            r.elapsed_ms = started.elapsed().as_millis() as u64;
            r.sql = stmt;
            out.push(r);
        }
        Ok(out)
    }

    async fn switch_database(&mut self, db: &str) -> Result<()> {
        self.database = db.to_string();
        Ok(())
    }

    async fn databases(&mut self) -> Result<Vec<String>> {
        Ok(self.client.list_database_names().await?)
    }

    async fn schemas(&mut self) -> Result<Vec<String>> {
        Ok(vec![self.database.clone()])
    }

    async fn objects(&mut self, schema: &str) -> Result<Vec<DbObject>> {
        let mut names = self.db().list_collection_names().await?;
        names.sort();
        Ok(names.into_iter().map(|name| DbObject { parent: None, arguments: None, schema: schema.into(), name, kind: ObjectKind::Collection }).collect())
    }

    async fn columns(&mut self, t: &TableRef) -> Result<Vec<ColumnInfo>> {
        let sample: Vec<Document> = self.db().collection::<Document>(&t.name).find(doc! {}).limit(100).await?.try_collect().await?;
        let mut cols = sampled_fields(&sample);
        if cols.is_empty() {
            cols.push(ColumnInfo { name: "_id".into(), data_type: "objectId".into(), primary_key: true, ..Default::default() });
        }
        Ok(cols)
    }

    async fn indexes(&mut self, t: &TableRef) -> Result<Vec<IndexInfo>> {
        let idx = self.db().collection::<Document>(&t.name).list_indexes().await?.try_collect::<Vec<_>>().await?;
        Ok(idx
            .into_iter()
            .map(|m| IndexInfo {
                name: m.options.as_ref().and_then(|o| o.name.clone()).unwrap_or_default(),
                columns: m.keys.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", "),
                unique: m.options.as_ref().and_then(|o| o.unique).unwrap_or(false),
                primary: m.keys.keys().eq(["_id"].iter()),
                method: "btree".into(),
            })
            .collect())
    }

    async fn foreign_keys(&mut self, _t: &TableRef) -> Result<Vec<ForeignKey>> {
        Ok(vec![])
    }

    async fn ddl(&mut self, obj: &DbObject) -> Result<String> {
        let info = self.db().run_command(doc! { "listCollections": 1, "filter": { "name": &obj.name } }).await?;
        Ok(Bson::Document(info).into_relaxed_extjson().to_string())
    }

    async fn browse(&mut self, t: &TableRef, req: &BrowseRequest, _c: &[ColumnInfo]) -> Result<QueryResult> {
        let started = std::time::Instant::now();
        let filter = filter_doc(req);
        let mut sort = Document::new();
        for (c, asc) in &req.sort {
            sort.insert(c.clone(), if *asc { 1 } else { -1 });
        }
        let docs: Vec<Document> = self
            .db()
            .collection::<Document>(&t.name)
            .find(filter.clone())
            .sort(sort.clone())
            .skip(req.offset as u64)
            .limit(req.limit as i64)
            .await?
            .try_collect()
            .await?;
        let mut r = docs_result(docs);
        r.elapsed_ms = started.elapsed().as_millis() as u64;
        r.sql = format!(
            // unslop-ignore UNBOUNDED_QUERY -- The generated Mongo command explicitly applies skip(req.offset) and limit(req.limit).
            "db.{}.find({}).sort({}).skip({}).limit({})",
            t.name,
            Bson::Document(filter).into_relaxed_extjson(),
            Bson::Document(sort).into_relaxed_extjson(),
            req.offset,
            req.limit
        );
        Ok(r)
    }

    async fn count(&mut self, t: &TableRef, req: &BrowseRequest, _c: &[ColumnInfo]) -> Result<u64> {
        let filter = filter_doc(req);
        let coll = self.db().collection::<Document>(&t.name);
        if filter.is_empty() {
            Ok(coll.estimated_document_count().await?)
        } else {
            Ok(coll.count_documents(filter).await?)
        }
    }

    fn preview_changes(&self, t: &TableRef, changes: &[RowChange]) -> Vec<String> {
        preview(t, changes)
    }

    async fn apply_changes(&mut self, t: &TableRef, changes: &[RowChange]) -> Result<()> {
        let coll = self.db().collection::<Document>(&t.name);
        for c in changes {
            match c {
                RowChange::Update { key, set } => {
                    let mut s = Document::new();
                    for (k, v) in set {
                        s.insert(k.clone(), edit_bson(v));
                    }
                    coll.update_one(key_filter(key), doc! { "$set": s }).await?;
                }
                RowChange::Insert { values } => {
                    let mut d = Document::new();
                    for (k, v) in values.iter().filter(|(_, v)| *v != EditValue::Default) {
                        d.insert(k.clone(), edit_bson(v));
                    }
                    coll.insert_one(d).await?;
                }
                RowChange::Delete { key } => {
                    coll.delete_one(key_filter(key)).await?;
                }
            }
        }
        Ok(())
    }

    async fn processes(&mut self) -> Result<QueryResult> {
        let res = self.client.database("admin").run_command(doc! { "currentOp": 1 }).await?;
        let ops: Vec<Document> = res.get_array("inprog").map(|a| a.iter().filter_map(|b| b.as_document().cloned()).collect()).unwrap_or_default();
        let slim = ops
            .into_iter()
            .map(|o| {
                let mut d = Document::new();
                for k in ["opid", "active", "op", "ns", "secs_running", "client", "desc"] {
                    if let Some(v) = o.get(k) {
                        d.insert(k, v.clone());
                    }
                }
                d
            })
            .collect();
        Ok(docs_result(slim))
    }

    async fn kill_process(&mut self, id: &str) -> Result<()> {
        let op = id.parse::<i64>().map(Bson::Int64).unwrap_or_else(|_| Bson::String(id.into()));
        self.client.database("admin").run_command(doc! { "killOp": 1, "op": op }).await?;
        Ok(())
    }

    async fn users(&mut self) -> Result<QueryResult> {
        let res = self.db().run_command(doc! { "usersInfo": 1 }).await?;
        let users: Vec<Document> = res.get_array("users").map(|a| a.iter().filter_map(|b| b.as_document().cloned()).collect()).unwrap_or_default();
        Ok(docs_result(users))
    }
}

impl MongoDriver {
    async fn run_one(&mut self, stmt: &str) -> Result<QueryResult> {
        let lower = stmt.to_ascii_lowercase();
        if lower == "show dbs" || lower == "show databases" {
            let names = self.client.list_database_names().await?;
            return Ok(QueryResult { columns: vec!["database".into()], rows: names.into_iter().map(|n| vec![Some(n)]).collect(), ..Default::default() });
        }
        if lower == "show collections" || lower == "show tables" {
            let names = self.db().list_collection_names().await?;
            return Ok(QueryResult { columns: vec!["collection".into()], rows: names.into_iter().map(|n| vec![Some(n)]).collect(), ..Default::default() });
        }
        if let Some(db) = lower.strip_prefix("use ").map(|_| stmt[4..].trim().to_string()) {
            self.database = db;
            return Ok(QueryResult::message(format!("switched to db {}", self.database)));
        }
        let rest = stmt.strip_prefix("db.").ok_or_else(|| anyhow::anyhow!("Expected a statement like db.collection.find({{}})"))?;
        let calls = split_calls(rest)?;
        let (first, first_args) = calls.first().cloned().ok_or_else(|| anyhow::anyhow!("Nothing to run"))?;

        // db.runCommand({...}) / db.getCollection("x").find()
        let (collection, calls) = if first == "runCommand" || first == "adminCommand" {
            let cmd = arg_doc(&parse_args(&first_args)?, 0);
            let target = if first == "adminCommand" { self.client.database("admin") } else { self.db() };
            return Ok(json_result(Bson::Document(target.run_command(cmd).await?)));
        } else if first == "getCollection" {
            let name = parse_args(&first_args)?.first().and_then(|b| b.as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("getCollection needs a name"))?;
            (name, calls[1..].to_vec())
        } else {
            // "users.find" → collection "users", method "find"
            let (coll, method) = first.rsplit_once('.').ok_or_else(|| anyhow::anyhow!("Expected db.<collection>.<method>(...)"))?;
            let mut v = vec![(method.to_string(), first_args)];
            v.extend(calls[1..].iter().cloned());
            (coll.to_string(), v)
        };
        let (method, raw) = calls.first().cloned().ok_or_else(|| anyhow::anyhow!("Missing method"))?;
        let args = parse_args(&raw)?;
        let coll = self.db().collection::<Document>(&collection);
        let modifiers = &calls[1..];
        let mut sort = None;
        let mut limit = None;
        let mut skip = None;
        for (m, a) in modifiers {
            let a = parse_args(a)?;
            match m.as_str() {
                "sort" => sort = Some(arg_doc(&a, 0)),
                "limit" => limit = a.first().and_then(|b| b.as_i64().or(b.as_i32().map(i64::from)).or(b.as_f64().map(|f| f as i64))),
                "skip" => skip = a.first().and_then(|b| b.as_i64().or(b.as_i32().map(i64::from)).or(b.as_f64().map(|f| f as i64))),
                "toArray" | "pretty" => {}
                other => anyhow::bail!("Unsupported cursor method .{other}()"),
            }
        }
        Ok(match method.as_str() {
            "find" => {
                let mut f = coll.find(arg_doc(&args, 0)).limit(limit.unwrap_or(1000));
                if let Some(Bson::Document(p)) = args.get(1) {
                    f = f.projection(p.clone());
                }
                if let Some(s) = sort {
                    f = f.sort(s);
                }
                if let Some(s) = skip {
                    f = f.skip(s as u64);
                }
                docs_result(f.await?.try_collect().await?)
            }
            "findOne" => docs_result(coll.find_one(arg_doc(&args, 0)).await?.into_iter().collect()),
            "aggregate" => {
                let pipeline: Vec<Document> = match args.first() {
                    Some(Bson::Array(a)) => a.iter().filter_map(|b| b.as_document().cloned()).collect(),
                    _ => args.iter().filter_map(|b| b.as_document().cloned()).collect(),
                };
                docs_result(coll.aggregate(pipeline).await?.try_collect().await?)
            }
            "countDocuments" | "count" => json_result(Bson::Int64(coll.count_documents(arg_doc(&args, 0)).await? as i64)),
            "estimatedDocumentCount" => json_result(Bson::Int64(coll.estimated_document_count().await? as i64)),
            "distinct" => {
                let field = args.first().and_then(|b| b.as_str()).unwrap_or("_id").to_string();
                let vals = coll.distinct(field, arg_doc(&args, 1)).await?;
                QueryResult { columns: vec!["value".into()], rows: vals.iter().map(|v| vec![bson_cell(v)]).collect(), ..Default::default() }
            }
            "insertOne" => {
                let r = coll.insert_one(arg_doc(&args, 0)).await?;
                json_result(Bson::Document(doc! { "insertedId": r.inserted_id }))
            }
            "insertMany" => {
                let docs: Vec<Document> = match args.first() {
                    Some(Bson::Array(a)) => a.iter().filter_map(|b| b.as_document().cloned()).collect(),
                    _ => vec![],
                };
                let r = coll.insert_many(docs).await?;
                QueryResult { affected: Some(r.inserted_ids.len() as u64), ..Default::default() }
            }
            "updateOne" | "updateMany" | "replaceOne" => {
                let (f, u) = (arg_doc(&args, 0), arg_doc(&args, 1));
                let r = match method.as_str() {
                    "updateOne" => coll.update_one(f, u).await?,
                    "updateMany" => coll.update_many(f, u).await?,
                    _ => coll.replace_one(f, u).await?,
                };
                QueryResult { affected: Some(r.modified_count), message: Some(format!("matched {}, modified {}", r.matched_count, r.modified_count)), ..Default::default() }
            }
            "deleteOne" => QueryResult { affected: Some(coll.delete_one(arg_doc(&args, 0)).await?.deleted_count), ..Default::default() },
            "deleteMany" => QueryResult { affected: Some(coll.delete_many(arg_doc(&args, 0)).await?.deleted_count), ..Default::default() },
            "drop" => {
                coll.drop().await?;
                QueryResult::message(format!("dropped {collection}"))
            }
            "createIndex" => {
                let opts = arg_doc(&args, 1);
                let options = if opts.is_empty() { None } else { Some(bson::from_document::<mongodb::options::IndexOptions>(opts).map_err(|e| anyhow::anyhow!("Invalid index options: {e}"))?) };
                let model = mongodb::IndexModel::builder().keys(arg_doc(&args, 0)).options(options).build();
                let r = coll.create_index(model).await?;
                QueryResult::message(format!("created index {}", r.index_name))
            }
            "dropIndex" => {
                let name = args.first().and_then(|b| b.as_str()).ok_or_else(|| anyhow::anyhow!("dropIndex needs an index name"))?;
                coll.drop_index(name).await?;
                QueryResult::message(format!("dropped index {name}"))
            }
            other => anyhow::bail!("Unsupported method {other}()"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::oid::ObjectId;

    #[test]
    fn splits_call_chain_with_nested_parens_and_strings() {
        let calls = split_calls(r#"users.find({name: "a(b)"}, {x: 1}).sort({n: -1}).limit(5)"#).unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, "users.find");
        assert_eq!(calls[0].1, r#"{name: "a(b)"}, {x: 1}"#);
        assert_eq!(calls[2], ("limit".to_string(), "5".to_string()));
    }

    #[test]
    fn parses_shell_values() {
        assert!(matches!(parse_value("ObjectId('64b7f0c2a1b2c3d4e5f60718')"), Bson::ObjectId(_)));
        assert_eq!(parse_value("42"), Bson::Int32(42));
        assert_eq!(parse_value("hello world"), Bson::String("hello world".into()));
        let d = parse_value("{a: 1, b: 'x'}");
        assert_eq!(d.as_document().and_then(|d| d.get_str("b").ok()), Some("x"));
    }

    #[test]
    fn object_id_cell_round_trips_through_parser() {
        let oid = ObjectId::new();
        let cell = bson_cell(&Bson::ObjectId(oid)).unwrap();
        assert_eq!(parse_value(&cell), Bson::ObjectId(oid));
    }

    #[test]
    fn sampled_fields_report_bson_types_and_optional_fields() {
        let f = sampled_fields(&[doc! { "_id": 1, "age": 3, "tag": "a" }, doc! { "_id": 2, "age": "3", "tag": Bson::Null }]);
        assert_eq!(f.iter().map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable)).collect::<Vec<_>>(), [("_id", "int", false), ("age", "int | string", false), ("tag", "string", true)]);
        assert!(f[0].primary_key);
    }

    #[test]
    fn docs_result_unions_keys_with_id_first() {
        let r = docs_result(vec![doc! { "name": "a", "_id": 1 }, doc! { "_id": 2, "age": 3 }]);
        assert_eq!(r.columns, vec!["_id", "name", "age"]);
        assert_eq!(r.rows[1], vec![Some("2".into()), None, Some("3".into())]);
    }

    #[test]
    fn filter_builds_and_of_conditions() {
        let req = BrowseRequest {
            filters: vec![
                Filter { column: Some("age".into()), op: FilterOp::Gt, value: "30".into(), ..Default::default() },
                Filter { column: Some("name".into()), op: FilterOp::HasPrefix, value: "J.".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let f = filter_doc(&req);
        let and = f.get_array("$and").unwrap();
        assert_eq!(and[0].as_document().unwrap().get_document("age").unwrap().get("$gt"), Some(&Bson::Int32(30)));
        assert_eq!(and[1].as_document().unwrap().get_document("name").unwrap().get_str("$regex").unwrap(), "^J\\.");
    }
}
