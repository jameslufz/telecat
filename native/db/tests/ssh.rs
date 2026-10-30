//! SSH tunnels against the test SSH server (scripts/test-ssh-env.sh): host keys, every login
//! method and key format, ~/.ssh/config, every kind of connection going through the tunnel,
//! startup SQL, and a dropped SSH link coming back.
use anyhow::{Context, Result};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};
use telecat_core::changes::Stmt;
use telecat_core::types::*;
use telecat_db::{
    Credentials, ImportOptions, ImportProgress, Session, is_auth_failure, is_connection_lost,
    ssh::{HostKeyKind, HostKeyProblem, SshEnv, SshError, SshSecrets, ssh_error, trust_host_key},
    test_proxy::TestProxy,
    validate_test_endpoint,
};

/// Keys and the server address written by scripts/test-ssh-env.sh.
fn keys() -> PathBuf {
    let dir = std::env::var_os("TELECAT_SSH_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root/telecat/ssh-test"));
    assert!(
        dir.join("sshd_ip").exists(),
        "No SSH test environment at {}: run scripts/test-ssh-env.sh",
        dir.display()
    );
    dir
}
fn sshd_ip() -> String {
    std::fs::read_to_string(keys().join("sshd_ip"))
        .unwrap()
        .trim()
        .to_owned()
}

/// A private temporary home: no real ~/.ssh, known hosts, agent or Keychain.
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "telecat-ssh-test-{}-{}",
            std::process::id(),
            uuid()
        ));
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();
        Self(dir)
    }
    fn env(&self) -> SshEnv {
        SshEnv::isolated(&self.0)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn config(d: Driver, ssh_user: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "telecat-ssh-test".into();
    let mysql = d == Driver::Mysql;
    c.host = if mysql {
        "telecat-test-mysql"
    } else {
        "telecat-test-pg"
    }
    .into();
    c.port = if mysql { 3306 } else { 5432 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = if mysql { "zixma" } else { "main_db" }.into();
    c.ssh = SshConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: 2222,
        user: ssh_user.into(),
        ..SshConfig::default()
    };
    c
}
fn with_key(mut c: ConnectionConfig, key: &str) -> ConnectionConfig {
    c.ssh.use_key = true;
    c.ssh.key_path = Some(keys().join(key).display().to_string());
    c
}
fn creds(ssh_password: Option<&str>, passphrase: Option<&str>) -> Credentials {
    Credentials {
        password: "secret".into(),
        ssh: SshSecrets::new(ssh_password.map(Into::into), passphrase.map(Into::into)),
    }
}
async fn connect(c: &ConnectionConfig, cr: Credentials, env: &SshEnv) -> Result<Session> {
    Session::connect_test_with(c, cr, Some(env)).await
}
fn host_key(e: &anyhow::Error) -> &HostKeyProblem {
    match ssh_error(e) {
        Some(SshError::HostKey(p)) => p,
        other => panic!("expected a host key problem, got {other:?}: {e:#}"),
    }
}
/// Trusts the test server's key the way the app does: refused once as unknown, then trusted.
async fn trusted(home: &Home) -> SshEnv {
    let env = home.env();
    let e = connect(
        &config(Driver::Mysql, "tpass"),
        creds(Some("sshsecret"), None),
        &env,
    )
    .await
    .err()
    .expect("an unknown host is refused");
    let p = host_key(&e);
    assert_eq!(p.kind, HostKeyKind::Unknown);
    trust_host_key(&env, p).unwrap();
    env
}
/// Trusts the server's key as recorded for a test proxy's port, as a user would.
async fn trust_proxy(env: &SshEnv, c: &ConnectionConfig) {
    if let Err(e) = connect(c, creds(Some("sshsecret"), None), env).await {
        trust_host_key(env, host_key(&e)).unwrap();
    }
}
async fn scalar(s: &Session, sql: &str) -> Result<Option<String>> {
    Ok(s.query(sql)
        .await?
        .rows
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten()))
}

