//! QA final-round live probes (127.0.0.1:3307 / :5434 only, via connect_test). Scratch prefix qads3_.
//! cargo test -p telecat-db --test qa_final -- --test-threads=1 --nocapture
use anyhow::Result;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
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
fn req(t: &TableRef, filters: Vec<Filter>) -> TableDataRequest {
    TableDataRequest {
        table: t.clone(),
        filters,
        combine: Combine::And,
        order_by: None,
        limit: 1000,
        offset: 0,
    }
}
fn raw(v: &str) -> Vec<Filter> {
    vec![Filter {
        enabled: true,
        column: RAW_SQL.into(),
        op: FilterOp::Eq,
        value: v.into(),
        value2: None,
    }]
}
async fn stage(
    db: &Session,
    t: &TableRef,
    edit: impl FnOnce(&mut Vec<EditRow>, &[String]),
) -> Result<Vec<Stmt>> {
    let data = db.table_data(&req(t, vec![])).await?;
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    edit(&mut er, &data.result.columns);
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    Ok(data_statements(db.driver, t, &meta, &er)?)
}
async fn commit(
    db: &Session,
    t: &TableRef,
    edit: impl FnOnce(&mut Vec<EditRow>, &[String]),
) -> Result<()> {
    let st = stage(db, t, edit).await?;
    db.execute_after_review(&st, true, false).await?;
    Ok(())
}
fn ci(cols: &[String], n: &str) -> usize {
    cols.iter().position(|c| c == n).unwrap()
}
fn find(er: &[EditRow], col: usize, v: Option<&str>) -> usize {
    er.iter()
        .position(|r| r.orig.as_ref().unwrap()[col].as_deref() == v)
        .unwrap_or_else(|| panic!("no row {v:?}"))
}
fn e(r: &Result<()>) -> String {
    match r {
        Ok(_) => "OK".into(),
        Err(e) => format!("ERR {}", e.to_string().chars().take(80).collect::<String>()),
    }
}

#[tokio::test]
async fn qa3_guard_residuals_live() -> Result<()> {
    let cases = [
        (
            Driver::Mysql,
            "DELETE FROM qads3_g WHERE id = 1 --\u{1}OR 1=1",
        ),
        (
            Driver::Mysql,
            "DELETE FROM qads3_g WHERE id = 1 --\u{a0}OR 1=1",
        ),
        (
            Driver::Mysql,
            "DELETE FROM qads3_g WHERE @qads3_unset IS NULL",
        ),
        (
            Driver::Mysql,
            "DELETE FROM qads3_g WHERE CAST('1' AS SIGNED)",
        ),
        (
            Driver::Mysql,
            "DELETE FROM qads3_g WHERE CONVERT('a' USING utf8mb4) = 'a'",
        ),
        (Driver::Mysql, "DELETE FROM qads3_g WHERE id = id + 0"),
        (
            Driver::Mysql,
            "DELETE FROM qads3_g WHERE id IN (SELECT id FROM (SELECT id FROM qads3_g) x)",
        ),
        (
            Driver::Mysql,
            "DELETE FROM qads3_g WHERE (id = 1) IS NOT NULL",
        ),
        (
            Driver::Mysql,
            "DELETE FROM qads3_g AS a\u{2028}WHERE\u{2028}b",
        ),
        (Driver::Mysql, "DELETE FROM qads3_g AS a\u{85}WHERE\u{85}b"),
        (
            Driver::Postgres,
            "DELETE FROM qads3_g WHERE U&'\\0061' <> 'b'",
        ),
        (Driver::Postgres, "DELETE FROM qads3_g WHERE 1_000 <> 2"),
        (
            Driver::Postgres,
            "DELETE FROM qads3_g WHERE current_date > DATE '2000-01-01'",
        ),
        (Driver::Postgres, "DELETE FROM qads3_g WHERE id = id + 0"),
        (
            Driver::Postgres,
            "DELETE FROM qads3_g WHERE EXISTS (SELECT 1 FROM qads3_g AS x WHERE x.id > 0)",
        ),
        (
            Driver::Postgres,
            "DELETE FROM qads3_g AS a\u{200b}WHERE\u{200b}b",
        ),
        (
            Driver::Postgres,
            "DELETE FROM qads3_g WHERE id = 1 --\u{a0}x\nOR 1=1",
        ),
    ];
    let mut wrote = vec![];
    for (d, sql) in cases {
        let db = connect(d).await?;
        db.query("DROP TABLE IF EXISTS qads3_g").await?;
        db.query("CREATE TABLE qads3_g (id int PRIMARY KEY, v int)")
            .await?;
        db.query("INSERT INTO qads3_g VALUES (1,0),(2,0),(3,0)")
            .await?;
        let r = db.query_checked(sql, false).await.map(|_| ());
        let left = rows(&db, "SELECT COUNT(*) FROM qads3_g").await[0][0].clone();
        println!(
            "{d:?} {:<72} -> {:<70} rows={left:?}",
            sql.escape_debug().to_string(),
            e(&r)
        );
        if left.as_deref() != Some("3") {
            wrote.push(format!("{d:?}: {}", sql.escape_debug()));
        }
        db.query("DROP TABLE qads3_g").await?;
    }
    println!(
        "WROTE WITHOUT CONFIRMATION ({}):\n{}",
        wrote.len(),
        wrote.join("\n")
    );
    Ok(())
}

