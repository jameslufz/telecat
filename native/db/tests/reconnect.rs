//! Lost connections through a proxy that cuts or blackholes the link to the test servers:
//! detection is quick and bounded, a reopened session is set up like the first one, and an
//! interrupted commit is judged by what reached the server, never by retrying it.
use anyhow::Result;
use std::time::{Duration, Instant};
use telecat_core::changes::{Check, RowVerify, Stmt};
use telecat_core::types::*;
use telecat_db::test_proxy::TestProxy;
use telecat_db::{CommitInterrupted, CommitVerdict, Session, is_auth_failure, is_connection_lost};

fn config(d: Driver, port: u16) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "telecat-reconnect".into();
    c.host = "127.0.0.1".into();
    c.port = port;
    let mysql = d == Driver::Mysql;
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = if mysql { "zixma" } else { "main_db" }.into();
    c
}
fn upstream(d: Driver) -> u16 {
    if d == Driver::Mysql { 3307 } else { 5434 }
}
async fn through_proxy(d: Driver) -> Result<(TestProxy, Session)> {
    let proxy = TestProxy::start(upstream(d)).await?;
    let session = Session::connect_test(&config(d, proxy.port()), "secret").await?;
    Ok((proxy, session))
}
fn direct(d: Driver) -> ConnectionConfig {
    config(d, upstream(d))
}

#[tokio::test]
async fn the_proxy_only_reaches_the_test_servers() {
    assert!(TestProxy::start(3306).await.is_err());
    assert!(TestProxy::start(5432).await.is_err());
    // A port that isn't a live proxy stays fenced.
    let proxy = TestProxy::start(3307).await.unwrap();
    let port = proxy.port();
    drop(proxy);
    assert!(
        Session::connect_test(&config(Driver::Mysql, port), "secret")
            .await
            .is_err()
    );
    // A proxy for one server doesn't admit the other driver.
    let proxy = TestProxy::start(5434).await.unwrap();
    let err = Session::connect_test(&config(Driver::Mysql, proxy.port()), "secret")
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("Test safety"), "{err}");
}

