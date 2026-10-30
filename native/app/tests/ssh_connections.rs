//! Saved connections in 0.7: older profiles keep working, a damaged file is never overwritten,
//! SSH secrets live in the Keychain next to the database password, and connect errors say
//! which side failed.
use std::{path::PathBuf, sync::Arc};
use telecat_app::{
    connect::host_key_prompt,
    credentials::{
        Asked, Keychain, MemoryKeychain, NATIVE_SERVICE, Typed, asked_secrets,
        connect_error_message, resolve_credentials, ssh_passphrase_account, ssh_password_account,
        with_keychain_secrets,
    },
    form,
    store::{BACKUP_FILE, Profile, Secrets},
};
use telecat_core::types::*;
use telecat_db::ssh::{
    HostKeyKind, HostKeyProblem, SshError,
    known_hosts::{HostKey, Recorded},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "telecat-ssh-connections-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn profile(t: &Temp) -> (Profile, Arc<MemoryKeychain>) {
    let keychain = Arc::new(MemoryKeychain::default());
    (Profile::new(t.0.clone(), keychain.clone()), keychain)
}
fn secret(k: &MemoryKeychain, account: &str) -> Option<String> {
    k.get(NATIVE_SERVICE, account)
        .unwrap()
        .map(|v| String::from_utf8(v).unwrap())
}
fn ssh_connection() -> ConnectionConfig {
    let mut c = empty_connection(Driver::Postgres);
    c.name = "Prod".into();
    c.host = "10.0.0.5".into();
    c.ssh = SshConfig {
        enabled: true,
        host: "bastion".into(),
        port: 2200,
        user: "deploy".into(),
        ..SshConfig::default()
    };
    c
}

/// A connections.json as Telecat 0.6 wrote it.
const V06: &str = r##"{
  "connections": [
    {
      "id": "c1",
      "driver": "mysql",
      "name": "Zixma",
      "color": "#3a7d44",
      "tag": "local",
      "safeMode": "silent",
      "host": "127.0.0.1",
      "port": 3307,
      "user": "root",
      "passwordMode": "keychain",
      "database": "zixma",
      "sslMode": "PREFERRED",
      "negotiation": "POSTGRES",
      "groupId": null
    }
  ],
  "groups": []
}"##;

#[test]
fn a_profile_from_0_6_loads_with_everything_new_off_and_is_backed_up_once() {
    let t = Temp::new();
    std::fs::write(t.0.join("connections.json"), V06).unwrap();
    let (p, _) = profile(&t);
    let snapshot = p.snapshot();
    assert_eq!(snapshot.connections.len(), 1);
    let c = &snapshot.connections[0];
    assert!(!c.ssh.enabled && c.ssh.is_unset());
    assert!(c.startup_sql.is_empty() && !c.disable_channel_binding && !c.load_system_schemas);
    // The first change keeps the old file as it was.
    let mut changed = c.clone();
    changed.name = "Zixma 2".into();
    p.save_connection(changed.clone(), None).unwrap();
    assert_eq!(std::fs::read_to_string(t.0.join(BACKUP_FILE)).unwrap(), V06);
    // Untouched new settings aren't written, so 0.6 reads the file the same way.
    let written = std::fs::read_to_string(t.0.join("connections.json")).unwrap();
    for key in [
        "ssh",
        "startupSql",
        "disableChannelBinding",
        "loadSystemSchemas",
    ] {
        assert!(!written.contains(key), "{key} in {written}");
    }
    // Later changes don't touch the backup.
    changed.name = "Zixma 3".into();
    p.save_connection(changed, None).unwrap();
    assert_eq!(std::fs::read_to_string(t.0.join(BACKUP_FILE)).unwrap(), V06);
}

