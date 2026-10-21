//! Live regression tests for the data-safety audit, against
//! the throwaway test containers only (127.0.0.1:3307 / 127.0.0.1:5434, enforced by
//! Session::connect_test). Scratch objects use the `qafix_` prefix and are dropped.
//! cargo test -p telecat-db --test data_safety_fixes -- --test-threads=1
use anyhow::Result;
use serde_json::json;
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
async fn load(db: &Session, t: &TableRef, raw_filter: Option<&str>) -> Result<TableDataResult> {
    db.table_data(&TableDataRequest {
        table: t.clone(),
        filters: raw_filter
            .map(|v| Filter {
                enabled: true,
                column: RAW_SQL.into(),
                op: FilterOp::Eq,
                value: v.into(),
                value2: None,
            })
            .into_iter()
            .collect(),
        combine: Combine::And,
        order_by: None,
        limit: 1000,
        offset: 0,
    })
    .await
}
fn edit_rows(data: &TableDataResult) -> (TableMeta, Vec<EditRow>) {
    (
        TableMeta {
            columns: data.result.columns.clone(),
            primary_key: data.primary_key.clone(),
            binary: data.result.binary.clone(),
        },
        to_edit_rows(&data.result.rows, data.row_ids.as_deref()),
    )
}
/// Same path as TableEditor::groups + execute_reviewed_part for a data unit.
async fn commit(
    db: &Session,
    t: &TableRef,
    data: &TableDataResult,
    edit: impl FnOnce(&mut Vec<EditRow>),
) -> Result<()> {
    let (meta, mut er) = edit_rows(data);
    edit(&mut er);
    let st = data_statements(db.driver, t, &meta, &er)?;
    db.execute_after_review(&st, true, false).await.map(|_| ())
}
fn row_with(er: &[EditRow], col: usize, v: &str) -> usize {
    er.iter()
        .position(|r| r.orig.as_ref().unwrap()[col].as_deref() == Some(v))
        .unwrap_or_else(|| panic!("no row with {v}"))
}

const TYPES: &str = "j JSON, g GEOMETRY, b BIT(9), e ENUM('a','b'), s SET('x','y'), y YEAR, f FLOAT, dbl DOUBLE, de DECIMAL(20,10), dt DATETIME(6), ts TIMESTAMP(3) NULL, bl BLOB, bn BINARY(4), gen INT AS (n * 2)";
const VALUES: &str = "'{\"k\": [1, \"ไทย\"]}', POINT(1,2), b'101', 'a', 'x,y', 2024, 1.2345678, 1e300, 0E-10, '2024-01-01 10:00:00.999999', '2024-11-03 05:30:00.123', 0x00FF, 'ab'";

/// Rows load on a New York session and commit on a UTC one, so any identity that depends on
/// the session's rendering of a TIMESTAMP would miss or hit the wrong instant.
async fn split_time_zones(db: &Session) -> Result<()> {
    db.query("SET time_zone = 'America/New_York'").await?;
    db.execute(&[Stmt::new("SET time_zone = '+00:00'")], false)
        .await?;
    Ok(())
}

#[tokio::test]
async fn mysql_identity_round_trips_every_type_across_time_zones() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qafix_types");
    db.query("DROP TABLE IF EXISTS qafix_types").await?;
    db.query(&format!(
        "CREATE TABLE qafix_types (id INT PRIMARY KEY, n INT, {TYPES})"
    ))
    .await?;
    db.query(&format!("INSERT INTO qafix_types (id, n, j, g, b, e, s, y, f, dbl, de, dt, ts, bl, bn) VALUES (1, 1, {VALUES}), (2, 2, {VALUES})")).await?;
    split_time_zones(&db).await?;
    let data = load(&db, &t, None).await?;
    let updated = commit(&db, &t, &data, |er| er[1].cur[1] = Cell::Text("20".into())).await;
    let deleted = commit(&db, &t, &load(&db, &t, None).await?, |er| {
        er[0].deleted = true
    })
    .await;
    let after = rows(&db, "SELECT id, n FROM qafix_types ORDER BY id").await;
    db.query("DROP TABLE qafix_types").await?;
    updated?;
    deleted?;
    assert_eq!(after, vec![vec![Some("2".into()), Some("20".into())]]);
    Ok(())
}

/// BUG-003: a TIMESTAMP key in the repeated DST hour, with the editor and commit sessions in
/// New York.
#[tokio::test]
async fn mysql_timestamp_key_edits_the_right_instant() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qafix_ts");
    db.query("DROP TABLE IF EXISTS qafix_ts").await?;
    db.query("CREATE TABLE qafix_ts (ts TIMESTAMP PRIMARY KEY, v INT)")
        .await?;
    db.query("SET time_zone = '+00:00'").await?;
    db.query("INSERT INTO qafix_ts VALUES ('2024-11-03 05:30:00', 1), ('2024-11-03 06:30:00', 2)")
        .await?;
    // The editor and the commit connection both show New York time, where 01:30 occurs twice.
    db.query("SET time_zone = 'America/New_York'").await?;
    db.execute(&[Stmt::new("SET time_zone = 'America/New_York'")], false)
        .await?;
    let data = load(&db, &t, None).await?;
    let r = commit(&db, &t, &data, |er| {
        let i = row_with(er, 1, "2");
        er[i].cur[1] = Cell::Text("222".into());
    })
    .await;
    db.query("SET time_zone = '+00:00'").await?;
    let after = rows(&db, "SELECT ts, v FROM qafix_ts ORDER BY ts").await;
    db.query("DROP TABLE qafix_ts").await?;
    r?;
    assert_eq!(
        after,
        vec![
            vec![Some("2024-11-03 05:30:00".into()), Some("1".into())],
            vec![Some("2024-11-03 06:30:00".into()), Some("222".into())]
        ]
    );
    Ok(())
}

