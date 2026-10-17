use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use telecat_app::{credentials::*, form, import::*, store::*};
use telecat_core::types::*;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../.cache/m2-tests")
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn profile(&self) -> Profile {
        Profile::new(self.0.join("native"), Arc::new(MemoryKeychain::default()))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn config() -> ConnectionConfig {
    let mut c = empty_connection(Driver::Mysql);
    c.id = "one".into();
    c.name = "ธนาคารกรุงเทพ 😺".into();
    c.port = 3307;
    c
}
#[test]
fn actual_electron_store_transitions_round_trip() -> Result<()> {
    let t = Temp::new();
    let p = t.profile();
    let fx: Value = serde_json::from_str(include_str!("fixtures/electron-store.json"))?;
    for case in fx["cases"].as_array().unwrap() {
        let a = &case["args"];
        let str_at = |i: usize| a[i].as_str().unwrap();
        match case["op"].as_str().unwrap() {
            "upsertConnection" => {
                p.save_connection(serde_json::from_value(a[0].clone())?, a[1].as_str())?;
            }
            "saveGroup" => {
                p.save_group(serde_json::from_value(a[0].clone())?)?;
            }
            "moveConnection" => p.move_connection(str_at(0), a[1].as_str())?,
            "deleteGroup" => p.delete_group(str_at(0))?,
            "deleteConnection" => p.delete_connection(str_at(0))?,
            "saveQuery" => {
                p.save_query(str_at(0), serde_json::from_value(a[1].clone())?)?;
            }
            "deleteQuery" => p.delete_query(str_at(0), str_at(1))?,
            "saveDrafts" => p.save_drafts(
                str_at(0),
                &serde_json::from_value::<Vec<SqlDraft>>(a[1].clone())?,
            )?,
            _ => unreachable!(),
        }
        assert_eq!(json!(p.snapshot()), case["store"], "{}", case["op"]);
        assert_eq!(
            read_json::<Value>(&p.root.join("queries.json")).unwrap_or(json!({})),
            case["queries"]
        );
        assert_eq!(
            read_json::<Value>(&p.root.join("drafts.json")).unwrap_or(json!({})),
            case["drafts"]
        );
        let document: StoreSnapshot =
            serde_json::from_slice(&fs::read(p.root.join("connections.json"))?)?;
        assert_eq!(document, p.snapshot());
    }
    assert!(p.keychain.get(NATIVE_SERVICE, "one")?.is_none());
    Ok(())
}
#[test]
fn store_password_modes_duplicate_delete_and_permissions() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let t = Temp::new();
    let p = t.profile();
    let mut c = config();
    c.id.clear();
    c = p.save_connection(c, Some("secret"))?;
    assert!(!c.id.is_empty());
    p.save_connection(c.clone(), None)?;
    assert_eq!(
        p.keychain.get(NATIVE_SERVICE, &c.id)?,
        Some(b"secret".to_vec())
    );
    let copy = p.duplicate_connection(&c.id)?;
    assert_eq!(copy.name, format!("{} copy", c.name));
    assert_ne!(copy.id, c.id);
    assert_eq!(
        p.keychain.get(NATIVE_SERVICE, &copy.id)?,
        Some(b"secret".to_vec())
    );
    let text = fs::read_to_string(p.root.join("connections.json"))?;
    assert!(!text.contains("secret"));
    assert_eq!(
        fs::metadata(p.root.join("connections.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for mode in [PasswordMode::Ask, PasswordMode::Command, PasswordMode::None] {
        c.password_mode = mode;
        p.save_connection(c.clone(), Some("not-stored"))?;
        assert!(p.keychain.get(NATIVE_SERVICE, &c.id)?.is_none());
    }
    p.delete_connection(&copy.id)?;
    assert!(p.keychain.get(NATIVE_SERVICE, &copy.id)?.is_none());
    assert!(p.connection(&copy.id).is_none());
    Ok(())
}
#[test]
fn groups_drag_between_into_out_collapsed_survive_reload() -> Result<()> {
    let t = Temp::new();
    let p = t.profile();
    p.save_connection(config(), None)?;
    for id in ["a", "b"] {
        p.save_group(ConnectionGroup {
            id: id.into(),
            name: id.into(),
            collapsed: Some(true),
        })?;
    }
    for target in [Some("a"), Some("b"), None] {
        p.drop_connection("one", target)?;
        let reloaded = Profile::new(p.root.clone(), p.keychain.clone());
        assert_eq!(
            reloaded.connection("one").unwrap().group_id.as_deref(),
            target
        );
        if let Some(id) = target {
            assert_eq!(
                reloaded
                    .snapshot()
                    .groups
                    .iter()
                    .find(|g| g.id == id)
                    .unwrap()
                    .collapsed,
                Some(false)
            );
        }
    }
    let before = p.snapshot();
    assert!(p.drop_connection("missing", Some("a")).is_err());
    assert!(p.drop_connection("one", Some("missing")).is_err());
    assert_eq!(p.snapshot(), before);
    p.move_connection("one", Some("b"))?;
    p.delete_group("b")?;
    assert_eq!(p.connection("one").unwrap().group_id, None);
    assert_eq!(p.snapshot().connections.len(), 1);
    Ok(())
}
#[test]
fn validation_and_search_match_electron() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let c = empty_connection(d);
        for (text, want) in [
            ("", default_port(d)),
            ("0", default_port(d)),
            ("12x34", 1234),
            ("65535", 65535),
        ] {
            assert_eq!(form::validate(&c, text)?.port, want);
        }
        for text in ["65536", "999999999999999999"] {
            assert!(form::validate(&c, text).is_err());
        }
    }
    let mut c = config();
    for field in [0, 1, 2, 3] {
        let mut bad = c.clone();
        match field {
            0 => bad.name = "a\0b".into(),
            1 => bad.host = "a\0b".into(),
            2 => bad.user = "a\0b".into(),
            _ => bad.database = "a\0b".into(),
        };
        assert!(form::validate(&bad, "3307").is_err());
    }
    c.tag = "local".into();
    c.user = "root".into();
    for s in ["ธนาคาร", "😺", " LOCAL ", "ROOT", "127.0", " "] {
        assert!(connection_matches(&c, s));
    }
    assert!(!connection_matches(&c, "missing"));
    let menu = connection_menu(
        &c,
        &StoreSnapshot {
            connections: vec![],
            groups: vec![],
        },
    );
    assert_eq!(menu[0]["label"], "Connect");
    assert_eq!(menu[1]["label"], "Edit…");
    assert_eq!(menu[4]["label"], "Move to Group");
    assert_eq!(menu[6]["label"], "Delete");
    Ok(())
}
#[tokio::test]
async fn command_password_success_failure_empty_unicode_and_timeout() -> Result<()> {
    let t = Temp::new();
    let run = |s| run_password_command_options(s, Duration::from_secs(2), Some(&t.0));
    for (command, expected) in [
        ("printf 'secret\\r\\n'", "secret"),
        ("printf 'a\\n\\n'", "a\n"),
        ("printf 'ธนาคารกรุงเทพ 😺'", "ธนาคารกรุงเทพ 😺"),
        ("true", ""),
        ("printf '  spaced  '", "  spaced  "),
    ] {
        assert_eq!(run(command).await?, expected);
    }
    assert_eq!(
        run(" \t ").await.unwrap_err().to_string(),
        "Password command is empty"
    );
    assert!(
        run("printf 'no access' >&2; exit 7")
            .await
            .unwrap_err()
            .to_string()
            .contains("no access")
    );
    assert!(run("exit 9").await.is_err());
    assert!(
        run("printf '%1100000s' x")
            .await
            .unwrap_err()
            .to_string()
            .contains("1 MiB")
    );
    let start = Instant::now();
    assert!(
        run_password_command_options("sleep 5", Duration::from_millis(50), Some(&t.0))
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    Ok(())
}
#[tokio::test]
async fn password_resolution_uses_injected_keychain_only() -> Result<()> {
    let keys = MemoryKeychain::default();
    let mut c = config();
    keys.set(NATIVE_SERVICE, &c.id, b"stored")?;
    assert_eq!(resolve_password(&c, None, &keys).await?, "stored");
    assert_eq!(resolve_password(&c, Some("typed"), &keys).await?, "typed");
    c.password_mode = PasswordMode::Ask;
    assert_eq!(resolve_password(&c, Some("asked"), &keys).await?, "asked");
    c.password_mode = PasswordMode::None;
    assert_eq!(resolve_password(&c, Some("ignored"), &keys).await?, "");
    Ok(())
}
#[test]
fn oscrypt_independent_node_vectors_and_failures() -> Result<()> {
    let fx: Value = serde_json::from_str(include_str!("fixtures/oscrypt.json"))?;
    let key = fx["key"].as_str().unwrap().as_bytes();
    for case in fx["cases"].as_array().unwrap() {
        assert_eq!(
            decrypt_v10(case["encrypted"].as_str().unwrap(), key)?,
            case["plain"]
        );
        assert!(decrypt_v10(case["encrypted"].as_str().unwrap(), b"wrong key").is_err());
    }
    assert_eq!(decrypt_v10("", key)?, "");
    for malformed in ["?", "djExYWJj", "djEw", "djEwYWJj"] {
        assert!(decrypt_v10(malformed, key).is_err());
    }
    Ok(())
}
fn electron_source(t: &Temp) -> Result<PathBuf> {
    let source = t.0.join("electron");
    fs::create_dir_all(&source)?;
    atomic_json(
        &source.join("connections.json"),
        &json!({"connections":[config()],"groups":[]}),
    )?;
    atomic_json(
        &source.join("queries.json"),
        &json!({"one":[{"id":"q","name":"😺","sql":"SELECT 1"}]}),
    )?;
    atomic_json(
        &source.join("drafts.json"),
        &json!({"one":[{"title":"Query","sql":"SELECT 2"}]}),
    )?;
    Ok(source)
}
fn digest_files(p: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = vec![];
    if p.is_dir() {
        for e in fs::read_dir(p).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(digest_files(&p));
            } else {
                out.push((p.clone(), fs::read(p).unwrap()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
fn history_db(source: &Path) -> Result<()> {
    let path = source.join("Local Storage/leveldb");
    fs::create_dir_all(&path)?;
    let mut db = rusty_leveldb::DB::open(
        path,
        rusty_leveldb::Options {
            create_if_missing: true,
            ..Default::default()
        },
    )?;
    let mut key = b"_file://\0\x01history:one".to_vec();
    let data =
        json!([{"sql":"SELECT 'ธนาคารกรุงเทพ 😺'","time":"2026-10-17T00:00:00Z"}]).to_string();
    let mut value = vec![0];
    for u in data.encode_utf16() {
        value.extend_from_slice(&u.to_le_bytes());
    }
    db.put(&key, &value)?;
    key[0] = b'x';
    db.put(&key, b"unrelated")?;
    db.flush()?;
    db.close()?;
    Ok(())
}
#[test]
fn import_all_formats_read_only_and_once() -> Result<()> {
    let t = Temp::new();
    let p = t.profile();
    let source = electron_source(&t)?;
    history_db(&source)?;
    let fx: Value = serde_json::from_str(include_str!("fixtures/oscrypt.json"))?;
    atomic_json(
        &source.join("secrets.json"),
        &json!({"one":fx["cases"][2]["encrypted"],"removed":"invalid"}),
    )?;
    let keys = MemoryKeychain::default();
    keys.set(
        ELECTRON_SERVICE,
        ELECTRON_ACCOUNT,
        fx["key"].as_str().unwrap().as_bytes(),
    )?;
    let before = digest_files(&source);
    let report = import_electron(&p, &source, &keys);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.files.len(), 3);
    assert_eq!(report.passwords, 1);
    assert_eq!(report.histories, 1);
    assert_eq!(p.connection("one"), Some(config()));
    assert_eq!(p.queries("one").len(), 1);
    assert_eq!(p.drafts("one").len(), 1);
    assert_eq!(
        p.keychain.get(NATIVE_SERVICE, "one")?.unwrap(),
        "ธนาคารกรุงเทพ 😺".as_bytes()
    );
    assert_eq!(before, digest_files(&source));
    assert!(import_electron(&p, &source, &keys).already_attempted);
    assert!(p.root.read_dir()?.all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".electron-leveldb")
    }));
    Ok(())
}
struct Denied;
impl Keychain for Denied {
    fn get(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>> {
        bail!("injected denial")
    }
    fn set(&self, _: &str, _: &str, _: &[u8]) -> Result<()> {
        bail!("injected denial")
    }
    fn delete(&self, _: &str, _: &str) -> Result<()> {
        bail!("injected denial")
    }
}
#[test]
fn import_failure_paths_are_isolated_and_do_not_overwrite() -> Result<()> {
    let t = Temp::new();
    let keys = MemoryKeychain::default();
    let p = t.profile();
    assert!(
        !import_electron(&p, &t.0.join("missing"), &keys)
            .errors
            .is_empty()
    );
    fs::create_dir_all(&p.root)?;
    assert!(!import_electron(&p, &p.root, &keys).errors.is_empty());
    let source = electron_source(&t)?;
    fs::write(source.join("queries.json"), "{broken")?;
    fs::write(source.join("drafts.json"), "[]")?;
    atomic_json(&source.join("secrets.json"), &json!({"one":"bad"}))?;
    let report = import_electron(&p, &source, &Denied);
    assert_eq!(report.files, ["connections.json"]);
    assert_eq!(report.errors.len(), 3);
    assert!(p.connection("one").is_some());
    let t = Temp::new();
    let p = t.profile();
    let source = electron_source(&t)?;
    let mut native = config();
    native.name = "native edit wins".into();
    p.save_connection(native.clone(), Some("native password"))?;
    let report = import_electron(&p, &source, &keys);
    assert!(!report.files.contains(&"connections.json".into()));
    assert_eq!(p.connection("one"), Some(native));
    assert_eq!(
        p.keychain.get(NATIVE_SERVICE, "one")?.unwrap(),
        b"native password"
    );
    Ok(())
}
#[test]
fn import_bad_documents_ciphertexts_history_and_write_failures() -> Result<()> {
    for (file, data) in [
        ("connections.json", "[]"),
        ("connections.json", "{\"connections\":[{}]}"),
        ("secrets.json", "[]"),
    ] {
        let t = Temp::new();
        let p = t.profile();
        let source = electron_source(&t)?;
        fs::write(source.join(file), data)?;
        assert!(
            import_electron(&p, &source, &MemoryKeychain::default())
                .errors
                .iter()
                .any(|e| e.starts_with(file))
        );
    }
    for key in [None, Some(b"wrong".as_slice())] {
        let t = Temp::new();
        let p = t.profile();
        let source = electron_source(&t)?;
        let fx: Value = serde_json::from_str(include_str!("fixtures/oscrypt.json"))?;
        atomic_json(
            &source.join("secrets.json"),
            &json!({"one":fx["cases"][1]["encrypted"]}),
        )?;
        let keys = MemoryKeychain::default();
        if let Some(k) = key {
            keys.set(ELECTRON_SERVICE, ELECTRON_ACCOUNT, k)?;
        }
        assert!(!import_electron(&p, &source, &keys).errors.is_empty());
        assert!(p.keychain.get(NATIVE_SERVICE, "one")?.is_none());
    }
    let t = Temp::new();
    let p = t.profile();
    let source = electron_source(&t)?;
    fs::create_dir_all(source.join("Local Storage/leveldb"))?;
    fs::write(source.join("Local Storage/leveldb/CURRENT"), "corrupt")?;
    assert!(
        import_electron(&p, &source, &MemoryKeychain::default())
            .errors
            .iter()
            .any(|e| e.starts_with("Local Storage"))
    );
    let t = Temp::new();
    let p = t.profile();
    let source = electron_source(&t)?;
    fs::write(&p.root, "not a directory")?;
    let report = import_electron(&p, &source, &MemoryKeychain::default());
    assert!(report.errors.iter().any(|e| e.starts_with("Import report")));
    assert!(!report.errors.is_empty());
    assert!(decode_storage_string(&[0, 1]).is_err());
    assert!(decode_storage_string(&[3, 1]).is_err());
    assert_eq!(decode_storage_string(&[1, 233])?, "é");
    Ok(())
}
#[test]
fn store_missing_corrupt_legacy_and_failed_write_are_recoverable() -> Result<()> {
    let t = Temp::new();
    let p = t.profile();
    assert!(p.snapshot().connections.is_empty());
    fs::create_dir_all(&p.root)?;
    fs::write(p.root.join("connections.json"), "malformed")?;
    assert!(p.snapshot().connections.is_empty());
    let mut legacy = json!(config());
    legacy["bootstrap"] = json!("NEVER EXECUTE");
    atomic_json(
        &p.root.join("connections.json"),
        &json!({"connections":[legacy]}),
    )?;
    assert_eq!(p.snapshot().connections.len(), 1);
    p.save_connection(config(), None)?;
    assert!(!fs::read_to_string(p.root.join("connections.json"))?.contains("bootstrap"));
    let t2 = Temp::new();
    fs::write(t2.0.join("block"), "file")?;
    let p2 = Profile::new(
        t2.0.join("block/child"),
        Arc::new(MemoryKeychain::default()),
    );
    assert!(p2.save_connection(config(), None).is_err());
    Ok(())
}

#[tokio::test]
async fn nontransactional_confirmation_precedes_every_write() -> Result<()> {
    use telecat_core::changes::Stmt;
    let mut c = config();
    c.user = "root".into();
    c.database = "zixma".into();
    let db = telecat_db::Session::connect_test(&c, "secret").await?;
    db.query("DROP TABLE IF EXISTS telecat_m2_confirm").await?;
    db.query("CREATE TABLE telecat_m2_confirm (v int) ENGINE=MyISAM")
        .await?;
    let stmt = Stmt {
        sql: "INSERT INTO telecat_m2_confirm VALUES (1)".into(),
        params: vec![],
        expect: Some(1),
        check: None,
    };
    let result = telecat_app::commit::execute_reviewed(&db, &[stmt.clone()], |review| async move {
        assert!(
            review
                .message()
                .contains("cannot undo a wrong affected-row count")
        );
        false
    })
    .await;
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(
        db.query("SELECT COUNT(*) FROM telecat_m2_confirm")
            .await?
            .rows[0][0]
            .as_deref(),
        Some("0")
    );
    telecat_app::commit::execute_reviewed(&db, &[stmt], |_| async { true }).await?;
    assert_eq!(
        db.query("SELECT COUNT(*) FROM telecat_m2_confirm")
            .await?
            .rows[0][0]
            .as_deref(),
        Some("1")
    );
    db.query("DROP TABLE telecat_m2_confirm").await?;
    db.close().await;
    Ok(())
}

#[test]
fn import_rejects_oversized_json_and_unsafe_leveldb_snapshot() -> Result<()> {
    let t = Temp::new();
    let p = t.profile();
    let source = electron_source(&t)?;
    fs::File::create(source.join("connections.json"))?.set_len(32 * 1024 * 1024 + 1)?;
    let report = import_electron(&p, &source, &MemoryKeychain::default());
    assert!(report.errors.iter().any(|e| e.contains("32 MB")));
    assert_eq!(report.files.len(), 2);
    for oversized in [false, true] {
        let t = Temp::new();
        let p = t.profile();
        let source = electron_source(&t)?;
        let level = source.join("Local Storage/leveldb");
        fs::create_dir_all(&level)?;
        if oversized {
            fs::File::create(level.join("oversized"))?.set_len(256 * 1024 * 1024 + 1)?;
        } else {
            std::os::unix::fs::symlink(source.join("connections.json"), level.join("link"))?;
        }
        let report = import_electron(&p, &source, &MemoryKeychain::default());
        assert!(report.errors.iter().any(|e| e.contains(if oversized {
            "256 MB"
        } else {
            "symlink"
        })));
        assert_eq!(report.files.len(), 3);
    }
    Ok(())
}
#[test]
fn import_preserves_native_history_and_reports_keychain_write_denial() -> Result<()> {
    struct FailSet;
    impl Keychain for FailSet {
        fn get(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str, _: &[u8]) -> Result<()> {
            bail!("injected write denial")
        }
        fn delete(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }
    let t = Temp::new();
    let source = electron_source(&t)?;
    let p = Profile::new(t.0.join("native"), Arc::new(FailSet));
    history_db(&source)?;
    atomic_json(
        &p.root.join("history.json"),
        &json!({"native":["retained"]}),
    )?;
    let fx: Value = serde_json::from_str(include_str!("fixtures/oscrypt.json"))?;
    atomic_json(
        &source.join("secrets.json"),
        &json!({"one":fx["cases"][1]["encrypted"]}),
    )?;
    let keys = MemoryKeychain::default();
    keys.set(
        ELECTRON_SERVICE,
        ELECTRON_ACCOUNT,
        fx["key"].as_str().unwrap().as_bytes(),
    )?;
    let report = import_electron(&p, &source, &keys);
    assert!(report.errors.iter().any(|e| e.contains("write denial")));
    assert_eq!(report.histories, 0);
    assert_eq!(
        read_json::<Value>(&p.root.join("history.json"))?,
        json!({"native":["retained"]})
    );
    assert!(p.connection("one").is_some());
    Ok(())
}

#[test]
fn a_save_during_import_wins_without_blocking_profile_reads() -> Result<()> {
    use std::sync::{
        Barrier,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    struct SlowKeychain {
        inner: MemoryKeychain,
        entered: Barrier,
        release: Barrier,
        once: AtomicBool,
        writes: mpsc::Sender<()>,
    }
    impl Keychain for SlowKeychain {
        fn get(&self, s: &str, a: &str) -> Result<Option<Vec<u8>>> {
            if s == NATIVE_SERVICE && self.once.swap(false, Ordering::SeqCst) {
                self.entered.wait();
                self.release.wait();
            }
            self.inner.get(s, a)
        }
        fn set(&self, s: &str, a: &str, v: &[u8]) -> Result<()> {
            if v == b"native edit" {
                self.writes.send(()).unwrap();
            }
            self.inner.set(s, a, v)
        }
        fn delete(&self, s: &str, a: &str) -> Result<()> {
            self.inner.delete(s, a)
        }
    }
    let t = Temp::new();
    let source = electron_source(&t)?;
    let fx: Value = serde_json::from_str(include_str!("fixtures/oscrypt.json"))?;
    atomic_json(
        &source.join("secrets.json"),
        &json!({"one":fx["cases"][1]["encrypted"]}),
    )?;
    let electron = MemoryKeychain::default();
    electron.set(
        ELECTRON_SERVICE,
        ELECTRON_ACCOUNT,
        fx["key"].as_str().unwrap().as_bytes(),
    )?;
    let (tx, rx) = mpsc::channel();
    let keys = Arc::new(SlowKeychain {
        inner: MemoryKeychain::default(),
        entered: Barrier::new(2),
        release: Barrier::new(2),
        once: AtomicBool::new(true),
        writes: tx,
    });
    let p = Arc::new(Profile::new(t.0.join("native"), keys.clone()));
    let importer = p.clone();
    let thread = std::thread::spawn(move || import_electron(&importer, &source, &electron));
    keys.entered.wait();
    // A pending Keychain operation must not hold the profile snapshot lock.
    assert_eq!(p.snapshot().connections.len(), 1);
    let saver = p.clone();
    let save = std::thread::spawn(move || saver.save_connection(config(), Some("native edit")));
    assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    keys.release.wait();
    assert!(thread.join().unwrap().errors.is_empty());
    save.join().unwrap()?;
    assert_eq!(
        keys.inner.get(NATIVE_SERVICE, "one")?,
        Some(b"native edit".to_vec())
    );
    Ok(())
}

#[test]
fn keychain_worker_panic_does_not_poison_future_profile_operations() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct PanicOnce {
        once: AtomicBool,
        keys: MemoryKeychain,
    }
    impl Keychain for PanicOnce {
        fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>> {
            self.keys.get(service, account)
        }
        fn delete(&self, service: &str, account: &str) -> Result<()> {
            self.keys.delete(service, account)
        }
        fn set(&self, service: &str, account: &str, value: &[u8]) -> Result<()> {
            assert!(
                !self.once.swap(false, Ordering::SeqCst),
                "synthetic Keychain worker failure"
            );
            self.keys.set(service, account, value)
        }
    }
    let t = Temp::new();
    let p = Arc::new(Profile::new(
        t.0.join("native"),
        Arc::new(PanicOnce {
            once: AtomicBool::new(true),
            keys: MemoryKeychain::default(),
        }),
    ));
    let runtime: telecat_app::worker::Runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .into();
    let save = p.clone();
    assert!(
        futures::executor::block_on(
            runtime.spawn_blocking(move || save.save_connection(config(), Some("synthetic")))
        )
        .is_err()
    );
    assert!(p.snapshot().connections.is_empty());
    p.save_connection(config(), Some("recovered"))?;
    assert_eq!(p.snapshot().connections.len(), 1);
    assert_eq!(
        p.keychain.get(NATIVE_SERVICE, "one")?,
        Some(b"recovered".to_vec())
    );
    Ok(())
}
