//! Live: Telecat 0.8 Table Data features against the throwaway test containers only (MySQL
//! :3307, PostgreSQL :5434): column details, foreign-key navigation, filter operators,
//! multi-column sort, estimated counts, NOW(), paste-to-cells and Copy As INSERT.
use anyhow::Result;
use telecat_core::{
    changes::{Cell, EditRow, TableMeta, data_statements, quote_ident, to_edit_rows},
    columns::ValueKind,
    copy::{CopyFormat, CopyRows, copy_text},
    export::insert_target,
    filters::foreign_key_filters,
    paste::{PasteTarget, plan_paste},
    types::*,
};
use telecat_db::Session;

async fn connect(d: Driver) -> Result<Session> {
    let mut c = empty_connection(d);
    c.host = "127.0.0.1".into();
    c.port = if d == Driver::Mysql { 3307 } else { 5434 };
    c.user = if d == Driver::Mysql {
        "root"
    } else {
        "postgres"
    }
    .into();
    c.database = if d == Driver::Mysql {
        "zixma"
    } else {
        "main_db"
    }
    .into();
    Session::connect_test(&c, "secret").await
}
async fn run(db: &Session, sqls: &[&str]) -> Result<()> {
    for s in sqls {
        db.query(s)
            .await
            .map_err(|e| anyhow::anyhow!("{s}: {e:#}"))?;
    }
    Ok(())
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
fn request(t: &TableRef, filters: Vec<Filter>) -> TableDataRequest {
    TableDataRequest {
        table: t.clone(),
        filters,
        combine: Combine::And,
        order_by: vec![],
        estimate: false,
        limit: 300,
        offset: 0,
    }
}
fn rule(column: &str, op: FilterOp, value: &str) -> Filter {
    Filter {
        enabled: true,
        column: column.into(),
        op,
        value: value.into(),
        value2: None,
    }
}
fn column(data: &TableDataResult, name: &str) -> Vec<Option<String>> {
    let i = data.result.columns.iter().position(|c| c == name).unwrap();
    data.result.rows.iter().map(|r| r[i].clone()).collect()
}
/// Stages `edit` on the loaded rows and saves it the way ⌘S does: generated statements through
/// the guarded, reviewed execute path.
async fn save(
    db: &Session,
    t: &TableRef,
    data: &TableDataResult,
    edit: impl FnOnce(&mut Vec<EditRow>),
) -> Result<()> {
    let meta = TableMeta {
        columns: data.result.columns.clone(),
        primary_key: data.primary_key.clone(),
        binary: data.result.binary.clone(),
    };
    let mut rows = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
    edit(&mut rows);
    let statements = data_statements(db.driver, t, &meta, &rows)?;
    db.execute_after_review(&statements, true, false).await?;
    Ok(())
}

#[tokio::test]
async fn column_details_read_kinds_values_and_composite_foreign_keys() -> Result<()> {
    let my = connect(Driver::Mysql).await?;
    run(
        &my,
        &[
            "DROP TABLE IF EXISTS t080_child",
            "DROP TABLE IF EXISTS t080_parent",
            "CREATE TABLE t080_parent (id DOUBLE AUTO_INCREMENT, part VARCHAR(5), PRIMARY KEY (id, part))",
            r"CREATE TABLE t080_child (id INT PRIMARY KEY, pid DOUBLE, ppart VARCHAR(5), flag TINYINT(1), e ENUM('a','it''s','x,y','back\\slash'), s SET('r','w'), j JSON, at DATETIME(3), d DATE, tm TIME, n INT, twice INT AS (n * 2), CONSTRAINT fk_parent FOREIGN KEY (pid, ppart) REFERENCES t080_parent (id, part))",
        ],
    )
    .await?;
    let details = my
        .column_details(&tref(Driver::Mysql, "t080_child"))
        .await?;
    let kind = |n: &str| details.get(n).unwrap().kind;
    assert_eq!(kind("flag"), ValueKind::Boolean);
    assert_eq!(kind("e"), ValueKind::Enum);
    assert_eq!(kind("s"), ValueKind::Set);
    assert_eq!(kind("j"), ValueKind::Json);
    assert_eq!(kind("at"), ValueKind::DateTime);
    assert_eq!(kind("d"), ValueKind::Date);
    assert_eq!(kind("tm"), ValueKind::Time);
    assert_eq!(
        details.get("e").unwrap().values,
        ["a", "it's", "x,y", "back\\slash"]
    );
    assert_eq!(details.get("s").unwrap().values, ["r", "w"]);
    assert!(details.get("twice").unwrap().generated);
    assert!(!details.get("n").unwrap().generated);
    assert!(!details.get("id").unwrap().nullable && details.get("n").unwrap().nullable);
    let fk = details.foreign_key("ppart").unwrap();
    assert_eq!(fk.columns, ["pid", "ppart"]);
    assert_eq!(fk.target_columns, ["id", "part"]);
    assert_eq!(fk.target, tref(Driver::Mysql, "t080_parent"));
    run(&my, &["DROP TABLE t080_child", "DROP TABLE t080_parent"]).await?;

    let pg = connect(Driver::Postgres).await?;
    run(
        &pg,
        &[
            "DROP TABLE IF EXISTS t080_child",
            "DROP SCHEMA IF EXISTS t080_s CASCADE",
            "DROP TYPE IF EXISTS t080_mood",
            "CREATE SCHEMA t080_s",
            "CREATE TYPE t080_mood AS ENUM ('sad', 'it''s ok', 'a,b')",
            "CREATE TABLE t080_s.parent (a int, b text, PRIMARY KEY (a, b))",
            "CREATE TABLE t080_child (id int PRIMARY KEY, x text, y int, ok boolean, mood t080_mood, doc jsonb, at timestamptz, n int, twice int GENERATED ALWAYS AS (n * 2) STORED, CONSTRAINT fk_two FOREIGN KEY (y, x) REFERENCES t080_s.parent (a, b))",
        ],
    )
    .await?;
    let details = pg
        .column_details(&tref(Driver::Postgres, "t080_child"))
        .await?;
    let kind = |n: &str| details.get(n).unwrap().kind;
    assert_eq!(kind("ok"), ValueKind::Boolean);
    assert_eq!(kind("mood"), ValueKind::Enum);
    assert_eq!(kind("doc"), ValueKind::Json);
    assert_eq!(kind("at"), ValueKind::DateTime);
    assert_eq!(
        details.get("mood").unwrap().values,
        ["sad", "it's ok", "a,b"]
    );
    assert!(details.get("twice").unwrap().generated);
    let fk = details.foreign_key("x").unwrap();
    assert_eq!(fk.columns, ["y", "x"]);
    assert_eq!(fk.target_columns, ["a", "b"]);
    assert_eq!(
        fk.target,
        TableRef {
            schema: "t080_s".into(),
            name: "parent".into()
        }
    );
    run(
        &pg,
        &[
            "DROP TABLE t080_child",
            "DROP SCHEMA t080_s CASCADE",
            "DROP TYPE t080_mood",
        ],
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn foreign_key_navigation_selects_exactly_the_referenced_row() -> Result<()> {
    // MySQL: the user's real shape, a DOUBLE AUTO_INCREMENT key, with a composite key too.
    let my = connect(Driver::Mysql).await?;
    run(
        &my,
        &[
            "DROP TABLE IF EXISTS t080_fk_child",
            "DROP TABLE IF EXISTS t080_fk_parent",
            "CREATE TABLE t080_fk_parent (id DOUBLE AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20), UNIQUE KEY (name, id))",
            "INSERT INTO t080_fk_parent (name) VALUES ('one'), ('two'), ('three')",
            "INSERT INTO t080_fk_parent (id, name) VALUES (1e15, 'big')",
            "CREATE TABLE t080_fk_child (id DOUBLE AUTO_INCREMENT PRIMARY KEY, parent DOUBLE, pname VARCHAR(20), pid DOUBLE, FOREIGN KEY (parent) REFERENCES t080_fk_parent (id), CONSTRAINT two FOREIGN KEY (pname, pid) REFERENCES t080_fk_parent (name, id))",
            "INSERT INTO t080_fk_child (parent, pname, pid) VALUES (2, 'two', 2), (1e15, 'big', 1e15), (NULL, NULL, NULL)",
        ],
    )
    .await?;
    for (d, db) in [(Driver::Mysql, &my)] {
        let child = tref(d, "t080_fk_child");
        let details = db.column_details(&child).await?;
        let data = db.table_data(&request(&child, vec![])).await?;
        for row in 0..2 {
            for fk in &details.foreign_keys {
                let values: Vec<Option<String>> = fk
                    .columns
                    .iter()
                    .map(|c| column(&data, c)[row].clone())
                    .collect();
                let filters = foreign_key_filters(fk, &values).expect("not NULL");
                let target = db.table_data(&request(&fk.target, filters)).await?;
                assert_eq!(target.total, 1, "{fk:?} {values:?}");
                let id = column(&target, "id")[0].clone();
                assert_eq!(
                    id,
                    column(
                        &data,
                        if fk.columns.len() == 1 {
                            "parent"
                        } else {
                            "pid"
                        }
                    )[row]
                );
            }
        }
        // The NULL row references nothing.
        let fk = details.foreign_key("parent").unwrap();
        assert_eq!(
            foreign_key_filters(fk, &[column(&data, "parent")[2].clone()]),
            None
        );
    }
    run(
        &my,
        &["DROP TABLE t080_fk_child", "DROP TABLE t080_fk_parent"],
    )
    .await?;

    let pg = connect(Driver::Postgres).await?;
    run(
        &pg,
        &[
            "DROP TABLE IF EXISTS t080_fk_child",
            "DROP TABLE IF EXISTS t080_fk_parent",
            "CREATE TABLE t080_fk_parent (a int, b text, label text, PRIMARY KEY (a, b))",
            "INSERT INTO t080_fk_parent VALUES (1, 'x', 'first'), (1, 'y', 'second'), (2, 'x', 'third')",
            "CREATE TABLE t080_fk_child (id serial PRIMARY KEY, pa int, pb text, FOREIGN KEY (pa, pb) REFERENCES t080_fk_parent (a, b))",
            "INSERT INTO t080_fk_child (pa, pb) VALUES (1, 'y'), (2, 'x')",
        ],
    )
    .await?;
    let child = tref(Driver::Postgres, "t080_fk_child");
    let details = pg.column_details(&child).await?;
    let fk = details.foreign_key("pb").unwrap().clone();
    let data = pg.table_data(&request(&child, vec![])).await?;
    for (row, label) in [(0, "second"), (1, "third")] {
        let values = vec![
            column(&data, "pa")[row].clone(),
            column(&data, "pb")[row].clone(),
        ];
        let target = pg
            .table_data(&request(
                &fk.target,
                foreign_key_filters(&fk, &values).unwrap(),
            ))
            .await?;
        assert_eq!(column(&target, "label"), [Some(label.into())]);
    }
    run(
        &pg,
        &["DROP TABLE t080_fk_child", "DROP TABLE t080_fk_parent"],
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn pattern_operators_match_case_like_tableplus() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS t080_names",
                "CREATE TABLE t080_names (id int PRIMARY KEY, name varchar(20))",
                "INSERT INTO t080_names VALUES (1, 'Bob'), (2, 'bobby'), (3, 'ALICE'), (4, 'a_b%c')",
            ],
        )
        .await?;
        let t = tref(d, "t080_names");
        let ids = |data: TableDataResult| -> Vec<String> {
            column(&data, "id").into_iter().flatten().collect()
        };
        let find = |op, value: &'static str| {
            let db = &db;
            let t = &t;
            async move {
                db.table_data(&request(t, vec![rule("name", op, value)]))
                    .await
                    .map(ids)
            }
        };
        if d == Driver::Postgres {
            // TablePlus's PostgreSQL Contains is case-sensitive; the Case insensitive ones aren't.
            assert_eq!(find(FilterOp::Contains, "bob").await?, ["2"]);
            assert_eq!(find(FilterOp::ContainsCi, "bob").await?, ["1", "2"]);
            assert_eq!(find(FilterOp::NotContainsCi, "bob").await?, ["3", "4"]);
            assert_eq!(find(FilterOp::PrefixCi, "ali").await?, ["3"]);
            assert_eq!(find(FilterOp::SuffixCi, "BY").await?, ["2"]);
            assert_eq!(find(FilterOp::Ilike, "B%").await?, ["1", "2"]);
        } else {
            // MySQL's Contains follows the collation (case-insensitive here); the Case sensitive
            // ones compare bytes.
            assert_eq!(find(FilterOp::Contains, "bob").await?, ["1", "2"]);
            assert_eq!(find(FilterOp::ContainsCs, "bob").await?, ["2"]);
            assert_eq!(find(FilterOp::NotContainsCs, "bob").await?, ["1", "3", "4"]);
            assert_eq!(find(FilterOp::PrefixCs, "B").await?, ["1"]);
            assert_eq!(find(FilterOp::SuffixCs, "CE").await?, ["3"]);
        }
        // Wildcards in the value are literal.
        assert_eq!(find(FilterOp::Contains, "_b%").await?, ["4"]);
        assert_eq!(find(FilterOp::Prefix, "a_").await?, ["4"]);
        run(&db, &["DROP TABLE t080_names"]).await?;
    }
    Ok(())
}