/// BUG-013: values whose display text doesn't round-trip (FLOAT, DOUBLE, JSON, DECIMAL 0E-10)
/// no longer make tables without a key uneditable, and duplicates are still refused.
#[tokio::test]
async fn mysql_rows_without_a_key_are_matched_exactly() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qafix_nopk");
    db.query("DROP TABLE IF EXISTS qafix_nopk").await?;
    db.query(&format!("CREATE TABLE qafix_nopk (n INT, {TYPES})"))
        .await?;
    db.query(&format!("INSERT INTO qafix_nopk (n, j, g, b, e, s, y, f, dbl, de, dt, ts, bl, bn) VALUES (1, {VALUES}), (2, {VALUES}), (3, {VALUES}), (3, {VALUES})")).await?;
    // Rows 1 and 2 then differ only beyond FLOAT's displayed precision.
    db.query("UPDATE qafix_nopk SET n = 1, f = 1.2345679 WHERE n = 2")
        .await?;
    split_time_zones(&db).await?;
    let data = load(&db, &t, None).await?;
    assert_eq!(
        data.result.rows[0][7], data.result.rows[1][7],
        "both show the same FLOAT text"
    );
    let first = commit(&db, &t, &data, |er| er[0].cur[0] = Cell::Text("10".into())).await;
    let data = load(&db, &t, None).await?;
    let duplicate = commit(&db, &t, &data, |er| {
        let i = row_with(er, 0, "3");
        er[i].deleted = true;
    })
    .await;
    let after = rows(
        &db,
        "SELECT n, CAST(f AS DOUBLE) FROM qafix_nopk ORDER BY n, f",
    )
    .await;
    db.query("DROP TABLE qafix_nopk").await?;
    first?;
    assert!(duplicate.unwrap_err().to_string().contains(NOT_UNIQUE));
    assert_eq!(after.len(), 4);
    assert_eq!(
        after
            .iter()
            .filter(|r| r[0].as_deref() == Some("10"))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn mysql_invisible_primary_key_edits_one_row() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qafix_gipk");
    db.query("DROP TABLE IF EXISTS qafix_gipk").await?;
    db.query("CREATE TABLE qafix_gipk (my_row_id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT INVISIBLE PRIMARY KEY, v INT, w INT)").await?;
    db.query("INSERT INTO qafix_gipk (v, w) VALUES (1, 1), (1, 2)")
        .await?;
    let data = load(&db, &t, None).await?;
    let r = commit(&db, &t, &data, |er| {
        let i = row_with(er, 1, "2");
        er[i].cur[0] = Cell::Text("5".into());
    })
    .await;
    let after = rows(&db, "SELECT v, w FROM qafix_gipk ORDER BY w").await;
    db.query("DROP TABLE qafix_gipk").await?;
    r?;
    assert_eq!(
        after,
        vec![
            vec![Some("1".into()), Some("1".into())],
            vec![Some("5".into()), Some("2".into())]
        ]
    );
    Ok(())
}

/// BUG-016: a row someone else changed after it was loaded is never overwritten.
#[tokio::test]
async fn concurrent_changes_fail_the_commit() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let other = connect(d).await?;
        let t = tref(d, "qafix_occ");
        db.query("DROP TABLE IF EXISTS qafix_occ").await?;
        db.query("CREATE TABLE qafix_occ (id INT PRIMARY KEY, v INT, w INT)")
            .await?;
        db.query("INSERT INTO qafix_occ VALUES (1, 0, 0), (2, 0, 0)")
            .await?;
        let data = load(&db, &t, None).await?;
        other
            .query("UPDATE qafix_occ SET v = 7 WHERE id = 1")
            .await?;
        let same_column = commit(&db, &t, &data, |er| er[0].cur[1] = Cell::Text("1".into())).await;
        let untouched_row =
            commit(&db, &t, &data, |er| er[1].cur[1] = Cell::Text("1".into())).await;
        let after = rows(&db, "SELECT id, v FROM qafix_occ ORDER BY id").await;
        db.query("DROP TABLE qafix_occ").await?;
        let e = same_column.unwrap_err().to_string();
        assert!(e.contains("not found"), "{d:?}: {e}");
        untouched_row?;
        assert_eq!(
            after,
            vec![
                vec![Some("1".into()), Some("7".into())],
                vec![Some("2".into()), Some("1".into())]
            ],
            "{d:?}"
        );
    }
    Ok(())
}

