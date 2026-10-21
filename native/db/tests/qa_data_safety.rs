//! QA adversarial live probes (data safety) against the throwaway test containers only
//! (127.0.0.1:3307 / 127.0.0.1:5434, enforced by Session::connect_test).
//! Assertions encode the SAFE expectation; a failing test is a confirmed defect.
//! cargo test -p telecat-db --test qa_data_safety -- --test-threads=1 --nocapture
use anyhow::Result;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use telecat_core::{changes::*, types::*};
use telecat_db::Session;

async fn connect(d: Driver) -> Result<Session> {
    let pg = d == Driver::Postgres;
    let mut c = empty_connection(d);
    c.host = "127.0.0.1".into();
    c.port = if pg { 5434 } else { 3307 };
    c.user = if pg { "postgres" } else { "root" }.into();
    c.database = if pg { "main_db" } else { "zixma" }.into();
    Session::connect_test(&c, "secret").await
}
fn tref(d: Driver, name: &str) -> TableRef {
    TableRef {
        schema: if d == Driver::Mysql {
            "zixma"
        } else {
            "public"
        }
        .into(),
        name: name.into(),
    }
}
async fn rows(db: &Session, sql: &str) -> Vec<Vec<Option<String>>> {
    db.query(sql).await.unwrap().rows
}
async fn load(db: &Session, t: &TableRef) -> Result<TableDataResult> {
    db.table_data(&TableDataRequest {
        table: t.clone(),
        filters: vec![],
        combine: Combine::And,
        order_by: None,
        limit: 1000,
        offset: 0,
    })
    .await
}
/// Mirrors TableEditor::groups + execute_reviewed_part (transaction=true, no consent).
async fn stage_and_commit(
    db: &Session,
    t: &TableRef,
    edit: impl FnOnce(&mut Vec<EditRow>),
) -> Result<Vec<Stmt>> {
    let data = load(db, t).await?;
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    edit(&mut er);
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(db.driver, t, &meta, &er)?;
    for s in &st {
        println!("  SQL: {}  params={:?}", s.sql, s.params);
    }
    db.execute_after_review(&st, true, false).await?;
    Ok(st)
}
fn find(er: &[EditRow], col: usize, v: &str) -> usize {
    er.iter()
        .position(|r| r.orig.as_ref().unwrap()[col].as_deref() == Some(v))
        .unwrap_or_else(|| panic!("row with {v} not found"))
}

// ---------------------------------------------------------------- MySQL

#[tokio::test]
async fn qa_my_binary_hex_lookalike_pk_wrong_row() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_bin").await?;
    db.query("CREATE TABLE qads_bin (k VARBINARY(10) PRIMARY KEY, v INT)")
        .await?;
    db.query("INSERT INTO qads_bin VALUES ('A',1),('0x41',2)")
        .await?;
    let t = tref(Driver::Mysql, "qads_bin");
    let shown = load(&db, &t).await?;
    println!(
        "grid shows: {:?} binary={:?}",
        shown.result.rows, shown.result.binary
    );
    let r = stage_and_commit(&db, &t, |er| {
        let i = find(er, 0, "0x41");
        er[i].cur[1] = Cell::Text("99".into());
    })
    .await;
    println!("commit: {:?}", r.as_ref().map(|_| ()));
    let after = rows(&db, "SELECT k, v FROM qads_bin ORDER BY k").await;
    println!("after: {after:?}");
    db.query("DROP TABLE qads_bin").await?;
    assert_eq!(
        after,
        vec![
            vec![Some("0x41".into()), Some("99".into())],
            vec![Some("A".into()), Some("1".into())]
        ],
        "edit of row '0x41' must change that row, never row 'A'"
    );
    Ok(())
}

#[tokio::test]
async fn qa_my_binary_hex_lookalike_nopk_delete() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_bin2").await?;
    db.query("CREATE TABLE qads_bin2 (k VARBINARY(10), v INT)")
        .await?;
    db.query("INSERT INTO qads_bin2 VALUES ('A',1),('0x41',1)")
        .await?;
    let t = tref(Driver::Mysql, "qads_bin2");
    let r = stage_and_commit(&db, &t, |er| {
        let i = find(er, 0, "0x41");
        er[i].deleted = true;
    })
    .await;
    println!("commit: {:?}", r.as_ref().map(|_| ()));
    let after = rows(&db, "SELECT k FROM qads_bin2 ORDER BY k").await;
    println!("after: {after:?}");
    db.query("DROP TABLE qads_bin2").await?;
    assert_eq!(
        after,
        vec![vec![Some("A".into())]],
        "deleting row '0x41' deleted row 'A'"
    );
    Ok(())
}

