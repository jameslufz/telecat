//! QA re-verification live probes (127.0.0.1:3307 / :5434 only, via connect_test).
//! cargo test -p telecat-db --test qa_reverify -- --test-threads=1 --nocapture
use anyhow::Result;
use serde_json::json;
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
fn req(t: &TableRef, filters: Vec<Filter>, limit: u64) -> TableDataRequest {
    TableDataRequest {
        table: t.clone(),
        filters,
        combine: Combine::And,
        order_by: None,
        limit,
        offset: 0,
    }
}
async fn commit(db: &Session, t: &TableRef, edit: impl FnOnce(&mut Vec<EditRow>)) -> Result<()> {
    let data = db.table_data(&req(t, vec![], 1000)).await?;
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    edit(&mut er);
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(db.driver, t, &meta, &er)?;
    db.execute_after_review(&st, true, false).await?;
    Ok(())
}
fn pos(er: &[EditRow], col: usize, v: Option<&str>) -> usize {
    er.iter()
        .position(|r| r.orig.as_ref().unwrap()[col].as_deref() == v)
        .expect("row")
}

#[tokio::test]
async fn qa2_guard_live() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let mut bad = vec![];
        for sql in [
            "DELETE FROM qads2_g WHERE 'a' <> 'b'",
            "DELETE FROM qads2_g WHERE 1=1 AND 'a' <> 'b'",
            "DELETE FROM qads2_g AS a\u{a0}WHERE\u{a0}b",
            "DELETE FROM qads2_g AS a·WHERE·b",
            "UPDATE qads2_g AS a·WHERE·x SET v = 9",
            "DELETE FROM qads2_g WHERE $ä$x$ä$ <> 'y'",
            "DELETE FROM qads2_g /*!99999 WHERE id = 1",
            "DELETE FROM qads2_g /* WHERE id = 1",
            "DELETE FROM qads2_g WHERE 1 IN (1)",
            "DELETE FROM qads2_g WHERE NULL IS NULL",
        ] {
            db.query("DROP TABLE IF EXISTS qads2_g").await?;
            db.query("CREATE TABLE qads2_g (id int PRIMARY KEY, v int)")
                .await?;
            db.query("INSERT INTO qads2_g VALUES (1,0),(2,0),(3,0)")
                .await?;
            let r = db.query_checked(sql, false).await;
            let left = rows(&db, "SELECT COUNT(*), SUM(v) FROM qads2_g").await;
            let msg = match &r {
                Ok(_) => "EXECUTED".to_string(),
                Err(e) => format!("ERR {}", e.to_string().chars().take(60).collect::<String>()),
            };
            println!(
                "{d:?} {:<50} -> {:<66} {:?}",
                sql.replace('\u{a0}', "<nbsp>"),
                msg,
                left[0]
            );
            if left[0][0].as_deref() != Some("3") || left[0][1].as_deref() != Some("0") {
                bad.push(sql.replace('\u{a0}', "<nbsp>"));
            }
        }
        db.query("DROP TABLE IF EXISTS qads2_g").await?;
        println!("{d:?} WROTE WITHOUT CONFIRMATION: {bad:#?}\n");
    }
    Ok(())
}