#[tokio::test]
async fn qa3_mysql_keys() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let mut out = vec![];
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        // name, ddl, inserts, key text of row to edit (as displayed)
        (
            "ubig",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(9007199254740992,0),(9007199254740993,0),(18446744073709551615,0),(9223372036854775808,0)",
            "9007199254740993",
        ),
        (
            "ubig_max",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(18446744073709551614,0),(18446744073709551615,0)",
            "18446744073709551615",
        ),
        (
            "sbig",
            "k BIGINT PRIMARY KEY, v INT",
            "(-9223372036854775808,0),(-9223372036854775807,0)",
            "-9223372036854775808",
        ),
        (
            "dec",
            "k DECIMAL(30,10) PRIMARY KEY, v INT",
            "(12345678901234567890.0000000001,0),(12345678901234567890.0000000002,0)",
            "12345678901234567890.0000000002",
        ),
        (
            "dec0",
            "k DECIMAL(5,2) PRIMARY KEY, v INT",
            "(1.10,0),(1.1,0) ON DUPLICATE KEY UPDATE v=v",
            "1.10",
        ),
        (
            "comp",
            "a INT, b VARCHAR(5), c DATE, v INT, PRIMARY KEY (a,b,c)",
            "(1,'x','2024-01-01',0),(1,'x','2024-01-02',0),(1,'X ','2024-01-01',0) ON DUPLICATE KEY UPDATE v=v",
            "1",
        ),
        (
            "ci",
            "k VARCHAR(10) COLLATE utf8mb4_0900_ai_ci PRIMARY KEY, v INT",
            "('a',0),('b',0)",
            "a",
        ),
        (
            "bin_ci",
            "k VARCHAR(10) COLLATE utf8mb4_bin PRIMARY KEY, v INT",
            "('a',0),('A',0)",
            "A",
        ),
        (
            "accent",
            "k VARCHAR(10) COLLATE utf8mb4_0900_as_cs PRIMARY KEY, v INT",
            "('e',0),('é',0)",
            "é",
        ),
        (
            "pad",
            "k VARCHAR(10) COLLATE utf8mb4_general_ci PRIMARY KEY, v INT",
            "('a',0),('b ',0)",
            "b ",
        ),
        (
            "double",
            "k DOUBLE PRIMARY KEY, v INT",
            "(0.1,0),(0.30000000000000004,0),(0.3,0)",
            "0.30000000000000004",
        ),
        (
            "bit",
            "k BIT(64) PRIMARY KEY, v INT",
            "(b'1',0),(18446744073709551615,0)",
            "",
        ),
        (
            "datetime6",
            "k DATETIME(6) PRIMARY KEY, v INT",
            "('2024-01-01 00:00:00.000001',0),('2024-01-01 00:00:00',0)",
            "2024-01-01 00:00:00.000001",
        ),
        (
            "year",
            "k YEAR PRIMARY KEY, v INT",
            "(1901,0),(2155,0)",
            "2155",
        ),
        (
            "blobprefix",
            "k BLOB, v INT, PRIMARY KEY (k(4))",
            "('abcd1',0),('abce1',0)",
            "abce1",
        ),
    ];
    for (name, ddl, ins, key) in cases {
        let tn = format!("qads3_k_{name}");
        db.query(&format!("DROP TABLE IF EXISTS {tn}")).await?;
        db.query(&format!("CREATE TABLE {tn} ({ddl})")).await?;
        db.query(&format!("INSERT INTO {tn} VALUES {ins}")).await?;
        let t = tref(Driver::Mysql, &tn);
        let before = rows(
            &db,
            &format!("SELECT HEX(CAST(v AS CHAR)), COUNT(*) FROM {tn} GROUP BY v"),
        )
        .await;
        let r = commit(&db, &t, |er, cols| {
            let i = if key.is_empty() {
                er.len() - 1
            } else {
                find(er, 0, Some(key))
            };
            er[i].cur[ci(cols, "v")] = Cell::Text("7".into());
        })
        .await;
        let changed =
            rows(&db, &format!("SELECT COUNT(*) FROM {tn} WHERE v = 7")).await[0][0].clone();
        let d = commit(&db, &t, |er, _| {
            er[0].deleted = true;
        })
        .await;
        let left = rows(&db, &format!("SELECT COUNT(*) FROM {tn}")).await[0][0].clone();
        println!(
            "{name:<11} edit {key:<28} {:<40} changed={changed:?}  delete-first {:<20} left={left:?} (before {before:?})",
            e(&r),
            e(&d)
        );
        out.push((name, r.is_ok(), changed));
        db.query(&format!("DROP TABLE {tn}")).await?;
    }
    for (n, ok, ch) in &out {
        assert!(!ok || ch.as_deref() == Some("1"), "{n} changed {ch:?}");
    }
    Ok(())
}