#[tokio::test]
async fn qa_my_guard_bypasses_live() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let reset = || async {
        db.query("DROP TABLE IF EXISTS qads_g").await.unwrap();
        db.query("CREATE TABLE qads_g (id INT PRIMARY KEY, v INT)")
            .await
            .unwrap();
        db.query("INSERT INTO qads_g VALUES (1,0),(2,0),(3,0)")
            .await
            .unwrap();
        db.query("DROP TABLE IF EXISTS qads_g2").await.unwrap();
        db.query("CREATE TABLE qads_g2 (id INT PRIMARY KEY)")
            .await
            .unwrap();
        db.query("INSERT INTO qads_g2 VALUES (1)").await.unwrap();
    };
    let mut bad = vec![];
    for sql in [
        "SELECT 1 --1; DELETE FROM qads_g",
        "DELETE FROM qads_g WHERE id = 1 --1 OR 1=1",
        "DELETE FROM qads_g /*!99999 WHERE id = 1 */",
        "DESC ANALYZE DELETE g FROM qads_g g JOIN qads_g2 u ON 1=1",
        "DESCRIBE ANALYZE UPDATE qads_g g JOIN qads_g2 u ON 1=1 SET g.v = 7",
        "EXPLAIN ANALYZE DELETE g FROM qads_g g JOIN qads_g2 u ON 1=1",
        "DELETE FROM qads_g WHERE (1=1)",
    ] {
        reset().await;
        let r = db.query_checked(sql, false).await;
        let left = rows(&db, "SELECT COUNT(*), SUM(v) FROM qads_g").await;
        println!(
            "{sql:<65} -> {:<60} rows/sum after = {:?}",
            match &r {
                Ok(_) => "EXECUTED".to_string(),
                Err(e) => format!("ERR {}", &e.to_string()[..e.to_string().len().min(55)]),
            },
            left[0]
        );
        if left[0] != vec![Some("3".into()), Some("0".into())] {
            bad.push(sql);
        }
    }
    db.query("DROP TABLE qads_g").await?;
    db.query("DROP TABLE qads_g2").await?;
    println!("WROTE WITHOUT CONFIRMATION: {bad:#?}");
    assert!(bad.is_empty());
    Ok(())
}

#[tokio::test]
async fn qa_my_rollback_and_affected_rows() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_rb").await?;
    db.query("CREATE TABLE qads_rb (id INT PRIMARY KEY, v INT)")
        .await?;
    db.query("INSERT INTO qads_rb VALUES (1,0),(2,0),(3,5)")
        .await?;
    let up = |sql: &str, p: Vec<Value>| Stmt {
        sql: sql.into(),
        params: p,
        expect: Some(1),
        check: None,
    };
    // 1) same-value update: CLIENT_FOUND_ROWS -> 1 matched, no false "not found".
    let r = db
        .execute(
            &[up(
                "UPDATE `zixma`.`qads_rb` SET `v` = ? WHERE `id` = ?;",
                vec![json!("0"), json!("1")],
            )],
            true,
        )
        .await;
    println!(
        "same-value update: {:?}",
        r.as_ref().map(|v| v[0].affected_rows)
    );
    assert!(r.is_ok());
    // 2) stmt1 ok, stmt2 matches 0 -> whole commit rolled back.
    let r = db
        .execute(
            &[
                up(
                    "UPDATE `zixma`.`qads_rb` SET `v` = ? WHERE `id` = ?;",
                    vec![json!("9"), json!("1")],
                ),
                up(
                    "DELETE FROM `zixma`.`qads_rb` WHERE `id` = ?;",
                    vec![json!("2")],
                ),
                up(
                    "UPDATE `zixma`.`qads_rb` SET `v` = ? WHERE `id` = ?;",
                    vec![json!("9"), json!("404")],
                ),
            ],
            true,
        )
        .await;
    println!(
        "partial failure: {:?}",
        r.as_ref().err().map(|e| e.to_string())
    );
    assert!(r.is_err());
    // 3) a statement matching 2 rows with expect 1 -> rolled back.
    let r = db
        .execute(
            &[up(
                "UPDATE `zixma`.`qads_rb` SET `v` = ? WHERE `v` = ?;",
                vec![json!("8"), json!("0")],
            )],
            true,
        )
        .await;
    println!("over-match: {:?}", r.as_ref().err().map(|e| e.to_string()));
    assert!(r.is_err());
    // 4) server error mid-commit (duplicate key) rolls back earlier statements
    let r = db
        .execute(
            &[
                up(
                    "DELETE FROM `zixma`.`qads_rb` WHERE `id` = ?;",
                    vec![json!("3")],
                ),
                up(
                    "INSERT INTO `zixma`.`qads_rb` (`id`,`v`) VALUES (?, ?);",
                    vec![json!("1"), json!("1")],
                ),
            ],
            true,
        )
        .await;
    println!("dup key: {:?}", r.as_ref().err().map(|e| e.to_string()));
    let after = rows(&db, "SELECT id, v FROM qads_rb ORDER BY id").await;
    println!("after: {after:?}");
    db.query("DROP TABLE qads_rb").await?;
    assert_eq!(
        after,
        vec![
            vec![Some("1".into()), Some("0".into())],
            vec![Some("2".into()), Some("0".into())],
            vec![Some("3".into()), Some("5".into())]
        ]
    );
    Ok(())
}