fn change_type(d: Driver, table: &str, old: &str, new: &str) -> Vec<Stmt> {
    let cols: Vec<String> = [
        "#",
        "column_name",
        "data_type",
        "is_nullable",
        "column_default",
        "extra",
        "comment",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let orig: Vec<Option<String>> = [
        Some("2"),
        Some("d"),
        Some(old),
        Some("YES"),
        None,
        Some(""),
        Some(""),
    ]
    .iter()
    .map(|v| v.map(str::to_owned))
    .collect();
    let mut cur: Vec<Cell> = orig.iter().cloned().map(Cell::from).collect();
    cur[2] = Cell::Text(new.into());
    structure_statements(
        d,
        &StructureChange {
            table: tref(d, table),
            new_name: table.into(),
            is_new: false,
            primary_key: vec![],
            columns: cols,
            rows: vec![EditRow {
                key: "k".into(),
                orig: Some(orig),
                cur,
                deleted: false,
                rid: None,
            }],
        },
    )
    .unwrap()
}

/// BUG-008: MySQL type changes run only when no stored value would change. MySQL structure
/// units run outside a transaction (execute_reviewed_part with transaction = false).
#[tokio::test]
async fn mysql_lossy_type_changes_are_refused() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let column_type = || async {
        rows(&db, "SELECT column_type FROM information_schema.columns WHERE table_schema = 'zixma' AND table_name = 'qafix_alter' AND column_name = 'd'").await[0][0].clone().unwrap()
    };
    db.query("DROP TABLE IF EXISTS qafix_alter").await?;
    db.query("CREATE TABLE qafix_alter (id INT PRIMARY KEY, d DECIMAL(10,4))")
        .await?;
    db.query("INSERT INTO qafix_alter VALUES (1, 12.3456), (2, NULL)")
        .await?;
    let lossy = db
        .execute_after_review(
            &change_type(
                Driver::Mysql,
                "qafix_alter",
                "decimal(10,4)",
                "decimal(10,1)",
            ),
            false,
            false,
        )
        .await;
    let unchanged = (
        column_type().await,
        rows(&db, "SELECT d FROM qafix_alter WHERE id = 1").await,
    );
    let widen = db
        .execute_after_review(
            &change_type(
                Driver::Mysql,
                "qafix_alter",
                "decimal(10,4)",
                "decimal(12,4)",
            ),
            false,
            false,
        )
        .await;
    let widened = column_type().await;
    db.query("UPDATE qafix_alter SET d = NULL").await?;
    let only_nulls = db
        .execute_after_review(
            &change_type(
                Driver::Mysql,
                "qafix_alter",
                "decimal(12,4)",
                "decimal(10,1)",
            ),
            false,
            false,
        )
        .await;
    let narrowed = column_type().await;
    db.query("DROP TABLE qafix_alter").await?;
    let e = lossy.unwrap_err().to_string();
    assert!(
        e.contains("would alter or truncate existing values") && e.contains("(matched 1)"),
        "{e}"
    );
    assert_eq!(
        unchanged,
        ("decimal(10,4)".into(), vec![vec![Some("12.3456".into())]])
    );
    widen?;
    assert_eq!(widened, "decimal(12,4)");
    only_nulls?;
    assert_eq!(narrowed, "decimal(10,1)");
    Ok(())
}

/// BUG-014: losing the commit connection mid-commit reports the real cause, saves nothing, and
/// the next commit reconnects.
#[tokio::test]
async fn lost_commit_connection_reports_the_cause_and_recovers() -> Result<()> {
    let db = Arc::new(connect(Driver::Mysql).await?);
    let locker = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qafix_kill").await?;
    db.query("CREATE TABLE qafix_kill (id INT PRIMARY KEY, v INT)")
        .await?;
    db.query("INSERT INTO qafix_kill VALUES (1, 0), (2, 0)")
        .await?;
    locker.query("START TRANSACTION").await?;
    locker
        .query("SELECT * FROM qafix_kill WHERE id = 2 FOR UPDATE")
        .await?;
    let up = |id: &str| Stmt {
        sql: "UPDATE `zixma`.`qafix_kill` SET `v` = ? WHERE `id` = ?;".into(),
        params: vec![json!("1"), json!(id)],
        expect: Some(1),
        check: None,
    };
    let st = vec![up("1"), up("2")];
    let d2 = db.clone();
    let commit = tokio::spawn(async move { d2.execute(&st, true).await });
    tokio::time::sleep(Duration::from_millis(700)).await;
    let blocked = rows(&locker, "SELECT ID FROM information_schema.PROCESSLIST WHERE INFO LIKE 'UPDATE `zixma`.`qafix_kill`%'").await;
    locker
        .query(&format!("KILL {}", blocked[0][0].clone().unwrap()))
        .await?;
    let result = commit.await?;
    locker.query("ROLLBACK").await?;
    let after = rows(&locker, "SELECT v FROM qafix_kill ORDER BY id").await;
    let next = db.execute(&[up("1")], true).await;
    db.query("DROP TABLE qafix_kill").await?;
    let e = result.unwrap_err().to_string();
    assert!(
        !e.starts_with("Rollback failed"),
        "the cause must come first: {e}"
    );
    assert!(e.contains("Nothing from this commit was saved"), "{e}");
    assert_eq!(after, vec![vec![Some("0".into())], vec![Some("0".into())]]);
    next?;
    Ok(())
}

