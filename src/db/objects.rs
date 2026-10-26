//! Object forms produce quoted DDL, reviewed before execution. SQL bodies are authored SQL.
use super::{sqlgen::{qualified, quote_ident, lit}, DbObject, ObjectKind, TableRef};
use crate::model::Dialect;
use anyhow::{bail, ensure, Result};

#[derive(Clone)]
pub struct ObjectDraft {
    pub kind: ObjectKind, pub schema: String, pub name: String, pub table: String,
    pub body: String, pub columns: String, pub parameters: String, pub returns: String,
    pub language: String, pub routine_access: String, pub timing: String, pub event: String, pub function: String,
    pub ref_schema: String, pub ref_table: String, pub ref_columns: String, pub on_delete: String,
    pub constraint: String, pub unique: bool, pub start: i64, pub increment: i64,
    pub interval: u32, pub unit: String, pub base_type: String, pub type_kind: String,
}
impl ObjectDraft {
    pub fn new(kind: ObjectKind, schema: String) -> Self {
        // unslop-ignore NO_SQL_INTERPOLATION -- Struct defaults only; these strings do not construct or execute a query.
        Self { kind, schema: schema.clone(), name: String::new(), table: String::new(), body: String::new(), columns: String::new(), parameters: String::new(), returns: "text".into(), language: "sql".into(), routine_access: "READS SQL DATA".into(), timing: "AFTER".into(), event: "INSERT".into(), function: String::new(), ref_schema: schema, ref_table: String::new(), ref_columns: String::new(), on_delete: "NO ACTION".into(), constraint: "FOREIGN KEY".into(), unique: false, start: 1, increment: 1, interval: 1, unit: "DAY".into(), base_type: "text".into(), type_kind: "DOMAIN".into() }
    }
    pub fn sql(&self, d: Dialect) -> Result<String> {
        ensure!(supported(d).contains(&self.kind), "This object is not supported by this engine");
        ident(&self.name)?;
        let q = qualified(d, &TableRef { schema: self.schema.clone(), name: self.name.trim().into() });
        let table = qualified(d, &TableRef { schema: self.schema.clone(), name: self.table.trim().into() });
        let body = self.body.trim();
        let sql = match self.kind {
            ObjectKind::View | ObjectKind::MaterializedView => {
                ensure!(!body.is_empty(), "Enter the SELECT statement for the view");
                format!("CREATE {} {q}{} AS\n{}", if self.kind == ObjectKind::View { "VIEW" } else { "MATERIALIZED VIEW" }, if d == Dialect::ClickHouse && self.kind == ObjectKind::MaterializedView {
                    ident(&self.ref_table)?;
                    format!(" TO {}", qualified(d, &TableRef { schema: self.ref_schema.clone(), name: self.ref_table.clone() }))
                } else { String::new() }, body.trim_end_matches(';'))
            }
            ObjectKind::Enum => {
                let values: Vec<_> = body.lines().filter(|s| !s.is_empty()).map(|s| lit(d, s)).collect();
                ensure!(!values.is_empty(), "Enter one enum value per line");
                let mut seen = std::collections::HashSet::new();
                ensure!(body.lines().all(|s| seen.insert(s)), "Enum values must be unique");
                format!("CREATE TYPE {q} AS ENUM ({})", values.join(", "))
            }
            ObjectKind::Type => {
                if self.type_kind == "COMPOSITE" {
                    let attributes = body.lines().filter(|line| !line.trim().is_empty()).map(|line| {
                        let (name, ty) = line.split_once(':').ok_or_else(|| anyhow::anyhow!("Use attribute_name: SQL type, one attribute per line"))?;
                        ident(name)?; fragment(ty)?;
                        Ok(format!("{} {}", quote_ident(d, name.trim()), ty.trim()))
                    }).collect::<Result<Vec<_>>>()?;
                    ensure!(!attributes.is_empty(), "Enter at least one composite attribute");
                    return Ok(format!("CREATE TYPE {q} AS ({})", attributes.join(", ")));
                }
                fragment(&self.base_type)?;
                fragment_allow_empty(body)?;
                format!("CREATE DOMAIN {q} AS {}{}", self.base_type.trim(), if body.is_empty() { String::new() } else { format!(" CHECK ({body})") })
            }
            ObjectKind::Sequence => {
                ensure!(self.increment != 0, "Sequence increment cannot be zero");
                format!("CREATE SEQUENCE {q} START WITH {} INCREMENT BY {}", self.start, self.increment)
            }
            ObjectKind::Index => {
                ident(&self.table)?;
                let cols = identifiers(d, &self.columns)?;
                let name = if d == Dialect::Sqlite { q.clone() } else { quote_ident(d, &self.name) };
                let target = if d == Dialect::Sqlite { quote_ident(d, &self.table) } else { table };
                format!("CREATE {}INDEX {name} ON {target} ({cols})", if self.unique { "UNIQUE " } else { "" })
            }
            ObjectKind::Extension => format!("CREATE EXTENSION {} WITH SCHEMA {}", quote_ident(d, &self.name), quote_ident(d, &self.schema)),
            ObjectKind::Function | ObjectKind::Procedure => {
                fragment_allow_empty(&self.parameters)?;
                ensure!(!body.is_empty(), "Enter the routine body");
                let function = self.kind == ObjectKind::Function;
                let kw = if function { "FUNCTION" } else { "PROCEDURE" };
                if function { fragment(&self.returns)?; }
                if d == Dialect::Postgres {
                    ensure!(["sql", "plpgsql"].contains(&self.language.as_str()), "Choose SQL or PL/pgSQL");
                    let tag = dollar_tag(body);
                    format!("CREATE {kw} {q}({}){} LANGUAGE {} AS {tag}\n{body}\n{tag}", self.parameters, if function { format!(" RETURNS {}", self.returns) } else { String::new() }, self.language)
                } else if d == Dialect::SqlServer {
                    format!("CREATE {kw} {q} {}{} AS\n{body}", if function { format!("({})", self.parameters) } else { self.parameters.clone() }, if function { format!(" RETURNS {}", self.returns) } else { String::new() })
                } else {
                    ensure!(["CONTAINS SQL", "NO SQL", "READS SQL DATA", "MODIFIES SQL DATA"].contains(&self.routine_access.as_str()), "Invalid routine data-access declaration");
                    format!("CREATE {kw} {q}({}){} {}\n{body}", self.parameters, if function { format!(" RETURNS {}", self.returns) } else { String::new() }, self.routine_access)
                }
            }
            ObjectKind::Trigger => {
                ident(&self.table)?;
                ensure!(["BEFORE", "AFTER", "INSTEAD OF"].contains(&self.timing.as_str()), "Invalid trigger timing");
                ensure!(["INSERT", "UPDATE", "DELETE"].contains(&self.event.as_str()), "Invalid trigger event");
                if d == Dialect::Postgres {
                    ident(&self.function)?;
                    let function = qualified(d, &TableRef { schema: self.schema.clone(), name: self.function.clone() });
                    format!("CREATE TRIGGER {} {} {} ON {table} FOR EACH ROW EXECUTE FUNCTION {function}()", quote_ident(d, &self.name), self.timing, self.event)
                } else {
                    ensure!(!body.is_empty(), "Enter the trigger body");
                    if d == Dialect::MySql { ensure!(self.timing != "INSTEAD OF", "MySQL supports BEFORE or AFTER triggers"); }
                    if d == Dialect::SqlServer {
                        ensure!(self.timing != "BEFORE", "SQL Server supports AFTER or INSTEAD OF triggers");
                        format!("CREATE TRIGGER {q} ON {table} {} {} AS\n{body}", self.timing, self.event)
                    } else { format!("CREATE TRIGGER {q} {} {} ON {table} FOR EACH ROW\n{body}", self.timing, self.event) }
                }
            }
            ObjectKind::Event => {
                ensure!(self.interval > 0, "Schedule interval must be positive");
                ensure!(["SECOND", "MINUTE", "HOUR", "DAY", "WEEK", "MONTH", "YEAR"].contains(&self.unit.as_str()), "Invalid interval unit");
                ensure!(!body.is_empty(), "Enter the event body");
                // Events start disabled. Enabling is an explicit maintenance operation.
                format!("CREATE EVENT {q} ON SCHEDULE EVERY {} {} DISABLE DO\n{body}", self.interval, self.unit)
            }
            ObjectKind::Constraint => {
                ident(&self.table)?;
                ensure!(["FOREIGN KEY", "CHECK", "UNIQUE", "PRIMARY KEY"].contains(&self.constraint.as_str()), "Invalid constraint type");
                let definition = if self.constraint == "CHECK" {
                    ensure!(!body.is_empty(), "Enter the CHECK expression"); fragment(body)?;
                    format!("CHECK ({body})")
                } else {
                    let cols = identifiers(d, &self.columns)?;
                    if self.constraint == "FOREIGN KEY" {
                        ident(&self.ref_table)?;
                        let left = self.columns.split(',').count(); let right = self.ref_columns.split(',').count();
                        ensure!(left == right, "Both sides of a foreign key must have the same number of columns");
                        ensure!(["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"].contains(&self.on_delete.as_str()), "Invalid delete rule");
                        let target = qualified(d, &TableRef { schema: self.ref_schema.clone(), name: self.ref_table.clone() });
                        format!("FOREIGN KEY ({cols}) REFERENCES {target} ({}) ON DELETE {}", identifiers(d, &self.ref_columns)?, self.on_delete)
                    } else { format!("{} ({cols})", self.constraint) }
                };
                format!("ALTER TABLE {table} ADD CONSTRAINT {} {definition}", quote_ident(d, &self.name))
            }
            _ => bail!("Use the table editor to create this object"),
        };
        Ok(sql)
    }
}
fn ident(value: &str) -> Result<()> { ensure!(!value.trim().is_empty() && !value.contains('\0'), "An object name is required"); Ok(()) }
fn identifiers(d: Dialect, value: &str) -> Result<String> {
    value.split(',').map(|s| { ident(s)?; Ok(quote_ident(d, s.trim())) }).collect::<Result<Vec<_>>>().map(|v| v.join(", "))
}
fn fragment_allow_empty(value: &str) -> Result<()> {
    ensure!(!value.contains([';', '\0']) && !value.contains("--") && !value.contains("/*"), "Use a single SQL type or expression without statement separators or comments"); Ok(())
}
fn fragment(value: &str) -> Result<()> { ensure!(!value.trim().is_empty(), "A SQL type or expression is required"); fragment_allow_empty(value) }
fn dollar_tag(body: &str) -> String { let mut tag = "$strata$".to_string(); while body.contains(&tag) { tag.insert(tag.len()-1, '_'); } tag }