#[tokio::test]
async fn several_sorted_columns_order_the_page() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS t080_sort",
                "CREATE TABLE t080_sort (id int PRIMARY KEY, g int, n varchar(5))",
                "INSERT INTO t080_sort VALUES (1, 2, 'b'), (2, 1, 'z'), (3, 2, 'a'), (4, 1, 'y')",
            ],
        )
        .await?;
        let mut req = request(&tref(d, "t080_sort"), vec![]);
        req.order_by = vec![
            OrderBy {
                column: "g".into(),
                dir: Direction::Desc,
            },
            OrderBy {
                column: "n".into(),
                dir: Direction::Asc,
            },
        ];
        let data = db.table_data(&req).await?;
        assert_eq!(
            column(&data, "id")
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
            ["3", "1", "4", "2"]
        );
        run(&db, &["DROP TABLE t080_sort"]).await?;
    }
    Ok(())
}

#[tokio::test]
async fn large_tables_show_an_estimate_and_count_on_request() -> Result<()> {
    let pg = connect(Driver::Postgres).await?;
    run(
        &pg,
        &[
            "DROP TABLE IF EXISTS t080_big",
            "CREATE TABLE t080_big (id int PRIMARY KEY, v text)",
            "INSERT INTO t080_big SELECT g, CASE WHEN g % 2 = 0 THEN 'even' END FROM generate_series(1, 50) g",
            // The planner's statistics, as if the table were huge (superuser on the test box).
            "UPDATE pg_class SET reltuples = 2500000 WHERE oid = 'public.t080_big'::regclass",
        ],
    )
    .await?;
    let t = tref(Driver::Postgres, "t080_big");
    let mut req = request(&t, vec![rule("v", FilterOp::Eq, "even")]);
    req.limit = 10;
    req.estimate = true;
    let data = pg.table_data(&req).await?;
    assert!(data.estimated);
    assert_eq!(data.total, 2_500_000);
    assert_eq!(data.result.rows.len(), 10);
    assert_eq!(pg.count_rows(&req, false).await?, 50);
    assert_eq!(pg.count_rows(&req, true).await?, 25);
    // A raw SQL filter that would write is refused while counting too.
    let mut raw = req.clone();
    raw.filters = vec![rule(
        RAW_SQL,
        FilterOp::Eq,
        "id IN (SELECT 1 FROM (DELETE FROM t080_big RETURNING 1) d)",
    )];
    assert!(pg.count_rows(&raw, true).await.is_err());
    assert_eq!(pg.count_rows(&req, false).await?, 50);
    // Without the estimate flag (or under the threshold) the count is exact.
    req.estimate = false;
    let exact = pg.table_data(&req).await?;
    assert!(!exact.estimated);
    assert_eq!(exact.total, 25);
    run(&pg, &["DROP TABLE t080_big"]).await?;

    let my = connect(Driver::Mysql).await?;
    run(
        &my,
        &[
            "DROP TABLE IF EXISTS t080_small",
            "CREATE TABLE t080_small (id int PRIMARY KEY)",
            "INSERT INTO t080_small VALUES (1), (2), (3)",
        ],
    )
    .await?;
    let mut req = request(&tref(Driver::Mysql, "t080_small"), vec![]);
    req.estimate = true;
    let data = my.table_data(&req).await?;
    assert!(!data.estimated);
    assert_eq!(data.total, 3);
    assert_eq!(my.count_rows(&req, true).await?, 3);
    assert!(
        my.estimated_rows(&tref(Driver::Mysql, "t080_small"))
            .await?
            .is_some()
    );
    run(&my, &["DROP TABLE t080_small"]).await?;
    Ok(())
}