#[test]
fn new_settings_round_trip_and_unknown_fields_from_newer_versions_are_ignored() {
    let t = Temp::new();
    let (p, _) = profile(&t);
    let mut c = ssh_connection();
    c.ssh.use_key = true;
    c.ssh.key_path = Some("~/.ssh/id_prod".into());
    c.ssh.password_mode = PasswordMode::None;
    c.ssh.passphrase_mode = PassphraseMode::Ask;
    c.startup_sql = "SET search_path TO app;".into();
    c.disable_channel_binding = true;
    c.load_system_schemas = true;
    let saved = p.save_connection(c, None).unwrap();
    assert_eq!(p.connection(&saved.id).unwrap(), saved);
    let path = t.0.join("connections.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["connections"][0]["fromTheFuture"] = serde_json::json!(true);
    json["connections"][0]["ssh"]["jumpHosts"] = serde_json::json!([]);
    std::fs::write(&path, json.to_string()).unwrap();
    assert_eq!(p.connection(&saved.id).unwrap(), saved);
}

#[test]
fn a_damaged_connections_file_is_never_overwritten() {
    let t = Temp::new();
    let path = t.0.join("connections.json");
    let damaged = r#"{"connections": [{"id": "c1", "driver": "oracle"}]}"#;
    std::fs::write(&path, damaged).unwrap();
    let (p, _) = profile(&t);
    let e = p
        .save_connection(empty_connection(Driver::Mysql), None)
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("couldn't read its saved connections"),
        "{e}"
    );
    assert!(p.delete_connection("c1").is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), damaged);
}

#[test]
fn ssh_secrets_are_saved_duplicated_and_deleted_with_the_connection() {
    let t = Temp::new();
    let (p, k) = profile(&t);
    let saved = p
        .save_connection_with(
            ssh_connection(),
            Secrets {
                password: Some("db"),
                ssh_password: Some("ssh"),
                passphrase: Some("phrase"),
            },
        )
        .unwrap();
    let id = saved.id.clone();
    assert_eq!(secret(&k, &id).as_deref(), Some("db"));
    assert_eq!(
        secret(&k, &ssh_password_account(&id)).as_deref(),
        Some("ssh")
    );
    assert_eq!(
        secret(&k, &ssh_passphrase_account(&id)).as_deref(),
        Some("phrase")
    );
    assert_eq!(
        p.saved_secrets(&id).unwrap(),
        [Some("db".into()), Some("ssh".into()), Some("phrase".into())]
    );

    let copy = p.duplicate_connection(&id).unwrap();
    assert_ne!(copy.id, id);
    assert_eq!(
        p.saved_secrets(&copy.id).unwrap(),
        [Some("db".into()), Some("ssh".into()), Some("phrase".into())]
    );

    // Saving without new values keeps them; switching a mode away from the Keychain removes
    // that secret only.
    let mut c = saved.clone();
    p.save_connection(c.clone(), None).unwrap();
    assert_eq!(
        secret(&k, &ssh_password_account(&id)).as_deref(),
        Some("ssh")
    );
    c.ssh.password_mode = PasswordMode::Ask;
    c.ssh.passphrase_mode = PassphraseMode::Ask;
    p.save_connection_with(c, Secrets::default()).unwrap();
    assert_eq!(secret(&k, &id).as_deref(), Some("db"));
    assert_eq!(secret(&k, &ssh_password_account(&id)), None);
    assert_eq!(secret(&k, &ssh_passphrase_account(&id)), None);

    p.delete_connection(&copy.id).unwrap();
    assert_eq!(p.saved_secrets(&copy.id).unwrap(), [None, None, None]);
}