#[tokio::test]
async fn qa_my_nopk_values_and_identifiers() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let tname = "qads_w`e\"ird.t\\b";
    let qt = format!("`zixma`.`{}`", tname.replace('`', "``"));
    db.query(&format!("DROP TABLE IF EXISTS {qt}")).await?;
    db.query(&format!(
        "CREATE TABLE {qt} (`c``1` VARCHAR(500) CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci, `c.2\\` DOUBLE, `c\"3` FLOAT, d DECIMAL(20,10), ts DATETIME(6), j JSON, b VARBINARY(20), n INT)"
    ))
    .await?;
    let long = "ก".repeat(400);
    for sql in [
        format!(
            "INSERT INTO {qt} VALUES ('a''b\\\\c', 0.1, 1.2345678, 1.1000000000, '2024-02-29 23:59:59.123456', '{{\"k\": [1, \"ไทย\"]}}', 0x00FF41, 1)"
        ),
        format!(
            "INSERT INTO {qt} VALUES ('A''B\\\\C', 0.1, 1.2345679, 1.1, '2024-02-29 23:59:59.123456', '{{\"k\": [1, \"ไทย\"]}}', 0x00FF41, 2)"
        ),
        format!("INSERT INTO {qt} VALUES ('😺 ', NULL, NULL, NULL, NULL, NULL, NULL, 3)"),
        format!("INSERT INTO {qt} VALUES ('😺', NULL, NULL, NULL, NULL, NULL, NULL, 4)"),
        format!("INSERT INTO {qt} VALUES ('{long}', 1e300, -0, 0, '1000-01-01', 'null', '', 5)"),
        format!("INSERT INTO {qt} VALUES ('nul\\0x', 2, 2, 2, '2024-01-01', '[]', 'x', 6)"),
    ] {
        db.query(&sql).await?;
    }
    let t = tref(Driver::Mysql, tname);
    let mut outcomes = vec![];
    for n in 1..=6 {
        let before = rows(&db, &format!("SELECT n FROM {qt} ORDER BY n")).await;
        let r = stage_and_commit(&db, &t, |er| {
            let i = find(er, 7, &n.to_string());
            er[i].cur[7] = Cell::Text((n * 100).to_string());
        })
        .await;
        let after = rows(&db, &format!("SELECT n FROM {qt} ORDER BY n")).await;
        let changed: Vec<_> = after.iter().filter(|a| !before.contains(a)).collect();
        println!(
            "row n={n}: {:?}; after={after:?}",
            r.as_ref().err().map(|e| e.to_string())
        );
        outcomes.push((n, r.is_ok(), after.len(), changed.len()));
        // Put it back for the next iteration.
        db.query(&format!("UPDATE {qt} SET n = {n} WHERE n = {}", n * 100))
            .await?;
    }
    db.query(&format!("DROP TABLE {qt}")).await?;
    println!("outcomes (n, ok, rows, changed): {outcomes:?}");
    for (n, ok, len, changed) in outcomes {
        assert_eq!(len, 6);
        assert!(changed <= 1, "row {n} edit changed {changed} rows");
        if ok {
            assert_eq!(changed, 1);
        }
    }
    Ok(())
}