/// BUG-015: raw filters run read-only on Telecat's own connection, never inside the user's
/// open transaction on the editor connection.
#[tokio::test]
async fn raw_filters_are_read_only_and_leave_the_users_transaction_alone() -> Result<()> {
    let db = connect(Driver::Postgres).await?;
    let t = tref(Driver::Postgres, "qafix_rf");
    db.query("DROP TABLE IF EXISTS qafix_rf, qafix_rf_log")
        .await?;
    db.query("CREATE TABLE qafix_rf (id int PRIMARY KEY, v int)")
        .await?;
    db.query("INSERT INTO qafix_rf VALUES (1, 0), (2, 0)")
        .await?;
    db.query("CREATE TABLE qafix_rf_log (n int)").await?;
    db.query("CREATE OR REPLACE FUNCTION qafix_rf_fn() RETURNS bool LANGUAGE sql VOLATILE AS 'INSERT INTO qafix_rf_log VALUES (1); SELECT true'").await?;
    let writing = load(&db, &t, Some("qafix_rf_fn()")).await;
    let logged = rows(&db, "SELECT COUNT(*) FROM qafix_rf_log").await;
    db.query("BEGIN").await?;
    db.query("UPDATE qafix_rf SET v = 9 WHERE id = 1").await?;
    let plain = load(&db, &t, Some("id >= 1")).await;
    let in_tx = rows(&db, "SELECT v FROM qafix_rf WHERE id = 1").await;
    db.query("ROLLBACK").await?;
    let after = rows(&db, "SELECT v FROM qafix_rf WHERE id = 1").await;
    db.query("DROP TABLE qafix_rf, qafix_rf_log").await?;
    db.query("DROP FUNCTION qafix_rf_fn()").await?;
    let e = writing.unwrap_err();
    assert!(format!("{e:#}").contains("read-only"), "{e:#}");
    assert_eq!(logged[0][0].as_deref(), Some("0"));
    assert_eq!(plain?.total, 2);
    assert_eq!(
        in_tx[0][0].as_deref(),
        Some("9"),
        "the user's transaction stayed open"
    );
    assert_eq!(
        after[0][0].as_deref(),
        Some("0"),
        "and its rollback still works"
    );

    let my = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qafix_rf");
    my.query("DROP TABLE IF EXISTS qafix_rf").await?;
    my.query("CREATE TABLE qafix_rf (id INT PRIMARY KEY, v INT)")
        .await?;
    my.query("INSERT INTO qafix_rf VALUES (1, 0)").await?;
    my.query("START TRANSACTION").await?;
    my.query("UPDATE qafix_rf SET v = 9 WHERE id = 1").await?;
    let plain = load(&my, &t, Some("id = 1")).await;
    my.query("ROLLBACK").await?;
    let after = rows(&my, "SELECT v FROM qafix_rf").await;
    my.query("DROP TABLE qafix_rf").await?;
    assert_eq!(plain?.total, 1);
    assert_eq!(
        after[0][0].as_deref(),
        Some("0"),
        "START TRANSACTION must not have committed the user's work"
    );
    Ok(())
}

/// BUG-009: the app sends every row edit of a save in one call, so a failure in a later table
/// rolls back the earlier tables too.
#[tokio::test]
async fn one_call_across_tables_is_all_or_nothing() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    db.query("DROP TABLE IF EXISTS qafix_a, qafix_b").await?;
    db.query("CREATE TABLE qafix_a (id INT PRIMARY KEY, v INT)")
        .await?;
    db.query("CREATE TABLE qafix_b (id INT PRIMARY KEY, v INT)")
        .await?;
    db.query("INSERT INTO qafix_a VALUES (1, 0)").await?;
    db.query("INSERT INTO qafix_b VALUES (1, 0)").await?;
    let up = |t: &str, id: &str| Stmt {
        sql: format!("UPDATE `zixma`.`{t}` SET `v` = ? WHERE `id` = ?;"),
        params: vec![json!("5"), json!(id)],
        expect: Some(1),
        check: None,
    };
    let r = db
        .execute_after_review(&[up("qafix_a", "1"), up("qafix_b", "404")], true, false)
        .await;
    let a = rows(&db, "SELECT v FROM qafix_a").await;
    db.query("DROP TABLE qafix_a, qafix_b").await?;
    assert!(r.is_err());
    assert_eq!(a[0][0].as_deref(), Some("0"));
    Ok(())
}

// ------------------------------------------------------------------ round 2 (QA re-verification)

