//! QA 0.4.0: adversarial checks of interrupted-commit verification (D-1) and of what counts as
//! a lost connection. Throwaway databases qa040* only; dropped at the end.
use anyhow::Result;
use telecat_core::changes::{
    Cell, EditRow, Stmt, TableMeta, data_statements, mysql_verify_checks, row_key,
};
use telecat_core::types::*;
use telecat_db::{CommitProgress, CommitVerdict, Session};

fn config(d: Driver, db: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "qa040".into();
    c.host = "127.0.0.1".into();
    c.port = if d == Driver::Mysql { 3307 } else { 5434 };
    c.user = if d == Driver::Mysql {
        "root"
    } else {
        "postgres"
    }
    .into();
    c.database = db.into();
    c
}
fn meta(cols: &[&str], pk: &[&str]) -> TableMeta {
    TableMeta {
        columns: cols.iter().map(|s| s.to_string()).collect(),
        primary_key: pk.iter().map(|s| s.to_string()).collect(),
        binary: None,
    }
}
fn row(orig: Option<Vec<Option<&str>>>, cur: Vec<Cell>, deleted: bool) -> EditRow {
    EditRow {
        key: row_key(),
        orig: orig.map(|o| o.into_iter().map(|v| v.map(str::to_owned)).collect()),
        cur,
        deleted,
        rid: None,
    }
}
/// Row 1 of `tr` read the way the table editor reads it (with the row identity that its saves
/// are guarded by), then edited to `cur`.
async fn loaded(s: &Session, tr: &TableRef, cur: Vec<Cell>) -> Result<(TableMeta, Vec<EditRow>)> {
    let data = s
        .table_data(&TableDataRequest {
            table: tr.clone(),
            filters: vec![],
            combine: Default::default(),
            order_by: None,
            limit: 100,
            offset: 0,
        })
        .await?;
    let mut rows = telecat_core::changes::to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    rows.truncate(1);
    rows[0].cur = cur;
    assert!(rows[0].rid.is_some(), "loaded with its identity");
    Ok((
        TableMeta {
            columns: data.result.columns.clone(),
            primary_key: data.primary_key.clone(),
            binary: data.result.binary.clone(),
        },
        rows,
    ))
}
fn t(name: &str) -> Cell {
    Cell::Text(name.into())
}
fn sent() -> CommitProgress {
    CommitProgress {
        transaction: true,
        commit_sent: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn qa_mysql_verify_attacks() -> Result<()> {
    let admin = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa040").await?;
    admin.query("CREATE DATABASE qa040").await?;
    let s = Session::connect_test(&config(Driver::Mysql, "qa040"), "secret").await?;
    let tr = TableRef {
        schema: "qa040".into(),
        name: "t".into(),
    };
    let mut bad: Vec<String> = vec![];

    // (A) Concurrent revert after our commit applied: the row is exactly as loaded again, which
    // can't be told from "never applied". The rule says NotApplied (the row equals its loaded
    // values), and a re-save goes through the same original-value guard.
    s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
        .await?;
    s.query("INSERT INTO t VALUES (1,'old')").await?;
    let m = meta(&["id", "v"], &["id"]);
    // Loaded the way the table editor loads it: with its row identity.
    let (m, rows) = loaded(&s, &tr, vec![t("1"), t("new")]).await?;
    let st = data_statements(Driver::Mysql, &tr, &m, &rows)?;
    let checks = mysql_verify_checks(&tr, &m, &rows);
    s.execute(&st, true).await?; // our commit lands
    s.query("UPDATE t SET v='old' WHERE id=1").await?; // another session reverts
    let v = s.verify_interrupted_commit(&sent(), &checks).await?;
    eprintln!("[A concurrent-revert] verdict={v:?}");
    if v != CommitVerdict::NotApplied {
        bad.push(format!(
            "A: row equal to its loaded values -> {v:?}, expected NotApplied"
        ));
    }
    // Changed again before the re-save: the guard refuses to overwrite it.
    s.query("UPDATE t SET v='other' WHERE id=1").await?;
    if s.execute(&st, true).await.is_ok() {
        bad.push("A: a re-save overwrote a row changed since it was loaded".into());
    }

    // (B) BEFORE UPDATE trigger rewrites the edited column to a different value.
    s.query("DROP TABLE t").await?;
    s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
        .await?;
    s.query("INSERT INTO t VALUES (1,'old')").await?;
    s.query("CREATE TRIGGER trg BEFORE UPDATE ON t FOR EACH ROW SET NEW.v = CONCAT(NEW.v, '!')")
        .await?;
    let rows = vec![row(
        Some(vec![Some("1"), Some("old")]),
        vec![t("1"), t("new")],
        false,
    )];
    let st = data_statements(Driver::Mysql, &tr, &m, &rows)?;
    let checks = mysql_verify_checks(&tr, &m, &rows);
    s.execute(&st, true).await?; // stores 'new!' not 'new'
    let stored = s.query("SELECT v FROM t WHERE id=1").await?.rows[0][0].clone();
    let v = s.verify_interrupted_commit(&sent(), &checks).await?;
    eprintln!("[B trigger-rewrite] stored={stored:?} verdict={v:?}");
    if !matches!(v, CommitVerdict::Unknown { .. }) {
        bad.push(format!(
            "B: trigger rewrote edited column -> {v:?} (stored {stored:?}), expected Unknown"
        ));
    }
    s.query("DROP TRIGGER trg").await?;

    // (C) ON UPDATE timestamp column the user didn't edit must not confuse the check.
    s.query("DROP TABLE t").await?;
    s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20), updated timestamp DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP)").await?;
    s.query("INSERT INTO t VALUES (1,'old', '2020-01-01 00:00:00')")
        .await?;
    let m3 = meta(&["id", "v", "updated"], &["id"]);
    let rows = vec![row(
        Some(vec![Some("1"), Some("old"), Some("2020-01-01 00:00:00")]),
        vec![t("1"), t("new"), Cell::Unset { undefined: true }],
        false,
    )];
    let st = data_statements(Driver::Mysql, &tr, &m3, &rows)?;
    let checks = mysql_verify_checks(&tr, &m3, &rows);
    s.execute(&st, true).await?;
    let v = s.verify_interrupted_commit(&sent(), &checks).await?;
    eprintln!("[C on-update-col] verdict={v:?} (want Applied)");
    if v != CommitVerdict::Applied {
        bad.push(format!("C: ON UPDATE column -> {v:?}, expected Applied"));
    }

    // (D) No-op: the edited value already equals the intended value in the DB (concurrent),
    // but our commit did NOT run. applied holds via coincidence; not_applied (orig) fails.
    s.query("DROP TABLE t").await?;
    s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
        .await?;
    s.query("INSERT INTO t VALUES (1,'old'),(2,'two')").await?;
    let rows = vec![
        row(
            Some(vec![Some("1"), Some("old")]),
            vec![t("1"), t("new")],
            false,
        ),
        row(
            Some(vec![Some("2"), Some("two")]),
            vec![t("2"), t("changed")],
            false,
        ),
    ];
    let checks = mysql_verify_checks(&tr, &m, &rows);
    s.query("UPDATE t SET v='new' WHERE id=1").await?; // another session set row 1 to intended; row 2 untouched; our tx never ran
    let v = s.verify_interrupted_commit(&sent(), &checks).await?;
    eprintln!("[D partial-coincidence] verdict={v:?}");
    if !matches!(v, CommitVerdict::Unknown { .. }) {
        bad.push(format!("D: the rows disagree -> {v:?}, expected Unknown"));
    }

    // (E) Delete verified while another session re-inserted the same key+values.
    s.query("DROP TABLE t").await?;
    s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
        .await?;
    s.query("INSERT INTO t VALUES (1,'old')").await?;
    let rows = vec![row(
        Some(vec![Some("1"), Some("old")]),
        vec![t("1"), t("old")],
        true,
    )];
    let checks = mysql_verify_checks(&tr, &m, &rows);
    s.execute(&data_statements(Driver::Mysql, &tr, &m, &rows)?, true)
        .await?; // deleted
    s.query("INSERT INTO t VALUES (1,'old')").await?; // another session re-created it identically
    let v = s.verify_interrupted_commit(&sent(), &checks).await?;
    eprintln!("[E delete-then-reinsert] verdict={v:?}");
    // A row with the key that equals what was loaded: by the rule, not applied.
    if v != CommitVerdict::NotApplied {
        bad.push(format!(
            "E: identical row present -> {v:?}, expected NotApplied"
        ));
    }
    // Gone instead: another session could explain that too, so it stays unknown.
    s.query("DELETE FROM t WHERE id=1").await?;
    let v = s.verify_interrupted_commit(&sent(), &checks).await?;
    if !matches!(v, CommitVerdict::Unknown { .. }) {
        bad.push(format!(
            "E: the deleted row is gone -> {v:?}, expected Unknown"
        ));
    }

    s.query("DROP TABLE IF EXISTS t").await?;
    admin.query("DROP DATABASE qa040").await?;
    eprintln!("\nBAD-MYSQL-VERIFY: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

#[tokio::test]
async fn qa_pg_verify_edge_cases() -> Result<()> {
    let admin = Session::connect_test(&config(Driver::Postgres, "main_db"), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa040 WITH (FORCE)")
        .await?;
    admin.query("CREATE DATABASE qa040").await?;
    let s = Session::connect_test(&config(Driver::Postgres, "qa040"), "secret").await?;
    let mut bad: Vec<String> = vec![];

    // Non-numeric / empty / absent xid -> never a definitive verdict.
    for xid in ["", "abc", "0", "999999999999999999999999999"] {
        let progress = CommitProgress {
            transaction: true,
            commit_sent: true,
            xid: Some(xid.into()),
            ..Default::default()
        };
        let v = s.verify_interrupted_commit(&progress, &[]).await;
        eprintln!(
            "[pg xid={xid:?}] {:?}",
            v.as_ref().map_err(|e| e.to_string())
        );
        match v {
            Ok(CommitVerdict::Unknown { .. }) => {}
            Ok(other) => bad.push(format!("pg xid={xid:?}: {other:?} (want Unknown)")),
            Err(e) => eprintln!("   err (acceptable if not a false verdict): {e}"),
        }
    }
    // A real committed xid reads "committed"; a real aborted xid reads "aborted".
    let got = s.query("SELECT txid_current()").await?.rows[0][0]
        .clone()
        .unwrap();
    s.query("SELECT 1").await?; // close that tx by starting another implicitly
    let progress = CommitProgress {
        transaction: true,
        commit_sent: true,
        xid: Some(got.clone()),
        ..Default::default()
    };
    let v = s.verify_interrupted_commit(&progress, &[]).await?;
    eprintln!("[pg real-committed xid={got}] {v:?} (want Applied)");
    if v != CommitVerdict::Applied {
        bad.push(format!("pg committed xid -> {v:?}"));
    }
    // Future xid that never existed.
    let future = s
        .query("SELECT (txid_current() + 1000000)::text")
        .await?
        .rows[0][0]
        .clone()
        .unwrap();
    let progress = CommitProgress {
        transaction: true,
        commit_sent: true,
        xid: Some(future.clone()),
        ..Default::default()
    };
    let v = s.verify_interrupted_commit(&progress, &[]).await;
    eprintln!(
        "[pg future xid={future}] {:?}",
        v.as_ref().map_err(|e| e.to_string())
    );
    if let Ok(CommitVerdict::Applied) = v {
        bad.push("pg future xid -> Applied".into());
    }

    drop(s);
    admin.query("DROP DATABASE qa040 WITH (FORCE)").await?;
    eprintln!("\nBAD-PG-VERIFY: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

/// Decisive: after a successful commit whose stored value differs from the intended value
/// (a BEFORE UPDATE trigger with a side effect), the verdict is Unknown, so nothing is re-saved
/// and the side effect fires once. Even a forced re-save is stopped by the original-value guard.
#[tokio::test]
async fn qa_mysql_false_not_applied_double_fires_side_effect() -> Result<()> {
    let admin = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa040b").await?;
    admin.query("CREATE DATABASE qa040b").await?;
    let s = Session::connect_test(&config(Driver::Mysql, "qa040b"), "secret").await?;
    s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
        .await?;
    s.query("CREATE TABLE audit (n int AUTO_INCREMENT PRIMARY KEY, who varchar(20))")
        .await?;
    s.query("INSERT INTO t VALUES (1,'old')").await?;
    s.query(
        "CREATE TRIGGER trg AFTER UPDATE ON t FOR EACH ROW INSERT INTO audit (who) VALUES (NEW.v)",
    )
    .await?;
    s.query("CREATE TRIGGER trg2 BEFORE UPDATE ON t FOR EACH ROW SET NEW.v = CONCAT(NEW.v,'!')")
        .await?;
    let tr = TableRef {
        schema: "qa040b".into(),
        name: "t".into(),
    };
    let m = meta(&["id", "v"], &["id"]);
    // Loaded the way the table editor loads it: with its row identity.
    let (m, rows) = loaded(&s, &tr, vec![t("1"), t("new")]).await?;
    let st = data_statements(Driver::Mysql, &tr, &m, &rows)?;
    let checks = mysql_verify_checks(&tr, &m, &rows);
    s.execute(&st, true).await?; // commit #1 lands; audit gets 1 row
    let v = s.verify_interrupted_commit(&sent(), &checks).await?; // link "dropped"; verify
    let audit1 = s.query("SELECT COUNT(*) FROM audit").await?.rows[0][0].clone();
    eprintln!("[double-fire] verdict after a successful commit = {v:?}; audit rows = {audit1:?}");
    let resave = s.execute(&st, true).await;
    let audit2 = s.query("SELECT COUNT(*) FROM audit").await?.rows[0][0].clone();
    s.query("DROP TABLE t").await?;
    admin.query("DROP DATABASE qa040b").await?;
    assert!(matches!(v, CommitVerdict::Unknown { .. }), "{v:?}");
    assert!(
        resave.is_err(),
        "the original-value guard stops a second apply"
    );
    assert_eq!(audit1.as_deref(), Some("1"));
    assert_eq!(audit2.as_deref(), Some("1"), "the side effect fired once");
    Ok(())
}

/// The common case: a batch of only-modified rows that committed successfully. What verdict?
#[tokio::test]
async fn qa_mysql_plain_modify_verdict() -> Result<()> {
    let admin = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa040c").await?;
    admin.query("CREATE DATABASE qa040c").await?;
    let s = Session::connect_test(&config(Driver::Mysql, "qa040c"), "secret").await?;
    s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
        .await?;
    s.query("INSERT INTO t VALUES (1,'old'),(2,'two')").await?;
    let tr = TableRef {
        schema: "qa040c".into(),
        name: "t".into(),
    };
    let m = meta(&["id", "v"], &["id"]);
    // one modified row
    let one = vec![row(
        Some(vec![Some("1"), Some("old")]),
        vec![t("1"), t("new")],
        false,
    )];
    // two modified rows
    let two = vec![
        row(
            Some(vec![Some("1"), Some("old")]),
            vec![t("1"), t("new")],
            false,
        ),
        row(
            Some(vec![Some("2"), Some("two")]),
            vec![t("2"), t("II")],
            false,
        ),
    ];
    s.execute(&data_statements(Driver::Mysql, &tr, &m, &two)?, true)
        .await?; // both succeed
    let v1 = s
        .verify_interrupted_commit(&sent(), &mysql_verify_checks(&tr, &m, &one))
        .await?;
    let v2 = s
        .verify_interrupted_commit(&sent(), &mysql_verify_checks(&tr, &m, &two))
        .await?;
    eprintln!("[plain-modify] single-row verdict={v1:?} two-row verdict={v2:?}");
    s.query("DROP TABLE t").await?;
    admin.query("DROP DATABASE qa040c").await?;
    assert_eq!(v1, CommitVerdict::Applied);
    assert_eq!(v2, CommitVerdict::Applied);
    Ok(())
}

/// Item 5: a lock wait timeout and a deadlock during commit must NOT look like a lost link.
#[tokio::test]
async fn qa_lock_timeout_is_not_connection_loss() -> Result<()> {
    use telecat_db::{is_auth_failure, is_connection_lost};
    let mut bad: Vec<String> = vec![];
    // MySQL: hold a row lock in one session, hit lock_wait_timeout in another.
    let admin = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa040d").await?;
    admin.query("CREATE DATABASE qa040d").await?;
    let holder = Session::connect_test(&config(Driver::Mysql, "qa040d"), "secret").await?;
    holder
        .query("CREATE TABLE t (id int PRIMARY KEY, v int) ENGINE=InnoDB")
        .await?;
    holder.query("INSERT INTO t VALUES (1,1)").await?;
    holder.query("SET autocommit=0").await?;
    holder
        .query("SELECT * FROM t WHERE id=1 FOR UPDATE")
        .await?;
    let victim = Session::connect_test(&config(Driver::Mysql, "qa040d"), "secret").await?;
    victim
        .query("SET SESSION innodb_lock_wait_timeout=1")
        .await?;
    let e = victim
        .execute(
            &[{
                let mut s = Stmt::new("UPDATE t SET v=2 WHERE id=1".to_string());
                s.expect = Some(1);
                s
            }],
            true,
        )
        .await
        .err()
        .unwrap();
    eprintln!(
        "[mysql lock-wait] lost={} auth={} err={}",
        is_connection_lost(&e),
        is_auth_failure(&e),
        e.to_string().lines().next().unwrap_or("")
    );
    if is_connection_lost(&e) {
        bad.push("MySQL lock wait timeout classified as connection loss".into());
    }
    holder.query("ROLLBACK").await?;
    admin.query("DROP DATABASE qa040d").await?;

    // PG: lock_timeout during an UPDATE blocked by another session.
    let admin = Session::connect_test(&config(Driver::Postgres, "main_db"), "secret").await?;
    admin
        .query("DROP DATABASE IF EXISTS qa040d WITH (FORCE)")
        .await?;
    admin.query("CREATE DATABASE qa040d").await?;
    let holder = Session::connect_test(&config(Driver::Postgres, "qa040d"), "secret").await?;
    holder
        .query("CREATE TABLE t (id int PRIMARY KEY, v int)")
        .await?;
    holder.query("INSERT INTO t VALUES (1,1)").await?;
    holder.query("BEGIN").await?;
    holder.query("UPDATE t SET v=9 WHERE id=1").await?;
    let victim = Session::connect_test(&config(Driver::Postgres, "qa040d"), "secret").await?;
    let e = victim
        .execute(
            &[{
                let mut s = Stmt::new("UPDATE t SET v=2 WHERE id=1".to_string());
                s.expect = Some(1);
                s
            }],
            true,
        )
        .await
        .err()
        .unwrap();
    eprintln!(
        "[pg lock-timeout] lost={} auth={} err={}",
        is_connection_lost(&e),
        is_auth_failure(&e),
        e.to_string().lines().next().unwrap_or("")
    );
    if is_connection_lost(&e) {
        bad.push("PG lock timeout classified as connection loss".into());
    }
    holder.query("ROLLBACK").await?;
    drop(holder);
    drop(victim);
    admin.query("DROP DATABASE qa040d WITH (FORCE)").await?;
    eprintln!("\nBAD-CLASSIFY: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}