#[tokio::test]
async fn qa_my_nopk_duplicates_refused() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_dup").await?;
    db.query("CREATE TABLE qads_dup (a INT, b VARCHAR(10))")
        .await?;
    db.query("INSERT INTO qads_dup VALUES (1,'x'),(1,'x'),(NULL,NULL),(NULL,NULL),(2,'a'),(2,'A'),(3,'a'),(3,'a ')").await?;
    let t = tref(Driver::Mysql, "qads_dup");
    let mut res = vec![];
    for (a, b) in [
        ("1", Some("x")),
        ("", None),
        ("2", Some("a")),
        ("3", Some("a")),
    ] {
        let r = stage_and_commit(&db, &t, |er| {
            let i = er
                .iter()
                .position(|r| {
                    let o = r.orig.as_ref().unwrap();
                    o[0].as_deref().unwrap_or("") == a && o[1].as_deref() == b
                })
                .unwrap();
            er[i].deleted = true;
        })
        .await;
        res.push((a, b, r.map(|_| ()).map_err(|e| e.to_string())));
    }
    let left = rows(&db, "SELECT COUNT(*) FROM qads_dup").await;
    println!("{res:#?}\nleft={left:?}");
    db.query("DROP TABLE qads_dup").await?;
    // duplicates refused (2 deletions refused), 'a' vs 'A' and 'a' vs 'a ' distinguished (2 deletes OK)
    assert_eq!(left[0][0].as_deref(), Some("6"));
    Ok(())
}

#[tokio::test]
async fn qa_my_myisam_paths() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_isam").await?;
    db.query(
        "CREATE TABLE qads_isam (id INT PRIMARY KEY, `we\\ird?` VARCHAR(10), v INT) ENGINE=MyISAM",
    )
    .await?;
    db.query("INSERT INTO qads_isam VALUES (1,'a',0),(2,'b',0)")
        .await?;
    let t = tref(Driver::Mysql, "qads_isam");
    let data = load(&db, &t).await?;
    let mut er = to_edit_rows(&data.result.rows, None);
    er[0].cur[1] = Cell::Text("zz".into());
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(Driver::Mysql, &t, &meta, &er)?;
    println!("{:?}", st);
    let no_consent = db.execute_after_review(&st, true, false).await;
    println!(
        "without consent: {:?}",
        no_consent.as_ref().err().map(|e| e.to_string())
    );
    assert!(no_consent.is_err());
    assert_eq!(
        rows(&db, "SELECT `we\\ird?` FROM qads_isam WHERE id=1").await[0][0].as_deref(),
        Some("a")
    );
    let with = db.execute_after_review(&st, true, true).await;
    println!(
        "with consent: {:?}",
        with.as_ref().err().map(|e| e.to_string())
    );
    let after = rows(&db, "SELECT id, `we\\ird?`, v FROM qads_isam ORDER BY id").await;
    println!("after: {after:?}");
    db.query("DROP TABLE qads_isam").await?;
    assert!(with.is_ok(), "valid single-row MyISAM edit refused");
    Ok(())
}