#[test]
fn the_fence_admits_only_the_test_ssh_server_and_test_databases() {
    for d in [Driver::Mysql, Driver::Postgres] {
        let c = config(d, "tpass");
        assert!(validate_test_endpoint(&c).is_ok());
        let mut zero = c.clone();
        zero.ssh.port = 0;
        assert!(
            validate_test_endpoint(&zero).is_ok(),
            "port from ssh config, checked again later"
        );
        for (host, port) in [
            ("127.0.0.1", 22),
            ("127.0.0.1", 2223),
            ("bastion.example.com", 2222),
            ("localhost", 2222),
        ] {
            let mut bad = c.clone();
            bad.ssh.host = host.into();
            bad.ssh.port = port;
            assert!(validate_test_endpoint(&bad).is_err(), "{host}:{port}");
        }
        for (host, port) in [
            ("127.0.0.1", 3306),
            ("127.0.0.1", 3307),
            ("127.0.0.1", 5432),
            ("db.example.com", 3306),
            ("telecat-test-mysql", 3307),
            ("telecat-test-pg", 5433),
            (
                if d == Driver::Mysql {
                    "telecat-test-pg"
                } else {
                    "telecat-test-mysql"
                },
                c.port,
            ),
        ] {
            let mut bad = c.clone();
            bad.host = host.into();
            bad.port = port;
            assert!(validate_test_endpoint(&bad).is_err(), "{host}:{port}");
        }
    }
}

#[tokio::test]
async fn the_tunnel_refuses_an_ssh_config_that_leads_elsewhere() {
    let home = Home::new();
    std::fs::write(
        home.0.join(".ssh/config"),
        "Host 127.0.0.1\n  HostName 10.255.255.1\n",
    )
    .unwrap();
    let mut c = config(Driver::Mysql, "tpass");
    c.ssh.port = 0;
    let e = connect(&c, creds(Some("sshsecret"), None), &home.env())
        .await
        .err()
        .unwrap();
    assert!(format!("{e:#}").contains("Test safety"), "{e:#}");
}

#[tokio::test]
async fn host_keys_unknown_trusted_changed_and_revoked() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    let c = config(Driver::Postgres, "tpass");
    let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
    assert_eq!(scalar(&s, "SELECT 1").await?.as_deref(), Some("1"));
    s.close().await;
    let ours = std::fs::read_to_string(&env.known_hosts)?;
    assert!(
        ours.starts_with("|1|"),
        "trusted entries are hashed: {ours}"
    );
    let real = ours.split_whitespace().collect::<Vec<_>>();
    let (algorithm, key) = (real[1].to_owned(), real[2].to_owned());

    // Telecat's file holds another key for the host: a hard mismatch, not trusted.
    let other = std::fs::read_to_string(keys().join("id_unauthorized.pub"))?;
    let other: Vec<&str> = other.split_whitespace().collect();
    std::fs::write(
        &env.known_hosts,
        format!("[127.0.0.1]:2222 {} {}\n", other[0], other[1]),
    )?;
    let e = connect(&c, creds(Some("sshsecret"), None), &env)
        .await
        .err()
        .unwrap();
    let p = host_key(&e);
    assert_eq!(p.kind, HostKeyKind::Changed);
    assert!(p.fingerprint().starts_with("SHA256:"));
    assert_eq!(p.recorded.len(), 1);
    assert!(!is_connection_lost(&e), "never retried automatically");
    assert!(e.to_string().starts_with("SSH: host key mismatch"), "{e}");

    // The same mismatch recorded in the user's own file (read only).
    std::fs::remove_file(&env.known_hosts)?;
    std::fs::write(
        &env.user_known_hosts[0],
        format!("[127.0.0.1]:2222 {} {}\n", other[0], other[1]),
    )?;
    let e = connect(&c, creds(Some("sshsecret"), None), &env)
        .await
        .err()
        .unwrap();
    let p = host_key(&e).clone();
    assert_eq!(p.kind, HostKeyKind::Changed);
    assert_eq!(p.recorded[0].path, env.user_known_hosts[0]);
    // "Trust New Key" writes Telecat's file only, which wins from then on.
    trust_host_key(&env, &p)?;
    let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
    s.close().await;
    assert_eq!(
        std::fs::read_to_string(&env.user_known_hosts[0])?,
        format!("[127.0.0.1]:2222 {} {}\n", other[0], other[1])
    );

    // A revoked key is refused and can't be trusted.
    std::fs::write(
        &env.user_known_hosts[0],
        format!("@revoked * {algorithm} {key}\n"),
    )?;
    let e = connect(&c, creds(Some("sshsecret"), None), &env)
        .await
        .err()
        .unwrap();
    let p = host_key(&e);
    assert_eq!(p.kind, HostKeyKind::Revoked);
    assert!(trust_host_key(&env, p).is_err());
    Ok(())
}