/// NEW-4: a type change runs only when every stored value converts back to exactly itself.
#[tokio::test]
async fn mysql_type_changes_round_trip_exactly() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    // (column type, stored value, new type, expected to be allowed)
    let cases = [
        ("BIGINT", "9007199254740993", "double", false),
        ("BIGINT", "9007199254740992", "double", true),
        ("VARCHAR(10)", "' 12'", "int", false),
        ("VARCHAR(10)", "'1e3'", "int", false),
        ("VARCHAR(10)", "'12'", "int", true),
        ("ENUM('a','b')", "'a'", "enum('A','b')", false),
        ("ENUM('a','b')", "'b'", "enum('A','b')", true),
        ("VARCHAR(5)", "'ab  '", "char(5)", false),
        ("VARCHAR(5)", "'ab'", "char(5)", true),
        ("DECIMAL(10,2)", "12.50", "decimal(10,1)", true),
        ("DECIMAL(10,4)", "12.3456", "decimal(10,1)", false),
        ("INT", "300", "tinyint", false),
        ("INT", "5", "bigint", true),
        ("FLOAT", "1.2345678", "varchar(20)", false),
        ("INT", "123", "varchar(2)", false),
        (
            "DATETIME(6)",
            "'2024-01-01 10:00:00.000000'",
            "datetime",
            true,
        ),
        ("DATETIME(6)", "'2024-01-01 10:00:00.5'", "datetime", false),
        ("DATETIME", "'2024-01-01 10:00:00'", "date", false),
        ("VARBINARY(4)", "0x0102", "binary(4)", false),
        ("BIT(8)", "b'1'", "int", false),
    ];
    let mut wrong = vec![];
    for (old, value, new, allowed) in cases {
        db.query("DROP TABLE IF EXISTS qafix_retype").await?;
        db.query(&format!(
            "CREATE TABLE qafix_retype (id INT PRIMARY KEY, d {old})"
        ))
        .await?;
        db.query(&format!("INSERT INTO qafix_retype VALUES (1, {value})"))
            .await?;
        let before = rows(&db, "SELECT HEX(CAST(d AS BINARY)) FROM qafix_retype").await;
        let declared = rows(&db, "SELECT column_type FROM information_schema.columns WHERE table_schema = 'zixma' AND table_name = 'qafix_retype' AND column_name = 'd'").await[0][0].clone().unwrap();
        let r = db
            .execute_after_review(
                &change_type(Driver::Mysql, "qafix_retype", &declared, new),
                false,
                false,
            )
            .await;
        let after = rows(&db, "SELECT HEX(CAST(d AS BINARY)) FROM qafix_retype").await;
        if r.is_ok() != allowed || (r.is_err() && before != after) {
            wrong.push(format!(
                "{old} {value} -> {new}: {:?}",
                r.map(|_| ()).map_err(|e| e.to_string())
            ));
        }
    }
    db.query("DROP TABLE IF EXISTS qafix_retype").await?;
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// NEW-5: a slow table read can be cancelled and never blocks ⌘S.
#[tokio::test]
async fn slow_table_reads_are_cancellable_and_dont_block_commits() -> Result<()> {
    let db = Arc::new(connect(Driver::Mysql).await?);
    let t = tref(Driver::Mysql, "qafix_slow");
    db.query("DROP TABLE IF EXISTS qafix_slow").await?;
    db.query("CREATE TABLE qafix_slow (id INT PRIMARY KEY, v INT)")
        .await?;
    db.query("INSERT INTO qafix_slow VALUES (1, 0)").await?;
    let started = std::time::Instant::now();
    let reader = db.clone();
    let tt = t.clone();
    let read = tokio::spawn(async move { load(&reader, &tt, Some("SLEEP(4) = 0")).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let commit_started = std::time::Instant::now();
    db.execute(
        &[Stmt {
            sql: "UPDATE `zixma`.`qafix_slow` SET `v` = ? WHERE `id` = ?;".into(),
            params: vec![json!("1"), json!("1")],
            expect: Some(1),
            check: None,
        }],
        true,
    )
    .await?;
    let commit_took = commit_started.elapsed();
    db.cancel().await?;
    let cancelled = read.await?;
    let total = started.elapsed();
    let after_cancel = load(&db, &t, None).await?.total;
    db.query("DROP TABLE qafix_slow").await?;
    assert!(
        commit_took < Duration::from_secs(2),
        "commit waited {commit_took:?}"
    );
    assert!(
        total < Duration::from_secs(3),
        "read ran {total:?}: {cancelled:?}"
    );
    assert!(
        cancelled.is_err() || cancelled.is_ok(),
        "either outcome, but quickly"
    );
    assert_eq!(
        after_cancel, 1,
        "the reader connection is usable after a cancel"
    );
    Ok(())
}

/// NEW-7: session settings changed in the SQL editor don't affect how rows are identified.
#[tokio::test]
async fn editor_session_settings_dont_break_row_identity() -> Result<()> {
    let my = connect(Driver::Mysql).await?;
    let t = tref(Driver::Mysql, "qafix_pad");
    my.query("DROP TABLE IF EXISTS qafix_pad").await?;
    my.query("CREATE TABLE qafix_pad (c CHAR(5), v INT)")
        .await?;
    my.query("INSERT INTO qafix_pad VALUES ('ab', 1), ('cd', 2)")
        .await?;
    my.query("SET SESSION sql_mode = CONCAT(@@sql_mode, ',PAD_CHAR_TO_FULL_LENGTH')")
        .await?;
    let data = load(&my, &t, None).await?;
    let mysql = commit(&my, &t, &data, |er| er[1].cur[1] = Cell::Text("20".into())).await;
    let after = rows(&my, "SELECT v FROM qafix_pad ORDER BY v").await;
    my.query("DROP TABLE qafix_pad").await?;
    mysql?;
    assert_eq!(after, vec![vec![Some("1".into())], vec![Some("20".into())]]);

    let pg = connect(Driver::Postgres).await?;
    let t = tref(Driver::Postgres, "qafix_tz");
    pg.query("DROP TABLE IF EXISTS qafix_tz").await?;
    pg.query("CREATE TABLE qafix_tz (at timestamptz, span interval, v int)")
        .await?;
    pg.query("INSERT INTO qafix_tz VALUES ('2024-11-03 05:30:00+00', '1 mon 2 days', 1), ('2024-11-03 06:30:00+00', '3 hours', 2)").await?;
    pg.query("SET TimeZone = 'America/New_York'").await?;
    pg.query("SET IntervalStyle = 'iso_8601'").await?;
    let data = load(&pg, &t, None).await?;
    let postgres = commit(&pg, &t, &data, |er| {
        let i = row_with(er, 2, "2");
        er[i].cur[2] = Cell::Text("20".into());
    })
    .await;
    let after = rows(&pg, "SELECT v FROM qafix_tz ORDER BY v").await;
    pg.query("DROP TABLE qafix_tz").await?;
    postgres?;
    assert_eq!(after, vec![vec![Some("1".into())], vec![Some("20".into())]]);
    Ok(())
}

/// NEW-8: BIT and FLOAT keys, and an invisible key with repeated visible values, edit one row.
#[tokio::test]
async fn mysql_bit_float_and_invisible_keys_edit_one_row() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let mut wrong = vec![];
    for (ddl, values, pick_col, pick) in [
        (
            "k BIT(8) PRIMARY KEY, v INT",
            "(b'01000001', 1), (b'00000001', 2)",
            1,
            "2",
        ),
        (
            "k BIT(8) PRIMARY KEY, v INT",
            "(b'01000001', 1), (b'00000001', 2)",
            1,
            "1",
        ),
        (
            "k FLOAT PRIMARY KEY, v INT",
            "(1.2345678, 1), (1.2345679, 2)",
            1,
            "2",
        ),
        // MySQL substitutes a key's constant into the rest of the WHERE; the fingerprint must
        // agree with that substitution for every key type.
        (
            "k DECIMAL(10,2) PRIMARY KEY, v INT",
            "(12.50, 1), (12.51, 2)",
            1,
            "1",
        ),
        (
            "k DATETIME(6) PRIMARY KEY, v INT",
            "('2024-01-01 10:00:00', 1), ('2024-01-01 10:00:00.500001', 2)",
            1,
            "1",
        ),
        (
            "k VARCHAR(10) PRIMARY KEY, v INT",
            "('a', 1), ('B ', 2)",
            1,
            "2",
        ),
        (
            "k ENUM('x','y') PRIMARY KEY, v INT",
            "('x', 1), ('y', 2)",
            1,
            "2",
        ),
        ("k YEAR PRIMARY KEY, v INT", "(2024, 1), (1999, 2)", 1, "2"),
        (
            "k DOUBLE PRIMARY KEY, v INT",
            "(0.1, 1), (1e300, 2)",
            1,
            "2",
        ),
        (
            "k BIGINT UNSIGNED PRIMARY KEY, v INT",
            "(18446744073709551615, 1), (0, 2)",
            1,
            "1",
        ),
        (
            "k DATE PRIMARY KEY, v INT",
            "('2024-02-29', 1), ('1000-01-01', 2)",
            1,
            "1",
        ),
        (
            "k TIME(3) PRIMARY KEY, v INT",
            "('-838:59:59', 1), ('10:00:00.123', 2)",
            1,
            "2",
        ),
        (
            "k CHAR(5) PRIMARY KEY, v INT",
            "('ab', 1), ('cd', 2)",
            1,
            "1",
        ),
        (
            "k VARBINARY(8) PRIMARY KEY, v INT",
            "(0x00FF, 1), ('0x41', 2)",
            1,
            "2",
        ),
        (
            "my_row_id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT INVISIBLE PRIMARY KEY, v INT",
            "(1), (1), (2)",
            0,
            "1",
        ),
    ] {
        db.query("DROP TABLE IF EXISTS qafix_keys").await?;
        db.query(&format!("CREATE TABLE qafix_keys ({ddl})"))
            .await?;
        let insert = if ddl.starts_with("my_row_id") {
            "INSERT INTO qafix_keys (v) VALUES"
        } else {
            "INSERT INTO qafix_keys VALUES"
        };
        db.query(&format!("{insert} {values}")).await?;
        let t = tref(Driver::Mysql, "qafix_keys");
        let before = rows(&db, "SELECT COUNT(*) FROM qafix_keys").await;
        let data = load(&db, &t, None).await?;
        let r = commit(&db, &t, &data, |er| {
            let i = row_with(er, pick_col, pick);
            er[i].deleted = true;
        })
        .await;
        let after = rows(&db, "SELECT COUNT(*) FROM qafix_keys").await;
        let left: i64 = after[0][0].as_deref().unwrap().parse()?;
        let had: i64 = before[0][0].as_deref().unwrap().parse()?;
        if r.is_err() || left != had - 1 {
            wrong.push(format!(
                "{ddl} delete {pick}: {:?}, rows {had} -> {left}",
                r.map_err(|e| e.to_string())
            ));
        }
    }
    db.query("DROP TABLE IF EXISTS qafix_keys").await?;
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// NEW-1: the guard splits words exactly where each server does. Every character class the
/// guard can disagree on is checked against the live servers: is `a<c>b` one identifier, is
/// `<c>` whitespace, or neither?
#[tokio::test]
async fn guard_word_boundaries_match_the_servers() -> Result<()> {
    use telecat_core::guard::{is_identifier_char, is_sql_space};
    let mut sample: Vec<char> = (1u32..128).filter_map(char::from_u32).collect();
    sample.extend([
        '\u{a0}', '\u{b7}', '\u{85}', '\u{feff}', '\u{200b}', '\u{2028}', '\u{3000}', 'ä', 'ธ',
        'Ω', '€', '\u{1680}', '\u{205f}', '\u{fffd}',
    ]);
    let mut seed = 0x9e3779b97f4a7c15u64;
    for _ in 0..60 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        if let Some(c) = char::from_u32(0x80 + (seed % 0xff00) as u32) {
            sample.push(c);
        }
    }
    let mut disagree = vec![];
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let quote = |s: &str| {
            if d == Driver::Mysql {
                format!("`{s}`")
            } else {
                format!("\"{s}\"")
            }
        };
        for &c in &sample {
            if c == '\0' || (d == Driver::Postgres && c == '\u{fffd}') {
                continue;
            }
            // One identifier: the alias comes back as written.
            let ident = db
                .query(&format!("SELECT 1 AS a{c}b"))
                .await
                .ok()
                .and_then(|r| r.columns.first().cloned());
            let server_ident = ident.as_deref() == Some(&format!("a{c}b"))
                || ident.as_deref() == Some(&format!("a{c}b").to_lowercase());
            // Whitespace: `1<c>AS<c>x` names the column x.
            let space = db
                .query(&format!("SELECT 1{c}AS{c}{}", quote("x")))
                .await
                .ok()
                .and_then(|r| r.columns.first().cloned());
            let server_space = space.as_deref() == Some("x");
            // Where the server rejects both forms nothing runs, so either answer is safe.
            if server_ident != is_identifier_char(c) && (server_ident || ident.is_some()) {
                disagree.push(format!(
                    "{d:?} U+{:04X}: server identifier={server_ident} ({ident:?}), guard={}",
                    c as u32,
                    is_identifier_char(c)
                ));
            }
            if server_space != is_sql_space(c) && (server_space || space.is_some()) {
                disagree.push(format!(
                    "{d:?} U+{:04X}: server whitespace={server_space} ({space:?}), guard={}",
                    c as u32,
                    is_sql_space(c)
                ));
            }
        }
    }
    assert!(disagree.is_empty(), "{disagree:#?}");
    Ok(())
}