#[tokio::test]
async fn qa_my_kill_admin_mid_commit() -> Result<()> {
    let db = Arc::new(connect(Driver::Mysql).await?);
    let locker = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_kill").await?;
    db.query("CREATE TABLE qads_kill (id INT PRIMARY KEY, v INT)")
        .await?;
    db.query("INSERT INTO qads_kill VALUES (1,0),(2,0)").await?;
    // Lock row 2 so statement 2 of the commit blocks after statement 1 has executed.
    locker.query("START TRANSACTION").await?;
    locker
        .query("SELECT * FROM qads_kill WHERE id = 2 FOR UPDATE")
        .await?;
    let st = vec![
        Stmt {
            sql: "UPDATE `zixma`.`qads_kill` SET `v` = ? WHERE `id` = ?;".into(),
            params: vec![json!("1"), json!("1")],
            expect: Some(1),
            check: None,
        },
        Stmt {
            sql: "UPDATE `zixma`.`qads_kill` SET `v` = ? WHERE `id` = ?;".into(),
            params: vec![json!("1"), json!("2")],
            expect: Some(1),
            check: None,
        },
    ];
    let d2 = db.clone();
    let h = tokio::spawn(async move { d2.execute(&st, true).await });
    tokio::time::sleep(Duration::from_millis(700)).await;
    let pl = rows(&locker, "SELECT ID FROM information_schema.PROCESSLIST WHERE INFO LIKE 'UPDATE `zixma`.`qads_kill`%'").await;
    println!("blocked admin conn: {pl:?}");
    let id = pl[0][0].clone().unwrap();
    locker.query(&format!("KILL {id}")).await?;
    let r = h.await?;
    println!(
        "commit result: {:?}",
        r.as_ref().err().map(|e| format!("{e:#}"))
    );
    locker.query("ROLLBACK").await?;
    let after = rows(&locker, "SELECT id, v FROM qads_kill ORDER BY id").await;
    println!("after: {after:?}");
    let next = db.execute(&[Stmt::new("SELECT 1")], true).await;
    println!(
        "next execute: {:?}",
        next.as_ref().err().map(|e| e.to_string())
    );
    db.query("DROP TABLE qads_kill").await?;
    assert!(r.is_err());
    assert_eq!(
        after,
        vec![
            vec![Some("1".into()), Some("0".into())],
            vec![Some("2".into()), Some("0".into())]
        ]
    );
    Ok(())
}

#[tokio::test]
async fn qa_my_raw_filter_and_filter_values() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_f").await?;
    db.query("CREATE TABLE qads_f (id INT PRIMARY KEY, s VARCHAR(60))")
        .await?;
    db.query("INSERT INTO qads_f VALUES (1,'50%_off'),(2,'50xyoff'),(3,'a\\\\b'),(4,'x''); DROP TABLE qads_f; --')").await?;
    let t = tref(Driver::Mysql, "qads_f");
    let req = |op: FilterOp, col: &str, v: &str| TableDataRequest {
        table: t.clone(),
        filters: vec![Filter {
            enabled: true,
            column: col.into(),
            op,
            value: v.into(),
            value2: None,
        }],
        combine: Combine::Or,
        order_by: None,
        limit: 100,
        offset: 0,
    };
    for (op, col, v) in [
        (FilterOp::Contains, "s", "%_"),
        (FilterOp::Contains, "s", "\\"),
        (FilterOp::Contains, "", "'); DROP TABLE qads_f; --"),
        (FilterOp::Contains, "s; DROP TABLE qads_f", "x"),
        (FilterOp::Like, "s", "50%"),
        (FilterOp::In, "id", "1, 2,,"),
        (FilterOp::In, "id", " , "),
    ] {
        let r = db.table_data(&req(op, col, v)).await;
        println!(
            "{op:?} {col:?} {v:?} -> {:?}",
            r.map(|r| r
                .result
                .rows
                .iter()
                .map(|r| r[0].clone())
                .collect::<Vec<_>>())
                .map_err(|e| e.to_string())
        );
    }
    let raw = db
        .table_data(&req(
            FilterOp::Eq,
            RAW_SQL,
            "1=1) --1; DROP TABLE qads_f; SELECT (1",
        ))
        .await;
    println!(
        "raw multi-stmt filter: {:?}",
        raw.map(|r| r.total).map_err(|e| e.to_string())
    );
    let exists = rows(&db, "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='zixma' AND table_name='qads_f'").await;
    db.query("DROP TABLE IF EXISTS qads_f").await?;
    assert_eq!(exists[0][0].as_deref(), Some("1"));
    Ok(())
}

// ---------------------------------------------------------------- PostgreSQL

#[tokio::test]
async fn qa_pg_stale_ctid_nopk_wrong_row() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    let other = connect(Driver::Postgres).await?;
    db.query("DROP TABLE IF EXISTS qads_np").await?;
    db.query("CREATE TABLE qads_np (name text, balance int)")
        .await?;
    db.query("INSERT INTO qads_np VALUES ('alice', 10), ('bob', 20)")
        .await?;
    let t = tref(Driver::Postgres, "qads_np");
    // User opens the table (grid has tableoid+ctid for each row)...
    let data = load(&db, &t).await?;
    println!("loaded rows={:?} rids={:?}", data.result.rows, data.row_ids);
    // ...meanwhile a nightly job reloads the table (TRUNCATE keeps the table OID).
    other.query("TRUNCATE qads_np").await?;
    other
        .query("INSERT INTO qads_np VALUES ('carol', 999), ('dave', 5)")
        .await?;
    // User deletes 'alice' and presses Cmd+S.
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    let i = find(&er, 0, "alice");
    er[i].deleted = true;
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(Driver::Postgres, &t, &meta, &er)?;
    println!("SQL: {} {:?}", st[0].sql, st[0].params);
    let r = db.execute_after_review(&st, true, false).await;
    println!(
        "commit: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    let after = rows(&db, "SELECT name, balance FROM qads_np ORDER BY name").await;
    println!("after: {after:?}");
    db.query("DROP TABLE qads_np").await?;
    assert_eq!(
        after.len(),
        2,
        "deleting stale row 'alice' deleted a different, unrelated row"
    );
    Ok(())
}