#[tokio::test]
async fn qa2_mysql_fingerprint_types() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let mut out = vec![];
    // (table ddl, inserts, column index to identify by, value to find, column index to edit)
    let cases: Vec<(&str, &str, &str)> = vec![
        (
            "bitpk",
            "CREATE TABLE qads2_bitpk (k BIT(8) PRIMARY KEY, v INT)",
            "INSERT INTO qads2_bitpk VALUES (b'00000001',0),(b'01000001',0)",
        ),
        (
            "enumpk",
            "CREATE TABLE qads2_enumpk (k ENUM('a','B','c') PRIMARY KEY, v INT)",
            "INSERT INTO qads2_enumpk VALUES ('a',0),('B',0)",
        ),
        (
            "setpk",
            "CREATE TABLE qads2_setpk (k SET('x','y') PRIMARY KEY, v INT)",
            "INSERT INTO qads2_setpk VALUES ('x',0),('x,y',0)",
        ),
        (
            "decpk",
            "CREATE TABLE qads2_decpk (k DECIMAL(10,3) PRIMARY KEY, v INT)",
            "INSERT INTO qads2_decpk VALUES (1.100,0),(1.000,0)",
        ),
        (
            "floatpk",
            "CREATE TABLE qads2_floatpk (k FLOAT PRIMARY KEY, v INT)",
            "INSERT INTO qads2_floatpk VALUES (1.2345678,0),(2.5,0)",
        ),
        (
            "dblpk",
            "CREATE TABLE qads2_dblpk (k DOUBLE PRIMARY KEY, v INT)",
            "INSERT INTO qads2_dblpk VALUES (0.1,0),(1e300,0)",
        ),
        (
            "tspk",
            "CREATE TABLE qads2_tspk (k TIMESTAMP(3) PRIMARY KEY, v INT)",
            "INSERT INTO qads2_tspk VALUES ('2024-01-01 00:00:00.123',0),('2024-06-01 00:00:00',0)",
        ),
        (
            "binpk",
            "CREATE TABLE qads2_binpk (k BINARY(4) PRIMARY KEY, v INT)",
            "INSERT INTO qads2_binpk VALUES ('ab',0),(0x00FF,0)",
        ),
        (
            "charpk",
            "CREATE TABLE qads2_charpk (k CHAR(5) CHARACTER SET latin1 PRIMARY KEY, v INT)",
            "INSERT INTO qads2_charpk VALUES ('é ',0),('x',0)",
        ),
        (
            "cpk",
            "CREATE TABLE qads2_cpk (a INT, b VARCHAR(5), v INT, PRIMARY KEY (a,b))",
            "INSERT INTO qads2_cpk VALUES (1,'x',0),(1,'y',0)",
        ),
        (
            "nopk_null_empty",
            "CREATE TABLE qads2_nopk_null_empty (s VARCHAR(5), v INT)",
            "INSERT INTO qads2_nopk_null_empty VALUES (NULL,0),('',0)",
        ),
        (
            "nopk_json",
            "CREATE TABLE qads2_nopk_json (j JSON, v INT)",
            "INSERT INTO qads2_nopk_json VALUES ('{\"b\":1,\"a\":[1.50,2]}',0),('{\"a\":1}',0)",
        ),
        (
            "nopk_geo",
            "CREATE TABLE qads2_nopk_geo (g POINT, v INT)",
            "INSERT INTO qads2_nopk_geo VALUES (ST_GeomFromText('POINT(1 2)'),0),(ST_GeomFromText('POINT(1 3)'),0)",
        ),
        (
            "nopk_gen",
            "CREATE TABLE qads2_nopk_gen (a INT, g INT AS (a*2) VIRTUAL, s INT AS (a+1) STORED, v INT)",
            "INSERT INTO qads2_nopk_gen (a,v) VALUES (1,0),(2,0)",
        ),
        (
            "nopk_float",
            "CREATE TABLE qads2_nopk_float (f FLOAT, d DOUBLE, t TIME(6), y YEAR, v INT)",
            "INSERT INTO qads2_nopk_float VALUES (1.2345678,1e-300,'-838:59:59.000000',2024,0),(1.2345679,1e-300,'-838:59:59.000000',2024,0)",
        ),
        (
            "nopk_blob",
            "CREATE TABLE qads2_nopk_blob (b LONGBLOB, t LONGTEXT, v INT)",
            "INSERT INTO qads2_nopk_blob VALUES (REPEAT(0x00,70000),REPEAT('ก',30000),0),(REPEAT(0x01,70000),REPEAT('ก',30000),0)",
        ),
    ];
    for (name, ddl, ins) in cases {
        let tn = format!("qads2_{name}");
        db.query(&format!("DROP TABLE IF EXISTS {tn}")).await?;
        db.query(ddl).await?;
        db.query(ins).await?;
        let t = tref(Driver::Mysql, &tn);
        let data = db.table_data(&req(&t, vec![], 100)).await?;
        let vcol = data.result.columns.iter().position(|c| c == "v").unwrap();
        let shown = data
            .result
            .rows
            .iter()
            .map(|r| r[0].clone())
            .collect::<Vec<_>>();
        // edit the 2nd displayed row's v, then delete the first displayed row
        let r = commit(&db, &t, |er| er[1].cur[vcol] = Cell::Text("7".into())).await;
        let after = rows(&db, &format!("SELECT v FROM {tn}")).await;
        let changed = after
            .iter()
            .filter(|r| r[0].as_deref() == Some("7"))
            .count();
        let d = commit(&db, &t, |er| er[0].deleted = true).await;
        let left = rows(&db, &format!("SELECT COUNT(*) FROM {tn}")).await[0][0].clone();
        println!(
            "{name:<16} shown={shown:?}\n                 edit={:?} changed={changed}; delete={:?} left={left:?}",
            r.as_ref()
                .map(|_| ())
                .map_err(|e| e.to_string().chars().take(90).collect::<String>()),
            d.as_ref()
                .map(|_| ())
                .map_err(|e| e.to_string().chars().take(90).collect::<String>()),
        );
        out.push((name, r.is_ok(), changed, d.is_ok(), left));
        db.query(&format!("DROP TABLE {tn}")).await?;
    }
    for (name, ok, changed, dok, left) in &out {
        assert!(*changed <= 1, "{name}: edit changed {changed} rows");
        if *ok {
            assert_eq!(*changed, 1, "{name}");
        }
        if *dok {
            assert_eq!(left.as_deref(), Some("1"), "{name}");
        } else {
            assert_eq!(left.as_deref(), Some("2"), "{name}");
        }
    }
    let refused: Vec<_> = out.iter().filter(|o| !o.1 || !o.3).map(|o| o.0).collect();
    println!("refused (edit or delete): {refused:?}");
    Ok(())
}

