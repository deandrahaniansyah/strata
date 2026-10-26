//! SQL text generation: identifier/literal quoting, browse queries, row edits and DDL.

use super::*;
use crate::model::Dialect;

pub fn quote_ident(d: Dialect, name: &str) -> String {
    match d {
        Dialect::MySql => format!("`{}`", name.replace('`', "``")),
        Dialect::SqlServer => format!("[{}]", name.replace(']', "]]")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

pub fn lit(d: Dialect, value: &str) -> String {
    let mut s = value.replace('\'', "''");
    if d == Dialect::Postgres && value.contains('\\') { return format!("E'{}'", s.replace('\\', "\\\\")); }
    if matches!(d, Dialect::MySql | Dialect::ClickHouse) {
        s = s.replace('\\', "\\\\");
        // the doubled quote above is still valid; backslashes are escapes in these engines
    }
    if d == Dialect::SqlServer && !value.is_ascii() {
        return format!("N'{s}'");
    }
    format!("'{s}'")
}

pub fn cell_lit(d: Dialect, v: &Cell) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => lit(d, s),
    }
}

pub fn edit_lit(d: Dialect, v: &EditValue) -> String {
    match v {
        EditValue::Null => "NULL".into(),
        EditValue::Default => "DEFAULT".into(),
        EditValue::Text(s) => lit(d, s),
    }
}

pub fn qualified(d: Dialect, t: &TableRef) -> String {
    let omit_schema = t.schema.is_empty() || (d == Dialect::Sqlite && t.schema == "main") || d == Dialect::Elastic;
    if omit_schema {
        quote_ident(d, &t.name)
    } else {
        format!("{}.{}", quote_ident(d, &t.schema), quote_ident(d, &t.name))
    }
}

pub fn ellipsize(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect::<String>() + "…"
    }
}

fn text_cast(d: Dialect, col: &str) -> String {
    match d {
        Dialect::Postgres => format!("{col}::text"),
        Dialect::MySql => format!("CAST({col} AS CHAR)"),
        Dialect::SqlServer => format!("CAST({col} AS NVARCHAR(MAX))"),
        Dialect::ClickHouse => format!("toString({col})"),
        Dialect::Oracle => format!("CAST({col} AS VARCHAR2(4000))"),
        _ => format!("CAST({col} AS TEXT)"),
    }
}

fn like_escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn like(d: Dialect, col: &str, pattern: &str, negate: bool, insensitive: bool) -> String {
    let not = if negate { "NOT " } else { "" };
    let lhs = text_cast(d, col);
    let escape = if matches!(d, Dialect::Sqlite | Dialect::SqlServer | Dialect::Oracle) { " ESCAPE '\\'" } else { "" };
    match d {
        Dialect::Postgres if insensitive => format!("{lhs} {not}ILIKE {}", lit(d, pattern)),
        Dialect::ClickHouse if insensitive => format!("{lhs} {not}ILIKE {}", lit(d, pattern)),
        _ if insensitive => format!("LOWER({lhs}) {not}LIKE LOWER({}){escape}", lit(d, pattern)),
        _ => format!("{lhs} {not}LIKE {}{escape}", lit(d, pattern)),
    }
}

fn list(d: Dialect, v: &str) -> String {
    v.split(',').map(|x| lit(d, x.trim())).collect::<Vec<_>>().join(", ")
}

fn one_filter(d: Dialect, col: &str, f: &Filter) -> String {
    let c = quote_ident(d, col);
    let v = &f.value;
    match f.op {
        FilterOp::Eq => format!("{c} = {}", lit(d, v)),
        FilterOp::Ne => format!("{c} <> {}", lit(d, v)),
        FilterOp::Lt => format!("{c} < {}", lit(d, v)),
        FilterOp::Gt => format!("{c} > {}", lit(d, v)),
        FilterOp::Le => format!("{c} <= {}", lit(d, v)),
        FilterOp::Ge => format!("{c} >= {}", lit(d, v)),
        FilterOp::In => format!("{c} IN ({})", list(d, v)),
        FilterOp::NotIn => format!("{c} NOT IN ({})", list(d, v)),
        FilterOp::IsNull => format!("{c} IS NULL"),
        FilterOp::IsNotNull => format!("{c} IS NOT NULL"),
        FilterOp::Between => format!("{c} BETWEEN {} AND {}", lit(d, v), lit(d, &f.value2)),
        FilterOp::NotBetween => format!("{c} NOT BETWEEN {} AND {}", lit(d, v), lit(d, &f.value2)),
        FilterOp::Like => like(d, &c, v, false, false),
        FilterOp::NotLike => like(d, &c, v, true, false),
        FilterOp::ILike => like(d, &c, v, false, true),
        FilterOp::Contains => like(d, &c, &format!("%{}%", like_escape(v)), false, true),
        FilterOp::NotContains => like(d, &c, &format!("%{}%", like_escape(v)), true, true),
        FilterOp::HasPrefix => like(d, &c, &format!("{}%", like_escape(v)), false, false),
        FilterOp::HasSuffix => like(d, &c, &format!("%{}", like_escape(v)), false, false),
        FilterOp::Raw => format!("({v})"),
    }
}