#[tokio::test]
async fn now_is_staged_as_the_exact_value_the_server_stores() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let ddl = if d == Driver::Mysql {
            "CREATE TABLE t080_now (id DOUBLE AUTO_INCREMENT PRIMARY KEY, at DATETIME(3), day DATE, t TIME, plain DATETIME)"
        } else {
            "CREATE TABLE t080_now (id int PRIMARY KEY, at timestamptz(3), day date, t time(0), plain timestamp)"
        };
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS t080_now",
                ddl,
                "INSERT INTO t080_now (id) VALUES (7)",
            ],
        )
        .await?;
        let t = tref(d, "t080_now");
        let details = db.column_details(&t).await?;
        let data = db.table_data(&request(&t, vec![])).await?;
        let mut staged = vec![];
        for name in ["at", "day", "t", "plain"] {
            let detail = details.get(name).unwrap();
            assert!(detail.kind.is_temporal());
            let now = db.now_text(detail).await?.expect("a temporal column");
            staged.push((name, now));
        }
        save(&db, &t, &data, |rows| {
            for (name, now) in &staged {
                let i = data.result.columns.iter().position(|c| c == name).unwrap();
                rows[0].cur[i] = Cell::Text(now.clone());
            }
        })
        .await?;
        let reloaded = db.table_data(&request(&t, vec![])).await?;
        for (name, now) in &staged {
            assert_eq!(
                column(&reloaded, name)[0].as_deref(),
                Some(now.as_str()),
                "{d:?} {name}"
            );
        }
        run(&db, &["DROP TABLE t080_now"]).await?;
    }
    Ok(())
}