#[tokio::test]
async fn qa_pg_stale_ctid_nopk_update_wrong_row() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    let other = connect(Driver::Postgres).await?;
    db.query("DROP TABLE IF EXISTS qads_np2").await?;
    db.query("CREATE TABLE qads_np2 (name text, balance int)")
        .await?;
    db.query("INSERT INTO qads_np2 VALUES ('alice', 10), ('bob', 20)")
        .await?;
    let t = tref(Driver::Postgres, "qads_np2");
    let data = load(&db, &t).await?;
    // DELETE + VACUUM frees the slots, the next insert reuses ctid (0,1).
    other.query("DELETE FROM qads_np2").await?;
    other.query("VACUUM qads_np2").await?;
    other
        .query("INSERT INTO qads_np2 VALUES ('carol', 999)")
        .await?;
    println!(
        "new ctids: {:?}",
        rows(&other, "SELECT ctid::text, name FROM qads_np2").await
    );
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    let i = find(&er, 0, "alice");
    er[i].cur[1] = Cell::Text("0".into());
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(Driver::Postgres, &t, &meta, &er)?;
    let r = db.execute_after_review(&st, true, false).await;
    println!(
        "commit: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    let after = rows(&db, "SELECT name, balance FROM qads_np2 ORDER BY name").await;
    println!("after: {after:?}");
    db.query("DROP TABLE qads_np2").await?;
    assert_eq!(
        after,
        vec![vec![Some("carol".into()), Some("999".into())]],
        "edit of stale row overwrote another row"
    );
    Ok(())
}

#[tokio::test]
async fn qa_pg_guard_bypasses_live() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    let mut bad = vec![];
    for sql in [
        "EXPLAIN ANALYZE VERBOSE DELETE FROM qads_pg",
        "EXPLAIN (ANALYZE) DELETE FROM qads_pg",
        "EXPLAIN (ANALYZE, VERBOSE) DELETE FROM qads_pg",
        "WITH d AS (DELETE FROM qads_pg RETURNING *) SELECT * FROM d WHERE false",
        "DELETE FROM qads_pg WHERE $$x$$ = $$x$$ OR TRUE",
        "DELETE FROM qads_pg WHERE (1=1)",
        "SELECT 1; DELETE FROM qads_pg",
        "SELECT 1 --; \nDELETE FROM qads_pg",
    ] {
        db.query("DROP TABLE IF EXISTS qads_pg").await?;
        db.query("CREATE TABLE qads_pg (id int PRIMARY KEY)")
            .await?;
        db.query("INSERT INTO qads_pg VALUES (1),(2),(3)").await?;
        let r = db.query_checked(sql, false).await;
        let left = rows(&db, "SELECT COUNT(*) FROM qads_pg").await;
        println!(
            "{:<70} -> {:<55} rows after = {:?}",
            sql.replace('\n', "\\n"),
            match &r {
                Ok(_) => "EXECUTED".to_string(),
                Err(e) => format!("ERR {}", &e.to_string()[..e.to_string().len().min(50)]),
            },
            left[0][0]
        );
        if left[0][0].as_deref() != Some("3") {
            bad.push(sql);
        }
    }
    db.query("DROP TABLE qads_pg").await?;
    println!("WROTE WITHOUT CONFIRMATION: {bad:#?}");
    assert!(bad.is_empty());
    Ok(())
}