#[tokio::test]
async fn password_logins_and_their_failures() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    for d in [Driver::Mysql, Driver::Postgres] {
        let s = connect(&config(d, "tpass"), creds(Some("sshsecret"), None), &env).await?;
        assert_eq!(scalar(&s, "SELECT 1").await?.as_deref(), Some("1"));
        assert!(s.tunnel_port().is_some());
        s.close().await;
    }
    // Keyboard-interactive only (PAM), answered with the password.
    let s = connect(
        &config(Driver::Mysql, "tkbd"),
        creds(Some("sshsecret"), None),
        &env,
    )
    .await?;
    s.close().await;

    let e = connect(
        &config(Driver::Mysql, "tpass"),
        creds(Some("wrong"), None),
        &env,
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(ssh_error(&e), Some(SshError::Auth(_))), "{e:#}");
    assert!(
        e.to_string()
            .starts_with("SSH: authentication failed for tpass@127.0.0.1:2222"),
        "{e}"
    );
    assert!(is_auth_failure(&e) && !is_connection_lost(&e));
    let e = connect(&config(Driver::Mysql, "tpass"), creds(None, None), &env)
        .await
        .err()
        .unwrap();
    assert!(
        e.to_string().contains("no password, key or ssh-agent key"),
        "{e}"
    );

    // The database refusing its password is the database's error, after SSH succeeded.
    let mut bad_db = creds(Some("sshsecret"), None);
    bad_db.password = "not-the-password".into();
    let e = connect(&config(Driver::Postgres, "tpass"), bad_db, &env)
        .await
        .err()
        .unwrap();
    assert!(ssh_error(&e).is_none(), "{e:#}");
    assert!(is_auth_failure(&e));

    // Nobody listening.
    let mut down = config(Driver::Mysql, "tpass");
    let proxy = TestProxy::start(2222).await?;
    proxy.cut();
    down.ssh.port = proxy.port();
    let e = connect(&down, creds(Some("sshsecret"), None), &env)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(ssh_error(&e), Some(SshError::Unreachable(_))),
        "{e:#}"
    );
    assert!(is_connection_lost(&e), "worth retrying");
    Ok(())
}

