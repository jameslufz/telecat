//! QA 0.3.0 regression tests: adversarial editable-result probes in throwaway databases qa030*.
use anyhow::Result;
use telecat_core::{
    changes::{Cell, EditRow, inline_params, to_edit_rows},
    result_edit::ResultEditPlan,
    types::*,
};
use telecat_db::Session;

fn config(d: Driver, db: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "qa030".into();
    c.host = "127.0.0.1".into();
    let mysql = d == Driver::Mysql;
    c.port = if mysql { 3307 } else { 5434 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = db.into();
    c
}
async fn rows(s: &Session, sql: &str) -> Vec<Vec<Option<String>>> {
    s.query(sql).await.map(|r| r.rows).unwrap_or_default()
}
struct Outcome {
    editable: Vec<bool>,
    delete: bool,
    reason: Option<String>,
    stmts: Vec<String>,
    result: Option<String>,
}
/// Runs `sql` on `s`, builds the plan, stages `value` into the first row matching `pick`
/// (column `col`) — or deletes it when `value` is None and `delete` — and commits.
async fn attempt(
    s: &Session,
    sql: &str,
    pick: impl Fn(&[Option<String>]) -> bool,
    col: usize,
    value: Option<Cell>,
) -> Outcome {
    let r = match s.query(sql).await {
        Ok(r) => r,
        Err(e) => {
            return Outcome {
                editable: vec![],
                delete: false,
                reason: Some(format!("query failed: {e}")),
                stmts: vec![],
                result: None,
            };
        }
    };
    let plan: ResultEditPlan = match s.result_edit_plan(sql, &r).await {
        Ok(p) => p,
        Err(e) => {
            return Outcome {
                editable: vec![],
                delete: false,
                reason: Some(format!("plan failed: {e}")),
                stmts: vec![],
                result: None,
            };
        }
    };
    let mut er: Vec<EditRow> = to_edit_rows(&r.rows, None);
    let mut out = Outcome {
        editable: plan.editable.clone(),
        delete: plan.delete.is_ok(),
        reason: plan.read_only.clone(),
        stmts: vec![],
        result: None,
    };
    let Some(row) = r.rows.iter().position(|x| pick(x)) else {
        out.result = Some("no row picked".into());
        return out;
    };
    let staged = match value {
        Some(v) => plan.stage(&mut er, row, col, v),
        None => plan.delete(&mut er, &[row]),
    };
    if let Err(e) = staged {
        out.result = Some(format!("refused to stage: {e}"));
        return out;
    }
    match plan.statements(&er) {
        Ok(st) => {
            out.stmts = st.iter().map(|x| inline_params(s.driver, x)).collect();
            // As the app commits: with the settings of the session the query ran in.
            out.result = Some(
                match s
                    .execute_after_review_with(&st, true, false, &plan.session)
                    .await
                {
                    Ok(_) => "COMMITTED".into(),
                    Err(e) => format!("ERROR {e}"),
                },
            );
        }
        Err(e) => out.result = Some(format!("statements refused: {e}")),
    }
    out
}
fn show(name: &str, o: &Outcome, before: &[Vec<Option<String>>], after: &[Vec<Option<String>>]) {
    eprintln!(
        "\n[{name}] editable={:?} delete={} ro={:?}\n  stmts={:?}\n  result={:?}\n  before={:?}\n  after ={:?}",
        o.editable, o.delete, o.reason, o.stmts, o.result, before, after
    );
}
fn changed(before: &[Vec<Option<String>>], after: &[Vec<Option<String>>]) -> usize {
    before.iter().zip(after).filter(|(a, b)| a != b).count() + before.len().abs_diff(after.len())
}
const T: &str = "SELECT id, name, code FROM t ORDER BY id";

async fn mysql_case(
    s: &Session,
    name: &str,
    sql: &str,
    pick: impl Fn(&[Option<String>]) -> bool,
    col: usize,
    value: Option<Cell>,
    expect_row: Option<&str>,
    bad: &mut Vec<String>,
) {
    let before = rows(s, T).await;
    let o = attempt(s, sql, pick, col, value).await;
    let after = rows(s, T).await;
    show(name, &o, &before, &after);
    let n = changed(&before, &after);
    if n > 1 {
        bad.push(format!("{name}: {n} rows changed"));
    }
    if n == 1 {
        let idx = before
            .iter()
            .zip(&after)
            .position(|(a, b)| a != b)
            .unwrap_or(usize::MAX);
        let id = before.get(idx).and_then(|r| r[0].clone());
        if expect_row != Some("?") && (expect_row.is_none() || id.as_deref() != expect_row) {
            bad.push(format!(
                "{name}: changed row id={id:?}, expected {expect_row:?}"
            ));
        }
    }
    // restore
    s.query("DELETE FROM t").await.unwrap();
    s.query("INSERT INTO t VALUES (1,'x','a'),(2,'x','b'),(3,'y','c'),(12,'x','d')")
        .await
        .unwrap();
}

#[tokio::test]
async fn qa_mysql_result_edit_attacks() -> Result<()> {
    let admin = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    for q in [
        "DROP DATABASE IF EXISTS qa030",
        "DROP DATABASE IF EXISTS qa030b",
        "CREATE DATABASE qa030",
        "CREATE DATABASE qa030b",
    ] {
        admin.query(q).await?;
    }
    drop(admin);
    let s = Session::connect_test(&config(Driver::Mysql, "qa030"), "secret").await?;
    for q in [
        "CREATE TABLE t (id int PRIMARY KEY, name varchar(20), code char(5))",
        "INSERT INTO t VALUES (1,'x','a'),(2,'x','b'),(3,'y','c'),(12,'x','d')",
        "CREATE TABLE u (id int PRIMARY KEY, note varchar(20))",
        "INSERT INTO u VALUES (1,'u1'),(5,'u5'),(12,'u12')",
        "CREATE TABLE c (a int, b int, v varchar(10), PRIMARY KEY (a,b))",
        "INSERT INTO c VALUES (1,2,'p'),(2,1,'q')",
        "CREATE VIEW v AS SELECT id, name FROM t",
        "CREATE TABLE qa030b.t (id int PRIMARY KEY, name varchar(20), code char(5))",
        "INSERT INTO qa030b.t VALUES (1,'B1','a'),(12,'B12','d')",
    ] {
        s.query(q).await?;
    }
    let mut bad = vec![];
    let id = |v: &'static str| move |r: &[Option<String>]| r[0].as_deref() == Some(v);
    let txt = |s: &str| Some(Cell::Text(s.into()));
    // Expressions on the key that might keep org_name.
    for (n, q) in [
        (
            "cast-char1",
            "SELECT CAST(id AS CHAR(1)) AS id, name FROM t",
        ),
        ("left1", "SELECT LEFT(id, 1) AS id, name FROM t"),
        ("coalesce", "SELECT COALESCE(id) AS id, name FROM t"),
        ("ifnull", "SELECT IFNULL(id, 0) AS id, name FROM t"),
        ("div", "SELECT id DIV 10 AS id, name FROM t"),
        ("convert", "SELECT CONVERT(id, CHAR(1)) AS id, name FROM t"),
        ("if", "SELECT IF(id > 10, 1, id) AS id, name FROM t"),
        (
            "case",
            "SELECT CASE WHEN id = 12 THEN 1 ELSE id END AS id, name FROM t",
        ),
        ("least", "SELECT LEAST(id, 1) AS id, name FROM t"),
    ] {
        // pick the displayed "1" row that is NOT id 1 when possible: edit row shown last with id "1"
        let r = s.query(q).await?;
        let last1 = r.rows.iter().rposition(|x| x[0].as_deref() == Some("1"));
        let target_real = last1.map(|i| ["1", "2", "3", "12"][i.min(3)]);
        let before = rows(&s, T).await;
        let o = attempt(
            &s,
            q,
            |x| x[0].as_deref() == Some("1") && true,
            1,
            txt("HIT"),
        )
        .await;
        let after = rows(&s, T).await;
        show(n, &o, &before, &after);
        if changed(&before, &after) > 0 {
            bad.push(format!(
                "{n}: expression on key was editable and changed {:?} (display-target {:?})",
                after, target_real
            ));
        }
        s.query("DELETE FROM t").await?;
        s.query("INSERT INTO t VALUES (1,'x','a'),(2,'x','b'),(3,'y','c'),(12,'x','d')")
            .await?;
    }
    // Expressions on the edited (non-key) column keeping org_name.
    mysql_case(
        &s,
        "name-collate",
        "SELECT id, name COLLATE utf8mb4_bin AS name FROM t",
        id("2"),
        1,
        txt("C2"),
        Some("2"),
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "name-left",
        "SELECT id, LEFT(name, 0) AS name FROM t",
        id("2"),
        1,
        txt("L2"),
        Some("2"),
        &mut bad,
    )
    .await;
    // Alias swaps.
    mysql_case(
        &s,
        "swap",
        "SELECT name AS id, id AS name, code FROM t",
        |r| r[1].as_deref() == Some("3"),
        0,
        txt("SW3"),
        Some("3"),
        &mut bad,
    )
    .await;
    // Self-joins.
    mysql_case(
        &s,
        "selfjoin-both",
        "SELECT a.id, b.name FROM t a JOIN t b ON b.id = a.id + 1",
        id("1"),
        1,
        txt("SJ"),
        None,
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "selfjoin-star",
        "SELECT * FROM t a JOIN t b ON b.id = a.id + 1",
        id("1"),
        1,
        txt("SJ"),
        None,
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "selfjoin-noalias-db",
        "SELECT qa030.t.id, x.name FROM qa030.t JOIN t x ON x.id = qa030.t.id + 1",
        id("1"),
        1,
        txt("SJ"),
        None,
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "view-join",
        "SELECT t.id, v.name FROM t JOIN v ON v.id = t.id + 1",
        id("1"),
        1,
        txt("VJ"),
        None,
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "view",
        "SELECT id, name FROM v",
        id("2"),
        1,
        txt("V"),
        None,
        &mut bad,
    )
    .await;
    // Joins with USING / NATURAL / outer.
    mysql_case(
        &s,
        "right-using",
        "SELECT * FROM t RIGHT JOIN u USING (id)",
        |r| r[0].as_deref() == Some("5"),
        1,
        txt("RU"),
        None,
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "left-join-null",
        "SELECT u.id, u.note, t.id AS tid, t.name FROM u LEFT JOIN t ON t.id = u.id",
        |r| r[0].as_deref() == Some("5"),
        3,
        txt("LJ"),
        None,
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "left-join-ok",
        "SELECT u.id, u.note, t.id AS tid, t.name FROM u LEFT JOIN t ON t.id = u.id",
        |r| r[0].as_deref() == Some("12"),
        3,
        txt("LJ12"),
        Some("12"),
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "fanout",
        "SELECT t.id, t.name, u.note FROM t JOIN u ON u.id >= t.id",
        |r| r[0].as_deref() == Some("1"),
        1,
        txt("FO"),
        Some("1"),
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "table-derived",
        "SELECT * FROM (TABLE t) AS d",
        id("3"),
        1,
        txt("TD"),
        Some("3"),
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "values-join",
        "SELECT t.id, t.name FROM t JOIN (VALUES ROW(1), ROW(1)) AS x(k) ON x.k = t.id",
        id("1"),
        1,
        txt("VAL"),
        Some("1"),
        &mut bad,
    )
    .await;
    mysql_case(
        &s,
        "order-rand-limit",
        "SELECT id, name FROM t ORDER BY RAND() LIMIT 2 OFFSET 1",
        |_| true,
        1,
        txt("RND"),
        Some("?"),
        &mut bad,
    )
    .await;
    // Other database with same table name; session USE.
    let o = attempt(
        &s,
        "SELECT id, name FROM qa030b.t",
        id("12"),
        1,
        txt("OTHERDB"),
    )
    .await;
    eprintln!("\n[otherdb] {:?} {:?} {:?}", o.editable, o.stmts, o.result);
    eprintln!(
        "  qa030b.t={:?} qa030.t={:?}",
        rows(&s, "SELECT * FROM qa030b.t ORDER BY id").await,
        rows(&s, T).await
    );
    s.query("USE qa030b").await?;
    let o = attempt(&s, "SELECT id, name FROM t", id("1"), 1, txt("AFTER-USE")).await;
    eprintln!(
        "\n[after-use] {:?} {:?} {:?}",
        o.editable, o.stmts, o.result
    );
    eprintln!(
        "  qa030b.t={:?} qa030.t={:?}",
        rows(&s, "SELECT * FROM qa030b.t ORDER BY id").await,
        rows(&s, "SELECT * FROM qa030.t ORDER BY id").await
    );
    if rows(&s, "SELECT name FROM qa030.t WHERE id=1").await != vec![vec![Some("x".to_owned())]] {
        bad.push("after-use: wrote to qa030.t instead of qa030b.t".into());
    }
    s.query("USE qa030").await?;
    // Temp table shadowing.
    s.query("CREATE TEMPORARY TABLE t (id int PRIMARY KEY, name varchar(20), code char(5))")
        .await?;
    s.query("INSERT INTO t VALUES (2,'TEMP','z')").await?;
    let o = attempt(&s, "SELECT id, name FROM t", id("2"), 1, txt("TMP")).await;
    eprintln!(
        "\n[temp-shadow] {:?} {:?} {:?}",
        o.editable, o.reason, o.result
    );
    s.query("DROP TEMPORARY TABLE t").await?;
    if rows(&s, "SELECT name FROM t WHERE id=2").await != vec![vec![Some("x".to_owned())]] {
        bad.push("temp-shadow: wrote to the stored table".into());
    }
    // ANSI_QUOTES in the editor session.
    s.query("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')")
        .await?;
    mysql_case(
        &s,
        "ansi-quotes",
        "SELECT \"id\", \"name\" FROM \"t\"",
        id("3"),
        1,
        txt("AQ'\"`\\"),
        Some("3"),
        &mut bad,
    )
    .await;
    s.query("SET SESSION sql_mode = DEFAULT").await?;
    // Key types.
    for q in [
        "CREATE TABLE kf (k float PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kd (k double PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kts (k timestamp PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kdec (k decimal(30,10) PRIMARY KEY, v varchar(10))",
        "CREATE TABLE ku (k bigint unsigned PRIMARY KEY, v varchar(10))",
        "CREATE TABLE ktxt (k varchar(10) COLLATE utf8mb4_0900_ai_ci PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kpad (k varchar(10) COLLATE utf8mb4_general_ci PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kdt (k datetime(6) PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kuuid (k binary(16) PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kchar (k char(36) PRIMARY KEY, v varchar(10))",
        "CREATE TABLE kenum (k enum('a','b') PRIMARY KEY, v varchar(10))",
        "INSERT INTO kf VALUES (0.1,'a'),(0.2,'b')",
        "INSERT INTO kd VALUES (0.1,'a'),(0.30000000000000004,'b'),(0.3,'c')",
        "INSERT INTO kts VALUES ('2024-01-01 00:00:00','a')",
        "INSERT INTO kdec VALUES (1.5,'a'),(1.5000000001,'b')",
        "INSERT INTO ku VALUES (18446744073709551615,'max'),(18446744073709551614,'max-1'),(9223372036854775808,'2^63')",
        "INSERT INTO ktxt VALUES ('a','plain'),('a ','space'),('ñ','enye')",
        "INSERT INTO kpad VALUES ('B','upper'),('é','e-acute')",
        "INSERT INTO kdt VALUES ('2024-01-01 00:00:00.000001','a'),('2024-01-01 00:00:00','b')",
        "INSERT INTO kchar VALUES ('6f1c9a7e-0000-0000-0000-000000000001','u1'),('6f1c9a7e-0000-0000-0000-000000000002','u2')",
        "INSERT INTO kenum VALUES ('a','ea'),('b','eb')",
    ] {
        s.query(q).await?;
    }
    for (t, pick) in [
        ("kf", "0.1"),
        ("kd", "0.3"),
        ("kts", "2024-01-01 00:00:00"),
        ("kdec", "1.5000000000"),
        ("ku", "18446744073709551614"),
        ("ktxt", "a "),
        ("kpad", "B"),
        ("kdt", "2024-01-01 00:00:00"),
        ("kchar", "6f1c9a7e-0000-0000-0000-000000000002"),
        ("kenum", "b"),
    ] {
        let q = format!("SELECT k, v FROM {t}");
        let all = format!("SELECT k, v FROM {t} ORDER BY v");
        let before = rows(&s, &all).await;
        let pick_s = pick.to_owned();
        let o = attempt(
            &s,
            &q,
            move |r| r[0].as_deref() == Some(pick_s.as_str()),
            1,
            txt("EDITED"),
        )
        .await;
        let after = rows(&s, &all).await;
        show(&format!("key-{t}"), &o, &before, &after);
        let hits: Vec<_> = after
            .iter()
            .filter(|r| r[1].as_deref() == Some("EDITED"))
            .collect();
        if hits.len() > 1 || hits.iter().any(|r| r[0].as_deref() != Some(pick)) {
            bad.push(format!(
                "key-{t}: wrong row edited {hits:?} (picked {pick})"
            ));
        }
    }
    // Composite key with one key column aliased.
    let before = rows(&s, "SELECT * FROM c ORDER BY a").await;
    let o = attempt(
        &s,
        "SELECT b AS a, a AS b, v FROM c",
        |r| r[0].as_deref() == Some("2"),
        2,
        txt("CK"),
    )
    .await;
    let after = rows(&s, "SELECT * FROM c ORDER BY a").await;
    show("composite-alias-swap", &o, &before, &after);
    if after
        .iter()
        .any(|r| r[2].as_deref() == Some("CK") && r[0].as_deref() != Some("1"))
    {
        bad.push("composite-alias-swap: wrong row".into());
    }
    // Session time zone vs admin connection: TIMESTAMP non-key column, NULL original.
    s.query("CREATE TABLE tz (id int PRIMARY KEY, ts timestamp NULL, ts2 timestamp NULL)")
        .await?;
    s.query("INSERT INTO tz VALUES (1, NULL, '2024-01-01 00:00:00')")
        .await?;
    s.query("SET time_zone = '+07:00'").await?;
    let o = attempt(
        &s,
        "SELECT id, ts FROM tz",
        id("1"),
        1,
        txt("2024-06-01 12:00:00"),
    )
    .await;
    let shown = rows(&s, "SELECT ts FROM tz").await;
    eprintln!(
        "\n[tz-null] {:?} {:?} shown in +07 session: {shown:?}",
        o.stmts, o.result
    );
    if shown != vec![vec![Some("2024-06-01 12:00:00".to_owned())]] {
        bad.push(format!(
            "tz-null: typed 2024-06-01 12:00:00 in a +07:00 session, session now shows {shown:?}"
        ));
    }
    let o = attempt(
        &s,
        "SELECT id, ts2 FROM tz",
        id("1"),
        1,
        txt("2024-06-01 12:00:00"),
    )
    .await;
    eprintln!(
        "[tz-nonnull] {:?} {:?} shown: {:?}",
        o.stmts,
        o.result,
        rows(&s, "SELECT ts2 FROM tz").await
    );
    s.query("SET time_zone = DEFAULT").await?;
    // Concurrent change / delete between read and commit -> rollback of the whole batch.
    let r = s.query("SELECT id, name FROM t").await?;
    let plan = s.result_edit_plan("SELECT id, name FROM t", &r).await?;
    let mut er = to_edit_rows(&r.rows, None);
    let i1 = r
        .rows
        .iter()
        .position(|x| x[0].as_deref() == Some("1"))
        .unwrap();
    let i3 = r
        .rows
        .iter()
        .position(|x| x[0].as_deref() == Some("3"))
        .unwrap();
    plan.stage(&mut er, i1, 1, Cell::Text("C1".into())).unwrap();
    plan.stage(&mut er, i3, 1, Cell::Text("C3".into())).unwrap();
    let st = plan.statements(&er)?;
    let other = Session::connect_test(&config(Driver::Mysql, "qa030"), "secret").await?;
    other
        .query("UPDATE t SET name = 'changed' WHERE id = 3")
        .await?;
    let res = s.execute_after_review(&st, true, false).await;
    let now = rows(&s, T).await;
    eprintln!(
        "\n[concurrent] {:?} -> {now:?}",
        res.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    if res.is_ok() || now.iter().any(|r| r[1].as_deref() == Some("C1")) {
        bad.push("concurrent: partial or overwriting commit".into());
    }
    let admin = Session::connect_test(&config(Driver::Mysql, "zixma"), "secret").await?;
    admin.query("DROP DATABASE qa030").await?;
    admin.query("DROP DATABASE qa030b").await?;
    eprintln!("\nBAD: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}

#[tokio::test]
async fn qa_postgres_result_edit_attacks() -> Result<()> {
    let admin = Session::connect_test(&config(Driver::Postgres, "main_db"), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa030").await?;
    admin.query("CREATE DATABASE qa030").await?;
    let s = Session::connect_test(&config(Driver::Postgres, "qa030"), "secret").await?;
    for q in [
        "CREATE TABLE t (id int PRIMARY KEY, name text, code text)",
        "INSERT INTO t VALUES (1,'x','a'),(2,'x','b'),(3,'y','c'),(12,'x','d')",
        "CREATE TABLE pt (id int, name text, PRIMARY KEY (id)) PARTITION BY RANGE (id)",
        "CREATE TABLE pt_lo PARTITION OF pt FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE pt_hi PARTITION OF pt FOR VALUES FROM (10) TO (100)",
        "INSERT INTO pt VALUES (1,'same'),(2,'same'),(11,'same'),(12,'other')",
        "CREATE TABLE par (id int PRIMARY KEY, name text)",
        "CREATE TABLE chi () INHERITS (par)",
        "INSERT INTO par VALUES (5,'p')",
        "INSERT INTO chi VALUES (5,'p'),(6,'c')",
        "CREATE VIEW v AS SELECT id, name FROM t",
        "CREATE SCHEMA other",
        "CREATE TABLE other.t (id int PRIMARY KEY, name text, code text)",
        "INSERT INTO other.t VALUES (1,'O1','a')",
        "CREATE TABLE kf (k float8 PRIMARY KEY, v text)",
        "INSERT INTO kf VALUES (0.3,'exact'),(0.30000000000000004,'inexact')",
        "CREATE TABLE ktz (k timestamptz PRIMARY KEY, v text)",
        "INSERT INTO ktz VALUES ('2024-01-01 00:00:00+00','a'),('2024-01-01 07:00:00+00','b')",
        "CREATE TABLE kmisc (u uuid PRIMARY KEY, n numeric, s text UNIQUE NOT NULL, v text)",
        "INSERT INTO kmisc VALUES ('00000000-0000-0000-0000-000000000001',1.0,'a ','one'),('00000000-0000-0000-0000-000000000002',1.00,'a','two')",
        "CREATE TABLE tz (id int PRIMARY KEY, ts timestamptz, ts2 timestamp)",
        "INSERT INTO tz VALUES (1, NULL, NULL)",
        "CREATE TABLE u (id int PRIMARY KEY, note text)",
        "INSERT INTO u VALUES (1,'u1'),(5,'u5')",
    ] {
        s.query(q).await?;
    }
    let mut bad: Vec<String> = vec![];
    let txt = |s: &str| Some(Cell::Text(s.into()));
    macro_rules! case {
        ($name:expr, $snap:expr, $sql:expr, $pick:expr, $col:expr, $val:expr) => {{
            let before = rows(&s, $snap).await;
            let o = attempt(&s, $sql, $pick, $col, $val).await;
            let after = rows(&s, $snap).await;
            show($name, &o, &before, &after);
            (o, before, after)
        }};
    }
    // Partitioned table self-join: a.id with b.name (b = a + 1 row).
    let (o, before, after) = case!(
        "pt-selfjoin",
        "SELECT * FROM pt ORDER BY id",
        "SELECT a.id, b.name FROM pt a JOIN pt b ON b.id = a.id + 1",
        |r| r[0].as_deref() == Some("1"),
        1,
        txt("PT-HIT")
    );
    let _ = o;
    if before != after {
        bad.push(format!("pt-selfjoin: {after:?}"));
    }
    let (_, before, after) = case!(
        "pt-selfjoin-10",
        "SELECT * FROM pt ORDER BY id",
        "SELECT a.id, b.name FROM pt a JOIN pt b ON b.id = a.id + 10",
        |r| r[0].as_deref() == Some("1"),
        1,
        txt("PT10")
    );
    if before != after {
        bad.push(format!(
            "pt-selfjoin-10 (row 11's name shown next to id 1): {after:?}"
        ));
    }
    let (_, before, after) = case!(
        "plain-selfjoin",
        "SELECT * FROM t ORDER BY id",
        "SELECT a.id, b.name FROM t a JOIN t b ON b.id = a.id + 1",
        |r| r[0].as_deref() == Some("1"),
        1,
        txt("SJ")
    );
    if before != after {
        bad.push(format!("plain-selfjoin: {after:?}"));
    }
    let (_, before, after) = case!(
        "in-subquery-self",
        "SELECT * FROM t ORDER BY id",
        "SELECT id, name FROM t WHERE id IN (1, 2)",
        |r| r[0].as_deref() == Some("2"),
        1,
        txt("OK2")
    );
    let _ = (before, after);
    // Inheritance: parent and child share id 5.
    let (_, before, after) = case!(
        "inherit-edit-parent",
        "SELECT tableoid::regclass::text, * FROM par ORDER BY 1, id",
        "SELECT id, name FROM par",
        |r| r[0].as_deref() == Some("5"),
        1,
        txt("INH")
    );
    if changed(&before, &after) > 1 {
        bad.push("inherit: two rows changed".into());
    }
    let (_, before, after) = case!(
        "inherit-delete",
        "SELECT tableoid::regclass::text, * FROM par ORDER BY 1, id",
        "SELECT id, name FROM par",
        |r| r[0].as_deref() == Some("6"),
        0,
        None
    );
    if after.len() + 1 != before.len() {
        bad.push("inherit-delete: not exactly one row deleted".into());
    }
    // Views, functions, casts, collations.
    for (n, q) in [
        ("view", "SELECT id, name FROM v"),
        ("coalesce", "SELECT id, COALESCE(name) AS name FROM t"),
        ("collate", "SELECT id, name COLLATE \"C\" AS name FROM t"),
        ("whole-row", "SELECT id, t AS name FROM t"),
    ] {
        let (o, before, after) = case!(
            n,
            "SELECT * FROM t ORDER BY id",
            q,
            |r| r[0].as_deref() == Some("2"),
            1,
            txt("FN")
        );
        let _ = o;
        if before != after {
            bad.push(format!(
                "{n}: editable through an expression/view: {after:?}"
            ));
        }
    }
    // Alias swap, LEFT JOIN null key, fan-out.
    case!(
        "swap",
        "SELECT * FROM t ORDER BY id",
        "SELECT name AS id, id AS name FROM t",
        |r| r[1].as_deref() == Some("3"),
        0,
        txt("SW3")
    );
    case!(
        "left-null",
        "SELECT * FROM t ORDER BY id",
        "SELECT u.id, t.id AS tid, t.name FROM u LEFT JOIN t ON t.id = u.id",
        |r| r[0].as_deref() == Some("5"),
        2,
        txt("LN")
    );
    case!(
        "fanout",
        "SELECT * FROM t ORDER BY id",
        "SELECT t.id, t.name FROM t, u",
        |r| r[0].as_deref() == Some("3"),
        1,
        txt("FO")
    );
    // search_path shadowing.
    s.query("SET search_path = other, public").await?;
    let (_, _, after) = case!(
        "search-path",
        "SELECT * FROM other.t ORDER BY id",
        "SELECT id, name FROM t",
        |r| r[0].as_deref() == Some("1"),
        1,
        txt("SP")
    );
    let public = rows(&s, "SELECT name FROM public.t WHERE id = 1").await;
    eprintln!("  public.t id1 = {public:?}, other.t = {after:?}");
    if public != vec![vec![Some("x".to_owned())]] {
        bad.push("search-path: wrote public.t".into());
    }
    s.query("SET search_path = public").await?;
    // Temp table shadowing.
    s.query("CREATE TEMP TABLE t (id int PRIMARY KEY, name text, code text)")
        .await?;
    s.query("INSERT INTO t VALUES (2, 'TEMP', 'z')").await?;
    case!(
        "temp",
        "SELECT * FROM public.t ORDER BY id",
        "SELECT id, name FROM t",
        |r| r[0].as_deref() == Some("2"),
        1,
        txt("TMP")
    );
    s.query("DROP TABLE pg_temp.t").await?;
    // Float key with lossy output in the editor session.
    s.query("SET extra_float_digits = -3").await?;
    let (_, _, after) = case!(
        "float-key-lossy",
        "SELECT k::text, v FROM kf ORDER BY v",
        "SELECT k, v FROM kf",
        |r| r[1].as_deref() == Some("inexact"),
        1,
        txt("EDITED")
    );
    let (_, _, afterd) = case!(
        "float-key-lossy-delete",
        "SELECT k::text, v FROM kf ORDER BY v",
        "SELECT k, v FROM kf",
        |r| r[1].as_deref() == Some("inexact"),
        0,
        None
    );
    s.query("SET extra_float_digits = DEFAULT").await?;
    let left = rows(&s, "SELECT k::text, v FROM kf ORDER BY k").await;
    eprintln!("  kf after deleting the 'inexact' row (full digits) = {left:?}");
    if !left.iter().any(|r| r[1].as_deref() == Some("exact")) {
        bad.push(format!("float-key-lossy-delete: user deleted the 'inexact' row, the 'exact' row was deleted: {left:?} {afterd:?}"));
    }
    let after2 = rows(&s, "SELECT k::text, v FROM kf ORDER BY k").await;
    eprintln!("  kf now (full digits) = {after2:?}");
    if after2
        .iter()
        .any(|r| r[1].as_deref() == Some("EDITED") && r[0].as_deref() == Some("0.3"))
    {
        bad.push(format!("float-key-lossy: edited the 0.3 row while the user edited 0.30000000000000004's row: {after:?}"));
    }
    // timestamptz key with session time zone.
    s.query("SET timezone = 'Asia/Bangkok'").await?;
    case!(
        "tstz-key",
        "SELECT k::text, v FROM ktz ORDER BY k",
        "SELECT k, v FROM ktz",
        |r| r[1].as_deref() == Some("b"),
        1,
        txt("TZ")
    );
    // NULL timestamp edits typed in the session's zone.
    let o = attempt(
        &s,
        "SELECT id, ts FROM tz",
        |r| r[0].as_deref() == Some("1"),
        1,
        txt("2024-06-01 12:00:00"),
    )
    .await;
    let shown = rows(&s, "SELECT ts::text FROM tz").await;
    eprintln!(
        "\n[tz-null-pg] {:?} {:?} shown in Bangkok: {shown:?}",
        o.stmts, o.result
    );
    if shown != vec![vec![Some("2024-06-01 12:00:00+07".to_owned())]] {
        bad.push(format!(
            "tz-null-pg: typed 12:00 in Asia/Bangkok session, now shows {shown:?}"
        ));
    }
    s.query("SET timezone = DEFAULT").await?;
    // DateStyle in the editor session; timestamp edit.
    s.query("SET DateStyle = 'SQL, DMY'").await?;
    let o = attempt(
        &s,
        "SELECT id, ts2 FROM tz",
        |r| r[0].as_deref() == Some("1"),
        1,
        txt("02/03/2024 10:00:00"),
    )
    .await;
    let shown = rows(&s, "SELECT to_char(ts2, 'YYYY-MM-DD HH24:MI:SS') FROM tz").await;
    eprintln!(
        "[datestyle-null] {:?} {:?} stored (ISO): {shown:?}",
        o.stmts, o.result
    );
    if shown != vec![vec![Some("2024-03-02 10:00:00".to_owned())]] {
        bad.push(format!(
            "datestyle: typed 02/03/2024 (DMY session) stored as {shown:?}"
        ));
    }
    s.query("SET DateStyle = DEFAULT").await?;
    // uuid / numeric / text keys.
    case!(
        "uuid",
        "SELECT u::text, v FROM kmisc ORDER BY v",
        "SELECT u, v FROM kmisc",
        |r| r[1].as_deref() == Some("two"),
        1,
        txt("U2")
    );
    case!(
        "text-space",
        "SELECT s, v FROM kmisc ORDER BY v",
        "SELECT s, v FROM kmisc",
        |r| r[1].as_deref() == Some("one"),
        1,
        txt("S1")
    );
    // Concurrent delete.
    let r = s.query("SELECT id, name FROM t").await?;
    let plan = s.result_edit_plan("SELECT id, name FROM t", &r).await?;
    let mut er = to_edit_rows(&r.rows, None);
    let i1 = r
        .rows
        .iter()
        .position(|x| x[0].as_deref() == Some("1"))
        .unwrap();
    let i3 = r
        .rows
        .iter()
        .position(|x| x[0].as_deref() == Some("3"))
        .unwrap();
    plan.stage(&mut er, i1, 1, Cell::Text("C1".into())).unwrap();
    plan.delete(&mut er, &[i3]).ok();
    let st = plan.statements(&er)?;
    let other = Session::connect_test(&config(Driver::Postgres, "qa030"), "secret").await?;
    other.query("DELETE FROM t WHERE id = 3").await?;
    let res = s.execute_after_review(&st, true, false).await;
    let now = rows(&s, "SELECT * FROM t ORDER BY id").await;
    eprintln!(
        "\n[concurrent-pg] {:?} {:?} -> {now:?}",
        st.iter()
            .map(|x| inline_params(Driver::Postgres, x))
            .collect::<Vec<_>>(),
        res.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    if res.is_ok() || now.iter().any(|r| r[1].as_deref() == Some("C1")) {
        bad.push("concurrent-pg: partial commit".into());
    }
    drop(s);
    drop(other);
    admin.query("DROP DATABASE qa030 WITH (FORCE)").await?;
    eprintln!("\nBAD-PG: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}