#[tokio::test]
async fn a_cut_link_is_reported_as_lost_at_once() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let (proxy, session) = through_proxy(d).await?;
        session.query("SELECT 1").await?;
        proxy.cut();
        let started = Instant::now();
        let err = session
            .query("SELECT 1")
            .await
            .err()
            .expect("query on a cut link");
        assert!(is_connection_lost(&err), "{d:?}: {err:#}");
        assert!(!is_auth_failure(&err));
        assert!(started.elapsed() < Duration::from_secs(3), "{d:?}");
        let err = session
            .ping(Duration::from_secs(2))
            .await
            .err()
            .expect("ping");
        assert!(is_connection_lost(&err), "{d:?}: {err:#}");
        // Reconnecting while the link is still down fails quickly and is retryable.
        let started = Instant::now();
        let err = session.reopen().await.err().expect("reopen while down");
        assert!(is_connection_lost(&err), "{d:?}: {err:#}");
        assert!(started.elapsed() < Duration::from_secs(5), "{d:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_blackholed_link_is_noticed_by_the_ping_within_its_timeout() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let (proxy, session) = through_proxy(d).await?;
        session.ping(Duration::from_secs(2)).await?;
        proxy.blackhole();
        let started = Instant::now();
        let err = session
            .ping(Duration::from_secs(2))
            .await
            .err()
            .expect("ping");
        let took = started.elapsed();
        assert!(is_connection_lost(&err), "{d:?}: {err:#}");
        assert!(
            took >= Duration::from_secs(2) && took < Duration::from_secs(4),
            "{took:?}"
        );
        // Opening a connection into the blackhole gives up after the connect timeout.
        let started = Instant::now();
        let err = Session::connect_test(&config(d, proxy.port()), "secret")
            .await
            .err()
            .expect("connect into a blackhole");
        assert!(is_connection_lost(&err), "{d:?}: {err:#}");
        assert!(started.elapsed() < telecat_db::CONNECT_TIMEOUT + Duration::from_secs(3));
        proxy.restore();
        // Restored, the held link answers again; a fresh session works too.
        session.ping(Duration::from_secs(5)).await?;
        let again = session.reopen().await?;
        again.query("SELECT 1").await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_query_running_when_the_link_drops_fails_and_is_not_repeated() -> Result<()> {
    let (proxy, session) = through_proxy(Driver::Mysql).await?;
    let marker = format!("telecat_reconnect_{}", std::process::id());
    let session = std::sync::Arc::new(session);
    let running = tokio::spawn({
        let session = session.clone();
        let marker = marker.clone();
        async move { session.query(&format!("SELECT SLEEP(3), '{marker}'")).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    proxy.cut();
    let err = running.await?.err().expect("interrupted query");
    assert!(is_connection_lost(&err), "{err:#}");
    proxy.restore();
    let fresh = session.reopen().await?;
    // The server finishes the one it was running; nothing runs it again after the reconnect.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let again = fresh
        .query(&format!(
            "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO LIKE '%{marker}%' AND ID <> CONNECTION_ID()"
        ))
        .await?;
    assert_eq!(again.rows[0][0].as_deref(), Some("0"));
    Ok(())
}

#[tokio::test]
async fn a_reopened_session_is_set_up_like_the_first() -> Result<()> {
    let (proxy, session) = through_proxy(Driver::Mysql).await?;
    let old = session.with_database("information_schema").await?;
    proxy.cut();
    proxy.restore();
    let fresh = old.reopen().await?;
    assert_eq!(fresh.database, "information_schema", "same database");
    assert_eq!(fresh.config().port, proxy.port(), "same endpoint");
    let mode = fresh
        .execute(&[Stmt::new("SELECT @@SESSION.sql_mode")], false)
        .await?;
    let mode = mode[0].rows[0][0].clone().unwrap_or_default();
    assert!(
        mode.contains("STRICT_ALL_TABLES"),
        "commit connection: {mode}"
    );
    // A user's session variable did not carry over.
    let var = fresh.query("SELECT @telecat_user_var").await?;
    assert_eq!(var.rows[0][0], None);

    let (proxy, session) = through_proxy(Driver::Postgres).await?;
    proxy.cut();
    proxy.restore();
    let fresh = session.reopen().await?;
    let db = fresh.query("SELECT current_database()").await?;
    assert_eq!(db.rows[0][0].as_deref(), Some("main_db"));
    Ok(())
}

#[tokio::test]
async fn a_wrong_password_is_an_auth_failure_not_a_lost_link() {
    for d in [Driver::Mysql, Driver::Postgres] {
        let err = Session::connect_test(&direct(d), "wrong-password")
            .await
            .err()
            .unwrap();
        assert!(is_auth_failure(&err), "{d:?}: {err:#}");
        assert!(!is_connection_lost(&err), "{d:?}: {err:#}");
    }
}

/// Saves one row change through the proxy, which goes dark at COMMIT: before the server gets
/// it (`deliver` false) or after (true). Returns the verdict from a fresh session.
async fn interrupted_commit(d: Driver, deliver: bool) -> Result<(CommitVerdict, Option<String>)> {
    let table = format!(
        "telecat_rc_{}_{}",
        if deliver { "after" } else { "before" },
        if d == Driver::Mysql { "my" } else { "pg" }
    );
    let setup = Session::connect_test(&direct(d), "secret").await?;
    setup
        .query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    setup
        .query(&format!(
            "CREATE TABLE {table} (id int PRIMARY KEY, v varchar(10))"
        ))
        .await?;
    setup
        .query(&format!("INSERT INTO {table} VALUES (1, 'old')"))
        .await?;
    // In plain text, so the proxy can see the COMMIT go by.
    let proxy = TestProxy::start(upstream(d)).await?;
    let mut plain = config(d, proxy.port());
    plain.ssl_mode = SslMode::Disabled;
    let session = std::sync::Arc::new(Session::connect_test(&plain, "secret").await?);
    let p = |n: usize| {
        if d == Driver::Mysql {
            "?".to_owned()
        } else {
            format!("${n}")
        }
    };
    let mut update = Stmt::new(format!(
        "UPDATE {table} SET v = {} WHERE id = {}",
        p(1),
        p(2)
    ));
    update.params = vec!["new".into(), "1".into()];
    update.expect = Some(1);
    // PostgreSQL parses the COMMIT first and runs it in a second write.
    let more = if d == Driver::Postgres { 1 } else { 0 };
    proxy.blackhole_on(b"COMMIT", deliver.then_some(more));
    let save = tokio::spawn({
        let session = session.clone();
        async move { session.execute(&[update], true).await }
    });
    let started = Instant::now();
    while !proxy.tripped() {
        if save.is_finished() || started.elapsed() > Duration::from_secs(10) {
            let result = save.await?.map(|_| ()).map_err(|e| format!("{e:#}"));
            panic!("COMMIT never sent: {result:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The save hangs on the dark link; the user (or the keepalive) gives up on it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !save.is_finished(),
        "the save waits for an answer that never comes"
    );
    let progress = session.commit_progress();
    assert!(progress.commit_sent && !progress.finished, "{progress:?}");
    proxy.cut();
    let err = save.await?.err().expect("interrupted save");
    assert!(err.downcast_ref::<CommitInterrupted>().is_some(), "{err:#}");
    assert!(is_connection_lost(&err));
    proxy.restore();
    let fresh = Session::connect_test(&direct(d), "secret").await?;
    let checks = [RowVerify {
        applied: Some(Check {
            sql: format!("SELECT COUNT(*) FROM {table} WHERE id = 1 AND v = 'new'"),
            params: vec![],
            expect: 1,
            message: String::new(),
        }),
        not_applied: Some(Check {
            sql: format!("SELECT COUNT(*) FROM {table} WHERE id = 1 AND v = 'old'"),
            params: vec![],
            expect: 1,
            message: String::new(),
        }),
    }];
    // The server notices the closed link in a moment; until then the old connection is open.
    let mut verdict = fresh.verify_interrupted_commit(&progress, &checks).await?;
    let started = Instant::now();
    while matches!(
        &verdict,
        CommitVerdict::Unknown {
            open_backend: Some(_),
            ..
        }
    ) && started.elapsed() < Duration::from_secs(10)
    {
        tokio::time::sleep(Duration::from_millis(200)).await;
        verdict = fresh.verify_interrupted_commit(&progress, &checks).await?;
    }
    let stored = fresh
        .query(&format!("SELECT v FROM {table} WHERE id = 1"))
        .await?
        .rows[0][0]
        .clone();
    setup.query(&format!("DROP TABLE {table}")).await?;
    Ok((verdict, stored))
}

#[tokio::test]
async fn a_commit_cut_before_the_server_got_it_is_found_not_applied() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let (verdict, stored) = interrupted_commit(d, false).await?;
        assert_eq!(verdict, CommitVerdict::NotApplied, "{d:?}");
        assert_eq!(stored.as_deref(), Some("old"), "{d:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_commit_whose_answer_was_lost_is_found_applied() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let (verdict, stored) = interrupted_commit(d, true).await?;
        assert_eq!(verdict, CommitVerdict::Applied, "{d:?}");
        assert_eq!(stored.as_deref(), Some("new"), "{d:?}");
    }
    Ok(())
}

#[tokio::test]
async fn an_open_old_connection_keeps_the_verdict_unknown() -> Result<()> {
    let d = Driver::Postgres;
    let (proxy, session) = through_proxy(d).await?;
    let progress = telecat_db::CommitProgress {
        transaction: true,
        commit_sent: true,
        backend: None,
        ..Default::default()
    };
    // The commit connection's id, while it is still connected.
    session.execute(&[Stmt::new("SELECT 1")], true).await?;
    let backend = session.commit_progress().backend;
    let fresh = Session::connect_test(&direct(d), "secret").await?;
    let verdict = fresh
        .verify_interrupted_commit(
            &telecat_db::CommitProgress {
                backend: backend.clone(),
                ..progress.clone()
            },
            &[],
        )
        .await?;
    assert!(
        matches!(&verdict, CommitVerdict::Unknown { open_backend, .. } if *open_backend == backend),
        "{verdict:?}"
    );
    // A call still hung before its COMMIT could yet send it once the link is back.
    let verdict = fresh
        .verify_interrupted_commit(
            &telecat_db::CommitProgress {
                transaction: true,
                backend: backend.clone(),
                ..Default::default()
            },
            &[],
        )
        .await?;
    assert!(
        matches!(verdict, CommitVerdict::Unknown { .. }),
        "{verdict:?}"
    );
    // Nothing sent and the old connection gone: not applied.
    let verdict = fresh
        .verify_interrupted_commit(&telecat_db::CommitProgress::default(), &[])
        .await?;
    assert_eq!(verdict, CommitVerdict::NotApplied);
    drop(proxy);
    Ok(())
}

#[tokio::test]
async fn mysql_row_checks_tell_applied_from_not_applied() -> Result<()> {
    use telecat_core::changes::{Cell, EditRow, TableMeta, data_statements, mysql_verify_checks};
    let d = Driver::Mysql;
    let table = "telecat_rc_rows";
    let s = Session::connect_test(&direct(d), "secret").await?;
    s.query(&format!("DROP TABLE IF EXISTS {table}")).await?;
    s.query(&format!(
        "CREATE TABLE {table} (id int PRIMARY KEY, v varchar(10), n decimal(6,2))"
    ))
    .await?;
    s.query(&format!(
        "INSERT INTO {table} VALUES (1, 'old', 1.50), (2, 'gone', NULL)"
    ))
    .await?;
    let t = TableRef {
        schema: "zixma".into(),
        name: table.into(),
    };
    let m = TableMeta {
        columns: vec!["id".into(), "v".into(), "n".into()],
        primary_key: vec!["id".into()],
        binary: None,
    };
    let text = |v: &str| Cell::Text(v.into());
    let row = |orig: Option<[Option<&str>; 3]>, cur: [Cell; 3], deleted: bool| EditRow {
        key: telecat_core::changes::row_key(),
        orig: orig.map(|o| o.iter().map(|v| v.map(str::to_owned)).collect()),
        cur: cur.to_vec(),
        deleted,
        rid: None,
    };
    let rows = [
        row(
            Some([Some("1"), Some("old"), Some("1.50")]),
            [text("1"), text("new"), text("1.5")],
            false,
        ),
        row(
            Some([Some("2"), Some("gone"), None]),
            [text("2"), text("gone"), Cell::Null],
            true,
        ),
        row(None, [text("3"), text("added"), Cell::Null], false),
    ];
    let statements = data_statements(d, &t, &m, &rows)?;
    let checks = mysql_verify_checks(&t, &m, &rows);
    assert_eq!(checks.len(), statements.len());
    // A deleted row that is gone could also have been deleted by someone else.
    assert!(checks[1].applied.is_none());
    assert!(checks.iter().all(|c| c.not_applied.is_some()));
    let progress = telecat_db::CommitProgress {
        transaction: true,
        commit_sent: true,
        ..Default::default()
    };
    assert_eq!(
        s.verify_interrupted_commit(&progress, &checks).await?,
        CommitVerdict::NotApplied
    );
    s.execute(&statements, true).await?;
    // The deleted row can't vouch for the save, so the whole save stays unknown...
    let verdict = s.verify_interrupted_commit(&progress, &checks).await?;
    assert!(
        matches!(&verdict, CommitVerdict::Unknown { reason, .. } if reason.contains("2 of 3 changed rows read as saved")),
        "{verdict:?}"
    );
    // ...while the update and the insert alone read as saved.
    let kept = [checks[0].clone(), checks[2].clone()];
    assert_eq!(
        s.verify_interrupted_commit(&progress, &kept).await?,
        CommitVerdict::Applied,
        "typed comparison: 1.5 is the stored 1.50"
    );
    // Changed again elsewhere: neither the loaded nor the intended row is there.
    s.query(&format!("UPDATE {table} SET v = 'other' WHERE id = 1"))
        .await?;
    s.query(&format!("DELETE FROM {table} WHERE id = 3"))
        .await?;
    s.query(&format!("INSERT INTO {table} VALUES (2, 'gone', NULL)"))
        .await?;
    assert!(matches!(
        s.verify_interrupted_commit(&progress, &checks).await?,
        CommitVerdict::Unknown { .. } | CommitVerdict::NotApplied
    ));
    // Without a primary key nothing is guessed.
    let keyless = TableMeta {
        primary_key: vec![],
        ..m.clone()
    };
    let checks = mysql_verify_checks(&t, &keyless, &rows[..1]);
    assert!(checks[0].applied.is_none() && checks[0].not_applied.is_none());
    assert!(matches!(
        s.verify_interrupted_commit(&progress, &checks).await?,
        CommitVerdict::Unknown { .. }
    ));
    s.query(&format!("DROP TABLE {table}")).await?;
    Ok(())
}

/// An import cut off by the network: PostgreSQL's single transaction is found rolled back;
/// MySQL, whose statements commit one by one, stays honestly unknown.
#[tokio::test]
async fn an_import_cut_off_is_judged_after_reconnecting() -> Result<()> {
    use std::sync::atomic::AtomicBool;
    use telecat_db::{ImportOptions, ImportProgress};
    for d in [Driver::Postgres, Driver::Mysql] {
        let table = format!(
            "telecat_rc_import_{}",
            if d == Driver::Mysql { "my" } else { "pg" }
        );
        let setup = Session::connect_test(&direct(d), "secret").await?;
        setup
            .query(&format!("DROP TABLE IF EXISTS {table}"))
            .await?;
        let mut dump = format!("CREATE TABLE {table} (id int);\n");
        for i in 0..40 {
            dump += &format!("INSERT INTO {table} VALUES ({i});\n");
        }
        let dir = std::env::temp_dir().join(format!("telecat-rc-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{table}.sql"));
        std::fs::write(&path, dump)?;
        let proxy = TestProxy::start(upstream(d)).await?;
        let mut plain = config(d, proxy.port());
        plain.ssl_mode = SslMode::Disabled;
        let session = std::sync::Arc::new(Session::connect_test(&plain, "secret").await?);
        proxy.blackhole_on(format!("VALUES (20)").as_bytes(), None);
        let import = tokio::spawn({
            let session = session.clone();
            let target = plain.database.clone();
            async move {
                session
                    .import_dump(
                        &path,
                        &target,
                        ImportOptions {
                            allow: Default::default(),
                            transaction: true,
                            expect: None,
                        },
                        &ImportProgress::default(),
                        &AtomicBool::new(false),
                    )
                    .await
            }
        });
        let started = Instant::now();
        while !proxy.tripped() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{d:?}: never reached"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The app gives up on the hung import (a detected drop aborts it), then the link dies.
        import.abort();
        let _ = import.await;
        proxy.cut();
        proxy.restore();
        let fresh = Session::connect_test(&direct(d), "secret").await?;
        let progress = session.import_progress();
        let mut verdict = fresh.verify_interrupted_commit(&progress, &[]).await?;
        let started = Instant::now();
        while matches!(
            &verdict,
            CommitVerdict::Unknown {
                open_backend: Some(_),
                ..
            }
        ) && started.elapsed() < Duration::from_secs(10)
        {
            tokio::time::sleep(Duration::from_millis(200)).await;
            verdict = fresh.verify_interrupted_commit(&progress, &[]).await?;
        }
        if d == Driver::Postgres {
            assert_eq!(verdict, CommitVerdict::NotApplied, "{progress:?}");
            let left = fresh
                .query(&format!("SELECT to_regclass('{table}')::text"))
                .await?;
            assert_eq!(left.rows[0][0], None, "rolled back, table included");
        } else {
            assert!(
                matches!(
                    &verdict,
                    CommitVerdict::Unknown {
                        open_backend: None,
                        ..
                    }
                ),
                "{verdict:?}"
            );
            let rows = fresh
                .query(&format!("SELECT COUNT(*) FROM {table}"))
                .await?;
            assert_eq!(
                rows.rows[0][0].as_deref(),
                Some("20"),
                "the part before the drop stays"
            );
        }
        setup
            .query(&format!("DROP TABLE IF EXISTS {table}"))
            .await?;
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

/// "End Old Connection" ends only that exact connection: an id the server handed to another
/// connection, or a wrong token or start time, ends nothing.
#[tokio::test]
async fn ending_the_old_connection_checks_its_identity_first() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let old = Session::connect_test(&direct(d), "secret").await?;
        old.execute(&[Stmt::new("SELECT 1")], true).await?;
        let progress = old.commit_progress();
        let identity = progress.identity.clone().expect("identity recorded");
        assert!(!identity.token.is_empty(), "{d:?}");
        let fresh = Session::connect_test(&direct(d), "secret").await?;
        let other = Session::connect_test(&direct(d), "secret").await?;
        other.execute(&[Stmt::new("SELECT 1")], true).await?;
        let other_id = other.commit_progress().backend.unwrap();
        // The same id reused by another connection: refused, and that connection lives on.
        let reused = telecat_db::CommitProgress {
            backend: Some(other_id.clone()),
            identity: Some(telecat_db::BackendIdentity {
                id: other_id,
                ..identity.clone()
            }),
            ..progress.clone()
        };
        let err = fresh.end_old_backend(&reused).await.err().expect("refused");
        assert!(err.to_string().contains("already gone"), "{d:?}: {err}");
        other.execute(&[Stmt::new("SELECT 1")], true).await?;
        assert_eq!(
            other.commit_progress().backend,
            reused.backend,
            "{d:?}: not ended"
        );
        // A wrong token: refused too.
        let forged = telecat_db::CommitProgress {
            identity: Some(telecat_db::BackendIdentity {
                token: "0000000000000000".into(),
                ..identity.clone()
            }),
            ..progress.clone()
        };
        assert!(fresh.end_old_backend(&forged).await.is_err(), "{d:?}");
        // No identity at all: nothing is ended.
        let bare = telecat_db::CommitProgress {
            identity: None,
            ..progress.clone()
        };
        assert!(fresh.end_old_backend(&bare).await.is_err(), "{d:?}");
        // The real one: ended, so the old session reopens its commit connection next time.
        fresh.end_old_backend(&progress).await?;
        let started = Instant::now();
        while fresh
            .verify_interrupted_commit(
                &telecat_db::CommitProgress {
                    transaction: true,
                    commit_sent: true,
                    ..progress.clone()
                },
                &[],
            )
            .await
            .is_ok_and(|v| {
                matches!(
                    v,
                    CommitVerdict::Unknown {
                        open_backend: Some(_),
                        ..
                    }
                )
            })
        {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{d:?}: still open"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Its next save may find the connection ended (nothing sent; PostgreSQL already
        // knows and reopens at once); the one after reopens it.
        let ended = old.execute(&[Stmt::new("SELECT 1")], true).await;
        assert!(
            ended.as_ref().is_ok() || ended.is_err_and(|e| is_connection_lost(&e)),
            "{d:?}"
        );
        old.execute(&[Stmt::new("SELECT 1")], true).await?;
        assert_ne!(
            old.commit_progress().backend,
            progress.backend,
            "{d:?}: reopened"
        );
    }
    Ok(())
}
