//! Editing SQL query results: which columns are editable from protocol metadata, and writes that
//! touch exactly the rows shown. Live against the local test databases.
use anyhow::Result;
use telecat_core::{
    changes::{Cell, EditRow, to_edit_rows},
    result_edit::ResultEditPlan,
    types::*,
};
use telecat_db::Session;

fn config(d: Driver) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "result-edit".into();
    c.host = "127.0.0.1".into();
    let mysql = d == Driver::Mysql;
    c.port = if mysql { 3307 } else { 5434 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = if mysql { "zixma" } else { "main_db" }.into();
    c
}
const TABLES: [&str; 6] = [
    "telecat_re_v",
    "telecat_re_ord",
    "telecat_re_cust",
    "telecat_re_nokey",
    "telecat_re_comp",
    "telecat_re_gen",
];
/// The tests share the telecat_re_* tables, so they take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}
async fn setup(d: Driver) -> Result<Session> {
    let s = Session::connect_test(&config(d), "secret").await?;
    teardown(&s).await?;
    let gen_col = if d == Driver::Mysql {
        "g int GENERATED ALWAYS AS (id * 2) VIRTUAL"
    } else {
        "g int GENERATED ALWAYS AS (id * 2) STORED"
    };
    for sql in [
        "CREATE TABLE telecat_re_cust (id int PRIMARY KEY, name varchar(40))".to_owned(),
        "CREATE TABLE telecat_re_ord (id int PRIMARY KEY, cust_id int, note varchar(40))".into(),
        "CREATE TABLE telecat_re_nokey (a int, b varchar(10))".into(),
        "CREATE TABLE telecat_re_comp (a int, b int, v varchar(10), PRIMARY KEY (a, b))".into(),
        format!(
            "CREATE TABLE telecat_re_gen (id int PRIMARY KEY, {gen_col}, bin {})",
            if d == Driver::Mysql {
                "varbinary(4)"
            } else {
                "bytea"
            }
        ),
        "CREATE VIEW telecat_re_v AS SELECT id, name FROM telecat_re_cust".into(),
        "INSERT INTO telecat_re_cust VALUES (1, 'ann'), (2, 'bob'), (3, NULL)".into(),
        "INSERT INTO telecat_re_ord VALUES (10, 1, 'a'), (11, 1, 'b'), (12, 2, 'c')".into(),
        "INSERT INTO telecat_re_nokey VALUES (1, 'x')".into(),
        "INSERT INTO telecat_re_comp VALUES (1, 1, 'p'), (1, 2, 'q')".into(),
        "INSERT INTO telecat_re_gen (id) VALUES (1)".into(),
    ] {
        s.query(&sql).await?;
    }
    Ok(s)
}
async fn teardown(s: &Session) -> Result<()> {
    s.query("DROP VIEW IF EXISTS telecat_re_v").await?;
    for t in TABLES.iter().skip(1) {
        s.query(&format!("DROP TABLE IF EXISTS {t}")).await?;
    }
    Ok(())
}
async fn plan(s: &Session, sql: &str) -> Result<(ResultEditPlan, Vec<EditRow>)> {
    let r = s.query(sql).await?;
    let plan = s.result_edit_plan(sql, &r).await?;
    Ok((plan, to_edit_rows(&r.rows, None)))
}
async fn cust(s: &Session) -> Result<Vec<Vec<Option<String>>>> {
    Ok(s.query("SELECT id, name FROM telecat_re_cust ORDER BY id")
        .await?
        .rows)
}
fn row(id: &str, name: Option<&str>) -> Vec<Option<String>> {
    vec![Some(id.into()), name.map(str::to_owned)]
}

