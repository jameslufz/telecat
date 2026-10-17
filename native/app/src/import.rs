//! Read-only Electron import. Run on a worker after the first window is visible.
use crate::{
    credentials::{ELECTRON_ACCOUNT, ELECTRON_SERVICE, Keychain},
    store::{Profile, atomic_json, read_json},
};
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use telecat_core::types::*;
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ImportReport {
    pub files: Vec<String>,
    pub passwords: usize,
    pub histories: usize,
    pub errors: Vec<String>,
    pub already_attempted: bool,
}
/// Chromium macOS OSCrypt v10: PBKDF2-HMAC-SHA1, saltysalt, 1003, AES128-CBC.
pub fn decrypt_v10(encoded: &str, keychain_password: &[u8]) -> Result<String> {
    if encoded.is_empty() {
        return Ok(String::new());
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("Invalid secret base64")?;
    if !data.starts_with(b"v10") {
        bail!("Unsupported Electron secret version");
    }
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(keychain_password, b"saltysalt", 1003, &mut key);
    let plain = cbc::Decryptor::<aes::Aes128>::new(&key.into(), &[b' '; 16].into())
        .decrypt_padded_vec_mut::<Pkcs7>(&data[3..])
        .map_err(|_| anyhow::anyhow!("Electron secret decryption failed"))?;
    Ok(String::from_utf8(plain).context("Electron secret is not UTF-8")?)
}
pub fn electron_default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support/Telecat"))
}
pub fn import_electron(profile: &Profile, source: &Path, keys: &dyn Keychain) -> ImportReport {
    let mut report = ImportReport::default();
    if profile.root.join("electron-import.json").exists() {
        report.already_attempted = true;
        return report;
    }
    if source == profile.root
        || source
            .canonicalize()
            .ok()
            .is_some_and(|p| Some(p) == profile.root.canonicalize().ok())
    {
        report
            .errors
            .push("Source and destination profiles must differ".into());
        return report;
    }
    if !source.is_dir() {
        report.errors.push("Electron profile is unavailable".into());
        return report;
    }
    for file in ["connections.json", "queries.json", "drafts.json"] {
        let path = source.join(file);
        if !path.exists() {
            continue;
        }
        let load = (|| -> Result<bool> {
            let mut value: Value = read_json(&path)?;
            match file {
                "connections.json" => {
                    let object = value
                        .as_object_mut()
                        .context("Invalid connections document")?;
                    object
                        .entry("connections")
                        .or_insert_with(|| Value::Array(vec![]));
                    object
                        .entry("groups")
                        .or_insert_with(|| Value::Array(vec![]));
                    if let Some(cs) = object.get_mut("connections").and_then(Value::as_array_mut) {
                        for c in cs {
                            if let Some(o) = c.as_object_mut() {
                                o.remove("bootstrap");
                            }
                        }
                    }
                    let s: StoreSnapshot = serde_json::from_value(value)?;
                    profile.import_file(file, &s)
                }
                "queries.json" => {
                    let v: SavedQueries = serde_json::from_value(value)?;
                    profile.import_file(file, &v)
                }
                _ => {
                    let v: Drafts = serde_json::from_value(value)?;
                    profile.import_file(file, &v)
                }
            }
        })();
        match load {
            Ok(true) => report.files.push(file.into()),
            Ok(false) => {}
            Err(e) => report.errors.push(format!("{file}: {e}")),
        }
    }
    if source.join("secrets.json").exists() {
        let result = (|| -> Result<()> {
            let secrets: BTreeMap<String, String> = read_json(&source.join("secrets.json"))?;
            // Never overwrite a native password or import unrelated/removed connection IDs.
            let connections = profile.snapshot().connections;
            let wanted: Vec<_> = connections
                .iter()
                .filter(|c| {
                    c.password_mode == PasswordMode::Keychain && secrets.contains_key(&c.id)
                })
                .collect();
            if wanted.is_empty() {
                return Ok(());
            }
            let key = keys
                .get(ELECTRON_SERVICE, ELECTRON_ACCOUNT)?
                .context("Electron Safe Storage key is unavailable")?;
            for c in wanted {
                let result = profile.import_secret(&c.id, || decrypt_v10(&secrets[&c.id], &key));
                match result {
                    Ok(true) => report.passwords += 1,
                    Ok(false) => {}
                    Err(e) => report.errors.push(format!("Password {}: {e}", c.id)),
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            report.errors.push(format!("secrets.json: {e}"));
        }
    }
    let level = source.join("Local Storage/leveldb");
    if level.exists() {
        match read_history_snapshot(&level, &profile.root) {
            Ok(history) => match profile.import_file("history.json", &history) {
                Ok(true) => report.histories = history.len(),
                Ok(false) => {}
                Err(e) => report.errors.push(format!("history.json: {e}")),
            },
            Err(e) => report.errors.push(format!("Local Storage: {e}")),
        }
    }
    if let Err(e) = atomic_json(&profile.root.join("electron-import.json"), &report) {
        report.errors.push(format!("Import report: {e}"));
    }
    report
}
pub fn decode_storage_string(data: &[u8]) -> Result<String> {
    match data.split_first() {
        Some((1, bytes)) => Ok(bytes.iter().map(|b| char::from(*b)).collect()),
        Some((0, bytes)) if bytes.len() % 2 == 0 => String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
        .context("Invalid UTF-16 localStorage value"),
        _ => bail!("Unknown localStorage string encoding"),
    }
}
fn read_history_snapshot(source: &Path, destination: &Path) -> Result<BTreeMap<String, Value>> {
    use rusty_leveldb::{DB, LdbIterator, Options};
    fs::create_dir_all(destination)?;
    let copy = destination.join(format!(".electron-leveldb-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&copy)?;
    let result = (|| -> Result<_> {
        let mut total = 0u64;
        for entry in fs::read_dir(source)? {
            let e = entry?;
            let meta = e.path().symlink_metadata()?;
            if meta.file_type().is_symlink() {
                bail!("LevelDB snapshot contains a symlink");
            }
            if !meta.is_file() {
                continue;
            }
            total = total.saturating_add(meta.len());
            if total > 256 * 1024 * 1024 {
                bail!("LevelDB snapshot exceeds 256 MB");
            }
            if e.file_name() == "LOCK" {
                continue;
            }
            fs::copy(e.path(), copy.join(e.file_name()))?;
        }
        // Open only our private copy: LevelDB recovery, locks and compaction can never alter Electron.
        let mut db = DB::open(
            &copy,
            Options {
                create_if_missing: false,
                ..Default::default()
            },
        )?;
        let mut iter = db.new_iter()?;
        let mut out = BTreeMap::new();
        while let Some((key, value)) = iter.next() {
            if key.first() != Some(&b'_') {
                continue;
            }
            let Some(n) = key.iter().position(|b| *b == 0) else {
                continue;
            };
            let Ok(key) = decode_storage_string(&key[n + 1..]) else {
                continue;
            };
            let Some(id) = key.strip_prefix("history:") else {
                continue;
            };
            let text = decode_storage_string(&value)?;
            let v: Value = serde_json::from_str(&text)?;
            if !v.is_array() {
                bail!("History entry is not an array");
            }
            out.insert(id.to_owned(), v);
        }
        drop(iter);
        db.close()?;
        Ok(out)
    })();
    let _ = fs::remove_dir_all(copy);
    result
}