#[tokio::test]
async fn credentials_come_from_what_was_typed_then_the_keychain() {
    let k = MemoryKeychain::default();
    let mut c = ssh_connection();
    c.id = "c1".into();
    k.set(NATIVE_SERVICE, "c1", b"db").unwrap();
    k.set(NATIVE_SERVICE, &ssh_password_account("c1"), b"ssh")
        .unwrap();
    k.set(NATIVE_SERVICE, &ssh_passphrase_account("c1"), b"phrase")
        .unwrap();
    let r = resolve_credentials(&c, &Typed::default(), &k)
        .await
        .unwrap();
    assert_eq!(r.password, "db");
    assert_eq!(r.ssh.password.as_deref().map(|s| s.as_str()), Some("ssh"));
    assert_eq!(
        r.ssh.passphrase.as_deref().map(|s| s.as_str()),
        Some("phrase")
    );

    let typed = Typed {
        password: Some("typed-db".into()),
        ssh_password: Some("typed-ssh".into()),
        passphrase: Some(String::new()),
    };
    let r = resolve_credentials(&c, &typed, &k).await.unwrap();
    assert_eq!(r.password, "typed-db");
    assert_eq!(
        r.ssh.password.as_deref().map(|s| s.as_str()),
        Some("typed-ssh")
    );
    assert!(r.ssh.passphrase.is_none(), "an empty passphrase is none");

    c.ssh.password_mode = PasswordMode::None;
    c.ssh.passphrase_mode = PassphraseMode::Ask;
    let r = resolve_credentials(&c, &Typed::default(), &k)
        .await
        .unwrap();
    assert!(r.ssh.password.is_none() && r.ssh.passphrase.is_none());

    // Without SSH no SSH secret is read.
    c.ssh.enabled = false;
    let r = resolve_credentials(&c, &Typed::default(), &k)
        .await
        .unwrap();
    assert!(r.ssh.password.is_none() && r.ssh.passphrase.is_none());

    // Reading the Keychain up front leaves the connect nothing to read.
    let mut c = ssh_connection();
    c.id = "c1".into();
    let filled = with_keychain_secrets(&c, Typed::default(), &k).unwrap();
    assert_eq!(filled.password.as_deref(), Some("db"));
    assert_eq!(filled.ssh_password.as_deref(), Some("ssh"));
    assert_eq!(filled.passphrase.as_deref(), Some("phrase"));
    assert!(!format!("{filled:?}").contains("phrase\""), "{filled:?}");
}

#[test]
fn ask_every_time_asks_for_each_secret_and_a_passphrase_only_for_an_encrypted_key() {
    let t = Temp::new();
    let mut c = ssh_connection();
    assert!(asked_secrets(&c).is_empty());
    c.password_mode = PasswordMode::Ask;
    c.ssh.password_mode = PasswordMode::Ask;
    c.ssh.passphrase_mode = PassphraseMode::Ask;
    assert_eq!(asked_secrets(&c), [Asked::Password, Asked::SshPassword]);
    let plain = t.0.join("plain");
    std::fs::write(
        &plain,
        "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\n",
    )
    .unwrap();
    let encrypted = t.0.join("encrypted");
    std::fs::write(
        &encrypted,
        "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIE\n-----END ENCRYPTED PRIVATE KEY-----\n",
    )
    .unwrap();
    c.ssh.use_key = true;
    c.ssh.key_path = Some(plain.display().to_string());
    assert_eq!(asked_secrets(&c), [Asked::Password, Asked::SshPassword]);
    c.ssh.key_path = Some(encrypted.display().to_string());
    assert_eq!(
        asked_secrets(&c),
        [Asked::Password, Asked::SshPassword, Asked::Passphrase]
    );
    c.ssh.enabled = false;
    assert_eq!(asked_secrets(&c), [Asked::Password]);
}

#[test]
fn connect_errors_say_which_side_failed() {
    let c = ssh_connection();
    let ssh: anyhow::Error =
        SshError::Auth("authentication failed for deploy@bastion:2200 (tried password).".into())
            .into();
    assert_eq!(
        connect_error_message(&c, &ssh),
        "SSH: authentication failed for deploy@bastion:2200 (tried password)."
    );
    let db = anyhow::anyhow!("password authentication failed for user \"app\"");
    assert_eq!(
        connect_error_message(&c, &db),
        "Database: password authentication failed for user \"app\""
    );
    let mut direct = c.clone();
    direct.ssh.enabled = false;
    assert_eq!(
        connect_error_message(&direct, &db),
        "password authentication failed for user \"app\""
    );
}