#[tokio::test]
async fn every_key_type_and_format() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    for key in [
        "id_ed25519",
        "id_rsa",
        "id_rsa_pem",
        "id_rsa_pkcs8",
        "id_ecdsa",
        "id_ecdsa_pem",
    ] {
        let c = with_key(config(Driver::Mysql, "tkey"), key);
        let s = connect(&c, creds(None, None), &env)
            .await
            .with_context(|| format!("key {key}"))?;
        assert_eq!(scalar(&s, "SELECT 1").await?.as_deref(), Some("1"), "{key}");
        s.close().await;
    }
    for key in ["id_ed25519_enc", "id_rsa_pem_enc"] {
        let c = with_key(config(Driver::Postgres, "tkey"), key);
        assert!(telecat_db::ssh::key_is_encrypted(&keys().join(key))?);
        let s = connect(&c, creds(None, Some("keypass")), &env)
            .await
            .with_context(|| format!("key {key}"))?;
        s.close().await;
        let e = connect(&c, creds(None, Some("wrong")), &env)
            .await
            .err()
            .unwrap();
        assert!(e.to_string().contains("passphrase is wrong"), "{key}: {e}");
        let e = connect(&c, creds(None, None), &env).await.err().unwrap();
        assert!(
            e.to_string().contains("is encrypted: enter its passphrase"),
            "{key}: {e}"
        );
        assert!(!is_connection_lost(&e));
    }
    assert!(!telecat_db::ssh::key_is_encrypted(
        &keys().join("id_ed25519")
    )?);
    let c = with_key(config(Driver::Mysql, "tkey"), "id_unauthorized");
    let e = connect(&c, creds(None, None), &env).await.err().unwrap();
    assert!(matches!(ssh_error(&e), Some(SshError::Auth(_))), "{e:#}");
    let c = with_key(config(Driver::Mysql, "tkey"), "no_such_key");
    let e = connect(&c, creds(None, None), &env).await.err().unwrap();
    assert!(matches!(ssh_error(&e), Some(SshError::Key(_))), "{e:#}");
    Ok(())
}

#[tokio::test]
async fn ssh_config_supplies_user_port_and_identity_files() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    std::fs::write(
        home.0.join(".ssh/config"),
        format!(
            "Host 127.0.0.1\n  User tkey\n  Port 2222\n  IdentityFile {}\n  IdentityFile {}\n",
            keys().join("id_ed25519_enc").display(),
            keys().join("id_ecdsa").display()
        ),
    )?;
    let mut c = config(Driver::Postgres, "");
    c.ssh.port = 0;
    // The encrypted key is skipped without a passphrase; the second one logs in.
    let s = connect(&c, creds(None, None), &env).await?;
    s.close().await;
    // Only an encrypted key and no passphrase: the error says it was skipped and why.
    std::fs::write(
        home.0.join(".ssh/config"),
        format!(
            "Host 127.0.0.1\n  User tkey\n  Port 2222\n  IdentityFile {}\n",
            keys().join("id_ed25519_enc").display()
        ),
    )?;
    let e = connect(&c, creds(None, None), &env).await.err().unwrap();
    assert!(
        e.to_string()
            .contains("Skipped encrypted key id_ed25519_enc (no passphrase)"),
        "{e}"
    );
    // No config: the default ~/.ssh/id_* keys of the (temporary) home.
    std::fs::remove_file(home.0.join(".ssh/config"))?;
    std::fs::copy(keys().join("id_rsa"), home.0.join(".ssh/id_rsa"))?;
    let mut c = config(Driver::Mysql, "tkey");
    c.ssh.port = 2222;
    let s = connect(&c, creds(None, None), &env).await?;
    s.close().await;
    Ok(())
}

#[tokio::test]
async fn ssh_agent_keys() -> Result<()> {
    let home = Home::new();
    let mut env = trusted(&home).await;
    let sock = home.0.join("agent.sock");
    let mut agent = std::process::Command::new("ssh-agent")
        .args(["-D", "-a"])
        .arg(&sock)
        .stdout(std::process::Stdio::null())
        .spawn()
        .context("ssh-agent (openssh-client) is needed for this test")?;
    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = std::process::Command::new("ssh-add")
        .arg(keys().join("id_ed25519"))
        .env("SSH_AUTH_SOCK", &sock)
        .stderr(std::process::Stdio::null())
        .status()?;
    assert!(status.success());
    env.agent = Some(sock.clone());
    let result = async {
        let s = connect(&config(Driver::Mysql, "tkey"), creds(None, None), &env).await?;
        s.close().await;
        // IdentitiesOnly in ~/.ssh/config keeps the agent out of it.
        std::fs::write(home.0.join(".ssh/config"), "IdentitiesOnly yes\n")?;
        let e = connect(&config(Driver::Mysql, "tkey"), creds(None, None), &env)
            .await
            .err()
            .unwrap();
        anyhow::ensure!(matches!(ssh_error(&e), Some(SshError::Auth(_))), "{e:#}");
        Ok(())
    }
    .await;
    let _ = agent.kill();
    let _ = agent.wait();
    result
}

