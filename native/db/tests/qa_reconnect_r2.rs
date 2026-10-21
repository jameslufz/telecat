//! QA 0.4.0 round 2: fingerprint verification attacks, End Old Connection without
//! performance_schema access, and result-plan re-derivation after a schema change.
//! Throwaway database qa041 only; dropped at the end.
use anyhow::Result;
use telecat_core::changes::{
    Cell, EditRow, TableMeta, data_statements, mysql_verify_checks, to_edit_rows,
};
use telecat_core::types::*;
use telecat_db::{BackendIdentity, CommitProgress, CommitVerdict, Session};

fn config(d: Driver, db: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "qa041".into();
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
/// Both tests use the qa041 database, so they take turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn sent() -> CommitProgress {
    CommitProgress {
        transaction: true,
        commit_sent: true,
        ..Default::default()
    }
}
/// Loads `t` the way the app does (with row identities), as EditRows + meta.
async fn load(s: &Session, table: &str) -> Result<(TableRef, TableMeta, Vec<EditRow>)> {
    let tr = TableRef {
        schema: "qa041".into(),
        name: table.into(),
    };
    let data = s
        .table_data(&TableDataRequest {
            table: tr.clone(),
            filters: vec![],
            combine: Combine::And,
            order_by: None,
            limit: 1000,
            offset: 0,
        })
        .await?;
    let rows = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    let m = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    Ok((tr, m, rows))
}
fn set(rows: &mut [EditRow], key: &str, col: usize, v: Cell) -> usize {
    let i = rows
        .iter()
        .position(|r| r.cur[0].value() == Some(key))
        .unwrap();
    rows[i].cur[col] = v;
    i
}