#[tokio::test]
async fn pasted_cells_are_saved_through_the_guarded_path() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let ddl = if d == Driver::Mysql {
            "CREATE TABLE t080_paste (id DOUBLE AUTO_INCREMENT PRIMARY KEY, a varchar(10), b varchar(10), c int)"
        } else {
            "CREATE TABLE t080_paste (id int PRIMARY KEY, a varchar(10), b varchar(10), c int)"
        };
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS t080_paste",
                ddl,
                "INSERT INTO t080_paste (id, a, b, c) VALUES (1, 'a1', 'b1', 1), (2, 'a2', 'b2', 2), (3, 'a3', 'b3', 3)",
            ],
        )
        .await?;
        let t = tref(d, "t080_paste");
        let data = db.table_data(&request(&t, vec![])).await?;
        let copied: Vec<Vec<String>> = vec![
            vec!["x".into(), "NULL".into()],
            vec!["y'z".into(), "ไทย".into()],
        ];
        let all_rows: Vec<usize> = (0..3).collect();
        let all_columns: Vec<usize> = (0..4).collect();
        let plan = plan_paste(
            &copied,
            &PasteTarget {
                rows: &[1],
                columns: &[1],
                all_rows: &all_rows,
                all_columns: &all_columns,
            },
            |_, _| true,
            |_| false,
            |_| false,
        )
        .unwrap();
        save(&db, &t, &data, |rows| {
            for (row, col, value) in plan.cells.clone() {
                rows[row].cur[col] = value;
            }
        })
        .await?;
        let after = db.table_data(&request(&t, vec![])).await?;
        assert_eq!(
            column(&after, "a"),
            [Some("a1".into()), Some("x".into()), Some("y'z".into())]
        );
        assert_eq!(
            column(&after, "b"),
            [Some("b1".into()), None, Some("ไทย".into())]
        );
        assert_eq!(
            column(&after, "c"),
            [Some("1".into()), Some("2".into()), Some("3".into())]
        );
        run(&db, &["DROP TABLE t080_paste"]).await?;
    }
    Ok(())
}