#[tokio::test]
async fn qa3_mysql_ci_key_concurrent_case_change() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let other = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads3_cc").await?;
    db.query("CREATE TABLE qads3_cc (k VARCHAR(10) COLLATE utf8mb4_0900_ai_ci PRIMARY KEY, v INT)")
        .await?;
    db.query("INSERT INTO qads3_cc VALUES ('a',0)").await?;
    let t = tref(Driver::Mysql, "qads3_cc");
    let st = stage(&db, &t, |er, _| er[0].cur[1] = Cell::Text("5".into())).await?;
    other
        .query("UPDATE qads3_cc SET k = 'A' WHERE k = 'a'")
        .await?;
    let r = db.execute_after_review(&st, true, false).await.map(|_| ());
    println!(
        "edit after concurrent key case change a->A: {} ; after {:?}",
        e(&r),
        rows(&db, "SELECT k, v FROM qads3_cc").await
    );
    db.query("DROP TABLE qads3_cc").await?;
    Ok(())
}

#[tokio::test]
async fn qa3_regression_crud() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        for (tn, ddl) in [
            (
                "qads3_pk",
                "id int PRIMARY KEY, s varchar(20), n int DEFAULT 5",
            ),
            ("qads3_nopk", "id int, s varchar(20), n int DEFAULT 5"),
        ] {
            db.query(&format!("DROP TABLE IF EXISTS {tn}")).await?;
            db.query(&format!("CREATE TABLE {tn} ({ddl})")).await?;
            db.query(&format!(
                "INSERT INTO {tn} VALUES (1,'a',1),(2,'b',2),(3,NULL,3)"
            ))
            .await?;
            let t = tref(d, tn);
            let r1 = commit(&db, &t, |er, _| {
                {
                    let i = find(er, 0, Some("1"));
                    &mut er[i]
                }
                .cur[1] = Cell::Text("ไทย 'q' \\ 😺".into());
                {
                    let i = find(er, 0, Some("3"));
                    &mut er[i]
                }
                .cur[1] = Cell::Text("".into());
                {
                    let i = find(er, 0, Some("2"));
                    &mut er[i]
                }
                .deleted = true;
                er.push(EditRow {
                    key: row_key(),
                    orig: None,
                    cur: vec![Cell::Text("4".into()), Cell::Null, Cell::unset()],
                    deleted: false,
                    rid: None,
                });
            })
            .await;
            let after = rows(&db, &format!("SELECT id, s, n FROM {tn} ORDER BY id")).await;
            // second round on freshly loaded data: edit the inserted row, set NULL
            let r2 = commit(&db, &t, |er, _| {
                {
                    let i = find(er, 0, Some("4"));
                    &mut er[i]
                }
                .cur[1] = Cell::Text("x".into());
                {
                    let i = find(er, 0, Some("1"));
                    &mut er[i]
                }
                .cur[1] = Cell::Null;
            })
            .await;
            let after2 = rows(&db, &format!("SELECT id, s, n FROM {tn} ORDER BY id")).await;
            println!(
                "{d:?} {tn:<11} round1 {} {after:?}\n                  round2 {} {after2:?}",
                e(&r1),
                e(&r2)
            );
            assert!(r1.is_ok() && r2.is_ok());
            assert_eq!(after2.len(), 3);
            db.query(&format!("DROP TABLE {tn}")).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn qa3_reader_connection() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = Arc::new(connect(d).await?);
        let pg = d == Driver::Postgres;
        db.query("DROP TABLE IF EXISTS qads3_r").await?;
        db.query("CREATE TABLE qads3_r (id int PRIMARY KEY, v int)")
            .await?;
        db.query("INSERT INTO qads3_r VALUES (1,0)").await?;
        let t = tref(d, "qads3_r");
        // 1) which connection ids exist; does a plain (non-raw) read write anything?
        let procs = |db: Arc<Session>| async move {
            rows(&db, if pg { "SELECT pid::text, state, left(query, 80) FROM pg_stat_activity WHERE application_name = 'Telecat' AND datname = 'main_db'" } else { "SELECT ID, COMMAND, LEFT(INFO, 80) FROM information_schema.PROCESSLIST WHERE DB = 'zixma' AND USER = 'root'" }).await
        };
        let _ = db.table_data(&req(&t, vec![])).await?;
        println!(
            "{d:?} connections after a table read: {}",
            procs(db.clone()).await.len()
        );
        // 2) Lock held by the editor's own transaction: read must fail after the timeout, not hang forever.
        if pg {
            db.query("BEGIN").await?;
            db.query("LOCK TABLE qads3_r IN ACCESS EXCLUSIVE MODE")
                .await?;
        } else {
            db.query("LOCK TABLES qads3_r WRITE").await?;
        }
        let s = Instant::now();
        let r =
            tokio::time::timeout(Duration::from_secs(40), db.table_data(&req(&t, vec![]))).await;
        println!(
            "{d:?} read while editor holds an exclusive lock: after {:?}: {:?}",
            s.elapsed(),
            r.map(|r| r.map(|x| x.total).map_err(|e| e
                .to_string()
                .chars()
                .take(80)
                .collect::<String>()))
        );
        if pg {
            db.query("ROLLBACK").await?;
        } else {
            db.query("UNLOCK TABLES").await?;
        }
        // 3) raw filter that tries to leave session state on the reader, then verify later reads/edits
        let probe = if pg {
            "set_config('search_path', 'pg_catalog', false) IS NOT NULL"
        } else {
            "(@qads3_x := 1) = 1"
        };
        let rr = db.table_data(&req(&t, raw(probe))).await;
        let after = db.table_data(&req(&t, vec![])).await;
        let ed = commit(&db, &t, |er, _| er[0].cur[1] = Cell::Text("9".into())).await;
        println!(
            "{d:?} raw filter {probe:?}: {:?}; next read {:?}; next edit {}",
            rr.map(|r| r.total)
                .map_err(|e| e.to_string().chars().take(60).collect::<String>()),
            after
                .map(|r| r.total)
                .map_err(|e| e.to_string().chars().take(60).collect::<String>()),
            e(&ed)
        );
        // 4) cancel a slow plain (non-raw) read? (filters are parameterised; use a big cross join view)
        // 5) a raw filter cannot write through a writable CTE / function
        let w = if pg {
            "id IN (SELECT 1 FROM (SELECT pg_advisory_lock(42)) x)"
        } else {
            "GET_LOCK('qads3', 0) = 1"
        };
        let rw = db.table_data(&req(&t, raw(w))).await;
        let held = if pg {
            rows(
                &db,
                "SELECT COUNT(*) FROM pg_locks WHERE locktype='advisory' AND objid=42",
            )
            .await
        } else {
            rows(&db, "SELECT IS_USED_LOCK('qads3') IS NOT NULL").await
        };
        println!(
            "{d:?} raw filter taking a session lock: {:?}; lock still held afterwards: {held:?}",
            rw.map(|r| r.total)
                .map_err(|e| e.to_string().chars().take(60).collect::<String>())
        );
        db.query("DROP TABLE qads3_r").await?;
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn qa3_mysql_type_change_roundtrip() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qads3_alt");
    let cases = [
        ("c INT", "1234", "varchar(3)"),
        ("c INT", "123", "varchar(3)"),
        ("c VARCHAR(20)", "'2024-02-30'", "date"),
        ("c VARCHAR(20)", "'2024-02-29'", "date"),
        ("c VARCHAR(20)", "'2024-2-9'", "date"),
        ("c DATETIME", "'2040-01-01 00:00:00'", "timestamp"),
        ("c DOUBLE", "1.005", "decimal(5,2)"),
        ("c DOUBLE", "1.25", "decimal(5,2)"),
        ("c BIGINT UNSIGNED", "18446744073709551615", "bigint"),
        ("c INT", "5", "bigint"),
        ("c VARCHAR(5)", "'abc'", "text"),
        ("c JSON", "'{\"b\": 1, \"a\": 2}'", "varchar(100)"),
        ("c VARCHAR(100)", "'{\"b\": 1, \"a\": 2}'", "json"),
        ("c VARCHAR(10)", "'b'", "enum('a','b')"),
        ("c VARCHAR(10)", "'B'", "enum('a','b')"),
        ("c SET('x','y')", "'x,y'", "varchar(10)"),
        ("c VARCHAR(10)", "'1'", "bit(1)"),
        ("c FLOAT", "1.2345678", "double"),
        ("c DECIMAL(10,3)", "1.234", "float"),
        ("c TIME(3)", "'10:00:00.123'", "time"),
        ("c DATETIME(3)", "'2024-01-01 10:00:00.500'", "date"),
        ("c TINYINT", "1", "bool"),
        ("c VARCHAR(10)", "'ไทย'", "varbinary(9)"),
        ("c VARCHAR(10)", "'ไทย'", "varbinary(8)"),
        ("c VARBINARY(10)", "0xFF", "varchar(10)"),
        ("c TEXT", "'x'", "mediumtext"),
        ("c INT", "-0", "int unsigned"),
        ("c DECIMAL(10,2)", "-0.00", "decimal(10,2) unsigned"),
    ];
    let cols: Vec<String> = [
        "#",
        "column_name",
        "data_type",
        "character_set",
        "collation",
        "is_nullable",
        "column_default",
        "extra",
        "foreign_key",
        "comment",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut silent = vec![];
    for (ddl, val, new_ty) in cases {
        db.query("DROP TABLE IF EXISTS qads3_alt").await?;
        db.query(&format!(
            "CREATE TABLE qads3_alt (id INT PRIMARY KEY, {ddl})"
        ))
        .await?;
        db.query(&format!("INSERT INTO qads3_alt VALUES (1, {val})"))
            .await?;
        let before = rows(&db, "SELECT CAST(c AS CHAR), HEX(c) FROM qads3_alt").await;
        let s = db.structure(&t).await?;
        let cx = |n: &str| s.columns.iter().position(|c| c == n);
        let row = s
            .rows
            .iter()
            .find(|r| cx("column_name").and_then(|i| r[i].as_deref()) == Some("c"))
            .unwrap()
            .clone();
        let orig: Vec<Option<String>> = cols
            .iter()
            .map(|c| cx(c).and_then(|i| row[i].clone()))
            .collect();
        let mut cur: Vec<Cell> = orig.iter().cloned().map(Cell::from).collect();
        cur[2] = Cell::Text(new_ty.into());
        let change = StructureChange {
            table: t.clone(),
            new_name: "qads3_alt".into(),
            is_new: false,
            primary_key: vec![],
            columns: cols.clone(),
            rows: vec![EditRow {
                key: "k".into(),
                orig: Some(orig),
                cur,
                deleted: false,
                rid: None,
            }],
        };
        let r = match structure_statements(Driver::Mysql, &change) {
            Ok(st) => db.execute_after_review(&st, false, false).await.map(|_| ()),
            Err(e) => Err(e),
        };
        let after = rows(&db, "SELECT CAST(c AS CHAR), HEX(c) FROM qads3_alt").await;
        let changed = r.is_ok() && before != after;
        println!(
            "{ddl:<22} {val:<24} -> {new_ty:<22} {:<62} {}",
            e(&r),
            if changed {
                format!("CHANGED {before:?} -> {after:?}")
            } else {
                String::new()
            }
        );
        if changed {
            silent.push(format!("{ddl} {val} -> {new_ty}: {before:?} -> {after:?}"));
        }
    }
    db.query("DROP TABLE IF EXISTS qads3_alt").await?;
    println!(
        "ALTERED WITH A CHANGED VALUE ({}):\n{}",
        silent.len(),
        silent.join("\n")
    );
    Ok(())
}

#[tokio::test]
async fn qa3_pg_regressions() -> Result<()> {
    // PG keyed + unkeyed tables after an editor session SET, and a commit while the editor holds a txn.
    let db = connect(Driver::Postgres).await?;
    db.query("DROP TABLE IF EXISTS qads3_p").await?;
    db.query("CREATE TABLE qads3_p (id bigint PRIMARY KEY, ts timestamptz, v int)")
        .await?;
    db.query(
        "INSERT INTO qads3_p VALUES (9007199254740993, now(), 0), (9007199254740992, now(), 0)",
    )
    .await?;
    db.query("SET TimeZone = 'America/New_York'").await?;
    let t = tref(Driver::Postgres, "qads3_p");
    let r = commit(&db, &t, |er, _| {
        let i = find(er, 0, Some("9007199254740993"));
        er[i].cur[2] = Cell::Text("1".into());
    })
    .await;
    println!(
        "PG bigint key edit: {} -> {:?}",
        e(&r),
        rows(&db, "SELECT id::text, v FROM qads3_p ORDER BY id").await
    );
    // editor holds an uncommitted update on row 1; grid commit on the same row
    db.query("BEGIN").await?;
    db.query("UPDATE qads3_p SET v = 50 WHERE id = 9007199254740992")
        .await?;
    let s = Instant::now();
    let r2 = commit(&db, &t, |er, _| {
        let i = find(er, 0, Some("9007199254740992"));
        er[i].cur[2] = Cell::Text("2".into());
    })
    .await;
    println!(
        "PG grid commit while editor txn holds the row: after {:?}: {}",
        s.elapsed(),
        e(&r2)
    );
    db.query("ROLLBACK").await?;
    db.query("DROP TABLE qads3_p").await?;
    Ok(())
}

#[tokio::test]
async fn qa3_ubig_debug() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    for (name, ddl, ins) in [
        (
            "a",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(18446744073709551614,0),(18446744073709551615,0)",
        ),
        (
            "b",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(1,0),(18446744073709551615,0)",
        ),
        (
            "c",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(1,0),(9223372036854775808,0)",
        ),
        (
            "d",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(1,0),(9223372036854775807,0)",
        ),
    ] {
        let tn = format!("qads3_dbg_{name}");
        db.query(&format!("DROP TABLE IF EXISTS {tn}")).await?;
        db.query(&format!("CREATE TABLE {tn} ({ddl})")).await?;
        db.query(&format!("INSERT INTO {tn} VALUES {ins}")).await?;
        let t = tref(Driver::Mysql, &tn);
        let data = db.table_data(&req(&t, vec![])).await?;
        let st = stage(&db, &t, |er, _| {
            let l = er.len() - 1;
            er[l].cur[1] = Cell::Text("7".into());
        })
        .await?;
        let r = db.execute_after_review(&st, true, false).await.map(|_| ());
        println!(
            "{name}: rows={:?}\n   rid={:?}\n   sql={}\n   params={:?}\n   -> {}",
            data.result.rows,
            data.row_ids.as_ref().map(|v| v.last().cloned()),
            st[0].sql.chars().take(200).collect::<String>(),
            st[0].params,
            e(&r)
        );
        db.query(&format!("DROP TABLE {tn}")).await?;
    }
    Ok(())
}

