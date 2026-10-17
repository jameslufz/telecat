//! Locks cover the exact-match precheck rows without SERIALIZABLE side effects.
use anyhow::Result;
use std::{sync::Arc, time::Duration};
use telecat_core::{
    changes::{Check, Stmt},
    types::*,
};
use telecat_db::Session;
async fn contract(driver: Driver) -> Result<()> {
    let pg = driver == Driver::Postgres;
    let mut c = empty_connection(driver);
    c.port = if pg { 5434 } else { 3307 };
    c.user = if pg { "postgres" } else { "root" }.into();
    c.database = if pg { "main_db" } else { "zixma" }.into();
    let db = Arc::new(Session::connect_test(&c, "secret").await?);
    let other = Arc::new(Session::connect_test(&c, "secret").await?);
    let isolation = if pg {
        "SHOW transaction_isolation"
    } else {
        "SELECT @@session.transaction_isolation"
    };
    let foreground = db.query(isolation).await?.rows;
    assert_eq!(
        db.execute(&[Stmt::new(isolation)], false).await?[0].rows,
        foreground
    );
    assert!(
        !foreground[0][0]
            .as_deref()
            .unwrap()
            .to_ascii_lowercase()
            .contains("serializable")
    );
    let timeout = if pg {
        "SHOW lock_timeout"
    } else {
        "SELECT @@session.innodb_lock_wait_timeout"
    };
    assert_eq!(
        db.execute(&[Stmt::new(timeout)], false).await?[0].rows[0][0].as_deref(),
        Some(if pg { "15s" } else { "15" })
    );
    if pg {
        assert_eq!(
            db.execute(&[Stmt::new("SHOW statement_timeout")], false)
                .await?[0]
                .rows[0][0]
                .as_deref(),
            Some("0")
        );
    }
    db.query("DROP TABLE IF EXISTS telecat_m2_locks").await?;
    db.query("CREATE TABLE telecat_m2_locks (id int PRIMARY KEY, v int)")
        .await?;
    db.query("INSERT INTO telecat_m2_locks VALUES (1,0),(2,0)")
        .await?;
    let pause = if pg {
        "SELECT pg_sleep(0.6)"
    } else {
        "SELECT SLEEP(0.6)"
    };
    let mut hold = Stmt::new(pause);
    hold.check = Some(Check {
        sql: "SELECT COUNT(*) FROM telecat_m2_locks WHERE id=1".into(),
        params: vec![],
        expect: 1,
        message: "stale".into(),
    });
    let mut write = Stmt::new("UPDATE telecat_m2_locks SET v=v+1 WHERE id=1");
    write.expect = Some(1);
    let writer = db.clone();
    let batch = tokio::spawn(async move { writer.execute(&[hold, write], true).await });
    let start = std::time::Instant::now();
    loop {
        let processes = other.process_list(None).await?;
        if processes
            .rows
            .iter()
            .flatten()
            .any(|s| s.as_deref() == Some(pause))
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "guarded batch did not enter pause"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A different primary-key row remains writable while the counted row is locked.
    tokio::time::timeout(
        Duration::from_millis(300),
        other.query("UPDATE telecat_m2_locks SET v=10 WHERE id=2"),
    )
    .await??;
    let writer = other.clone();
    let mut same_row = tokio::spawn(async move {
        writer
            .query("UPDATE telecat_m2_locks SET v=v+100 WHERE id=1")
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut same_row)
            .await
            .is_err(),
        "matching row was not locked by precheck"
    );
    batch.await??;
    same_row.await??;
    assert_eq!(
        db.query("SELECT v FROM telecat_m2_locks ORDER BY id")
            .await?
            .rows,
        vec![vec![Some("101".into())], vec![Some("10".into())]]
    );
    db.query("DROP TABLE telecat_m2_locks").await?;
    db.close().await;
    other.close().await;
    Ok(())
}
#[tokio::test]
async fn mysql_default_isolation_and_exact_row_locks() -> Result<()> {
    contract(Driver::Mysql).await
}
#[tokio::test]
async fn postgres_default_isolation_and_exact_row_locks() -> Result<()> {
    contract(Driver::Postgres).await
}