#[tokio::test]
async fn copied_insert_statements_load_back_the_same_rows() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let ddl = format!(
            "CREATE TABLE t080_copy (id int PRIMARY KEY, {} varchar(40), n int)",
            quote_ident(d, "we`ird")
        );
        run(&db, &["DROP TABLE IF EXISTS t080_copy", &ddl]).await?;
        run(
            &db,
            &[if d == Driver::Mysql {
                "INSERT INTO t080_copy VALUES (1, 'it''s \\\\ ไทย', NULL), (2, '', 5)"
            } else {
                "INSERT INTO t080_copy VALUES (1, 'it''s \\ ไทย', NULL), (2, '', 5)"
            }],
        )
        .await?;
        let t = tref(d, "t080_copy");
        let data = db.table_data(&request(&t, vec![])).await?;
        let text = copy_text(
            CopyFormat::Insert,
            &CopyRows {
                driver: d,
                target: insert_target(d, &t),
                columns: &data.result.columns,
                binary: &data.result.binary.clone().unwrap_or_default(),
                rows: &data.result.rows,
            },
        );
        assert!(text.contains(&quote_ident(d, "we`ird")));
        run(&db, &["DELETE FROM t080_copy"]).await?;
        for statement in text.lines() {
            db.query(statement).await?;
        }
        let again = db.table_data(&request(&t, vec![])).await?;
        assert_eq!(again.result.rows, data.result.rows, "{d:?}");
        run(&db, &["DROP TABLE t080_copy"]).await?;
    }
    Ok(())
}