pub fn supported(d: Dialect) -> Vec<ObjectKind> {
    use ObjectKind::*;
    match d {
        Dialect::Postgres => vec![View, MaterializedView, Function, Procedure, Sequence, Trigger, Enum, Type, Index, Extension, Constraint],
        Dialect::MySql => vec![View, Function, Procedure, Trigger, Event, Index, Constraint],
        Dialect::SqlServer => vec![View, Function, Procedure, Sequence, Trigger, Index, Constraint],
        Dialect::Sqlite => vec![View, Trigger, Index],
        Dialect::ClickHouse => vec![View, MaterializedView],
        _ => vec![],
    }
}
pub fn label(k: ObjectKind) -> &'static str {
    use ObjectKind::*;
    match k { View => "View", MaterializedView => "Materialized view", Function => "Function", Procedure => "Procedure", Sequence => "Sequence", Trigger => "Trigger", Enum => "Enum", Type => "Domain / type", Index => "Index", Extension => "Extension", Constraint => "Constraint / foreign key", Event => "Scheduled event", _ => "Table" }
}
pub fn requires_standalone(d: Dialect, sql: &str) -> bool {
    let sql = sql.trim_start().to_ascii_uppercase();
    ["CREATE DATABASE", "DROP DATABASE", "VACUUM", "REINDEX"].iter().any(|p| sql.starts_with(p)) ||
        d == Dialect::MySql && ["CREATE ", "ALTER ", "DROP ", "TRUNCATE ", "ANALYZE ", "OPTIMIZE ", "CHECK ", "REPAIR "].iter().any(|p| sql.starts_with(p))
}
pub fn maintenance(d: Dialect, obj: &DbObject) -> Vec<(&'static str, String)> {
    let q = qualified(d, &TableRef { schema: obj.schema.clone(), name: obj.name.clone() });
    if obj.kind == ObjectKind::MaterializedView && d == Dialect::Postgres { return vec![("Refresh materialized view", format!("REFRESH MATERIALIZED VIEW {q}"))]; }
    if obj.kind == ObjectKind::Event && d == Dialect::MySql { return vec![("Enable scheduled event", format!("ALTER EVENT {q} ENABLE")), ("Disable scheduled event", format!("ALTER EVENT {q} DISABLE"))]; }
    if obj.kind == ObjectKind::Trigger && d == Dialect::Postgres {
        let parent = qualified(d, &TableRef { schema: obj.schema.clone(), name: obj.parent.clone().unwrap_or_default() });
        let trigger = quote_ident(d, &obj.name);
        return vec![("Enable trigger", format!("ALTER TABLE {parent} ENABLE TRIGGER {trigger}")), ("Disable trigger", format!("ALTER TABLE {parent} DISABLE TRIGGER {trigger}"))];
    }
    if obj.kind == ObjectKind::Extension && d == Dialect::Postgres { return vec![("Update extension", format!("ALTER EXTENSION {} UPDATE", quote_ident(d, &obj.name)))]; }
    if obj.kind != ObjectKind::Table { return vec![]; }
    match d {
        Dialect::Postgres => vec![("Analyze table", format!("ANALYZE {q}")), ("Vacuum and analyze", format!("VACUUM (ANALYZE) {q}")), ("Reindex table", format!("REINDEX TABLE {q}"))],
        Dialect::MySql => vec![("Analyze table", format!("ANALYZE TABLE {q}")), ("Optimize table", format!("OPTIMIZE TABLE {q}")), ("Check table", format!("CHECK TABLE {q}"))],
        Dialect::Sqlite => vec![("Analyze table", format!("ANALYZE {q}")), ("Reindex table", format!("REINDEX {q}")), ("Vacuum database", "VACUUM".into())],
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        Dialect::SqlServer => vec![("Update statistics", format!("UPDATE STATISTICS {q}")), ("Check table", format!("DBCC CHECKTABLE ({})", lit(d, &q)))],
        Dialect::ClickHouse => vec![("Optimize table", format!("OPTIMIZE TABLE {q} FINAL"))],
        _ => vec![],
    }
}
pub fn drop_sql(d: Dialect, obj: &DbObject, cascade: bool) -> Option<String> {
    let q = qualified(d, &TableRef { schema: obj.schema.clone(), name: obj.name.clone() });
    let parent = qualified(d, &TableRef { schema: obj.schema.clone(), name: obj.parent.clone().unwrap_or_default() });
    let suffix = if cascade && d == Dialect::Postgres { " CASCADE" } else { "" };
    Some(match obj.kind {
        ObjectKind::Trigger if d == Dialect::Postgres => format!("DROP TRIGGER {} ON {parent}{suffix}", quote_ident(d, &obj.name)),
        ObjectKind::Trigger => format!("DROP TRIGGER {q}"),
        ObjectKind::Enum | ObjectKind::Type => format!("DROP {} {q}{suffix}", if obj.arguments.as_deref() == Some("DOMAIN") { "DOMAIN" } else { "TYPE" }),
        ObjectKind::Event => format!("DROP EVENT {q}"),
        ObjectKind::Index if d == Dialect::MySql || d == Dialect::SqlServer => if obj.name == "PRIMARY" && d == Dialect::MySql { format!("ALTER TABLE {parent} DROP PRIMARY KEY") } else { format!("DROP INDEX {} ON {parent}", quote_ident(d, &obj.name)) },
        ObjectKind::Index => format!("DROP INDEX {q}{suffix}"),
        ObjectKind::Extension => format!("DROP EXTENSION {}{suffix}", quote_ident(d, &obj.name)),
        ObjectKind::Constraint => {
            let kw = if d == Dialect::MySql { match obj.arguments.as_deref() { Some("FOREIGN KEY") => "FOREIGN KEY", Some("CHECK") => "CHECK", Some("PRIMARY KEY") => return Some(format!("ALTER TABLE {parent} DROP PRIMARY KEY")), _ => "INDEX" } } else { "CONSTRAINT" };
            format!("ALTER TABLE {parent} DROP {kw} {}{suffix}", quote_ident(d, &obj.name))
        }
        ObjectKind::Function | ObjectKind::Procedure if d == Dialect::Postgres => format!("DROP {} {q}({}){suffix}", if obj.kind == ObjectKind::Function { "FUNCTION" } else { "PROCEDURE" }, obj.arguments.as_deref().unwrap_or("")),
        _ => return None,
    })
}
pub fn definition_sql(d: Dialect, obj: &DbObject) -> Option<String> {
    let name = lit(d, &obj.name); let schema = lit(d, &obj.schema);
    let parent = lit(d, obj.parent.as_deref().unwrap_or(""));
    Some(match (d, obj.kind) {
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        (Dialect::Postgres, ObjectKind::Trigger) => format!("SELECT pg_get_triggerdef(t.oid,true) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname={schema} AND c.relname={parent} AND t.tgname={name}"),
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        (Dialect::Postgres, ObjectKind::Index) => format!("SELECT indexdef FROM pg_indexes WHERE schemaname={schema} AND indexname={name}"),
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        (Dialect::Postgres, ObjectKind::Enum) => format!("SELECT 'CREATE TYPE ' || quote_ident(n.nspname) || '.' || quote_ident(t.typname) || ' AS ENUM (' || string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder) || ');' FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace JOIN pg_enum e ON e.enumtypid=t.oid WHERE n.nspname={schema} AND t.typname={name} GROUP BY n.nspname,t.typname"),
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        (Dialect::Postgres, ObjectKind::Type) => format!("SELECT CASE WHEN t.typtype='d' THEN 'CREATE DOMAIN ' || quote_ident(n.nspname) || '.' || quote_ident(t.typname) || ' AS ' || format_type(t.typbasetype,t.typtypmod) || CASE WHEN t.typnotnull THEN ' NOT NULL' ELSE '' END || COALESCE(' DEFAULT ' || t.typdefault,'') || COALESCE((SELECT string_agg(' CONSTRAINT ' || quote_ident(c.conname) || ' ' || pg_get_constraintdef(c.oid), ' ') FROM pg_constraint c WHERE c.contypid=t.oid),'') || ';' ELSE 'CREATE TYPE ' || quote_ident(n.nspname) || '.' || quote_ident(t.typname) || ' AS (' || (SELECT string_agg(quote_ident(a.attname) || ' ' || format_type(a.atttypid,a.atttypmod), ', ' ORDER BY a.attnum) FROM pg_attribute a WHERE a.attrelid=t.typrelid AND a.attnum>0 AND NOT a.attisdropped) || ');' END FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname={schema} AND t.typname={name}"),
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        (Dialect::Postgres, ObjectKind::Constraint) => format!("SELECT 'ALTER TABLE ' || quote_ident(n.nspname) || '.' || quote_ident(t.relname) || ' ADD CONSTRAINT ' || quote_ident(c.conname) || ' ' || pg_get_constraintdef(c.oid,true) || ';' FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname={schema} AND t.relname={parent} AND c.conname={name}"),
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        (Dialect::Postgres, ObjectKind::Extension) => format!("SELECT 'CREATE EXTENSION ' || quote_ident(e.extname) || ' WITH SCHEMA ' || quote_ident(n.nspname) || ' VERSION ' || quote_literal(e.extversion) || ';' FROM pg_extension e JOIN pg_namespace n ON n.oid=e.extnamespace WHERE e.extname={name}"),
        (Dialect::MySql, ObjectKind::Trigger) => format!("SHOW CREATE TRIGGER {}", qualified(d, &TableRef { schema: obj.schema.clone(), name: obj.name.clone() })),
        (Dialect::MySql, ObjectKind::Event) => format!("SHOW CREATE EVENT {}", qualified(d, &TableRef { schema: obj.schema.clone(), name: obj.name.clone() })),
        (Dialect::MySql, ObjectKind::Index | ObjectKind::Constraint) => format!("SHOW CREATE TABLE {}", qualified(d, &TableRef { schema: obj.schema.clone(), name: obj.parent.clone()? })),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::{Driver, sqlite::SqliteDriver}, model::{ConnectionConfig, DriverKind}};
    #[test]
    fn quotes_names_enum_values_and_routine_delimiters() {
        let mut draft = ObjectDraft::new(ObjectKind::Enum, "strata".into()); draft.name = "odd\"type".into(); draft.body = "ready\ncan't".into();
        assert_eq!(draft.sql(Dialect::Postgres).unwrap(), "CREATE TYPE \"strata\".\"odd\"\"type\" AS ENUM ('ready', 'can''t')");
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        draft.kind = ObjectKind::Function; draft.name = "f".into(); draft.body = "SELECT '$strata$'".into();
        let sql = draft.sql(Dialect::Postgres).unwrap();
        assert!(sql.contains("AS $strata_$\nSELECT '$strata$'\n$strata_$"), "{sql}");
    }
    #[test]
    fn rejects_invalid_foreign_keys_and_statement_in_type_fields() {
        let mut draft = ObjectDraft::new(ObjectKind::Constraint, "public".into());
        draft.name = "fk".into(); draft.table = "orders".into(); draft.columns = "a,b".into(); draft.ref_table = "customers".into(); draft.ref_columns = "id".into();
        assert!(draft.sql(Dialect::Postgres).unwrap_err().to_string().contains("same number"));
        draft.kind = ObjectKind::Type; draft.base_type = "int; DROP TABLE orders".into();
        assert!(draft.sql(Dialect::Postgres).unwrap_err().to_string().contains("statement separators"));
        draft.kind = ObjectKind::Sequence; draft.increment = 0;
        assert!(draft.sql(Dialect::Postgres).unwrap_err().to_string().contains("cannot be zero"));
    }
    #[test]
    fn scoped_objects_drop_the_correct_parent_and_signature() {
        let object = DbObject { schema: "public".into(), name: "audit".into(), kind: ObjectKind::Trigger, parent: Some("orders".into()), arguments: None };
        assert_eq!(drop_sql(Dialect::Postgres, &object, false).unwrap(), "DROP TRIGGER \"audit\" ON \"public\".\"orders\"");
        let function = DbObject { name: "f".into(), kind: ObjectKind::Function, arguments: Some("value integer".into()), ..object };
        assert_eq!(drop_sql(Dialect::Postgres, &function, false).unwrap(), "DROP FUNCTION \"public\".\"f\"(value integer)");
        assert!(requires_standalone(Dialect::Postgres, "CREATE DATABASE db"));
        assert!(requires_standalone(Dialect::Sqlite, "VACUUM"));
        assert!(!requires_standalone(Dialect::Postgres, "CREATE VIEW v AS SELECT 1"));
    }
    #[tokio::test]
    async fn sqlite_objects_create_list_define_trigger_and_drop() {
        let mut cfg = ConnectionConfig::new(DriverKind::Sqlite); cfg.path = ":memory:".into();
        let mut driver = SqliteDriver::open(&cfg).await.unwrap();
        driver.execute("CREATE TABLE source (id INTEGER PRIMARY KEY, note TEXT); CREATE TABLE audit (note TEXT)").await.unwrap();
        assert!(driver.server_version().await.unwrap().starts_with('3'));
        // unslop-ignore NO_SQL_INTERPOLATION -- Workbench SQL: identifiers use quote_ident/qualified and literal values use lit; SQL bodies are explicitly authored and reviewed by the user.
        let mut view = ObjectDraft::new(ObjectKind::View, "main".into()); view.name = "source_view".into(); view.body = "SELECT id, note FROM source".into();
        let mut index = ObjectDraft::new(ObjectKind::Index, "main".into()); index.name = "source_note_idx".into(); index.table = "source".into(); index.columns = "note".into();
        let mut trigger = ObjectDraft::new(ObjectKind::Trigger, "main".into()); trigger.name = "source_audit".into(); trigger.table = "source".into(); trigger.body = "BEGIN INSERT INTO audit(note) VALUES (NEW.note); END".into();
        for draft in [&view, &index, &trigger] { driver.execute(&draft.sql(Dialect::Sqlite).unwrap()).await.unwrap(); }
        driver.execute("INSERT INTO source(id,note) VALUES (1,'saved')").await.unwrap();
        assert_eq!(driver.query_one("SELECT note FROM audit").await.unwrap().cell(0,0), Some("saved"));
        let objects = driver.objects("main").await.unwrap();
        for (name, kind) in [("source_view", ObjectKind::View), ("source_note_idx", ObjectKind::Index), ("source_audit", ObjectKind::Trigger)] {
            let object = objects.iter().find(|o| o.name == name && o.kind == kind).expect("created object is listed");
            assert!(driver.ddl(object).await.unwrap().contains(name));
            driver.execute(&crate::db::sqlgen::drop_sql(Dialect::Sqlite, object, false)).await.unwrap();
        }
        let objects = driver.objects("main").await.unwrap();
        assert!(!objects.iter().any(|o| o.name == "source_audit" || o.name == "source_note_idx" || o.name == "source_view"));
    }
}