#[tokio::test]
async fn qa3_ubig_wrong_row() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    if std::env::var("QA_NONSTRICT").is_ok() {
        // Simulate a server whose default sql_mode is not strict (session-level, commit connection only).
        db.execute(
            &[Stmt::new("SET SESSION sql_mode = 'NO_ENGINE_SUBSTITUTION'")],
            false,
        )
        .await?;
        println!(
            "commit connection sql_mode: {:?}",
            db.execute(&[Stmt::new("SELECT @@session.sql_mode")], false)
                .await?[0]
                .rows
        );
    }
    let mut res = vec![];
    for (name, ddl, ins, target) in [
        (
            "ubig",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(9223372036854775807,0),(18446744073709551615,0)",
            "18446744073709551615",
        ),
        (
            "bit64",
            "k BIT(64) PRIMARY KEY, v INT",
            "(9223372036854775807,0),(18446744073709551615,0)",
            "",
        ),
        (
            "ubig_del",
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(9223372036854775807,0),(10000000000000000000,0)",
            "10000000000000000000",
        ),
    ] {
        let tn = format!("qads3_w_{name}");
        db.query(&format!("DROP TABLE IF EXISTS {tn}")).await?;
        db.query(&format!("CREATE TABLE {tn} ({ddl})")).await?;
        db.query(&format!("INSERT INTO {tn} VALUES {ins}")).await?;
        let t = tref(Driver::Mysql, &tn);
        let del = name.ends_with("del");
        let st = stage(&db, &t, |er, _| {
            let i = if target.is_empty() {
                er.iter()
                    .position(|r| {
                        r.orig.as_ref().unwrap()[0].as_deref() != Some("0x7FFFFFFFFFFFFFFF")
                    })
                    .unwrap()
            } else {
                find(er, 0, Some(target))
            };
            if del {
                er[i].deleted = true
            } else {
                er[i].cur[1] = Cell::Text("99".into())
            }
        })
        .await?;
        println!(
            "{name}: {} {:?}",
            st[0].sql.chars().take(120).collect::<String>(),
            &st[0].params[..st[0].params.len().min(2)]
        );
        let r = db.execute_after_review(&st, true, false).await.map(|_| ());
        let after = rows(
            &db,
            &format!("SELECT CAST(k AS UNSIGNED), v FROM {tn} ORDER BY k"),
        )
        .await;
        println!("   -> {} ; after {after:?}", e(&r));
        res.push((name, del, r.is_ok(), after));
        db.query(&format!("DROP TABLE {tn}")).await?;
    }
    // Corrected after Round 3 review: a correct delete legitimately leaves only the twin.
    let twin = Some("9223372036854775807");
    for (n, del, ok, a) in res {
        assert!(
            a.iter()
                .any(|r| r[0].as_deref() == twin && r[1].as_deref() == Some("0")),
            "{n}: 2^63-1 twin changed or deleted"
        );
        let target: Vec<_> = a.iter().filter(|r| r[0].as_deref() != twin).collect();
        if del {
            assert_eq!(
                target.len(),
                usize::from(!ok),
                "{n}: delete reported {ok} but target rows = {target:?}"
            );
        } else {
            assert_eq!(target.len(), 1, "{n}: target row missing");
            assert_eq!(
                target[0][1].as_deref(),
                Some(if ok { "99" } else { "0" }),
                "{n}: edit reported {ok}"
            );
        }
        assert!(ok, "{n}: correct fix should commit this edit/delete");
    }
    Ok(())
}