/// Startup SQL that fails unless the connection came from the SSH server: every connection
/// Telecat opens runs it, so each kind of connection used below proves it went through the
/// tunnel.
fn tunnel_only(d: Driver) -> String {
    let ip = sshd_ip();
    match d {
        Driver::Mysql => format!(
            "SET time_zone = '+07:00'; SELECT IF(SUBSTRING_INDEX(USER(), '@', -1) = '{ip}', 1, (SELECT 1 UNION SELECT 2))"
        ),
        Driver::Postgres => format!(
            "SET TIME ZONE 'Asia/Bangkok'; SELECT 1 / (host(inet_client_addr()) = '{ip}')::int"
        ),
    }
}

#[tokio::test]
async fn every_connection_goes_through_the_tunnel_with_startup_sql() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    for d in [Driver::Mysql, Driver::Postgres] {
        let mut c = config(d, "tpass");
        c.startup_sql = tunnel_only(d);
        // The same check refuses a direct connection to the test server.
        let mut direct = c.clone();
        direct.ssh = SshConfig::default();
        direct.host = "127.0.0.1".into();
        direct.port = if d == Driver::Mysql { 3307 } else { 5434 };
        let e = Session::connect_test(&direct, "secret")
            .await
            .err()
            .unwrap();
        assert!(
            e.to_string().starts_with("Startup SQL statement 2 failed"),
            "{e}"
        );

        let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
        let zone = if d == Driver::Mysql {
            scalar(&s, "SELECT @@session.time_zone").await?
        } else {
            scalar(&s, "SHOW TIME ZONE").await?
        };
        assert!(
            matches!(zone.as_deref(), Some("+07:00" | "Asia/Bangkok")),
            "{zone:?}"
        );
        let table = format!("t070_ssh_{}", uuid().replace('-', "_"));
        let schema = if d == Driver::Mysql {
            "zixma"
        } else {
            "public"
        };
        // Commit connection.
        s.execute(
            &[
                Stmt::new(format!(
                    "CREATE TABLE {table} (id INT PRIMARY KEY, v VARCHAR(20))"
                )),
                Stmt::new(format!("INSERT INTO {table} VALUES (1, 'ก'), (2, 'b')")),
            ],
            false,
        )
        .await?;
        let tref = TableRef {
            schema: schema.into(),
            name: table.clone(),
        };
        // Table reads, metadata, autocomplete columns, running queries, export.
        let rows = s
            .table_data(&TableDataRequest {
                table: tref.clone(),
                filters: vec![],
                combine: Combine::And,
                order_by: None,
                limit: 10,
                offset: 0,
            })
            .await?;
        assert_eq!(rows.total, 2);
        assert!(
            s.list_objects()
                .await?
                .iter()
                .any(|o| o.table.name == table)
        );
        assert_eq!(s.column_names(&[tref.clone()]).await?.len(), 1);
        s.process_list(None).await?;
        let mut exported = 0;
        s.export_table(
            &TableDataRequest {
                table: tref.clone(),
                filters: vec![],
                combine: Combine::And,
                order_by: None,
                limit: 0,
                offset: 0,
            },
            &mut |_, _, rows| {
                exported += rows.len();
                Ok(())
            },
        )
        .await?;
        assert_eq!(exported, 2);
        // Import, on its own connection.
        let dump = home.0.join("dump.sql");
        std::fs::write(&dump, format!("INSERT INTO {table} VALUES (3, 'c');\n"))?;
        s.import_dump(
            &dump,
            &s.database.clone(),
            ImportOptions::default(),
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .await
        .map_err(|f| anyhow::anyhow!("{}", f.message))?;
        assert_eq!(
            scalar(&s, &format!("SELECT COUNT(*) FROM {table}"))
                .await?
                .as_deref(),
            Some("3")
        );
        s.cancel().await?;
        // Another database over the same tunnel.
        let other = s
            .with_database(if d == Driver::Mysql {
                "information_schema"
            } else {
                "postgres"
            })
            .await?;
        assert_eq!(other.tunnel_port(), s.tunnel_port());
        other.close().await;
        s.execute(&[Stmt::new(format!("DROP TABLE {table}"))], false)
            .await?;
        s.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn tls_to_the_database_through_the_tunnel() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    let mut c = config(Driver::Mysql, "tpass");
    c.ssl_mode = SslMode::Required;
    let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
    assert!(
        s.tls.as_deref().is_some_and(|v| v.starts_with("TLS")),
        "{:?}",
        s.tls
    );
    s.close().await;
    Ok(())
}

#[tokio::test]
async fn startup_sql_problems_stop_the_connection() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    let mut c = config(Driver::Mysql, "tpass");
    c.startup_sql = "SET time_zone = '+07:00'; INSERT INTO t VALUES (1)".into();
    let e = connect(&c, creds(Some("sshsecret"), None), &env)
        .await
        .err()
        .unwrap();
    assert!(
        e.to_string()
            .starts_with("Startup SQL statement 2 can't be used"),
        "{e}"
    );
    c.startup_sql = "SET no_such_variable = 1".into();
    let e = connect(&c, creds(Some("sshsecret"), None), &env)
        .await
        .err()
        .unwrap();
    assert!(
        e.to_string()
            .starts_with("Startup SQL statement 1 failed: "),
        "{e}"
    );
    assert!(!is_connection_lost(&e), "not retried");
    // Telecat keeps UTF-8 whatever the startup SQL sets.
    c.startup_sql = "SET NAMES latin1".into();
    let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
    assert_eq!(
        scalar(&s, "SELECT 'ธนาคาร 😺'").await?.as_deref(),
        Some("ธนาคาร 😺")
    );
    s.close().await;
    Ok(())
}