#[tokio::test]
async fn qa2_mysql_session_changes_between_load_and_commit() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads2_sess").await?;
    db.query("CREATE TABLE qads2_sess (id INT PRIMARY KEY, ts TIMESTAMP NULL, c CHAR(5), d DATETIME, v INT)").await?;
    db.query("INSERT INTO qads2_sess VALUES (1,'2024-03-10 12:00:00','ab','2024-01-01',0),(2,NULL,'x',NULL,0)").await?;
    let t = tref(Driver::Mysql, "qads2_sess");
    // Editor connection changes session state after load (user ran SET in the SQL editor).
    let data = db.table_data(&req(&t, vec![], 100)).await?;
    db.query("SET time_zone = '+07:00'").await?;
    if std::env::var("QA_PAD").is_ok() {
        db.query("SET sql_mode = CONCAT(@@sql_mode, ',PAD_CHAR_TO_FULL_LENGTH')")
            .await
            .ok();
    }
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    er[0].cur[4] = Cell::Text("5".into());
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(Driver::Mysql, &t, &meta, &er)?;
    let r = db.execute_after_review(&st, true, false).await;
    println!(
        "edit after editor SET time_zone/sql_mode: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    // And reload under the changed session, then edit again
    let r2 = commit(&db, &t, |er| er[0].cur[4] = Cell::Text("6".into())).await;
    println!(
        "reload+edit under changed session: {:?}",
        r2.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    println!(
        "after: {:?}",
        rows(&db, "SELECT id, v FROM qads2_sess ORDER BY id").await
    );
    db.query("DROP TABLE qads2_sess").await?;
    assert!(r.is_ok() && r2.is_ok());
    Ok(())
}

#[tokio::test]
async fn qa2_pg_nopk_types() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    db.query("DROP TABLE IF EXISTS qads2_pgt; DROP TYPE IF EXISTS qads2_comp")
        .await
        .ok();
    db.query("DROP TABLE IF EXISTS qads2_pgt").await?;
    db.query("DROP TYPE IF EXISTS qads2_comp").await?;
    db.query("CREATE TYPE qads2_comp AS (a int, b text)")
        .await?;
    db.query("CREATE TABLE qads2_pgt (f float8, r real, n numeric, a int[], t text[], j json, jb jsonb, c qads2_comp, b bool, tz timestamptz, iv interval, by bytea, u uuid, v int)").await?;
    db.query("INSERT INTO qads2_pgt VALUES ('NaN','-Infinity',1.10,'{1,NULL}','{\"a b\",\"c,d\",NULL}','{\"b\":1, \"a\":2}','{\"b\":1,\"a\":2}',ROW(1,'x y'),true,'2024-03-10 02:30:00+00','1 day 02:00','\\x00ff','a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',0)").await?;
    db.query("INSERT INTO qads2_pgt (v) VALUES (0)").await?;
    let t = tref(Driver::Postgres, "qads2_pgt");
    let r = commit(&db, &t, |er| {
        let i = pos(er, 0, Some("NaN"));
        let last = er[i].cur.len() - 1;
        er[i].cur[last] = Cell::Text("1".into());
    })
    .await;
    println!(
        "PG exotic types edit: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    let r2 = commit(&db, &t, |er| {
        let i = pos(er, 0, None);
        er[i].deleted = true;
    })
    .await;
    println!(
        "PG all-NULL row delete: {:?}",
        r2.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    // Editor connection changes TimeZone between load and commit.
    let data = db.table_data(&req(&t, vec![], 100)).await?;
    db.query("SET TimeZone = 'Asia/Bangkok'").await?;
    db.query("SET IntervalStyle = 'iso_8601'").await?;
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    let last = er[0].cur.len() - 1;
    er[0].cur[last] = Cell::Text("2".into());
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(Driver::Postgres, &t, &meta, &er)?;
    let r3 = db.execute_after_review(&st, true, false).await;
    println!(
        "PG edit after editor SET TimeZone: {:?}",
        r3.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    let r4 = commit(&db, &t, |er| {
        let l = er[0].cur.len() - 1;
        er[0].cur[l] = Cell::Text("3".into())
    })
    .await;
    println!(
        "PG reload+edit under changed TimeZone: {:?}",
        r4.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    println!("after: {:?}", rows(&db, "SELECT v FROM qads2_pgt").await);
    db.query("DROP TABLE qads2_pgt").await?;
    db.query("DROP TYPE qads2_comp").await?;
    assert!(r.is_ok() && r2.is_ok());
    Ok(())
}

#[tokio::test]
async fn qa2_pg_single_transaction_batch() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    for s in [
        "DROP TABLE IF EXISTS qads2_a, qads2_b, qads2_c",
        "CREATE TABLE qads2_a (id int PRIMARY KEY, v int)",
        "CREATE TABLE qads2_b (id int)",
        "CREATE TABLE qads2_c (id int)",
        "INSERT INTO qads2_a VALUES (1,0)",
        "INSERT INTO qads2_b VALUES (1),(2)",
    ] {
        db.query(s).await?;
    }
    let st = vec![
        Stmt {
            sql: "UPDATE \"public\".\"qads2_a\" SET \"v\" = $1 WHERE \"id\" = $2;".into(),
            params: vec![json!("9"), json!("1")],
            expect: Some(1),
            check: None,
        },
        Stmt::new("ALTER TABLE \"public\".\"qads2_a\" ADD COLUMN \"z\" int;"),
        table_op_statement(
            Driver::Postgres,
            &tref(Driver::Postgres, "qads2_b"),
            true,
            false,
        ),
        table_op_statement(
            Driver::Postgres,
            &tref(Driver::Postgres, "qads2_c"),
            false,
            false,
        ),
        Stmt {
            sql: "DELETE FROM \"public\".\"qads2_a\" WHERE \"id\" = $1;".into(),
            params: vec![json!("404")],
            expect: Some(1),
            check: None,
        },
    ];
    let r = db.execute_after_review(&st, true, false).await;
    println!(
        "PG batch with failing last stmt: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    let a = rows(&db, "SELECT v FROM qads2_a").await;
    let b = rows(&db, "SELECT COUNT(*) FROM qads2_b").await;
    let c = rows(&db, "SELECT to_regclass('public.qads2_c')::text").await;
    let z = rows(&db, "SELECT COUNT(*) FROM information_schema.columns WHERE table_name='qads2_a' AND column_name='z'").await;
    println!("a.v={a:?} b.count={b:?} c={c:?} z={z:?}");
    db.query("DROP TABLE IF EXISTS qads2_a, qads2_b, qads2_c")
        .await?;
    assert!(r.is_err());
    assert_eq!(a[0][0].as_deref(), Some("0"));
    assert_eq!(b[0][0].as_deref(), Some("2"));
    assert!(c[0][0].is_some());
    assert_eq!(z[0][0].as_deref(), Some("0"));
    Ok(())
}

#[tokio::test]
async fn qa2_raw_filter_readonly() -> Result<()> {
    // MySQL: function writing to a MyISAM (non-transactional) log
    let db = Arc::new(connect(Driver::Mysql).await?);
    for s in [
        "DROP TABLE IF EXISTS qads2_rf",
        "DROP TABLE IF EXISTS qads2_rf_log",
        "DROP FUNCTION IF EXISTS qads2_rf_fn",
        "CREATE TABLE qads2_rf (id INT PRIMARY KEY)",
        "INSERT INTO qads2_rf VALUES (1)",
        "CREATE TABLE qads2_rf_log (n INT) ENGINE=MyISAM",
        "CREATE FUNCTION qads2_rf_fn() RETURNS INT DETERMINISTIC MODIFIES SQL DATA BEGIN INSERT INTO qads2_rf_log VALUES (1); RETURN 1; END",
    ] {
        db.query(s).await?;
    }
    let t = tref(Driver::Mysql, "qads2_rf");
    let raw = |v: &str| {
        vec![Filter {
            enabled: true,
            column: RAW_SQL.into(),
            op: FilterOp::Eq,
            value: v.into(),
            value2: None,
        }]
    };
    let r = db.table_data(&req(&t, raw("qads2_rf_fn() = 1"), 10)).await;
    let n = rows(&db, "SELECT COUNT(*) FROM qads2_rf_log").await;
    println!(
        "MySQL MyISAM-writing raw filter: {:?}; log rows={:?}",
        r.as_ref().map(|r| r.total).map_err(|e| e
            .to_string()
            .chars()
            .take(120)
            .collect::<String>()),
        n
    );
    // Cancel during a slow raw-filter read: Session::cancel kills the data connection only.
    let d2 = db.clone();
    let slow = raw("SLEEP(4) = 0");
    let t2 = t.clone();
    let start = Instant::now();
    let h = tokio::spawn(async move { d2.table_data(&req(&t2, slow, 10)).await.map(|r| r.total) });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let c = db.cancel().await;
    // Meanwhile, does a commit wait behind it?
    let commit_start = Instant::now();
    let cr = db
        .execute(
            &[Stmt {
                sql: "UPDATE `zixma`.`qads2_rf` SET `id` = ? WHERE `id` = ?;".into(),
                params: vec![json!("1"), json!("1")],
                expect: Some(1),
                check: None,
            }],
            true,
        )
        .await;
    let commit_wait = commit_start.elapsed();
    let res = h.await?;
    println!(
        "cancel()={:?}; slow raw read finished after {:?}: {:?}; a commit issued meanwhile waited {:?} ({:?})",
        c.map_err(|e| e.to_string()),
        start.elapsed(),
        res.map_err(|e| e.to_string()),
        commit_wait,
        cr.map(|_| ()).map_err(|e| e.to_string())
    );
    for s in [
        "DROP TABLE qads2_rf",
        "DROP TABLE qads2_rf_log",
        "DROP FUNCTION qads2_rf_fn",
    ] {
        db.query(s).await?;
    }
    // PG: try to flip the transaction to read-write from inside the filter
    let pg = connect(Driver::Postgres).await?;
    for s in [
        "DROP TABLE IF EXISTS qads2_rf, qads2_rf_log",
        "CREATE TABLE qads2_rf (id int PRIMARY KEY)",
        "INSERT INTO qads2_rf VALUES (1)",
        "CREATE TABLE qads2_rf_log (n int)",
        "CREATE OR REPLACE FUNCTION qads2_rf_fn() RETURNS bool LANGUAGE sql VOLATILE AS 'INSERT INTO qads2_rf_log VALUES (1); SELECT true'",
    ] {
        pg.query(s).await?;
    }
    let t = tref(Driver::Postgres, "qads2_rf");
    for f in [
        "set_config('transaction_read_only','off',true) IS NOT NULL AND qads2_rf_fn()",
        "set_config('default_transaction_read_only','off',false) IS NOT NULL",
        "qads2_rf_fn()",
    ] {
        let r = pg.table_data(&req(&t, raw(f), 10)).await;
        println!(
            "PG raw {f:?}: {:?}",
            r.map(|r| r.total)
                .map_err(|e| e.to_string().chars().take(110).collect::<String>())
        );
    }
    let n = rows(&pg, "SELECT COUNT(*) FROM qads2_rf_log").await;
    // After default_transaction_read_only=off attempt, is the admin connection still read-only for raw filters, and do commits still work?
    let ok = pg
        .execute(
            &[Stmt {
                sql: "UPDATE \"public\".\"qads2_rf\" SET \"id\" = $1 WHERE \"id\" = $2;".into(),
                params: vec![json!("1"), json!("1")],
                expect: Some(1),
                check: None,
            }],
            true,
        )
        .await;
    println!(
        "PG log rows={n:?}; commit after filters: {:?}",
        ok.map(|_| ()).map_err(|e| e.to_string())
    );
    pg.query("DROP TABLE qads2_rf, qads2_rf_log").await?;
    pg.query("DROP FUNCTION qads2_rf_fn()").await?;
    Ok(())
}

#[tokio::test]
async fn qa2_mysql_load_performance() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qads2_perf").await?;
    db.query("CREATE TABLE qads2_perf (id INT PRIMARY KEY AUTO_INCREMENT, a VARCHAR(255), b TEXT, c DOUBLE, d DATETIME, e JSON, f BLOB, g TIMESTAMP NULL, h FLOAT, i DECIMAL(12,2))").await?;
    db.query("INSERT INTO qads2_perf (a,b,c,d,e,f,g,h,i) WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM s WHERE n<1000) SELECT REPEAT('x',200), REPEAT('ก',2000), n*1.1, NOW(), JSON_OBJECT('n',n,'s',REPEAT('y',200)), RANDOM_BYTES(1024), NOW(), n/3, n FROM s").await?;
    let cols: Vec<String> = (0..50).map(|i| format!("c{i} VARCHAR(100)")).collect();
    db.query("DROP TABLE IF EXISTS qads2_wide").await?;
    db.query(&format!(
        "CREATE TABLE qads2_wide (id INT PRIMARY KEY AUTO_INCREMENT, {})",
        cols.join(", ")
    ))
    .await?;
    let names: Vec<String> = (0..50).map(|i| format!("c{i}")).collect();
    let vals: Vec<String> = (0..50).map(|i| format!("CONCAT('v{i}_', n)")).collect();
    db.query(&format!("INSERT INTO qads2_wide ({}) WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM s WHERE n<1000) SELECT {} FROM s", names.join(","), vals.join(","))).await?;
    for tn in ["qads2_perf", "qads2_wide"] {
        let t = tref(Driver::Mysql, tn);
        let _ = db.table_data(&req(&t, vec![], 1000)).await?; // warm
        let mut with = vec![];
        let mut plain = vec![];
        for _ in 0..3 {
            let s = Instant::now();
            let r = db.table_data(&req(&t, vec![], 1000)).await?;
            with.push(s.elapsed());
            assert_eq!(r.result.rows.len(), 1000);
            let s = Instant::now();
            let _ = db
                .query(&format!(
                    "SELECT * FROM `zixma`.`{tn}` ORDER BY id LIMIT 1000"
                ))
                .await?;
            plain.push(s.elapsed());
        }
        println!("{tn}: table_data (with identity) {with:?}  vs plain SELECT * {plain:?}");
    }
    db.query("DROP TABLE qads2_perf").await?;
    db.query("DROP TABLE qads2_wide").await?;
    Ok(())
}

#[tokio::test]
async fn qa2_mysql_lossy_alter_edges() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qads2_alt");
    // (column ddl, value literal, new type, collation override)
    let cases = [
        ("c VARCHAR(10)", "'ไทยไทย'", "varchar(5)", None),
        ("c VARCHAR(10)", "'ไทย'", "varchar(5)", None),
        ("c INT", "-1", "tinyint unsigned", None),
        (
            "c DATETIME(6)",
            "'2024-01-01 10:00:00.999999'",
            "datetime",
            None,
        ),
        ("c DOUBLE", "0.5", "float", None),
        ("c DOUBLE", "0.1", "float", None),
        ("c TEXT", "'abcd'", "varchar(3)", None),
        ("c VARCHAR(10)", "'12abc'", "int", None),
        ("c VARCHAR(10)", "' 12'", "int", None),
        ("c VARCHAR(10)", "'1e3'", "int", None),
        ("c ENUM('a','b')", "'a'", "enum('A','b')", None),
        (
            "c VARCHAR(10) CHARACTER SET utf8mb4",
            "'ไทย'",
            "varchar(10)",
            Some("latin1_swedish_ci"),
        ),
        ("c DECIMAL(10,2)", "12.50", "decimal(10,1)", None),
        ("c BIGINT", "9007199254740993", "double", None),
        ("c CHAR(5)", "'ab'", "varchar(5)", None),
        ("c VARCHAR(5)", "'ab  '", "char(5)", None),
        ("c TIME", "'-01:00:00'", "time", None),
        ("c JSON", "'{\"a\": 1.0}'", "text", None),
        ("c YEAR", "2024", "smallint", None),
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
    for (ddl, val, new_ty, coll) in cases {
        db.query("DROP TABLE IF EXISTS qads2_alt").await?;
        db.query(&format!(
            "CREATE TABLE qads2_alt (id INT PRIMARY KEY, {ddl})"
        ))
        .await?;
        db.query(&format!("INSERT INTO qads2_alt VALUES (1, {val})"))
            .await?;
        let before = rows(&db, "SELECT CAST(c AS CHAR), HEX(c) FROM qads2_alt").await;
        let s = db.structure(&t).await?;
        let ci = |n: &str| s.columns.iter().position(|c| c == n);
        let row = s
            .rows
            .iter()
            .find(|r| ci("column_name").and_then(|i| r[i].as_deref()) == Some("c"))
            .unwrap()
            .clone();
        let orig: Vec<Option<String>> = cols
            .iter()
            .map(|c| ci(c).and_then(|i| row[i].clone()))
            .collect();
        let mut cur: Vec<Cell> = orig.iter().cloned().map(Cell::from).collect();
        cur[2] = Cell::Text(new_ty.into());
        if let Some(c) = coll {
            cur[4] = Cell::Text(c.into());
        }
        let change = StructureChange {
            table: t.clone(),
            new_name: "qads2_alt".into(),
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
        let st = structure_statements(Driver::Mysql, &change)?;
        let r = db.execute_after_review(&st, false, false).await;
        let after = rows(&db, "SELECT CAST(c AS CHAR), HEX(c) FROM qads2_alt").await;
        let silent = r.is_ok() && before != after;
        println!(
            "{ddl:<40} {val:<30} -> {new_ty:<16}{:<20} {:<60} {}",
            coll.unwrap_or(""),
            r.as_ref()
                .map(|_| "ALTERED".to_string())
                .map_err(|e| e.to_string().chars().take(55).collect::<String>())
                .unwrap_or_else(|e| e),
            if silent {
                format!("SILENT CHANGE {before:?} -> {after:?}")
            } else {
                String::new()
            }
        );
    }
    db.query("DROP TABLE IF EXISTS qads2_alt").await?;
    Ok(())
}

#[tokio::test]
async fn qa2_mysql_preview_matches() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS `qads2_p?v`").await?;
    db.query("CREATE TABLE `qads2_p?v` (`i?d` INT PRIMARY KEY, `s` VARCHAR(20), `b` VARBINARY(5))")
        .await?;
    db.query("INSERT INTO `qads2_p?v` VALUES (1, 'why?', 0x00), (2, NULL, 'x')")
        .await?;
    let t = tref(Driver::Mysql, "qads2_p?v");
    let data = db.table_data(&req(&t, vec![], 10)).await?;
    let mut er = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    er[0].cur[1] = Cell::Text("it's ? ok".into());
    er[1].deleted = true;
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let st = data_statements(Driver::Mysql, &t, &meta, &er)?;
    // The preview the user approves, executed verbatim (text protocol), must do exactly the same.
    let preview: Vec<String> = st.iter().map(|s| inline_params(Driver::Mysql, s)).collect();
    for p in &preview {
        println!("preview: {}", p.chars().take(300).collect::<String>());
    }
    let mut ok = true;
    for p in &preview {
        let r = db
            .query(&format!("EXPLAIN {}", p.trim_end_matches(';')))
            .await;
        if r.is_err() {
            ok = false;
            println!("preview does not parse: {:?}", r.err());
        }
    }
    db.query("START TRANSACTION").await?;
    let mut affected = vec![];
    for p in &preview {
        affected.push(db.query(p).await?.affected_rows);
    }
    db.query("ROLLBACK").await?;
    println!("affected by executing the previews verbatim: {affected:?}");
    let sql = db
        .table_data_sql(&req(
            &t,
            vec![Filter {
                enabled: true,
                column: "s".into(),
                op: FilterOp::Eq,
                value: "a'?b".into(),
                value2: None,
            }],
            10,
        ))
        .await?;
    println!(
        "tableDataSql: {}",
        sql.chars().take(200).collect::<String>()
    );
    db.query("DROP TABLE `qads2_p?v`").await?;
    assert!(ok);
    assert_eq!(affected, vec![Some(1), Some(1)]);
    Ok(())
}

#[tokio::test]
async fn qa2_raw_filter_session_contamination() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    for s in [
        "DROP TABLE IF EXISTS qads2_sc",
        "DROP FUNCTION IF EXISTS qads2_sc_fn",
        "CREATE TABLE qads2_sc (id INT PRIMARY KEY)",
        "INSERT INTO qads2_sc VALUES (1)",
        "CREATE FUNCTION qads2_sc_fn() RETURNS INT DETERMINISTIC NO SQL BEGIN SET SESSION foreign_key_checks = 0; RETURN 1; END",
    ] {
        db.query(s).await?;
    }
    let before = db
        .execute(&[Stmt::new("SELECT @@session.foreign_key_checks")], false)
        .await?[0]
        .rows
        .clone();
    let r = db
        .table_data(&req(
            &tref(Driver::Mysql, "qads2_sc"),
            vec![Filter {
                enabled: true,
                column: RAW_SQL.into(),
                op: FilterOp::Eq,
                value: "qads2_sc_fn() = 1".into(),
                value2: None,
            }],
            10,
        ))
        .await;
    let after = db
        .execute(&[Stmt::new("SELECT @@session.foreign_key_checks")], false)
        .await?[0]
        .rows
        .clone();
    println!(
        "raw filter: {:?}; admin foreign_key_checks before={before:?} after={after:?}",
        r.map(|r| r.total).map_err(|e| e.to_string())
    );
    db.query("DROP TABLE qads2_sc").await?;
    db.query("DROP FUNCTION qads2_sc_fn").await?;
    Ok(())
}