#[tokio::test]
async fn qa4_boundary_keys_and_strict() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    // Simulate a non-strict editor session; the commit connection must still be strict.
    db.query("SET SESSION sql_mode = ''").await?;
    println!(
        "commit connection sql_mode: {:?}",
        db.execute(&[Stmt::new("SELECT @@session.sql_mode")], false)
            .await?[0]
            .rows
    );
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        (
            "d65_0",
            "k DECIMAL(65,0) PRIMARY KEY, v INT",
            "(99999999999999999999999999999999999999999999999999999999999999999,0),(99999999999999999999999999999999999999999999999999999999999999998,0),(-99999999999999999999999999999999999999999999999999999999999999999,0)",
            "99999999999999999999999999999999999999999999999999999999999999998",
        ),
        (
            "d65_30",
            "k DECIMAL(65,30) PRIMARY KEY, v INT",
            "(12345678901234567890123456789012345.000000000000000000000000000001,0),(12345678901234567890123456789012345.000000000000000000000000000002,0)",
            "12345678901234567890123456789012345.000000000000000000000000000001",
        ),
        (
            "d65_neg",
            "k DECIMAL(65,30) PRIMARY KEY, v INT",
            "(-0.000000000000000000000000000001,0),(0,0)",
            "-0.000000000000000000000000000001",
        ),
        (
            "tinyu",
            "k TINYINT UNSIGNED PRIMARY KEY, v INT",
            "(255,0),(254,0)",
            "255",
        ),
        (
            "tiny",
            "k TINYINT PRIMARY KEY, v INT",
            "(-128,0),(127,0)",
            "-128",
        ),
        (
            "bigs",
            "k BIGINT PRIMARY KEY, v INT",
            "(9223372036854775807,0),(9223372036854775806,0),(-9223372036854775808,0)",
            "9223372036854775807",
        ),
        ("bit1", "k BIT(1) PRIMARY KEY, v INT", "(0,0),(1,0)", ""),
        (
            "medu",
            "k MEDIUMINT UNSIGNED PRIMARY KEY, v INT",
            "(16777215,0),(0,0)",
            "16777215",
        ),
        (
            "nopad",
            "k VARCHAR(5) COLLATE utf8mb4_0900_as_cs PRIMARY KEY, v INT",
            "('a',0),('a ',0),('A',0)",
            "a ",
        ),
        (
            "charpad",
            "k CHAR(5) COLLATE utf8mb4_bin PRIMARY KEY, v INT",
            "('a',0),('b',0)",
            "a",
        ),
        (
            "comp_u",
            "a BIGINT UNSIGNED, b DECIMAL(65,30), v INT, PRIMARY KEY (a,b)",
            "(18446744073709551615,1.5,0),(18446744073709551615,1.50000000000000000000000000001,0),(9223372036854775807,1.5,0)",
            "18446744073709551615",
        ),
    ];
    for (name, ddl, ins, key) in cases {
        let tn = format!("qads4_{name}");
        db.query(&format!("DROP TABLE IF EXISTS {tn}")).await?;
        db.query(&format!("CREATE TABLE {tn} ({ddl})")).await?;
        db.query(&format!("INSERT INTO {tn} VALUES {ins}")).await?;
        let t = tref(Driver::Mysql, &tn);
        let total: usize = rows(&db, &format!("SELECT COUNT(*) FROM {tn}")).await[0][0]
            .clone()
            .unwrap()
            .parse()?;
        let r = commit(&db, &t, |er, cols| {
            let i = if key.is_empty() {
                er.len() - 1
            } else {
                find(er, 0, Some(key))
            };
            er[i].cur[ci(cols, "v")] = Cell::Text("7".into());
        })
        .await;
        let changed =
            rows(&db, &format!("SELECT COUNT(*) FROM {tn} WHERE v = 7")).await[0][0].clone();
        let d = commit(&db, &t, |er, _| {
            let i = if key.is_empty() {
                er.len() - 1
            } else {
                find(er, 0, Some(key))
            };
            er[i].deleted = true;
        })
        .await;
        let left: usize = rows(&db, &format!("SELECT COUNT(*) FROM {tn}")).await[0][0]
            .clone()
            .unwrap()
            .parse()?;
        let left7 =
            rows(&db, &format!("SELECT COUNT(*) FROM {tn} WHERE v = 7")).await[0][0].clone();
        println!(
            "{name:<8} edit {:<24} changed={changed:?} | delete same row {:<24} left={left}/{total} remaining v=7: {left7:?}",
            e(&r),
            e(&d)
        );
        assert!(r.is_ok() && changed.as_deref() == Some("1"), "{name}: edit");
        assert!(
            d.is_ok() && left == total - 1 && left7.as_deref() == Some("0"),
            "{name}: delete removed the wrong row"
        );
        db.query(&format!("DROP TABLE {tn}")).await?;
    }
    // Legacy zero date row (inserted non-strict) stays editable through the strict commit connection.
    db.query("DROP TABLE IF EXISTS qads4_zero").await?;
    db.query("CREATE TABLE qads4_zero (id INT PRIMARY KEY, d DATETIME, v INT)")
        .await?;
    db.query("INSERT INTO qads4_zero VALUES (1,'0000-00-00 00:00:00',0),(2,'2024-01-01',0)")
        .await?;
    let t = tref(Driver::Mysql, "qads4_zero");
    let r = commit(&db, &t, |er, _| {
        let i = find(er, 0, Some("1"));
        er[i].cur[2] = Cell::Text("5".into());
    })
    .await;
    let ins = commit(&db, &t, |er, _| {
        er.push(EditRow {
            key: row_key(),
            orig: None,
            cur: vec![
                Cell::Text("3".into()),
                Cell::Text("0000-00-00 00:00:00".into()),
                Cell::Text("1".into()),
            ],
            deleted: false,
            rid: None,
        })
    })
    .await;
    let long = {
        db.query("CREATE TABLE IF NOT EXISTS qads4_len (id INT PRIMARY KEY, s VARCHAR(3))")
            .await?;
        db.query("INSERT INTO qads4_len VALUES (1,'a')").await?;
        commit(&db, &tref(Driver::Mysql, "qads4_len"), |er, _| {
            er[0].cur[1] = Cell::Text("abcdef".into())
        })
        .await
    };
    println!(
        "zero-date row edit: {} ; insert of a zero date: {} ; over-long value with non-strict editor: {} -> {:?}",
        e(&r),
        e(&ins),
        e(&long),
        rows(&db, "SELECT s FROM qads4_len").await
    );
    db.query("DROP TABLE qads4_zero").await?;
    db.query("DROP TABLE qads4_len").await?;
    Ok(())
}
