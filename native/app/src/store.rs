use crate::credentials::{Keychain, NATIVE_SERVICE};
use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use telecat_core::types::*;
use uuid::Uuid;
pub struct Profile {
    pub root: PathBuf,
    pub keychain: Arc<dyn Keychain>,
    // Guards serialize atomic file operations; no mutable in-memory state is protected.
    // After a worker panic, recovering the guard re-reads the last complete JSON file.
    gate: Mutex<()>,
    secret_gate: Mutex<()>,
}
impl Profile {
    pub fn new(root: PathBuf, keychain: Arc<dyn Keychain>) -> Self {
        Self {
            root,
            keychain,
            gate: Mutex::new(()),
            secret_gate: Mutex::new(()),
        }
    }
    pub fn default_path() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("TELECAT_USER_DATA") {
            return Ok(p.into());
        }
        // Keep the original native profile location across the visible Telecat rename.
        // The separate Electron directory remains the read-only import source.
        Ok(
            PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?)
                .join("Library/Application Support/Telecat Native"),
        )
    }
    pub fn snapshot(&self) -> StoreSnapshot {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.snapshot_unlocked()
    }
    fn snapshot_unlocked(&self) -> StoreSnapshot {
        read_json(&self.root.join("connections.json")).unwrap_or(StoreSnapshot {
            connections: vec![],
            groups: vec![],
        })
    }
    fn edit<T>(&self, f: impl FnOnce(&mut StoreSnapshot) -> Result<T>) -> Result<T> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut s = self.snapshot_unlocked();
        let value = f(&mut s)?;
        atomic_json(&self.root.join("connections.json"), &s)?;
        Ok(value)
    }
    pub fn seed(&self, s: &StoreSnapshot) -> Result<()> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        atomic_json(&self.root.join("connections.json"), s)
    }
    pub fn connection(&self, id: &str) -> Option<ConnectionConfig> {
        self.snapshot().connections.into_iter().find(|c| c.id == id)
    }
    pub fn save_connection(
        &self,
        mut c: ConnectionConfig,
        password: Option<&str>,
    ) -> Result<ConnectionConfig> {
        let _secret_lock = self
            .secret_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if c.id.is_empty() {
            c.id = Uuid::new_v4().to_string();
        }
        if c.password_mode == PasswordMode::Keychain {
            if let Some(p) = password {
                self.keychain.set(NATIVE_SERVICE, &c.id, p.as_bytes())?;
            }
        } else {
            self.keychain.delete(NATIVE_SERVICE, &c.id)?;
        }
        self.edit(|s| {
            if let Some(old) = s.connections.iter_mut().find(|v| v.id == c.id) {
                *old = c.clone();
            } else {
                s.connections.push(c.clone());
            }
            Ok(c)
        })
    }
    pub fn delete_connection(&self, id: &str) -> Result<()> {
        let _secret_lock = self
            .secret_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.edit(|s| {
            s.connections.retain(|c| c.id != id);
            Ok(())
        })?;
        self.keychain.delete(NATIVE_SERVICE, id)
    }
    /// Mode-only edits must not resolve or modify the stored credential.
    pub fn set_safe_mode(&self, id: &str, mode: SafeMode) -> Result<()> {
        self.edit(|s| {
            let c = s
                .connections
                .iter_mut()
                .find(|c| c.id == id)
                .ok_or_else(|| anyhow::anyhow!("Connection not found"))?;
            c.safe_mode = mode;
            Ok(())
        })
    }
    pub fn duplicate_connection(&self, id: &str) -> Result<ConnectionConfig> {
        let mut c = self.connection(id).context("Connection not found")?;
        let password = self
            .keychain
            .get(NATIVE_SERVICE, id)?
            .map(String::from_utf8)
            .transpose()?;
        c.id.clear();
        c.name.push_str(" copy");
        self.save_connection(c, password.as_deref())
    }
    pub fn save_group(&self, mut g: ConnectionGroup) -> Result<ConnectionGroup> {
        if g.id.is_empty() {
            g.id = Uuid::new_v4().to_string();
        }
        self.edit(|s| {
            if let Some(old) = s.groups.iter_mut().find(|v| v.id == g.id) {
                *old = g.clone();
            } else {
                s.groups.push(g.clone());
            }
            Ok(g)
        })
    }
    pub fn delete_group(&self, id: &str) -> Result<()> {
        self.edit(|s| {
            s.groups.retain(|g| g.id != id);
            for c in &mut s.connections {
                if c.group_id.as_deref() == Some(id) {
                    c.group_id = None;
                }
            }
            Ok(())
        })
    }
    pub fn move_connection(&self, id: &str, group: Option<&str>) -> Result<()> {
        self.edit(|s| {
            if let Some(c) = s.connections.iter_mut().find(|c| c.id == id) {
                c.group_id = group.map(str::to_owned);
            }
            Ok(())
        })
    }
    /// Drag-drop also opens the receiving group, in one persisted snapshot.
    pub fn drop_connection(&self, id: &str, group: Option<&str>) -> Result<()> {
        self.edit(|s| {
            if group.is_some_and(|id| !s.groups.iter().any(|g| g.id == id)) {
                bail!("Group not found");
            }
            let c = s
                .connections
                .iter_mut()
                .find(|c| c.id == id)
                .context("Connection not found")?;
            if c.group_id.as_deref() != group {
                c.group_id = group.map(str::to_owned);
                if let Some(g) = s.groups.iter_mut().find(|g| Some(g.id.as_str()) == group) {
                    if g.collapsed == Some(true) {
                        g.collapsed = Some(false);
                    }
                }
            }
            Ok(())
        })
    }
    pub fn queries(&self, id: &str) -> Vec<SavedQuery> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read_json::<SavedQueries>(&self.root.join("queries.json"))
            .unwrap_or_default()
            .remove(id)
            .unwrap_or_default()
    }
    pub fn save_query(&self, id: &str, mut q: SavedQuery) -> Result<Vec<SavedQuery>> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all: SavedQueries = read_json(&self.root.join("queries.json")).unwrap_or_default();
        if q.id.is_empty() {
            q.id = Uuid::new_v4().to_string();
        }
        let list = all.entry(id.into()).or_default();
        if let Some(old) = list.iter_mut().find(|v| v.id == q.id) {
            *old = q;
        } else {
            list.push(q);
        }
        let out = list.clone();
        atomic_json(&self.root.join("queries.json"), &all)?;
        Ok(out)
    }
    pub fn delete_query(&self, id: &str, qid: &str) -> Result<()> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all: SavedQueries = read_json(&self.root.join("queries.json")).unwrap_or_default();
        all.entry(id.into()).or_default().retain(|q| q.id != qid);
        atomic_json(&self.root.join("queries.json"), &all)
    }
    pub fn drafts(&self, id: &str) -> Vec<SqlDraft> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read_json::<Drafts>(&self.root.join("drafts.json"))
            .unwrap_or_default()
            .remove(id)
            .unwrap_or_default()
    }
    pub fn history(&self, id: &str) -> Vec<LogEntry> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read_json::<std::collections::BTreeMap<String, Vec<LogEntry>>>(
            &self.root.join("history.json"),
        )
        .unwrap_or_default()
        .remove(id)
        .unwrap_or_default()
    }
    pub fn add_history(&self, id: &str, entry: LogEntry) -> Result<()> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all: std::collections::BTreeMap<String, Vec<LogEntry>> =
            read_json(&self.root.join("history.json")).unwrap_or_default();
        let rows = all.entry(id.into()).or_default();
        rows.retain(|x| x.sql != entry.sql);
        rows.insert(0, entry);
        rows.truncate(500);
        atomic_json(&self.root.join("history.json"), &all)
    }
    pub fn clear_history(&self, id: &str) -> Result<()> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all: std::collections::BTreeMap<String, Vec<LogEntry>> =
            read_json(&self.root.join("history.json")).unwrap_or_default();
        all.remove(id);
        atomic_json(&self.root.join("history.json"), &all)
    }
    pub fn save_drafts(&self, id: &str, drafts: &[SqlDraft]) -> Result<()> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all: Drafts = read_json(&self.root.join("drafts.json")).unwrap_or_default();
        let keep: Vec<_> = drafts
            .iter()
            .filter(|d| !telecat_core::sql::trim(&d.sql).is_empty())
            .cloned()
            .collect();
        if keep.is_empty() {
            all.remove(id);
        } else {
            all.insert(id.into(), keep);
        }
        atomic_json(&self.root.join("drafts.json"), &all)
    }
    /// Serialize import against Save/Delete without holding the JSON lock during Keychain I/O.
    pub(crate) fn import_secret(
        &self,
        id: &str,
        decode: impl FnOnce() -> Result<String>,
    ) -> Result<bool> {
        let _secret_lock = self
            .secret_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self
            .connection(id)
            .is_some_and(|c| c.password_mode == PasswordMode::Keychain)
            || self.keychain.get(NATIVE_SERVICE, id)?.is_some()
        {
            return Ok(false);
        }
        let secret = decode()?;
        self.keychain.set(NATIVE_SERVICE, id, secret.as_bytes())?;
        Ok(true)
    }
    /// Import only missing files, serialized with edits so an early Save always wins.
    pub(crate) fn import_file<T: Serialize>(&self, name: &str, value: &T) -> Result<bool> {
        let _lock = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.root.join(name);
        if path.exists() {
            return Ok(false);
        }
        atomic_json(&path, value)?;
        Ok(true)
    }
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let meta = fs::metadata(path)?;
    if meta.len() > 32 * 1024 * 1024 {
        bail!("JSON file exceeds 32 MB");
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("Profile path has no parent")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut opt = OpenOptions::new();
        opt.write(true).create_new(true);
        #[cfg(unix)]
        opt.mode(0o600);
        let mut file = opt.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok::<_, anyhow::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub fn connection_matches(c: &ConnectionConfig, search: &str) -> bool {
    let q = search.trim().to_lowercase();
    q.is_empty()
        || [&c.name, &c.host, &c.database, &c.tag, &c.user]
            .iter()
            .any(|s| s.to_lowercase().contains(&q))
}
/// Exact Electron context-menu model; renderer supplies the native presentation.
pub fn connection_menu(c: &ConnectionConfig, s: &StoreSnapshot) -> Value {
    let mut groups = vec![json!({"id":"move:","label":"None","enabled":c.group_id.is_some()})];
    if !s.groups.is_empty() {
        groups.push(json!({"type":"separator"}));
    }
    for g in &s.groups {
        groups.push(json!({"id":format!("move:{}",g.id),"label":g.name,"enabled":c.group_id.as_ref()!=Some(&g.id)}));
    }
    groups.extend([
        json!({"type":"separator"}),
        json!({"id":"move-new","label":"New Group…"}),
    ]);
    json!([{"id":"connect","label":"Connect"},{"id":"edit","label":"Edit…"},{"id":"duplicate","label":"Duplicate"},{"type":"separator"},{"label":"Move to Group","submenu":groups},{"type":"separator"},{"id":"delete","label":"Delete"}])
}