async fn rules(d: Driver) -> Result<()> {
    let s = setup(d).await?;
    let check =
        |sql: &'static str, editable: &'static [bool], delete: bool| (sql, editable, delete);
    for (sql, editable, delete) in [
        check("SELECT id, name FROM telecat_re_cust", &[false, true], true),
        check(
            "SELECT name, id AS ident FROM telecat_re_cust WHERE id > 0 ORDER BY id",
            &[true, false],
            true,
        ),
        check(
            "SELECT id, name AS n, UPPER(name) AS u, id + 0 AS x FROM telecat_re_cust",
            &[false, true, false, false],
            false,
        ),
        // A JOIN: only the side whose full key is in the result; never delete.
        check(
            "SELECT o.id, o.note, c.name FROM telecat_re_ord o JOIN telecat_re_cust c ON c.id = o.cust_id",
            &[false, true, false],
            false,
        ),
        // Fan-out: each customer shows once per order.
        check(
            "SELECT c.id, c.name, o.note FROM telecat_re_cust c JOIN telecat_re_ord o ON o.cust_id = c.id",
            &[false, true, false],
            false,
        ),
        check(
            "SELECT a, b, v FROM telecat_re_comp",
            &[false, false, true],
            true,
        ),
        check(
            "SELECT id, g, bin FROM telecat_re_gen",
            &[false, false, false],
            true,
        ),
    ] {
        let (p, _) = plan(&s, sql).await?;
        assert_eq!(p.editable, editable, "{d:?}: {sql}: {:?}", p.badge());
        assert_eq!(p.delete.is_ok(), delete, "{d:?}: {sql}: {:?}", p.delete);
    }
    for (sql, reason) in [
        (
            "SELECT name FROM telecat_re_cust",
            "telecat_re_cust's key column id is not in the result",
        ),
        (
            "SELECT a, v FROM telecat_re_comp",
            "telecat_re_comp's key column b is not in the result",
        ),
        (
            "SELECT a, b FROM telecat_re_nokey",
            "telecat_re_nokey has no primary key or unique NOT NULL key",
        ),
        (
            "SELECT id, name FROM telecat_re_v",
            "telecat_re_v is a view",
        ),
        ("SELECT DISTINCT id, name FROM telecat_re_cust", "DISTINCT"),
        (
            "SELECT id, name FROM telecat_re_cust GROUP BY id, name",
            "GROUP BY",
        ),
        ("SELECT COUNT(*) FROM telecat_re_cust", "aggregate"),
        (
            "SELECT id, name FROM telecat_re_cust UNION SELECT id, note FROM telecat_re_ord",
            "UNION/INTERSECT/EXCEPT",
        ),
        (
            "SELECT id, name FROM telecat_re_cust WHERE id IN (SELECT cust_id FROM telecat_re_ord)",
            "subquery",
        ),
        (
            "WITH x AS (SELECT id, name FROM telecat_re_cust) SELECT id, name FROM x",
            "not a SELECT",
        ),
        (
            "SELECT id, name, ROW_NUMBER() OVER (ORDER BY id) FROM telecat_re_cust",
            "window function",
        ),
        (
            "SELECT id, name FROM telecat_re_cust FOR UPDATE",
            "locking read",
        ),
        (
            "SELECT a.id, a.name, b.name FROM telecat_re_cust a JOIN telecat_re_cust b ON b.id = a.id",
            "telecat_re_cust appears more than once in the query",
        ),
        // A self-join whose sides show different columns: PostgreSQL reports only the table.
        (
            "SELECT b.id, a.name FROM telecat_re_cust a JOIN telecat_re_cust b ON b.id = a.id + 1",
            "telecat_re_cust appears more than once in the query",
        ),
        (
            "SELECT 1 AS id, 'x' AS name",
            "no column comes from a table",
        ),
    ] {
        let (p, _) = plan(&s, sql).await?;
        assert_eq!(p.read_only.as_deref(), Some(reason), "{d:?}: {sql}");
        assert!(
            p.editable.iter().all(|e| !e) && p.delete.is_err(),
            "{d:?}: {sql}"
        );
    }
    // Statements that aren't SELECTs.
    let sql = if d == Driver::Mysql {
        "SHOW TABLES"
    } else {
        "EXPLAIN SELECT id FROM telecat_re_cust"
    };
    let (p, _) = plan(&s, sql).await?;
    assert_eq!(p.read_only.as_deref(), Some("not a SELECT"));
    if d == Driver::Postgres {
        // Partitions and inheritance count as their root table; reads through a function aren't
        // attributable; float keys have no exact text.
        for sql in [
            "CREATE TABLE telecat_re_pt (id int PRIMARY KEY, name text) PARTITION BY RANGE (id)",
            "CREATE TABLE telecat_re_pt_lo PARTITION OF telecat_re_pt FOR VALUES FROM (0) TO (10)",
            "CREATE TABLE telecat_re_pt_hi PARTITION OF telecat_re_pt FOR VALUES FROM (10) TO (100)",
            "INSERT INTO telecat_re_pt VALUES (1, 'a'), (2, 'b'), (11, 'c')",
            "CREATE FUNCTION telecat_re_fn() RETURNS SETOF telecat_re_cust LANGUAGE sql AS 'SELECT * FROM telecat_re_cust'",
            "CREATE TABLE telecat_re_kf (k float8 PRIMARY KEY, v text, f float8)",
        ] {
            s.query(sql).await?;
        }
        for (sql, editable, reason) in [
            ("SELECT id, name FROM telecat_re_pt", true, None),
            ("SELECT id, name FROM telecat_re_pt_lo", true, None),
            (
                "SELECT a.id, b.name FROM telecat_re_pt a JOIN telecat_re_pt b ON b.id = a.id + 1",
                false,
                Some("telecat_re_pt appears more than once in the query"),
            ),
            (
                "SELECT id, name FROM telecat_re_fn()",
                false,
                Some("the query reads rows through a Function Scan"),
            ),
            (
                "SELECT k, v FROM telecat_re_kf",
                false,
                Some("telecat_re_kf's key type can't be matched exactly"),
            ),
        ] {
            let (p, _) = plan(&s, sql).await?;
            assert_eq!(
                p.editable.iter().any(|e| *e),
                editable,
                "{sql}: {:?}",
                p.read_only
            );
            assert_eq!(p.read_only.as_deref(), reason, "{sql}");
        }
        // Rounded float output: float columns can't be checked, so they stay read-only.
        s.query("CREATE TABLE telecat_re_f (id int PRIMARY KEY, f float8)")
            .await?;
        let (p, _) = plan(&s, "SELECT id, f FROM telecat_re_f").await?;
        assert_eq!(p.editable, [false, true]);
        s.query("SET extra_float_digits = -3").await?;
        let (p, _) = plan(&s, "SELECT id, f FROM telecat_re_f").await?;
        assert_eq!(p.editable, [false, false]);
        assert!(
            p.session
                .contains(&("extra_float_digits".to_owned(), "-3".to_owned()))
        );
        s.query("SET extra_float_digits = DEFAULT").await?;
        for sql in [
            "DROP TABLE telecat_re_pt",
            "DROP FUNCTION telecat_re_fn()",
            "DROP TABLE telecat_re_kf",
            "DROP TABLE telecat_re_f",
        ] {
            s.query(sql).await?;
        }
    }
    // A temporary table, which on MySQL hides the stored table of the same name.
    let name = if d == Driver::Mysql {
        "telecat_re_cust"
    } else {
        "telecat_re_tmp"
    };
    s.query(&format!(
        "CREATE TEMPORARY TABLE {name} (id int PRIMARY KEY, name varchar(40))"
    ))
    .await?;
    let (p, _) = plan(&s, &format!("SELECT id, name FROM {name}")).await?;
    assert_eq!(
        p.read_only,
        Some(format!("{name} is a temporary table")),
        "{d:?}"
    );
    s.query(&format!("DROP TEMPORARY TABLE IF EXISTS {name}").replace(
        "TEMPORARY TABLE",
        if d == Driver::Mysql {
            "TEMPORARY TABLE"
        } else {
            "TABLE"
        },
    ))
    .await?;
    teardown(&s).await
}
#[tokio::test]
async fn mysql_result_editability_rules() -> Result<()> {
    let _turn = serial();
    rules(Driver::Mysql).await
}
#[tokio::test]
async fn postgres_result_editability_rules() -> Result<()> {
    let _turn = serial();
    rules(Driver::Postgres).await
}