#[tokio::test]
async fn qa_mysql_fingerprint_attacks() -> Result<()> {
    let _turn = SERIAL.lock().await;
    let admin = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa041").await?;
    admin.query("CREATE DATABASE qa041").await?;
    let s = Session::connect_test(&config(Driver::Mysql, "qa041"), "secret").await?;
    let mut bad: Vec<String> = vec![];
    // Columns: float, decimal, large text, blob, invisible column (excluded from SELECT *).
    s.query("CREATE TABLE t (id int PRIMARY KEY, f float, d decimal(12,4), big longtext, b longblob, hid int INVISIBLE, v varchar(20))").await?;
    s.query("INSERT INTO t (id, f, d, big, b, hid, v) VALUES (1, 0.1, 1.5, REPEAT('x', 300000), REPEAT(0x00FF, 100000), 7, 'old'), (2, 0.2, 2.5, 'small', 0x01, 8, 'two')").await?;
    for (name, col, val, mutate_after) in [
        ("float-edit", 1usize, "0.3", None),
        ("decimal-edit", 2, "1.50", None),
        ("big-text-edit", 3, "y", None),
        (
            "varchar-edit+invisible-changed-concurrently",
            5,
            "new",
            Some("UPDATE t SET hid = 99 WHERE id = 1"),
        ),
    ] {
        s.query("UPDATE t SET f=0.1, d=1.5, big=REPEAT('x',300000), hid=7, v='old' WHERE id=1")
            .await?;
        let (tr, m, mut rows) = load(&s, "t").await?;
        set(&mut rows, "1", col, Cell::Text(val.into()));
        let st = data_statements(Driver::Mysql, &tr, &m, &rows)?;
        let checks = mysql_verify_checks(&tr, &m, &rows);
        let before = s.verify_interrupted_commit(&sent(), &checks).await?; // not run yet
        s.execute(&st, true).await?; // commit lands
        if let Some(q) = mutate_after {
            s.query(q).await?;
        }
        let after = s.verify_interrupted_commit(&sent(), &checks).await?;
        eprintln!("[{name}] before-commit={before:?} after-commit={after:?}");
        // R2-3: 1.5000 typed as 1.50 is the same number, and the reason says so.
        if name == "decimal-edit"
            && !matches!(&before, CommitVerdict::Unknown { reason, .. } if reason.contains("matches both"))
        {
            bad.push(format!("{name}: numeric no-op reason: {before:?}"));
        }
        if before == CommitVerdict::Applied {
            bad.push(format!("{name}: un-run commit reported Applied"));
        }
        if after == CommitVerdict::NotApplied {
            bad.push(format!("{name}: committed change reported NotApplied"));
        }
    }
    // Table altered between commit and verify.
    s.query("UPDATE t SET v='old' WHERE id=1").await?;
    let (tr, m, mut rows) = load(&s, "t").await?;
    set(&mut rows, "1", 5, Cell::Text("new".into()));
    let checks = mysql_verify_checks(&tr, &m, &rows);
    s.execute(&data_statements(Driver::Mysql, &tr, &m, &rows)?, true)
        .await?;
    for (label, ddl) in [
        ("add-column", "ALTER TABLE t ADD COLUMN extra int"),
        ("modify-type", "ALTER TABLE t MODIFY v varchar(40)"),
        (
            "drop-column",
            "ALTER TABLE t DROP COLUMN extra, DROP COLUMN d",
        ),
    ] {
        s.query(ddl).await?;
        let v = s.verify_interrupted_commit(&sent(), &checks).await;
        eprintln!(
            "[altered {label}] {:?}",
            v.as_ref().map_err(|e| e.to_string())
        );
        if let Ok(CommitVerdict::NotApplied) = v {
            bad.push(format!(
                "altered {label}: committed change reported NotApplied"
            ));
        }
    }
    // Duplicate rows (no primary key): never a definitive verdict.
    s.query("CREATE TABLE dup (a int, v varchar(10))").await?;
    s.query("INSERT INTO dup VALUES (1,'x'),(1,'x')").await?;
    let (tr, m, rows) = load(&s, "dup").await?;
    let mut rows = rows;
    rows[0].cur[1] = Cell::Text("y".into());
    let checks = mysql_verify_checks(&tr, &m, &rows);
    let v = s.verify_interrupted_commit(&sent(), &checks).await?;
    eprintln!("[duplicates no-PK] {v:?}");
    if !matches!(v, CommitVerdict::Unknown { .. }) {
        bad.push(format!("duplicates: {v:?}"));
    }

    // End Old Connection with an identity that has no user/host (performance_schema unreadable).
    let partial = BackendIdentity {
        id: "1".into(),
        token: "nope".into(),
        ..Default::default()
    };
    let progress = CommitProgress {
        identity: Some(partial),
        backend: Some("1".into()),
        ..sent()
    };
    let r = s.end_old_backend(&progress).await;
    eprintln!(
        "[end-old partial identity] {:?}",
        r.as_ref().map_err(|e| e.to_string())
    );
    if r.is_ok() {
        bad.push("end-old with partial identity ended something".into());
    }
    // A live, unrelated session's real id with a wrong token: must refuse.
    let other = Session::connect_test(&config(Driver::Mysql, "qa041"), "secret").await?;
    let oid = other.query("SELECT CONNECTION_ID()").await?.rows[0][0]
        .clone()
        .unwrap();
    let wrong = BackendIdentity {
        id: oid.clone(),
        token: "not-telecat".into(),
        user: Some("root".into()),
        host: Some("localhost".into()),
        ..Default::default()
    };
    let progress = CommitProgress {
        identity: Some(wrong),
        backend: Some(oid.clone()),
        ..sent()
    };
    let r = s.end_old_backend(&progress).await;
    let alive = other.query("SELECT 1").await.is_ok();
    eprintln!(
        "[end-old wrong token, id {oid}] {:?} other session alive={alive}",
        r.as_ref().map_err(|e| e.to_string())
    );
    if !alive || r.is_ok() {
        bad.push("end-old ended (or claimed to end) an unrelated session".into());
    }

    // A low-privilege user without performance_schema access.
    let _ = admin.query("DROP USER IF EXISTS 'qa041lp'@'%'").await;
    admin
        .query("CREATE USER 'qa041lp'@'%' IDENTIFIED BY 'lowpw'")
        .await?;
    admin.query("GRANT ALL ON qa041.* TO 'qa041lp'@'%'").await?;
    let mut lc = config(Driver::Mysql, "qa041");
    lc.user = "qa041lp".into();
    let low = Session::connect_test(&lc, "lowpw").await?;
    let probe = low
        .query("SELECT COUNT(*) FROM performance_schema.threads")
        .await;
    eprintln!(
        "[low-priv] performance_schema readable: {:?}",
        probe.as_ref().map(|r| r.rows.clone()).map_err(|e| e
            .to_string()
            .lines()
            .next()
            .unwrap_or("")
            .to_owned())
    );
    let p2 = CommitProgress {
        identity: Some(BackendIdentity {
            id: oid.clone(),
            token: "x".into(),
            ..Default::default()
        }),
        backend: Some(oid.clone()),
        ..sent()
    };
    let r = low.end_old_backend(&p2).await;
    eprintln!(
        "[low-priv end-old] {:?} other alive={}",
        r.as_ref().map_err(|e| e.to_string()),
        other.query("SELECT 1").await.is_ok()
    );
    if r.is_ok() || other.query("SELECT 1").await.is_err() {
        bad.push("low-privilege end-old ended something".into());
    }
    drop(low);
    admin.query("DROP USER 'qa041lp'@'%'").await?;
    admin.query("DROP DATABASE qa041").await?;
    eprintln!("\nBAD-R2: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

#[tokio::test]
async fn qa_result_plan_rederive_after_schema_change() -> Result<()> {
    let _turn = SERIAL.lock().await;
    let mut bad: Vec<String> = vec![];
    for d in [Driver::Mysql, Driver::Postgres] {
        let home = if d == Driver::Mysql {
            "zixma"
        } else {
            "main_db"
        };
        let admin = Session::connect_test(&config(d, home), "secret").await?;
        if d == Driver::Postgres {
            admin
                .query("DROP DATABASE IF EXISTS qa041 WITH (FORCE)")
                .await?;
        } else {
            admin.query("DROP DATABASE IF EXISTS qa041").await?;
        }
        admin.query("CREATE DATABASE qa041").await?;
        for (label, ddl) in [
            ("none", ""),
            ("rename-edited-column", "ALTER TABLE t RENAME COLUMN v TO w"),
            (
                "change-primary-key",
                "ALTER TABLE t DROP PRIMARY KEY, ADD PRIMARY KEY (id, v)",
            ),
            ("make-edited-generated", "__gen"),
            (
                "change-edited-type",
                if d == Driver::Mysql {
                    "ALTER TABLE t MODIFY v varchar(3)"
                } else {
                    "ALTER TABLE t ALTER COLUMN v TYPE varchar(3)"
                },
            ),
            ("drop-recreate-same", "__recreate"),
        ] {
            let s = Session::connect_test(&config(d, "qa041"), "secret").await?;
            s.query("DROP TABLE IF EXISTS t").await?;
            s.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
                .await?;
            s.query("INSERT INTO t VALUES (1,'old')").await?;
            let sql = "SELECT id, v FROM t";
            let old_result = s.query(sql).await?;
            let old = s.result_edit_plan(sql, &old_result).await?;
            // "reconnect": a new session; schema changes in between
            let s2 = Session::connect_test(&config(d, "qa041"), "secret").await?;
            match ddl {
                "" => {}
                "__gen" => {
                    s2.query("ALTER TABLE t DROP COLUMN v").await?;
                    s2.query(if d == Driver::Mysql { "ALTER TABLE t ADD COLUMN v varchar(20) GENERATED ALWAYS AS (CONCAT('g', id)) VIRTUAL" } else { "ALTER TABLE t ADD COLUMN v varchar(20) GENERATED ALWAYS AS ('g' || id) STORED" }).await?;
                }
                "__recreate" => {
                    s2.query("DROP TABLE t").await?;
                    s2.query("CREATE TABLE t (id int PRIMARY KEY, v varchar(20))")
                        .await?;
                    s2.query("INSERT INTO t VALUES (1,'old')").await?;
                }
                q => {
                    let _ = s2
                        .query(if d == Driver::Postgres && q.contains("DROP PRIMARY KEY") {
                            "ALTER TABLE t DROP CONSTRAINT t_pkey, ADD PRIMARY KEY (id, v)"
                        } else {
                            q
                        })
                        .await;
                }
            }
            let new = s2.result_edit_plan(sql, &old_result).await;
            let drift = new
                .as_ref()
                .map(|n| n.drift_from(&old))
                .map_err(|e| e.to_string());
            let same = matches!(&drift, Ok(None));
            eprintln!("[{d:?} {label}] re-derive same={same} drift={drift:?}");
            // R2-2: a renamed, re-keyed, generated or (MySQL) retyped column fails the re-check
            // with a reason. PostgreSQL keeps a retyped column's attribute (the save's own guard
            // covers it), and a recreated table there is a new OID.
            let expect_same = match label {
                "none" => true,
                "change-edited-type" => d == Driver::Postgres,
                "drop-recreate-same" => d == Driver::Mysql,
                _ => false,
            };
            if same != expect_same {
                bad.push(format!("{d:?} {label}: same={same}, drift={drift:?}"));
            }
            if label == "rename-edited-column"
                && d == Driver::Mysql
                && !matches!(&drift, Ok(Some(why)) if why.contains("no longer exists"))
            {
                bad.push(format!("{d:?} rename: unclear reason {drift:?}"));
            }
            drop(s);
            drop(s2);
        }
        if d == Driver::Postgres {
            admin.query("DROP DATABASE qa041 WITH (FORCE)").await?;
        } else {
            admin.query("DROP DATABASE qa041").await?;
        }
    }
    eprintln!("\nBAD-R2-REDERIVE: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}