/// QA 0.8.0 BUG-001: MySQL 8 writes `DEFAULT_GENERATED` in EXTRA for expression defaults; such
/// columns are ordinary and writable. Only computed (GENERATED ALWAYS) columns are read-only.
#[tokio::test]
async fn qa080_default_generated_columns_are_not_generated() -> Result<()> {
    let db = connect(Driver::Mysql).await?;
    run(
        &db,
        &[
            "DROP TABLE IF EXISTS qa080_dg2",
            "CREATE TABLE qa080_dg2 (id DOUBLE NOT NULL AUTO_INCREMENT PRIMARY KEY, name VARCHAR(50), created_at TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP, updated_at DATETIME(3) DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3), uid VARCHAR(36) DEFAULT (uuid()), g INT AS (CHAR_LENGTH(name)) VIRTUAL, s INT AS (CHAR_LENGTH(name) * 2) STORED)",
        ],
    )
    .await?;
    let d = db.column_details(&tref(Driver::Mysql, "qa080_dg2")).await?;
    run(&db, &["DROP TABLE qa080_dg2"]).await?;
    let g = |n: &str| d.get(n).unwrap().generated;
    assert!(g("g") && g("s"), "computed columns are generated");
    assert!(
        !g("created_at"),
        "DEFAULT CURRENT_TIMESTAMP column flagged generated"
    );
    assert!(!g("updated_at"), "ON UPDATE column flagged generated");
    assert!(!g("uid"), "DEFAULT (uuid()) column flagged generated");
    assert!(!g("id") && !g("name"));
    // PostgreSQL: identity columns stay editable as in 0.7; only GENERATED … STORED is computed.
    let pg = connect(Driver::Postgres).await?;
    run(
        &pg,
        &[
            "DROP TABLE IF EXISTS qa080_dg2",
            "CREATE TABLE qa080_dg2 (a int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, b int GENERATED BY DEFAULT AS IDENTITY, at timestamptz DEFAULT now(), n int, twice int GENERATED ALWAYS AS (n * 2) STORED)",
        ],
    )
    .await?;
    let d = pg
        .column_details(&tref(Driver::Postgres, "qa080_dg2"))
        .await?;
    run(&pg, &["DROP TABLE qa080_dg2"]).await?;
    let g = |n: &str| d.get(n).unwrap().generated;
    assert!(g("twice"));
    assert!(!g("a") && !g("b") && !g("at") && !g("n"));
    Ok(())
}