async fn writes(d: Driver) -> Result<()> {
    let s = setup(d).await?;
    let (p, mut rows) = plan(&s, "SELECT id, name FROM telecat_re_cust ORDER BY id").await?;
    // Edit, NULL and Thai; then commit in one transaction.
    p.stage(&mut rows, 0, 1, Cell::Text("ธนาคาร 😺".into()))
        .unwrap();
    p.stage(&mut rows, 1, 1, Cell::Null).unwrap();
    p.stage(&mut rows, 2, 1, Cell::Text(String::new())).unwrap();
    let st = p.statements(&rows)?;
    assert_eq!(st.len(), 3);
    s.execute_after_review(&st, true, false).await?;
    assert_eq!(
        cust(&s).await?,
        [
            row("1", Some("ธนาคาร 😺")),
            row("2", None),
            row("3", Some(""))
        ]
    );
    // Delete by key.
    let (p, mut rows) = plan(&s, "SELECT id, name FROM telecat_re_cust ORDER BY id").await?;
    p.delete(&mut rows, &[2]).unwrap();
    s.execute_after_review(&p.statements(&rows)?, true, false)
        .await?;
    assert_eq!(
        cust(&s).await?,
        [row("1", Some("ธนาคาร 😺")), row("2", None)]
    );

    // A row another session changed since it was shown: everything rolls back.
    let (p, mut rows) = plan(&s, "SELECT id, name FROM telecat_re_cust ORDER BY id").await?;
    p.stage(&mut rows, 0, 1, Cell::Text("first".into()))
        .unwrap();
    p.stage(&mut rows, 1, 1, Cell::Text("second".into()))
        .unwrap();
    s.query("UPDATE telecat_re_cust SET name = 'changed elsewhere' WHERE id = 2")
        .await?;
    let err = s
        .execute_after_review(&p.statements(&rows)?, true, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{d:?}: {err:#}");
    assert_eq!(
        cust(&s).await?,
        [
            row("1", Some("ธนาคาร 😺")),
            row("2", Some("changed elsewhere"))
        ],
        "{d:?}: 0 rows changed"
    );
    // A row another session deleted: everything rolls back.
    let (p, mut rows) = plan(&s, "SELECT id, name FROM telecat_re_cust ORDER BY id").await?;
    p.stage(&mut rows, 0, 1, Cell::Text("first".into()))
        .unwrap();
    p.delete(&mut rows, &[1]).unwrap();
    s.query("DELETE FROM telecat_re_cust WHERE id = 2").await?;
    let err = s
        .execute_after_review(&p.statements(&rows)?, true, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{d:?}: {err:#}");
    assert_eq!(
        cust(&s).await?,
        [row("1", Some("ธนาคาร 😺"))],
        "{d:?}: 0 rows changed"
    );

    // Composite key and a JOIN fan-out: one UPDATE per stored row.
    let (p, mut rows) = plan(&s, "SELECT a, b, v FROM telecat_re_comp ORDER BY b").await?;
    p.stage(&mut rows, 1, 2, Cell::Text("Q".into())).unwrap();
    s.execute_after_review(&p.statements(&rows)?, true, false)
        .await?;
    assert_eq!(
        s.query("SELECT v FROM telecat_re_comp ORDER BY b")
            .await?
            .rows,
        [[Some("p".to_owned())], [Some("Q".to_owned())]]
    );
    s.query("INSERT INTO telecat_re_cust VALUES (2, 'bob')")
        .await?;
    let (p, mut rows) = plan(
        &s,
        "SELECT c.id, c.name, o.note FROM telecat_re_cust c JOIN telecat_re_ord o ON o.cust_id = c.id ORDER BY o.id",
    )
    .await?;
    assert_eq!(rows.len(), 3, "customer 1 shows twice");
    p.stage(&mut rows, 1, 1, Cell::Text("ann 2".into()))
        .unwrap();
    assert_eq!(
        rows[0].cur[1],
        Cell::Text("ann 2".into()),
        "both copies change"
    );
    let st = p.statements(&rows)?;
    assert_eq!(st.len(), 1);
    s.execute_after_review(&st, true, false).await?;
    assert_eq!(
        cust(&s).await?,
        [row("1", Some("ann 2")), row("2", Some("bob"))]
    );
    teardown(&s).await
}
#[tokio::test]
async fn mysql_result_writes_touch_exactly_the_rows_shown() -> Result<()> {
    let _turn = serial();
    writes(Driver::Mysql).await
}
#[tokio::test]
async fn postgres_result_writes_touch_exactly_the_rows_shown() -> Result<()> {
    let _turn = serial();
    writes(Driver::Postgres).await
}