#[tokio::test]
async fn a_dropped_ssh_link_is_noticed_and_reconnects() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    for d in [Driver::Mysql, Driver::Postgres] {
        let proxy = TestProxy::start(2222).await?;
        let mut c = config(d, "tpass");
        c.ssh.port = proxy.port();
        c.startup_sql = tunnel_only(d);
        trust_proxy(&env, &c).await;
        let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
        s.ping(Duration::from_secs(5)).await?;
        proxy.cut();
        let mut noticed = None;
        for _ in 0..50 {
            if let Err(e) = s.ping(Duration::from_secs(2)).await {
                noticed = Some(e);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let e = noticed.context("the drop was noticed")?;
        assert!(is_connection_lost(&e), "{e:#}");
        let e = s
            .query("SELECT 1")
            .await
            .err()
            .context("the old connection is gone")?;
        assert!(is_connection_lost(&e), "{e:#}");
        // While the SSH server can't be reached, reconnecting fails as worth retrying.
        let e = s.reopen().await.err().context("still down")?;
        assert!(is_connection_lost(&e), "{e:#}");
        proxy.restore();
        let again = s.reopen().await?;
        assert_eq!(scalar(&again, "SELECT 1").await?.as_deref(), Some("1"));
        again.ping(Duration::from_secs(5)).await?;
        s.close().await;
        // Closing the old session keeps the tunnel the new one uses.
        assert_eq!(scalar(&again, "SELECT 2").await?.as_deref(), Some("2"));
        again.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_silent_ssh_link_is_replaced_on_reconnect() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    let proxy = TestProxy::start(2222).await?;
    let mut c = config(Driver::Postgres, "tpass");
    c.ssh.port = proxy.port();
    trust_proxy(&env, &c).await;
    let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
    proxy.blackhole();
    let e = s
        .ping(Duration::from_secs(2))
        .await
        .err()
        .context("no answer")?;
    assert!(is_connection_lost(&e), "{e:#}");
    // Reconnecting while it is silent gives up in bounded time…
    let started = std::time::Instant::now();
    let e = s.reopen().await.err().context("still silent")?;
    assert!(is_connection_lost(&e), "{e:#}");
    assert!(
        started.elapsed() < Duration::from_secs(40),
        "{:?}",
        started.elapsed()
    );
    // …and once the network is back, logs in to SSH again before the database.
    proxy.cut();
    proxy.restore();
    let again = s.reopen().await?;
    assert_eq!(scalar(&again, "SELECT 1").await?.as_deref(), Some("1"));
    again.close().await;
    s.close().await;
    Ok(())
}

#[tokio::test]
async fn the_tunnel_serves_only_telecat_and_closes_with_the_session() -> Result<()> {
    use tokio::io::AsyncReadExt;
    let home = Home::new();
    let env = trusted(&home).await;
    let s = connect(
        &config(Driver::Mysql, "tpass"),
        creds(Some("sshsecret"), None),
        &env,
    )
    .await?;
    let port = s.tunnel_port().unwrap();
    // A connection Telecat didn't prepare gets nothing and is closed.
    let mut stranger = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), stranger.read(&mut buf)).await??;
    assert_eq!(n, 0, "closed without a byte from the database");
    s.close().await;
    drop(s);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "no listener left behind"
    );
    Ok(())
}