#[tokio::test]
async fn qa_pg_rollback_view_and_identifiers() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    db.query("DROP SCHEMA IF EXISTS \"qads s.\"\"x\" CASCADE")
        .await?;
    db.query("CREATE SCHEMA \"qads s.\"\"x\"").await?;
    db.query("CREATE TABLE \"qads s.\"\"x\".\"t$1.\"\"q\" (\"i\"\"d\" int PRIMARY KEY, \"v$2\" text, f float8, ts timestamptz, j jsonb, b bytea)").await?;
    db.query("INSERT INTO \"qads s.\"\"x\".\"t$1.\"\"q\" VALUES (1,'ไทย''😺\\', 0.1, '2024-03-10 02:30:00+00', '{\"a\":1}', '\\x00ff'), (2,'b', 0.30000000000000004, now(), '[]', '\\x5c783431')").await?;
    db.query(
        "CREATE VIEW \"qads s.\"\"x\".vw AS SELECT \"v$2\", f FROM \"qads s.\"\"x\".\"t$1.\"\"q\"",
    )
    .await?;
    db.query("CREATE TABLE \"qads s.\"\"x\".dup (a int, b text)")
        .await?;
    db.query("INSERT INTO \"qads s.\"\"x\".dup VALUES (1,'x'),(1,'x')")
        .await?;
    db.query("CREATE VIEW \"qads s.\"\"x\".dupv AS SELECT a, b FROM \"qads s.\"\"x\".dup")
        .await?;
    let schema = "qads s.\"x";
    let t = TableRef {
        schema: schema.into(),
        name: "t$1.\"q".into(),
    };
    let r1 = stage_and_commit(&db, &t, |er| {
        er[0].cur[1] = Cell::Text("changed".into());
        er[1].deleted = true;
    })
    .await;
    println!(
        "pk table edit+delete: {:?}",
        r1.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    // view, no PK, no rid -> ::text path + COUNT check
    let v = TableRef {
        schema: schema.into(),
        name: "vw".into(),
    };
    let r2 = stage_and_commit(&db, &v, |er| {
        er[0].cur[0] = Cell::Text("via view".into());
    })
    .await;
    println!(
        "view edit: {:?}",
        r2.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    let dv = TableRef {
        schema: schema.into(),
        name: "dupv".into(),
    };
    let r3 = stage_and_commit(&db, &dv, |er| er[0].deleted = true).await;
    println!(
        "dup view delete: {:?}",
        r3.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    // rollback: stmt 2 fails
    let q = "\"qads s.\"\"x\".\"t$1.\"\"q\"";
    let rb = db
        .execute(
            &[
                Stmt {
                    sql: format!("UPDATE {q} SET \"v$2\" = $1 WHERE \"i\"\"d\" = $2;"),
                    params: vec![json!("ROLLME"), json!("1")],
                    expect: Some(1),
                    check: None,
                },
                Stmt {
                    sql: format!("DELETE FROM {q} WHERE \"i\"\"d\" = $1;"),
                    params: vec![json!("404")],
                    expect: Some(1),
                    check: None,
                },
            ],
            true,
        )
        .await;
    println!("rollback: {:?}", rb.as_ref().err().map(|e| e.to_string()));
    // over-match: 2 rows match an expect-1 statement -> error, nothing changed
    db.query(&format!(
        "INSERT INTO {q} (\"i\"\"d\", \"v$2\") VALUES (10,'same'),(11,'same')"
    ))
    .await?;
    let om = db
        .execute(
            &[Stmt {
                sql: format!("UPDATE {q} SET \"v$2\" = $1 WHERE \"v$2\" = $2;"),
                params: vec![json!("OVER"), json!("same")],
                expect: Some(1),
                check: None,
            }],
            true,
        )
        .await;
    println!(
        "pg over-match: {:?}",
        om.as_ref().err().map(|e| e.to_string())
    );
    let over = rows(
        &db,
        &format!("SELECT COUNT(*) FROM {q} WHERE \"v$2\" = 'OVER'"),
    )
    .await;
    assert!(om.is_err());
    assert_eq!(over[0][0].as_deref(), Some("0"));
    db.query(&format!("DELETE FROM {q} WHERE \"i\"\"d\" IN (10,11)"))
        .await?;
    let after = rows(
        &db,
        &format!("SELECT \"i\"\"d\", \"v$2\" FROM {q} ORDER BY 1"),
    )
    .await;
    let dup = rows(&db, "SELECT COUNT(*) FROM \"qads s.\"\"x\".dup").await;
    println!("after={after:?} dup={dup:?}");
    // Preview text (what the Safe Mode gate and log show) for a $n-looking identifier:
    if let Ok(st) = &r1 {
        for s in st {
            println!("preview: {}", inline_params(Driver::Postgres, s));
        }
    }
    db.query("DROP SCHEMA \"qads s.\"\"x\" CASCADE").await?;
    assert!(r1.is_ok() && r2.is_ok() && r3.is_err() && rb.is_err());
    assert_eq!(after, vec![vec![Some("1".into()), Some("via view".into())]]);
    assert_eq!(dup[0][0].as_deref(), Some("2"));
    Ok(())
}

#[tokio::test]
async fn qa_pg_raw_filter_side_effects() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    db.query("DROP TABLE IF EXISTS qads_rf, qads_rf_log")
        .await?;
    db.query("CREATE TABLE qads_rf (id int PRIMARY KEY)")
        .await?;
    db.query("INSERT INTO qads_rf VALUES (1)").await?;
    db.query("CREATE TABLE qads_rf_log (n int)").await?;
    db.query("CREATE OR REPLACE FUNCTION qads_rf_fn() RETURNS bool LANGUAGE sql VOLATILE AS 'INSERT INTO qads_rf_log VALUES (1); SELECT true'").await?;
    let r = db
        .table_data(&TableDataRequest {
            table: tref(Driver::Postgres, "qads_rf"),
            filters: vec![Filter {
                enabled: true,
                column: RAW_SQL.into(),
                op: FilterOp::Eq,
                value: "qads_rf_fn()".into(),
                value2: None,
            }],
            combine: Combine::And,
            order_by: None,
            limit: 10,
            offset: 0,
        })
        .await;
    let n = rows(&db, "SELECT COUNT(*) FROM qads_rf_log").await;
    println!(
        "raw filter result: {:?}; side-effect writes: {:?}",
        r.map(|r| r.total).map_err(|e| e.to_string()),
        n
    );
    db.query("DROP TABLE qads_rf, qads_rf_log").await?;
    db.query("DROP FUNCTION qads_rf_fn()").await?;
    Ok(())
}

#[tokio::test]
async fn qa_my_timestamp_pk_dst_ambiguity() -> Result<()> {
    // Simulates a production server whose time_zone is a DST zone (e.g. SYSTEM = America/New_York).
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_ts").await?;
    db.query("CREATE TABLE qads_ts (ts TIMESTAMP PRIMARY KEY, v INT)")
        .await?;
    db.query("SET time_zone = '+00:00'").await?;
    db.query("INSERT INTO qads_ts VALUES ('2024-11-03 05:30:00', 1), ('2024-11-03 06:30:00', 2)")
        .await?;
    db.query("SET time_zone = 'America/New_York'").await?;
    db.execute(&[Stmt::new("SET time_zone = 'America/New_York'")], false)
        .await?;
    let t = tref(Driver::Mysql, "qads_ts");
    println!("grid shows: {:?}", load(&db, &t).await?.result.rows);
    let r = stage_and_commit(&db, &t, |er| {
        let i = find(er, 1, "2");
        er[i].cur[1] = Cell::Text("222".into());
    })
    .await;
    println!(
        "commit: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    db.query("SET time_zone = '+00:00'").await?;
    let after = rows(&db, "SELECT ts, v FROM qads_ts ORDER BY ts").await;
    println!("after (UTC): {after:?}");
    db.execute(&[Stmt::new("SET time_zone = SYSTEM")], false)
        .await?;
    db.query("DROP TABLE qads_ts").await?;
    assert_eq!(
        after[0][1].as_deref(),
        Some("1"),
        "edit of the 06:30 UTC row changed the 05:30 UTC row"
    );
    Ok(())
}

#[tokio::test]
async fn qa_my_invisible_pk() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads_gipk").await?;
    db.query("CREATE TABLE qads_gipk (my_row_id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT INVISIBLE PRIMARY KEY, v INT)").await?;
    db.query("INSERT INTO qads_gipk (v) VALUES (1),(1)").await?;
    let t = tref(Driver::Mysql, "qads_gipk");
    let r = stage_and_commit(&db, &t, |er| er[0].deleted = true).await;
    println!(
        "invisible-PK delete: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    let left = rows(&db, "SELECT COUNT(*) FROM qads_gipk").await;
    db.query("DROP TABLE qads_gipk").await?;
    assert_eq!(
        left[0][0].as_deref(),
        Some(if r.is_ok() { "1" } else { "2" })
    );
    Ok(())
}