/// QA 0.8.0: NOW() read on the control connection and written on the editor agree under a
/// startup time zone, so the saved value equals the staged (previewed) one.
#[tokio::test]
async fn qa080_now_with_startup_time_zone() -> Result<()> {
    for (d, startup) in [
        (Driver::Mysql, "SET time_zone = '+07:00'"),
        (Driver::Postgres, "SET TIME ZONE 'Asia/Bangkok'"),
    ] {
        let mut c = empty_connection(d);
        c.host = "127.0.0.1".into();
        c.port = if d == Driver::Mysql { 3307 } else { 5434 };
        c.user = if d == Driver::Mysql {
            "root"
        } else {
            "postgres"
        }
        .into();
        c.database = if d == Driver::Mysql {
            "zixma"
        } else {
            "main_db"
        }
        .into();
        c.startup_sql = startup.into();
        let db = Session::connect_test(&c, "secret").await?;
        let ddl = if d == Driver::Mysql {
            "CREATE TABLE qa080_now (id DOUBLE AUTO_INCREMENT PRIMARY KEY, ts TIMESTAMP(3) NULL, dt DATETIME NULL)"
        } else {
            "CREATE TABLE qa080_now (id int PRIMARY KEY, ts timestamptz(3), dt timestamp(0))"
        };
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS qa080_now",
                ddl,
                "INSERT INTO qa080_now (id) VALUES (1)",
            ],
        )
        .await?;
        let t = tref(d, "qa080_now");
        let details = db.column_details(&t).await?;
        let data = db.table_data(&request(&t, vec![])).await?;
        let mut staged = vec![];
        for n in ["ts", "dt"] {
            staged.push((n, db.now_text(details.get(n).unwrap()).await?.unwrap()));
        }
        save(&db, &t, &data, |rows| {
            for (n, v) in &staged {
                let i = data.result.columns.iter().position(|c| c == n).unwrap();
                rows[0].cur[i] = Cell::Text(v.clone());
            }
        })
        .await?;
        let after = db.table_data(&request(&t, vec![])).await?;
        for (n, v) in &staged {
            assert_eq!(
                column(&after, n)[0].as_deref(),
                Some(v.as_str()),
                "{d:?} {n}"
            );
        }
        run(&db, &["DROP TABLE qa080_now"]).await?;
    }
    Ok(())
}
