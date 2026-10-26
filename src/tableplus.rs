//! Imports connections from TablePlus' own files (Connections.plist and ConnectionGroups.plist).
//! Passwords stay in TablePlus' keychain items, which other apps cannot read; Strata asks for
//! them on first connect.

use crate::model::*;
use anyhow::{Context, Result};
use plist::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Where TablePlus keeps its data on this platform.
pub fn default_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Application Support/com.tinyapp.TablePlus/Data"))
    } else {
        dirs::data_dir().map(|d| d.join("com.tinyapp.TablePlus").join("Data"))
    }
}

pub fn driver_from_name(name: &str) -> Option<DriverKind> {
    let n = name.to_ascii_lowercase().replace([' ', '_', '-'], "");
    Some(match n.as_str() {
        "postgresql" | "postgres" => DriverKind::Postgres,
        "cockroachdb" | "cockroach" => DriverKind::CockroachDb,
        "redshift" | "amazonredshift" => DriverKind::Redshift,
        "greenplum" => DriverKind::Greenplum,
        "mysql" => DriverKind::MySql,
        "mariadb" => DriverKind::MariaDb,
        "sqlite" => DriverKind::Sqlite,
        "libsql" | "turso" => DriverKind::LibSql,
        "cloudflared1" | "d1" => DriverKind::CloudflareD1,
        "microsoftsqlserver" | "sqlserver" | "mssql" => DriverKind::SqlServer,
        "clickhouse" => DriverKind::ClickHouse,
        "redis" => DriverKind::Redis,
        "mongodb" | "mongo" => DriverKind::MongoDb,
        "elasticsearch" | "opensearch" => DriverKind::Elasticsearch,
        other => return DriverKind::ALL.into_iter().find(|d| d.label().to_ascii_lowercase().replace([' ', '_', '-', '/'], "") == other),
    })
}

fn text(d: &plist::Dictionary, key: &str) -> String {
    match d.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Integer(i)) => i.to_string(),
        Some(Value::Real(r)) => r.to_string(),
        _ => String::new(),
    }
}

fn flag(d: &plist::Dictionary, key: &str) -> bool {
    match d.get(key) {
        Some(Value::Boolean(b)) => *b,
        Some(Value::Integer(i)) => i.as_signed() == Some(1),
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

pub struct Imported {
    pub connections: Vec<ConnectionConfig>,
    pub groups: Vec<Group>,
    pub skipped: Vec<String>,
}

/// Reads `Connections.plist` (and `ConnectionGroups.plist` next to it, when present).
pub fn import(connections_plist: &Path) -> Result<Imported> {
    let root = Value::from_file(connections_plist).with_context(|| format!("Can't read {}", connections_plist.display()))?;
    let list = root.as_array().context("Connections.plist is not a list")?;
    let mut groups = Vec::new();
    let mut group_ids = std::collections::HashMap::new();
    if let Some(Ok(Value::Array(gs))) = connections_plist.parent().map(|p| Value::from_file(p.join("ConnectionGroups.plist"))) {
        let palette = [[242, 178, 82], [92, 200, 224], [91, 214, 154], [143, 180, 216], [255, 138, 126]];
        for (i, g) in gs.iter().filter_map(|g| g.as_dictionary()).enumerate() {
            let id = Uuid::new_v4();
            group_ids.insert(text(g, "ID"), id);
            groups.push(Group { id, name: text(g, "Name"), color: palette[i % palette.len()], collapsed: false });
        }
    }
    let mut connections = Vec::new();
    let mut skipped = Vec::new();
    for d in list.iter().filter_map(|v| v.as_dictionary()) {
        let name = text(d, "ConnectionName");
        let driver_name = text(d, "Driver");
        let Some(driver) = driver_from_name(&driver_name) else {
            skipped.push(format!("{name} ({driver_name})"));
            continue;
        };
        let mut c = ConnectionConfig::new(driver);
        c.name = name;
        let host = text(d, "DatabaseHost");
        if !host.is_empty() {
            c.host = host;
        }
        if let Ok(port) = text(d, "DatabasePort").parse() {
            c.port = port;
        }
        let user = text(d, "DatabaseUser");
        if !user.is_empty() {
            c.user = user;
        }
        c.database = text(d, "DatabaseName");
        if driver.is_file_based() {
            c.path = text(d, "DatabasePath");
        }
        c.tag = match text(d, "Enviroment").to_ascii_lowercase().as_str() {
            "local" => Tag::Local,
            "development" => Tag::Development,
            "testing" => Tag::Testing,
            "staging" => Tag::Staging,
            "production" => Tag::Production,
            _ => Tag::None,
        };
        c.startup_query = text(d, "StartupCommands");
        c.ssl = match text(d, "tLSMode").as_str() {
            "1" => SslMode::Prefer,
            "2" | "3" => SslMode::Require,
            "4" => SslMode::VerifyFull,
            _ => SslMode::Disable,
        };
        if flag(d, "isOverSSH") {
            c.ssh.enabled = true;
            c.ssh.host = text(d, "ServerAddress");
            c.ssh.port = text(d, "ServerPort").parse().unwrap_or(22);
            c.ssh.user = text(d, "ServerUser");
            c.ssh.use_key = flag(d, "isUsePrivateKey");
            let key = text(d, "ServerPrivateKeyName");
            if !key.is_empty() {
                c.ssh.key_path = if key.contains('/') { key } else { format!("~/.ssh/{key}") };
            }
        }
        c.group = group_ids.get(&text(d, "GroupID")).copied();
        connections.push(c);
    }
    Ok(Imported { connections, groups, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_tableplus_driver_names() {
        assert_eq!(driver_from_name("PostgreSQL"), Some(DriverKind::Postgres));
        assert_eq!(driver_from_name("Microsoft SQL Server"), Some(DriverKind::SqlServer));
        assert_eq!(driver_from_name("MongoDB"), Some(DriverKind::MongoDb));
        assert_eq!(driver_from_name("Unknown Engine"), None);
    }

    #[test]
    fn imports_a_connections_plist() {
        let dir = std::env::temp_dir().join(format!("strata-tp-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = plist::Dictionary::new();
        d.insert("ConnectionName".into(), "Shop".into());
        d.insert("Driver".into(), "PostgreSQL".into());
        d.insert("DatabaseHost".into(), "db.internal".into());
        d.insert("DatabasePort".into(), "6543".into());
        d.insert("DatabaseUser".into(), "app".into());
        d.insert("DatabaseName".into(), "shop".into());
        d.insert("Enviroment".into(), "production".into());
        d.insert("isOverSSH".into(), Value::Boolean(true));
        d.insert("ServerAddress".into(), "bastion".into());
        d.insert("ServerPort".into(), "2222".into());
        d.insert("ServerUser".into(), "ops".into());
        let mut odd = plist::Dictionary::new();
        odd.insert("ConnectionName".into(), "Legacy".into());
        odd.insert("Driver".into(), "Informix".into());
        let path = dir.join("Connections.plist");
        Value::Array(vec![Value::Dictionary(d), Value::Dictionary(odd)]).to_file_xml(&path).unwrap();
        let got = import(&path).unwrap();
        assert_eq!(got.connections.len(), 1);
        let c = &got.connections[0];
        assert_eq!((c.host.as_str(), c.port, c.user.as_str(), c.database.as_str()), ("db.internal", 6543, "app", "shop"));
        assert_eq!(c.tag, Tag::Production);
        assert!(c.ssh.enabled && c.ssh.port == 2222 && c.ssh.user == "ops");
        assert_eq!(got.skipped, vec!["Legacy (Informix)".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