/// A table the editor's own transaction holds locked fails the read instead of hanging the grid.
#[tokio::test]
async fn locked_tables_fail_reads_instead_of_hanging() -> Result<()> {
    for d in [Driver::Postgres, Driver::Mysql] {
        let db = connect(d).await?;
        let t = tref(d, "qafix_lock");
        db.query("DROP TABLE IF EXISTS qafix_lock").await?;
        db.query("CREATE TABLE qafix_lock (id INT PRIMARY KEY)")
            .await?;
        if d == Driver::Postgres {
            db.query("BEGIN").await?;
            db.query("LOCK TABLE qafix_lock IN ACCESS EXCLUSIVE MODE")
                .await?;
        } else {
            db.query("LOCK TABLES qafix_lock WRITE").await?;
        }
        let blocked = tokio::time::timeout(Duration::from_secs(30), load(&db, &t, None)).await;
        db.query(if d == Driver::Postgres {
            "ROLLBACK"
        } else {
            "UNLOCK TABLES"
        })
        .await?;
        let after = load(&db, &t, None).await;
        db.query("DROP TABLE qafix_lock").await?;
        let blocked = blocked.expect("the read must give up, not hang");
        assert!(blocked.is_err(), "{d:?}: read a table locked by the editor");
        after?;
    }
    Ok(())
}