pub fn where_clause(d: Dialect, req: &BrowseRequest, columns: &[ColumnInfo]) -> String {
    let parts: Vec<String> = req
        .filters
        .iter()
        .filter(|f| f.enabled && (!f.op.needs_value() || !f.value.is_empty() || f.op == FilterOp::Eq))
        .filter_map(|f| match &f.column {
            _ if f.op == FilterOp::Raw => (!f.value.trim().is_empty()).then(|| format!("({})", f.value)),
            Some(c) => Some(one_filter(d, c, f)),
            None if columns.is_empty() => None,
            // "Any column": OR over every column.
            None => Some(format!("({})", columns.iter().map(|c| one_filter(d, &c.name, f)).collect::<Vec<_>>().join(" OR "))),
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", parts.join(if req.match_any { " OR " } else { " AND " }))
    }
}

pub fn order_clause(d: Dialect, req: &BrowseRequest) -> String {
    if req.sort.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = req.sort.iter().map(|(c, asc)| format!("{} {}", quote_ident(d, c), if *asc { "ASC" } else { "DESC" })).collect();
    format!(" ORDER BY {}", parts.join(", "))
}

pub fn browse_sql(d: Dialect, t: &TableRef, req: &BrowseRequest, columns: &[ColumnInfo]) -> String {
    let table = qualified(d, t);
    let w = where_clause(d, req, columns);
    let o = order_clause(d, req);
    let projection = if columns.is_empty() { "*".into() } else { columns.iter().map(|c| quote_ident(d, &c.name)).collect::<Vec<_>>().join(", ") };
    match d {
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        Dialect::Oracle => format!("SELECT {projection} FROM {table}{w}{o} OFFSET {} ROWS FETCH NEXT {} ROWS ONLY", req.offset, req.limit),
        Dialect::SqlServer => {
            // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
            let o = if o.is_empty() { " ORDER BY (SELECT NULL)".to_string() } else { o };
            // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
            format!("SELECT {projection} FROM {table}{w}{o} OFFSET {} ROWS FETCH NEXT {} ROWS ONLY", req.offset, req.limit)
        }
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        _ => format!("SELECT {projection} FROM {table}{w}{o} LIMIT {} OFFSET {}", req.limit, req.offset),
    }
}

pub fn count_sql(d: Dialect, t: &TableRef, req: &BrowseRequest, columns: &[ColumnInfo]) -> String {
    // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
    format!("SELECT COUNT(*) FROM {}{}", qualified(d, t), where_clause(d, req, columns))
}

fn key_condition(d: Dialect, key: &[(String, Cell)]) -> String {
    key.iter()
        .map(|(c, v)| match v {
            None => format!("{} IS NULL", quote_ident(d, c)),
            Some(_) => format!("{} = {}", quote_ident(d, c), cell_lit(d, v)),
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

pub fn change_sql(d: Dialect, t: &TableRef, change: &RowChange) -> String {
    let table = qualified(d, t);
    match change {
        RowChange::Update { key, set } => {
            let assignments = set.iter().map(|(c, v)| format!("{} = {}", quote_ident(d, c), edit_lit(d, v))).collect::<Vec<_>>().join(", ");
            match d {
                Dialect::ClickHouse => format!("ALTER TABLE {table} UPDATE {assignments} WHERE {}", key_condition(d, key)),
                // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
                _ => format!("UPDATE {table} SET {assignments} WHERE {}", key_condition(d, key)),
            }
        }
        RowChange::Insert { values } => {
            let explicit: Vec<&(String, EditValue)> = values.iter().filter(|(_, v)| *v != EditValue::Default).collect();
            if explicit.is_empty() {
                return match d {
                    // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
                    Dialect::MySql => format!("INSERT INTO {table} () VALUES ()"),
                    // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
                    _ => format!("INSERT INTO {table} DEFAULT VALUES"),
                };
            }
            let cols = explicit.iter().map(|(c, _)| quote_ident(d, c)).collect::<Vec<_>>().join(", ");
            let vals = explicit.iter().map(|(_, v)| edit_lit(d, v)).collect::<Vec<_>>().join(", ");
            // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
            format!("INSERT INTO {table} ({cols}) VALUES ({vals})")
        }
        RowChange::Delete { key } => match d {
            Dialect::ClickHouse => format!("ALTER TABLE {table} DELETE WHERE {}", key_condition(d, key)),
            // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
            _ => format!("DELETE FROM {table} WHERE {}", key_condition(d, key)),
        },
    }
}

pub fn transaction_words(d: Dialect) -> (Option<&'static str>, Option<&'static str>, Option<&'static str>) {
    match d {
        Dialect::Postgres | Dialect::Sqlite => (Some("BEGIN"), Some("COMMIT"), Some("ROLLBACK")),
        Dialect::MySql => (Some("START TRANSACTION"), Some("COMMIT"), Some("ROLLBACK")),
        Dialect::SqlServer => (Some("BEGIN TRANSACTION"), Some("COMMIT TRANSACTION"), Some("ROLLBACK TRANSACTION")),
        _ => (None, None, None),
    }
}

pub fn insert_statement(d: Dialect, t: &TableRef, columns: &[String], rows: &[Vec<Cell>]) -> String {
    let cols = columns.iter().map(|c| quote_ident(d, c)).collect::<Vec<_>>().join(", ");
    let values = rows
        .iter()
        .map(|r| format!("({})", r.iter().map(|v| cell_lit(d, v)).collect::<Vec<_>>().join(", ")))
        .collect::<Vec<_>>()
        .join(",\n  ");
    // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
    format!("INSERT INTO {} ({cols}) VALUES\n  {values};", qualified(d, t))
}

fn column_def(d: Dialect, c: &ColumnInfo, inline_pk: bool) -> String {
    let mut s = format!("{} {}", quote_ident(d, &c.name), if c.data_type.is_empty() { "text" } else { &c.data_type });
    if !c.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(def) = &c.default {
        s.push_str(&format!(" DEFAULT {def}"));
    }
    if d == Dialect::MySql && !c.extra.is_empty() && c.extra.to_ascii_lowercase().contains("auto_increment") {
        s.push_str(" AUTO_INCREMENT");
    }
    if inline_pk {
        s.push_str(" PRIMARY KEY");
    }
    s
}

pub fn create_table_ddl(d: Dialect, t: &TableRef, cols: &[ColumnInfo], indexes: &[IndexInfo]) -> String {
    let pks: Vec<&ColumnInfo> = cols.iter().filter(|c| c.primary_key).collect();
    let mut lines: Vec<String> = cols.iter().map(|c| format!("  {}", column_def(d, c, false))).collect();
    if !pks.is_empty() {
        lines.push(format!("  PRIMARY KEY ({})", pks.iter().map(|c| quote_ident(d, &c.name)).collect::<Vec<_>>().join(", ")));
    }
    let mut out = format!("CREATE TABLE {} (\n{}\n);", qualified(d, t), lines.join(",\n"));
    for i in indexes.iter().filter(|i| !i.primary) {
        out.push_str(&format!(
            "\nCREATE {}INDEX {} ON {} ({});",
            if i.unique { "UNIQUE " } else { "" },
            quote_ident(d, &i.name),
            qualified(d, t),
            i.columns
        ));
    }
    if d == Dialect::Postgres {
        for c in cols.iter().filter(|c| c.comment.is_some()) {
            out.push_str(&format!(
                "\nCOMMENT ON COLUMN {}.{} IS {};",
                qualified(d, t),
                quote_ident(d, &c.name),
                lit(d, c.comment.as_deref().unwrap_or(""))
            ));
        }
    }
    out
}

/// One edit made in the structure editor.
#[derive(Debug, Clone, PartialEq)]
pub enum StructureChange {
    AddColumn(ColumnInfo),
    DropColumn(String),
    AlterColumn { original: ColumnInfo, updated: ColumnInfo },
    /// `method` is one of `meta::structure_form(d).index_methods`; empty means the engine default.
    AddIndex { name: String, columns: Vec<String>, unique: bool, method: String },
    DropIndex(String),
    /// `name` is the constraint to drop; MySQL ignores it.
    DropPrimaryKey { name: String },
    AddPrimaryKey { columns: Vec<String> },
    /// `on_delete` / `on_update` are one of `meta::structure_form(d).fk_actions`; empty means the default.
    AddForeignKey { name: String, column: String, ref_table: TableRef, ref_column: String, on_delete: String, on_update: String },
    DropForeignKey(String),
}

pub fn structure_sql(d: Dialect, t: &TableRef, change: &StructureChange) -> Vec<String> {
    if d == Dialect::Mongo {
        return mongo_structure(t, change);
    }
    let table = qualified(d, t);
    let q = |n: &str| quote_ident(d, n);
    match change {
        StructureChange::AddColumn(c) => {
            let mut def = column_def(d, c, false);
            if let (Some(comment), Dialect::MySql | Dialect::ClickHouse) = (&c.comment, d) {
                def.push_str(&format!(" COMMENT {}", lit(d, comment)));
            }
            let mut out = vec![format!("ALTER TABLE {table} ADD {}{def}", if d == Dialect::Postgres { "COLUMN " } else { "" })];
            if let (Some(comment), Dialect::Postgres) = (&c.comment, d) {
                out.push(format!("COMMENT ON COLUMN {table}.{} IS {}", q(&c.name), lit(d, comment)));
            }
            out
        }
        StructureChange::DropColumn(c) => vec![format!("ALTER TABLE {table} DROP COLUMN {}", q(c))],
        StructureChange::AddIndex { name, columns, unique, method } => {
            let cols = columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
            let unique = if *unique { "UNIQUE " } else { "" };
            vec![match (d, method.as_str()) {
                (_, "") => format!("CREATE {unique}INDEX {} ON {table} ({cols})", q(name)),
                (Dialect::Postgres, m) => format!("CREATE {unique}INDEX {} ON {table} USING {m} ({cols})", q(name)),
                (Dialect::MySql, m @ ("FULLTEXT" | "SPATIAL")) => format!("CREATE {m} INDEX {} ON {table} ({cols})", q(name)),
                (Dialect::MySql, m) => format!("CREATE {unique}INDEX {} ON {table} ({cols}) USING {m}", q(name)),
                (Dialect::SqlServer, m) => format!("CREATE {unique}{m} INDEX {} ON {table} ({cols})", q(name)),
                (Dialect::ClickHouse, m) => format!("ALTER TABLE {table} ADD INDEX {} ({cols}) TYPE {m} GRANULARITY 1", q(name)),
                _ => format!("CREATE {unique}INDEX {} ON {table} ({cols})", q(name)),
            }]
        }
        StructureChange::DropIndex(name) => vec![match d {
            Dialect::ClickHouse => format!("ALTER TABLE {table} DROP INDEX {}", q(name)),
            Dialect::MySql | Dialect::SqlServer => format!("DROP INDEX {} ON {table}", q(name)),
            Dialect::Postgres if !t.schema.is_empty() => format!("DROP INDEX {}.{}", q(&t.schema), q(name)),
            _ => format!("DROP INDEX {}", q(name)),
        }],
        StructureChange::DropPrimaryKey { name } => vec![match d {
            Dialect::MySql => format!("ALTER TABLE {table} DROP PRIMARY KEY"),
            _ => format!("ALTER TABLE {table} DROP CONSTRAINT {}", q(name)),
        }],
        StructureChange::AddPrimaryKey { columns } => {
            vec![format!("ALTER TABLE {table} ADD PRIMARY KEY ({})", columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", "))]
        }
        StructureChange::AddForeignKey { name, column, ref_table, ref_column, on_delete, on_update } => {
            let action = |clause: &str, a: &str| if a.is_empty() { String::new() } else { format!(" {clause} {a}") };
            vec![format!(
                "ALTER TABLE {table} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}){}{}",
                q(name),
                q(column),
                qualified(d, ref_table),
                q(ref_column),
                action("ON DELETE", on_delete),
                action("ON UPDATE", on_update)
            )]
        }
        StructureChange::DropForeignKey(name) => vec![match d {
            Dialect::MySql => format!("ALTER TABLE {table} DROP FOREIGN KEY {}", q(name)),
            _ => format!("ALTER TABLE {table} DROP CONSTRAINT {}", q(name)),
        }],
        StructureChange::AlterColumn { original: o, updated: u } => {
            let mut out = Vec::new();
            let name = if o.name != u.name {
                out.push(match d {
                    Dialect::SqlServer => format!("EXEC sp_rename {}, {}, 'COLUMN'", lit(d, &format!("{}.{}", qualified(d, t), o.name)), lit(d, &u.name)),
                    _ => format!("ALTER TABLE {table} RENAME COLUMN {} TO {}", q(&o.name), q(&u.name)),
                });
                u.name.clone()
            } else {
                o.name.clone()
            };
            let type_or_null_changed = o.data_type != u.data_type || o.nullable != u.nullable;
            match d {
                Dialect::Postgres => {
                    if o.data_type != u.data_type {
                        out.push(format!("ALTER TABLE {table} ALTER COLUMN {} TYPE {} USING {}::{}", q(&name), u.data_type, q(&name), u.data_type));
                    }
                    if o.nullable != u.nullable {
                        out.push(format!("ALTER TABLE {table} ALTER COLUMN {} {} NOT NULL", q(&name), if u.nullable { "DROP" } else { "SET" }));
                    }
                    if o.default != u.default {
                        out.push(match &u.default {
                            Some(v) => format!("ALTER TABLE {table} ALTER COLUMN {} SET DEFAULT {v}", q(&name)),
                            None => format!("ALTER TABLE {table} ALTER COLUMN {} DROP DEFAULT", q(&name)),
                        });
                    }
                    if o.comment != u.comment {
                        out.push(format!(
                            "COMMENT ON COLUMN {table}.{} IS {}",
                            q(&name),
                            u.comment.as_deref().map(|c| lit(d, c)).unwrap_or_else(|| "NULL".into())
                        ));
                    }
                }
                Dialect::MySql => {
                    if type_or_null_changed || o.default != u.default || o.comment != u.comment {
                        let mut def = column_def(d, &ColumnInfo { name: name.clone(), ..u.clone() }, false);
                        if let Some(c) = &u.comment {
                            def.push_str(&format!(" COMMENT {}", lit(d, c)));
                        }
                        out.push(format!("ALTER TABLE {table} MODIFY COLUMN {def}"));
                    }
                }
                Dialect::SqlServer => {
                    if type_or_null_changed {
                        out.push(format!("ALTER TABLE {table} ALTER COLUMN {} {}{}", q(&name), u.data_type, if u.nullable { " NULL" } else { " NOT NULL" }));
                    }
                }
                Dialect::ClickHouse => {
                    if o.data_type != u.data_type || (o.default != u.default && u.default.is_some()) {
                        let default = u.default.as_deref().map(|v| format!(" DEFAULT {v}")).unwrap_or_default();
                        out.push(format!("ALTER TABLE {table} MODIFY COLUMN {} {}{default}", q(&name), u.data_type));
                    }
                    if o.default.is_some() && u.default.is_none() {
                        out.push(format!("ALTER TABLE {table} MODIFY COLUMN {} REMOVE DEFAULT", q(&name)));
                    }
                    if o.comment != u.comment {
                        out.push(format!("ALTER TABLE {table} COMMENT COLUMN {} {}", q(&name), lit(d, u.comment.as_deref().unwrap_or(""))));
                    }
                }
                // SQLite cannot alter a column's type in place; we surface that rather than rebuild silently.
                _ => {
                    if type_or_null_changed || o.default != u.default {
                        out.push(format!("-- {} cannot change column type/nullability/default in place; recreate the table", "SQLite"));
                    }
                }
            }
            out
        }
    }
}

/// Mongo shell statements for index changes; fields are schemaless, so column changes produce nothing.
/// Index columns read `field` (uses `method`) or `field:-1` (per-field key).
fn mongo_structure(t: &TableRef, change: &StructureChange) -> Vec<String> {
    let coll = serde_json::Value::String(t.name.clone());
    match change {
        StructureChange::AddIndex { name, columns, unique, method } => {
            let mut keys = serde_json::Map::new();
            for c in columns {
                let (field, kind) = c.split_once(':').map(|(f, k)| (f.trim(), k.trim())).unwrap_or((c.trim(), method.as_str()));
                let kind = if kind.is_empty() { "1" } else { kind };
                let value = kind.parse::<i64>().map(serde_json::Value::from).unwrap_or_else(|_| kind.into());
                keys.insert(field.to_string(), value);
            }
            let mut opts = serde_json::json!({ "name": name });
            if *unique {
                opts["unique"] = true.into();
            }
            vec![format!("db.getCollection({coll}).createIndex({}, {opts})", serde_json::Value::Object(keys))]
        }
        StructureChange::DropIndex(name) => vec![format!("db.getCollection({coll}).dropIndex({})", serde_json::Value::String(name.clone()))],
        _ => vec![],
    }
}

pub fn truncate_sql(d: Dialect, t: &TableRef, cascade: bool, restart_identity: bool) -> String {
    let table = qualified(d, t);
    match d {
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        Dialect::Sqlite => format!("DELETE FROM {table}"),
        Dialect::Postgres => format!(
            "TRUNCATE TABLE {table}{}{}",
            if restart_identity { " RESTART IDENTITY" } else { "" },
            if cascade { " CASCADE" } else { "" }
        ),
        _ => format!("TRUNCATE TABLE {table}"),
    }
}

pub fn drop_sql(d: Dialect, obj: &DbObject, cascade: bool) -> String {
    if let Some(sql) = super::objects::drop_sql(d, obj, cascade) { return sql; }
    let t = TableRef { schema: obj.schema.clone(), name: obj.name.clone() };
    let kw = match obj.kind {
        ObjectKind::View => "VIEW",
        ObjectKind::MaterializedView => "MATERIALIZED VIEW",
        ObjectKind::Function => "FUNCTION",
        ObjectKind::Procedure => "PROCEDURE",
        ObjectKind::Sequence => "SEQUENCE",
        _ => "TABLE",
    };
    let cascade = if cascade && d == Dialect::Postgres { " CASCADE" } else { "" };
    format!("DROP {kw} {}{cascade}", qualified(d, &t))
}

pub fn rename_sql(d: Dialect, t: &TableRef, new_name: &str) -> String {
    match d {
        Dialect::MySql => format!("RENAME TABLE {} TO {}", qualified(d, t), qualified(d, &TableRef { schema: t.schema.clone(), name: new_name.into() })),
        Dialect::SqlServer => format!("EXEC sp_rename {}, {}", lit(d, &format!("{}.{}", t.schema, t.name)), lit(d, new_name)),
        Dialect::ClickHouse => format!("RENAME TABLE {} TO {}", qualified(d, t), qualified(d, &TableRef { schema: t.schema.clone(), name: new_name.into() })),
        _ => format!("ALTER TABLE {} RENAME TO {}", qualified(d, t), quote_ident(d, new_name)),
    }
}

pub fn clone_table_sql(d: Dialect, t: &TableRef, new_name: &str, with_data: bool) -> Vec<String> {
    let dst = qualified(d, &TableRef { schema: t.schema.clone(), name: new_name.into() });
    let src = qualified(d, t);
    match d {
        Dialect::Postgres => {
            let mut v = vec![format!("CREATE TABLE {dst} (LIKE {src} INCLUDING ALL)")];
            if with_data {
                // unslop-ignore NO_SELECT_STAR -- Table-clone SQL intentionally copies the complete source schema; both identifiers are quoted and execution requires user approval.
                // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
                v.push(format!("INSERT INTO {dst} SELECT * FROM {src}")); // unslop-ignore NO_SELECT_STAR -- Explicit whole-table clone; quoted identifiers and user-requested full data copy.
            }
            v
        }
        Dialect::MySql => {
            let mut v = vec![format!("CREATE TABLE {dst} LIKE {src}")];
            if with_data {
                // unslop-ignore NO_SELECT_STAR -- Table-clone SQL intentionally copies the complete source schema; both identifiers are quoted and execution requires user approval.
                // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
                v.push(format!("INSERT INTO {dst} SELECT * FROM {src}")); // unslop-ignore NO_SELECT_STAR -- Explicit whole-table clone; quoted identifiers and user-requested full data copy.
            }
            v
        }
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        Dialect::SqlServer => vec![format!("SELECT * INTO {dst} FROM {src}{}", if with_data { "" } else { " WHERE 1 = 0" })],
        // unslop-ignore NO_SELECT_STAR -- Table-clone SQL intentionally copies the complete source schema; both identifiers are quoted and execution requires user approval.
        _ => vec![format!("CREATE TABLE {dst} AS SELECT * FROM {src}{}", if with_data { "" } else { " WHERE 0" })],
    }
}

/// Keep database selection and first collection creation in one serialized driver job.
pub fn mongo_create_database(name: &str, collection: &str) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(!name.is_empty() && name.len() <= 63 && !name.chars().any(|c| c.is_whitespace() || c.is_control() || "/\\.\"$<>:?*|;".contains(c)), "Database name must be 1–63 bytes without spaces or reserved characters");
    let collection = if collection.is_empty() { "documents" } else { collection };
    anyhow::ensure!(!collection.contains('\0') && !collection.starts_with("system."), "Choose a collection name without NUL or the system. prefix");
    let command = serde_json::json!({"create": collection});
    Ok(vec![format!("use {name}"), format!("db.runCommand({command})")])
}

pub fn create_database_sql(d: Dialect, name: &str, encoding: &str) -> String {
    let n = quote_ident(d, name);
    match d {
        Dialect::Postgres if !encoding.is_empty() => format!("CREATE DATABASE {n} ENCODING {}", lit(d, encoding)),
        Dialect::MySql if !encoding.is_empty() => format!("CREATE DATABASE {n} CHARACTER SET {}", quote_ident(d, encoding)),
        _ => format!("CREATE DATABASE {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> TableRef {
        TableRef { schema: "public".into(), name: "users".into() }
    }

    #[test]
    fn mongo_database_creation_selects_before_creating_and_rejects_commands() {
        assert_eq!(mongo_create_database("inventory", "").unwrap(), ["use inventory", "db.runCommand({\"create\":\"documents\"})"]);
        assert!(mongo_create_database("shop; db.dropDatabase()", "").is_err());
        assert!(mongo_create_database("shop", "system.users").is_err());
        assert_eq!(mongo_create_database("shop", "a\"b").unwrap()[1], "db.runCommand({\"create\":\"a\\\"b\"})");
    }

    #[test]
    fn quotes_identifiers_per_dialect() {
        assert_eq!(quote_ident(Dialect::Postgres, "we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_ident(Dialect::MySql, "a`b"), "`a``b`");
        assert_eq!(quote_ident(Dialect::SqlServer, "a]b"), "[a]]b]");
    }

    #[test]
    fn escapes_literals_including_mysql_backslashes() {
        assert_eq!(lit(Dialect::Postgres, "O'Brien\\"), "E'O''Brien\\\\'");
        assert_eq!(lit(Dialect::MySql, "a\\'"), "'a\\\\'''");
        assert_eq!(lit(Dialect::SqlServer, "Ñ"), "N'Ñ'");
    }

    #[test]
    fn builds_filtered_sorted_paged_select() {
        let req = BrowseRequest {
            filters: vec![
                Filter { column: Some("age".into()), op: FilterOp::Ge, value: "18".into(), ..Default::default() },
                Filter { column: Some("name".into()), op: FilterOp::Contains, value: "50%".into(), ..Default::default() },
                Filter { enabled: false, column: Some("x".into()), op: FilterOp::Eq, value: "1".into(), ..Default::default() },
            ],
            sort: vec![("id".into(), false)],
            limit: 300,
            offset: 600,
            ..Default::default()
        };
        let sql = browse_sql(Dialect::Postgres, &t(), &req, &[]);
        assert_eq!(
            sql,
            // unslop-ignore NO_SELECT_STAR -- The workbench bootstraps unknown result columns with a bounded LIMIT; later browsing projects known catalog columns explicitly.
            "SELECT * FROM \"public\".\"users\" WHERE \"age\" >= '18' AND \"name\"::text ILIKE E'%50\\\\%%' ORDER BY \"id\" DESC LIMIT 300 OFFSET 600"
        );
    }

    #[test]
    fn sql_server_paging_needs_order_by() {
        let req = BrowseRequest { limit: 10, offset: 0, ..Default::default() };
        let sql = browse_sql(Dialect::SqlServer, &TableRef { schema: "dbo".into(), name: "t".into() }, &req, &[]);
        assert!(sql.ends_with("ORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"), "{sql}");
    }

    #[test]
    fn any_column_filter_ors_every_column() {
        let cols = vec![ColumnInfo { name: "a".into(), ..Default::default() }, ColumnInfo { name: "b".into(), ..Default::default() }];
        let req = BrowseRequest { filters: vec![Filter { value: "x".into(), ..Default::default() }], ..Default::default() };
        let w = where_clause(Dialect::Sqlite, &req, &cols);
        assert!(w.contains(" OR "), "{w}");
        assert!(w.contains("CAST(\"a\" AS TEXT)") && w.contains("CAST(\"b\" AS TEXT)"));
    }

    #[test]
    fn update_uses_is_null_for_null_keys() {
        let sql = change_sql(
            Dialect::MySql,
            &TableRef { schema: "app".into(), name: "t".into() },
            &RowChange::Update {
                key: vec![("id".into(), Some("7".into())), ("tenant".into(), None)],
                set: vec![("name".into(), EditValue::Text("Zoë".into())), ("note".into(), EditValue::Null)],
            },
        );
        // unslop-ignore WRITE_WITHOUT_WHERE -- The generated UPDATE includes a WHERE on the full key; the scanner misreads the quoted non-ASCII test value.
        assert_eq!(sql, "UPDATE `app`.`t` SET `name` = 'Zoë', `note` = NULL WHERE `id` = '7' AND `tenant` IS NULL");
    }

    #[test]
    fn insert_skips_default_columns_and_handles_all_defaults() {
        let tbl = TableRef { schema: "main".into(), name: "t".into() };
        let sql = change_sql(
            Dialect::Sqlite,
            &tbl,
            &RowChange::Insert { values: vec![("id".into(), EditValue::Default), ("n".into(), EditValue::Text("1".into()))] },
        );
        assert_eq!(sql, "INSERT INTO \"t\" (\"n\") VALUES ('1')");
        let all_default = change_sql(Dialect::Sqlite, &tbl, &RowChange::Insert { values: vec![("id".into(), EditValue::Default)] });
        assert_eq!(all_default, "INSERT INTO \"t\" DEFAULT VALUES");
    }

    #[test]
    fn postgres_alter_column_emits_each_change() {
        let o = ColumnInfo { name: "a".into(), data_type: "int".into(), nullable: true, ..Default::default() };
        let u = ColumnInfo { name: "b".into(), data_type: "bigint".into(), nullable: false, default: Some("0".into()), ..Default::default() };
        let sql = structure_sql(Dialect::Postgres, &t(), &StructureChange::AlterColumn { original: o, updated: u });
        assert_eq!(sql.len(), 4, "{sql:?}");
        assert!(sql[0].contains("RENAME COLUMN \"a\" TO \"b\""));
        assert!(sql[1].contains("TYPE bigint"));
        assert!(sql[2].ends_with("SET NOT NULL"));
        assert!(sql[3].ends_with("SET DEFAULT 0"));
    }

    #[test]
    fn ddl_lists_composite_primary_key() {
        let cols = vec![
            ColumnInfo { name: "a".into(), data_type: "int".into(), primary_key: true, ..Default::default() },
            ColumnInfo { name: "b".into(), data_type: "int".into(), primary_key: true, nullable: true, ..Default::default() },
        ];
        let ddl = create_table_ddl(Dialect::Postgres, &t(), &cols, &[]);
        assert!(ddl.contains("PRIMARY KEY (\"a\", \"b\")"), "{ddl}");
        assert!(ddl.contains("\"a\" int NOT NULL"));
    }

    fn add_index(method: &str, unique: bool, columns: &[&str]) -> StructureChange {
        StructureChange::AddIndex { name: "ix".into(), columns: columns.iter().map(|c| c.to_string()).collect(), unique, method: method.into() }
    }

    #[test]
    fn index_method_lands_where_each_engine_expects_it() {
        assert_eq!(structure_sql(Dialect::Postgres, &t(), &add_index("gin", false, &["tags"])), ["CREATE INDEX \"ix\" ON \"public\".\"users\" USING gin (\"tags\")"]);
        assert_eq!(structure_sql(Dialect::MySql, &t(), &add_index("FULLTEXT", true, &["bio"]))[0], "CREATE FULLTEXT INDEX `ix` ON `public`.`users` (`bio`)");
        assert!(structure_sql(Dialect::MySql, &t(), &add_index("HASH", true, &["a"]))[0].ends_with("(`a`) USING HASH"));
        assert!(structure_sql(Dialect::SqlServer, &t(), &add_index("CLUSTERED", true, &["a"]))[0].starts_with("CREATE UNIQUE CLUSTERED INDEX [ix]"));
        assert!(structure_sql(Dialect::ClickHouse, &t(), &add_index("minmax", false, &["ts"]))[0].ends_with("ADD INDEX \"ix\" (\"ts\") TYPE minmax GRANULARITY 1"));
        assert_eq!(structure_sql(Dialect::Sqlite, &t(), &add_index("", false, &["a", "b"])), ["CREATE INDEX \"ix\" ON \"public\".\"users\" (\"a\", \"b\")"]);
    }

    #[test]
    fn mongo_index_changes_become_shell_calls_and_fields_are_ignored() {
        let sql = structure_sql(Dialect::Mongo, &t(), &add_index("1", true, &["email", "created_at:-1", "body:text"]));
        assert_eq!(sql, [r#"db.getCollection("users").createIndex({"email":1,"created_at":-1,"body":"text"}, {"name":"ix","unique":true})"#]);
        assert_eq!(structure_sql(Dialect::Mongo, &t(), &StructureChange::DropIndex("ix".into())), [r#"db.getCollection("users").dropIndex("ix")"#]);
        assert!(structure_sql(Dialect::Mongo, &t(), &StructureChange::DropColumn("email".into())).is_empty());
    }

    #[test]
    fn new_column_comment_is_not_dropped() {
        let c = ColumnInfo { name: "a".into(), data_type: "int".into(), nullable: true, comment: Some("it's".into()), ..Default::default() };
        let pg = structure_sql(Dialect::Postgres, &t(), &StructureChange::AddColumn(c.clone()));
        assert_eq!(pg[1], "COMMENT ON COLUMN \"public\".\"users\".\"a\" IS 'it''s'");
        assert!(structure_sql(Dialect::MySql, &t(), &StructureChange::AddColumn(c))[0].ends_with("int COMMENT 'it''s'"));
    }

    #[test]
    fn clickhouse_alter_covers_default_and_comment() {
        let o = ColumnInfo { name: "a".into(), data_type: "String".into(), default: Some("''".into()), ..Default::default() };
        let u = ColumnInfo { comment: Some("note".into()), default: None, ..o.clone() };
        let sql = structure_sql(Dialect::ClickHouse, &t(), &StructureChange::AlterColumn { original: o, updated: u });
        assert_eq!(sql, ["ALTER TABLE \"public\".\"users\" MODIFY COLUMN \"a\" REMOVE DEFAULT", "ALTER TABLE \"public\".\"users\" COMMENT COLUMN \"a\" 'note'"]);
    }

    #[test]
    fn key_changes_use_each_engines_syntax() {
        let drop_pk = StructureChange::DropPrimaryKey { name: "users_pkey".into() };
        assert_eq!(structure_sql(Dialect::Postgres, &t(), &drop_pk), ["ALTER TABLE \"public\".\"users\" DROP CONSTRAINT \"users_pkey\""]);
        assert_eq!(structure_sql(Dialect::MySql, &t(), &drop_pk), ["ALTER TABLE `public`.`users` DROP PRIMARY KEY"]);
        let add_pk = StructureChange::AddPrimaryKey { columns: vec!["a".into(), "b".into()] };
        assert_eq!(structure_sql(Dialect::SqlServer, &t(), &add_pk), ["ALTER TABLE [public].[users] ADD PRIMARY KEY ([a], [b])"]);
        let drop_fk = StructureChange::DropForeignKey("fk".into());
        assert_eq!(structure_sql(Dialect::MySql, &t(), &drop_fk), ["ALTER TABLE `public`.`users` DROP FOREIGN KEY `fk`"]);
        assert_eq!(structure_sql(Dialect::SqlServer, &t(), &drop_fk), ["ALTER TABLE [public].[users] DROP CONSTRAINT [fk]"]);
        let add_fk = StructureChange::AddForeignKey {
            name: "fk".into(),
            column: "team_id".into(),
            ref_table: TableRef { schema: "public".into(), name: "teams".into() },
            ref_column: "id".into(),
            on_delete: "SET NULL".into(),
            on_update: String::new(),
        };
        assert_eq!(structure_sql(Dialect::MySql, &t(), &add_fk), ["ALTER TABLE `public`.`users` ADD CONSTRAINT `fk` FOREIGN KEY (`team_id`) REFERENCES `public`.`teams` (`id`) ON DELETE SET NULL"]);
        assert!(structure_sql(Dialect::Mongo, &t(), &add_fk).is_empty());
    }

    #[test]
    fn every_editable_form_attribute_has_sql_behind_it() {
        // SQL Server and SQLite cannot alter these in place, so the form must not offer it.
        let ms = meta::structure_form(Dialect::SqlServer);
        assert_eq!((ms.default, ms.comment), (meta::Attr::NewOnly, meta::Attr::Hidden));
        let lite = meta::structure_form(Dialect::Sqlite);
        assert!(!lite.data_type.editable(false) && lite.data_type.editable(true));
        assert!(!meta::structure_form(Dialect::Mongo).columns_editable);
        assert!(!meta::structure_form(Dialect::ClickHouse).nullable.shown());
        // Keys are only offered where ALTER TABLE can change them.
        for d in [Dialect::Postgres, Dialect::MySql, Dialect::SqlServer] {
            assert!(meta::structure_form(d).keys_editable && !meta::structure_form(d).fk_actions.is_empty());
        }
        for d in [Dialect::Sqlite, Dialect::ClickHouse, Dialect::Mongo] {
            assert!(!meta::structure_form(d).keys_editable);
        }
    }
}
