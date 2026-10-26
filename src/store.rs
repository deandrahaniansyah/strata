//! On-disk persistence. Plain JSON under the platform config dir; secrets go to the OS keychain
//! (macOS Keychain, Windows Credential Manager, Linux Secret Service), falling back to a 0600
//! `secrets.json` when the keychain refuses the write or no keychain service is reachable.

use crate::model::*;
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

const KEYRING_SERVICE: &str = "dev.strata.app";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionFile {
    pub connections: Vec<ConnectionConfig>,
    pub groups: Vec<Group>,
}

/// Exported connection bundle (`.strataconnection`), passwords never included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionExport {
    pub format: String,
    pub connections: Vec<ConnectionConfig>,
}

pub struct Store {
    root: PathBuf,
    /// False in tests, so they never touch the real keychain.
    keychain: bool,
    /// Last value read or written per secret key; unchanged secrets are not rewritten, which
    /// keeps routine saves (last used, safe mode) from hitting the keychain at all.
    known: Mutex<HashMap<String, String>>,
    keychain_error: Mutex<Option<String>>,
}

impl Store {
    #[cfg(test)]
    pub fn for_test(root: PathBuf) -> Self { Self { root, keychain: false, known: Mutex::default(), keychain_error: Mutex::default() } }
    pub fn open() -> Result<Self> {
        let root = std::env::var_os("STRATA_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|d| d.join("Strata")))
            .context("no config directory on this platform")?;
        std::fs::create_dir_all(root.join("history"))?;
        std::fs::create_dir_all(root.join("themes"))?;
        Ok(Self { root, keychain: true, known: Mutex::default(), keychain_error: Mutex::default() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn read<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        let path = self.root.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                // Keep the unreadable file for the user instead of silently overwriting it.
                let _ = std::fs::copy(&path, path.with_extension("json.bk"));
                eprintln!("strata: {name} is unreadable ({e}); a backup was written next to it");
                T::default()
            }),
            Err(_) => T::default(),
        }
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let path = self.root.join(name);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn load_connections(&self) -> ConnectionFile {
        let mut file: ConnectionFile = self.read("connections.json");
        for c in &mut file.connections {
            c.password = self.secret(c.id, "db");
            c.ssh.password = self.secret(c.id, "ssh");
            c.ssh.key_passphrase = self.secret(c.id, "sshkey");
        }
        file
    }

    pub fn save_connections(&self, file: &ConnectionFile) -> Result<()> {
        for c in &file.connections {
            self.set_secret(c.id, "db", &c.password);
            self.set_secret(c.id, "ssh", &c.ssh.password);
            self.set_secret(c.id, "sshkey", &c.ssh.key_passphrase);
        }
        self.write("connections.json", file)
    }

    pub fn forget_secrets(&self, id: Uuid) {
        for kind in ["db", "ssh", "sshkey"] {
            self.set_secret(id, kind, "");
        }
    }

    pub fn load_settings(&self) -> Settings {
        self.read::<Option<Settings>>("settings.json").unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.write("settings.json", s)
    }

    pub fn load_queries(&self) -> Vec<SavedQuery> {
        self.read("queries.json")
    }

    pub fn save_queries(&self, q: &[SavedQuery]) -> Result<()> {
        self.write("queries.json", &q)
    }

    pub fn load_boards(&self) -> Vec<MetricsBoard> {
        self.read("boards.json")
    }

    pub fn save_boards(&self, b: &[MetricsBoard]) -> Result<()> {
        self.write("boards.json", &b)
    }

    pub fn load_secret_value(&self, key: &str) -> String {
        self.secret_raw(key)
    }

    pub fn save_secret_value(&self, key: &str, value: &str) {
        self.set_secret_raw(key, value)
    }

    /// Why the last secret write fell back to `secrets.json`, reported once.
    pub fn take_keychain_error(&self) -> Option<String> {
        self.keychain_error.lock().ok()?.take()
    }

    pub fn append_history(&self, conn: Uuid, entry: &HistoryEntry) {
        use std::io::Write;
        let path = self.root.join("history").join(format!("{conn}.jsonl"));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            if let Ok(line) = serde_json::to_string(entry) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    pub fn load_history(&self, conn: Uuid, keep_days: u32) -> Vec<HistoryEntry> {
        let path = self.root.join("history").join(format!("{conn}.jsonl"));
        let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
        let cutoff = chrono::Utc::now() - chrono::Duration::days(keep_days.max(1) as i64);
        let all: Vec<HistoryEntry> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let kept: Vec<HistoryEntry> = all.iter().filter(|e| e.at >= cutoff).cloned().collect();
        if kept.len() != all.len() {
            let body: String = kept.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
            let _ = std::fs::write(&path, body);
        }
        kept
    }

    pub fn clear_history(&self, conn: Uuid) {
        let _ = std::fs::remove_file(self.root.join("history").join(format!("{conn}.jsonl")));
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.root.join("themes")
    }

    fn secret(&self, id: Uuid, kind: &str) -> String {
        self.secret_raw(&format!("{id}:{kind}"))
    }

    fn set_secret(&self, id: Uuid, kind: &str, value: &str) {
        self.set_secret_raw(&format!("{id}:{kind}"), value)
    }

    fn secret_raw(&self, key: &str) -> String {
        if let Some(v) = self.known.lock().ok().and_then(|k| k.get(key).cloned()) {
            return v;
        }
        // A file copy only exists because the latest keychain write failed, so it is newer than
        // whatever the keychain still holds.
        // It is left out of `known`, so the next save retries the keychain and moves it there.
        if let Some(v) = self.fallback_secrets().remove(key) {
            return v;
        }
        let value = if self.keychain {
            keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.get_password()).unwrap_or_default()
        } else {
            String::new()
        };
        if let Ok(mut k) = self.known.lock() {
            k.insert(key.to_string(), value.clone());
        }
        value
    }

    fn set_secret_raw(&self, key: &str, value: &str) {
        if self.known.lock().ok().and_then(|k| k.get(key).cloned()).as_deref() == Some(value) {
            return;
        }
        let keychain_result = if self.keychain { keychain_write(key, value) } else { Err("keychain disabled".into()) };
        let mut fallback = self.fallback_secrets();
        let changed = match &keychain_result {
            Ok(()) => fallback.remove(key).is_some(),
            Err(_) if value.is_empty() => fallback.remove(key).is_some(),
            Err(e) => {
                if self.keychain {
                    if let Ok(mut slot) = self.keychain_error.lock() {
                        *slot = Some(e.clone());
                    }
                }
                fallback.insert(key.to_string(), value.to_string()).as_deref() != Some(value)
            }
        };
        if changed {
            self.write_fallback_secrets(&fallback);
        }
        if let Ok(mut k) = self.known.lock() {
            k.insert(key.to_string(), value.to_string());
        }
    }

    fn fallback_secrets(&self) -> HashMap<String, String> {
        std::fs::read(self.root.join("secrets.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write_fallback_secrets(&self, map: &HashMap<String, String>) {
        let path = self.root.join("secrets.json");
        if map.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        if std::fs::write(&path, serde_json::to_vec(map).unwrap_or_default()).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

/// Stores `value` under `key`, or removes the item when `value` is empty.
fn keychain_write(key: &str, value: &str) -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, key).map_err(|e| e.to_string())?;
    if value.is_empty() {
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    entry.set_password(value).or_else(|first| {
        // macOS ties an item to the build that created it; a rebuilt or updated app may be
        // refused an in-place update but allowed to replace the item.
        let _ = entry.delete_credential();
        entry.set_password(value).map_err(|_| first.to_string())
    })
}

pub fn hash_passcode(code: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"strata-passcode:");
    h.update(code.as_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let root = std::env::temp_dir().join(format!("strata-store-{name}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Store::for_test(root)
    }

    #[test]
    fn passwords_survive_a_restart_without_a_keychain() {
        let store = temp_store("restart");
        let mut c = ConnectionConfig::new(DriverKind::Postgres);
        c.password = "hunter2".into();
        store.save_connections(&ConnectionFile { connections: vec![c.clone()], groups: vec![] }).unwrap();

        let reopened = Store::for_test(store.root().to_path_buf());
        let loaded = reopened.load_connections();
        assert_eq!(loaded.connections[0].password, "hunter2");
        assert!(!std::fs::read_to_string(store.root().join("connections.json")).unwrap().contains("hunter2"));
    }

    #[test]
    fn a_changed_password_replaces_the_old_one_and_clearing_removes_it() {
        let store = temp_store("change");
        store.save_secret_value("k", "old");
        store.save_secret_value("k", "new");
        assert_eq!(Store::for_test(store.root().to_path_buf()).load_secret_value("k"), "new");
        store.save_secret_value("k", "");
        assert_eq!(Store::for_test(store.root().to_path_buf()).load_secret_value("k"), "");
        assert!(!store.root().join("secrets.json").exists());
    }
}