// ------------------------------------------------------------------ round 3 (QA final verification)

/// FINAL-1: unsigned 64-bit and BIT(64) keys ≥ 2^63, and DECIMAL keys wider than 35 digits, hit
/// exactly their row, on a strict and a non-strict commit session alike.
#[tokio::test]
async fn wide_numeric_keys_never_clamp() -> Result<()> {
    for strict in [true, false] {
        let db = connect(Driver::Mysql).await?;
        if !strict {
            db.execute(
                &[Stmt::new("SET SESSION sql_mode = 'NO_ENGINE_SUBSTITUTION'")],
                false,
            )
            .await?;
        }
        let mut wrong = vec![];
        for (ddl, values, target) in [
            (
                "k BIGINT UNSIGNED PRIMARY KEY, v INT",
                "(9223372036854775807, 0), (18446744073709551615, 0)",
                "18446744073709551615",
            ),
            (
                "k BIGINT UNSIGNED PRIMARY KEY, v INT",
                "(9223372036854775807, 0), (10000000000000000000, 0)",
                "10000000000000000000",
            ),
            (
                "k BIT(64) PRIMARY KEY, v INT",
                "(9223372036854775807, 0), (18446744073709551615, 0)",
                "18446744073709551615",
            ),
            (
                "k BIGINT PRIMARY KEY, v INT",
                "(-9223372036854775808, 0), (-9223372036854775807, 0)",
                "-9223372036854775808",
            ),
            (
                "k DECIMAL(65,0) PRIMARY KEY, v INT",
                "(99999999999999999999999999999999999999999999999999999999999999998, 0), (99999999999999999999999999999999999999999999999999999999999999999, 0)",
                "99999999999999999999999999999999999999999999999999999999999999999",
            ),
            (
                "k DECIMAL(40,2) PRIMARY KEY, v INT",
                "(12345678901234567890123456789012345678.00, 0), (12345678901234567890123456789012345678.01, 0)",
                "12345678901234567890123456789012345678.01",
            ),
        ] {
            for delete in [false, true] {
                db.query("DROP TABLE IF EXISTS qafix_wide").await?;
                db.query(&format!("CREATE TABLE qafix_wide ({ddl})"))
                    .await?;
                db.query(&format!("INSERT INTO qafix_wide VALUES {values}"))
                    .await?;
                let t = tref(Driver::Mysql, "qafix_wide");
                let data = load(&db, &t, None).await?;
                let exact = if ddl.contains("BIT") {
                    "CAST(CAST(k AS UNSIGNED) AS CHAR)"
                } else {
                    "CAST(k AS CHAR)"
                };
                let target_row = rows(
                    &db,
                    &format!("SELECT k FROM qafix_wide WHERE {exact} = '{target}'"),
                )
                .await;
                let r = commit(&db, &t, &data, |er| {
                    let i = er
                        .iter()
                        .position(|r| {
                            let shown = r.orig.as_ref().unwrap()[0].clone().unwrap();
                            shown == target
                                || (shown.starts_with("0x") && shown == "0xFFFFFFFFFFFFFFFF")
                        })
                        .expect("target row");
                    if delete {
                        er[i].deleted = true
                    } else {
                        er[i].cur[1] = Cell::Text("9".into())
                    }
                })
                .await;
                let after = rows(&db, &format!("SELECT {exact} = '{target}', v FROM qafix_wide ORDER BY {exact} = '{target}'")).await;
                let expected = if delete {
                    vec![vec![Some("0".to_owned()), Some("0".to_owned())]]
                } else {
                    vec![
                        vec![Some("0".to_owned()), Some("0".to_owned())],
                        vec![Some("1".to_owned()), Some("9".to_owned())],
                    ]
                };
                if r.is_err() || after != expected || target_row.len() != 1 {
                    wrong.push(format!(
                        "strict={strict} {ddl} delete={delete}: {:?} -> {after:?}",
                        r.map_err(|e| e.to_string())
                    ));
                }
            }
        }
        db.query("DROP TABLE IF EXISTS qafix_wide").await?;
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
    Ok(())
}

/// The commit connection is strict whatever the server default: a value MySQL would truncate
/// fails the statement and the commit.
#[tokio::test]
async fn commit_connection_is_strict() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    let mode = db
        .execute(&[Stmt::new("SELECT @@SESSION.sql_mode")], false)
        .await?[0]
        .rows[0][0]
        .clone()
        .unwrap();
    assert!(mode.contains("STRICT_ALL_TABLES"), "{mode}");
    db.query("DROP TABLE IF EXISTS qafix_strict").await?;
    db.query("CREATE TABLE qafix_strict (id INT PRIMARY KEY, s VARCHAR(3))")
        .await?;
    db.query("SET SESSION sql_mode = ''").await?;
    let r = db
        .execute(
            &[Stmt {
                sql: "INSERT INTO `zixma`.`qafix_strict` (`id`, `s`) VALUES (?, ?);".into(),
                params: vec![json!("1"), json!("toolong")],
                expect: Some(1),
                check: None,
            }],
            true,
        )
        .await;
    let left = rows(&db, "SELECT COUNT(*) FROM qafix_strict").await;
    db.query("DROP TABLE qafix_strict").await?;
    assert!(r.unwrap_err().to_string().contains("ER_DATA_TOO_LONG"));
    assert_eq!(left[0][0].as_deref(), Some("0"));
    Ok(())
}