fn problem(kind: HostKeyKind) -> HostKeyProblem {
    let mut blob = 11u32.to_be_bytes().to_vec();
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&[0, 0, 0, 32]);
    blob.extend([7u8; 32]);
    HostKeyProblem {
        kind,
        host: "bastion".into(),
        port: 2200,
        key: HostKey::from_blob(blob).unwrap(),
        recorded: if kind == HostKeyKind::Unknown {
            vec![]
        } else {
            vec![Recorded {
                path: "/Users/me/.ssh/known_hosts".into(),
                line: 12,
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:old".into(),
            }]
        },
    }
}

#[test]
fn host_key_sheets() {
    let unknown = host_key_prompt(&problem(HostKeyKind::Unknown));
    assert_eq!(unknown.title, "Trust the SSH server bastion:2200?");
    assert!(
        unknown
            .presented
            .starts_with("ED25519 key fingerprint: SHA256:")
    );
    assert_eq!(unknown.trust, Some("Trust"));
    assert!(!unknown.warning);

    let changed = host_key_prompt(&problem(HostKeyKind::Changed));
    assert!(
        changed
            .title
            .starts_with("Warning: the SSH host key of bastion:2200 has changed")
    );
    assert!(changed.detail.contains("man-in-the-middle"));
    assert_eq!(changed.trust, Some("Trust New Key"));
    assert!(changed.warning, "Cancel is the default");
    assert_eq!(
        changed.recorded,
        ["ED25519 SHA256:old (/Users/me/.ssh/known_hosts:12)"]
    );

    let revoked = host_key_prompt(&problem(HostKeyKind::Revoked));
    assert_eq!(revoked.trust, None);
}

#[test]
fn the_form_checks_its_ssh_settings_and_startup_sql() {
    let mut c = ssh_connection();
    let ok = form::validate_ssh(&c, "").unwrap();
    assert_eq!(
        ok.ssh.port, 0,
        "an empty port comes from ~/.ssh/config, else 22"
    );
    assert_eq!(form::validate_ssh(&c, "2222").unwrap().ssh.port, 2222);
    assert!(form::validate_ssh(&c, "70000").is_err());
    c.ssh.host = "  ".into();
    assert!(
        form::validate_ssh(&c, "22")
            .unwrap_err()
            .to_string()
            .contains("SSH server")
    );
    for pattern in ["*,x.example", "x?", "a b", "[x]"] {
        c.ssh.host = pattern.into();
        let e = form::validate_ssh(&c, "22").unwrap_err().to_string();
        assert!(e.starts_with("The SSH server name "), "{pattern}: {e}");
    }
    c.ssh.host = "bastion".into();
    c.ssh.use_key = true;
    assert!(
        form::validate_ssh(&c, "22")
            .unwrap_err()
            .to_string()
            .contains("private key")
    );
    c.ssh.use_key = false;
    c.host = "/tmp/mysql.sock".into();
    assert!(form::validate_ssh(&c, "22").is_err());
    c.host = "db".into();
    c.startup_sql = "DELETE FROM t".into();
    assert!(form::validate_ssh(&c, "22").is_err());
    c.ssh.enabled = false;
    assert!(
        form::validate_ssh(&c, "22").is_err(),
        "startup SQL is checked without SSH too"
    );
    c.startup_sql = "SET search_path TO app; SET TIME ZONE 'UTC'".into();
    assert!(form::validate_ssh(&c, "not used").is_ok());
    assert_eq!(form::startup_summary(&c), "2 statements");
    c.startup_sql.clear();
    assert_eq!(form::startup_summary(&c), "None");
}