#[tokio::test]
async fn postgres_channel_binding_and_system_schemas() -> Result<()> {
    let home = Home::new();
    let env = trusted(&home).await;
    let mut c = config(Driver::Postgres, "tpass");
    c.disable_channel_binding = true;
    let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
    let objects = s.list_objects().await?;
    assert!(!objects.iter().any(|o| o.table.schema == "pg_catalog"));
    s.close().await;
    c.load_system_schemas = true;
    let s = connect(&c, creds(Some("sshsecret"), None), &env).await?;
    let objects = s.list_objects().await?;
    assert!(
        objects
            .iter()
            .any(|o| o.table.schema == "pg_catalog" && o.table.name == "pg_class")
    );
    assert!(
        objects
            .iter()
            .any(|o| o.table.schema == "information_schema" && o.table.name == "tables")
    );
    s.close().await;
    Ok(())
}

#[test]
fn credentials_never_print_secrets() {
    let c = Credentials {
        password: "db-secret".into(),
        ssh: SshSecrets::new(Some("ssh-secret".into()), Some("key-secret".into())),
    };
    let text = format!("{c:?}");
    for secret in ["db-secret", "ssh-secret", "key-secret"] {
        assert!(!text.contains(secret), "{text}");
    }
}

/// SEC-001 (security-0.7.0.md): trusting a server whose name looks like a known_hosts pattern
/// must never make its key trusted for other hosts.
#[test]
fn sec001_trusting_a_pattern_named_host_never_trusts_its_key_elsewhere() {
    use telecat_db::ssh::known_hosts::{HostKey, Verdict, verify};
    let home = Home::new();
    let env = home.env();
    let mut blob = 11u32.to_be_bytes().to_vec();
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&[0, 0, 0, 32]);
    blob.extend([9u8; 32]);
    let k = HostKey::from_blob(blob).unwrap();
    for port in [22, 2222] {
        let problem = HostKeyProblem {
            kind: HostKeyKind::Unknown,
            host: "*,x.example".into(),
            port,
            key: k.clone(),
            recorded: vec![],
        };
        assert!(trust_host_key(&env, &problem).is_err());
    }
    for port in [22, 2200] {
        assert_ne!(
            verify(
                &env.known_hosts,
                &env.user_known_hosts,
                "other.example",
                port,
                &k
            ),
            Verdict::Trusted
        );
    }
}