/// FINAL-2: another session changing a text key's case or trailing spaces fails the commit.
#[tokio::test]
async fn key_case_and_padding_changes_are_detected() -> Result<()> {
    for change in [
        "UPDATE qafix_ci SET k = 'A' WHERE k = 'a'",
        "UPDATE qafix_ci SET k = 'a ' WHERE k = 'a'",
    ] {
        let db = connect(Driver::Mysql).await?;
        let other = connect(Driver::Mysql).await?;
        let t = tref(Driver::Mysql, "qafix_ci");
        db.query("DROP TABLE IF EXISTS qafix_ci").await?;
        db.query(
            "CREATE TABLE qafix_ci (k VARCHAR(10) COLLATE utf8mb4_0900_ai_ci PRIMARY KEY, v INT)",
        )
        .await?;
        db.query("INSERT INTO qafix_ci VALUES ('a', 0)").await?;
        let data = load(&db, &t, None).await?;
        other.query(change).await?;
        let r = commit(&db, &t, &data, |er| er[0].cur[1] = Cell::Text("1".into())).await;
        let v = rows(&db, "SELECT v FROM qafix_ci").await;
        db.query("DROP TABLE qafix_ci").await?;
        assert!(r.unwrap_err().to_string().contains("not found"), "{change}");
        assert_eq!(v[0][0].as_deref(), Some("0"), "{change}");
    }
    Ok(())
}

/// FINAL-4, FINAL-5: a raw filter can't leave a session lock behind, and a cancelled read says so.
#[tokio::test]
async fn raw_filter_session_state_is_discarded_and_cancel_is_reported() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let t = tref(d, "qafix_rl");
        db.query("DROP TABLE IF EXISTS qafix_rl").await?;
        db.query("CREATE TABLE qafix_rl (id INT PRIMARY KEY)")
            .await?;
        db.query("INSERT INTO qafix_rl VALUES (1)").await?;
        let (lock, held) = if d == Driver::Mysql {
            (
                "GET_LOCK('qafix_rl', 0) = 1",
                "SELECT IS_USED_LOCK('qafix_rl') IS NOT NULL",
            )
        } else {
            (
                "pg_try_advisory_lock(4242)",
                "SELECT (COUNT(*) > 0)::int FROM pg_locks WHERE locktype = 'advisory' AND objid = 4242",
            )
        };
        assert_eq!(load(&db, &t, Some(lock)).await?.total, 1);
        let still = rows(&db, held).await;
        assert_eq!(
            still[0][0].as_deref(),
            Some(if d == Driver::Mysql { "0" } else { "0" }),
            "{d:?}: lock left behind"
        );
        db.query("DROP TABLE qafix_rl").await?;
    }
    let db = Arc::new(connect(Driver::Mysql).await?);
    let t = tref(Driver::Mysql, "qafix_rl");
    db.query("DROP TABLE IF EXISTS qafix_rl").await?;
    db.query("CREATE TABLE qafix_rl (id INT PRIMARY KEY)")
        .await?;
    db.query("INSERT INTO qafix_rl VALUES (1)").await?;
    let reader = db.clone();
    let read = tokio::spawn(async move { load(&reader, &t, Some("SLEEP(4) = 0")).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    db.cancel().await?;
    let e = read.await?.unwrap_err().to_string();
    db.query("DROP TABLE qafix_rl").await?;
    assert_eq!(e, "Loading the table was cancelled.");
    Ok(())
}
