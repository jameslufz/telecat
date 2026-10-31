//! Live: views and materialized views, routines and triggers, autocomplete columns and export.
//! Only the throwaway test containers (MySQL :3307, PostgreSQL :5434) are used.
use anyhow::Result;
use telecat_core::{
    changes::{Stmt, table_op_statement},
    export::{ExportFormat, ExportWriter, insert_target},
    objects::{Trigger, TriggerEdit, routine_save, trigger_saves},
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
        db.query(s).await?;
    }
    Ok(())
}
fn schema(d: Driver) -> &'static str {
    if d == Driver::Mysql {
        "zixma"
    } else {
        "public"
    }
}
fn tref(d: Driver, name: &str) -> TableRef {
    TableRef {
        schema: schema(d).into(),
        name: name.into(),
    }
}
async fn scalar(db: &Session, sql: &str) -> Result<Option<String>> {
    Ok(db.query(sql).await?.rows.first().and_then(|r| r[0].clone()))
}

#[tokio::test]
async fn views_have_their_own_kind_ddl_and_drop() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        run(
            &db,
            &[
                "DROP VIEW IF EXISTS t060_v",
                "DROP TABLE IF EXISTS t060_base",
                "CREATE TABLE t060_base (id int PRIMARY KEY, name varchar(20))",
                "INSERT INTO t060_base VALUES (1, 'a'), (2, 'b')",
                "CREATE VIEW t060_v AS SELECT id, name FROM t060_base WHERE id > 1",
            ],
        )
        .await?;
        if d == Driver::Postgres {
            run(
                &db,
                &[
                    "DROP MATERIALIZED VIEW IF EXISTS t060_mv",
                    "CREATE MATERIALIZED VIEW t060_mv AS SELECT id FROM t060_base",
                ],
            )
            .await?;
        }
        let objects = db.list_objects().await?;
        let kind = |n: &str| objects.iter().find(|o| o.table.name == n).map(|o| o.kind);
        assert_eq!(kind("t060_base"), Some(ObjectKind::Table));
        assert_eq!(kind("t060_v"), Some(ObjectKind::View));
        assert_eq!(
            db.relation_kind(&tref(d, "t060_v")).await?,
            ObjectKind::View
        );
        let ddl = db.ddl(&tref(d, "t060_v")).await?;
        assert!(!ddl.contains("CREATE TABLE"), "{d:?}: {ddl}");
        assert!(
            ddl.contains("VIEW") && ddl.contains("t060_base"),
            "{d:?}: {ddl}"
        );
        if d == Driver::Postgres {
            assert!(
                ddl.starts_with("CREATE OR REPLACE VIEW \"public\".\"t060_v\" AS\n"),
                "{ddl}"
            );
            assert!(ddl.ends_with(';') && !ddl.ends_with(";;"), "{ddl}");
            assert_eq!(kind("t060_mv"), Some(ObjectKind::MaterializedView));
            let ddl = db.ddl(&tref(d, "t060_mv")).await?;
            assert!(
                ddl.starts_with("CREATE MATERIALIZED VIEW \"public\".\"t060_mv\" AS\n"),
                "{ddl}"
            );
            // The staged Delete of a materialized view is DROP MATERIALIZED VIEW, which PostgreSQL
            // accepts (DROP VIEW is rejected for it).
            let mv = objects.iter().find(|o| o.table.name == "t060_mv").unwrap();
            db.execute(&[table_op_statement(d, mv, false)?], true)
                .await?;
            assert!(
                db.list_objects()
                    .await?
                    .iter()
                    .all(|o| o.table.name != "t060_mv")
            );
            assert!(
                table_op_statement(
                    d,
                    &objects
                        .iter()
                        .find(|o| o.table.name == "t060_v")
                        .unwrap()
                        .clone(),
                    true
                )
                .is_err()
            );
        }
        let view = objects.iter().find(|o| o.table.name == "t060_v").unwrap();
        db.execute(
            &[table_op_statement(d, view, false)?],
            d == Driver::Postgres,
        )
        .await?;
        assert!(
            db.list_objects()
                .await?
                .iter()
                .all(|o| o.table.name != "t060_v")
        );
        run(&db, &["DROP TABLE t060_base"]).await?;
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn mysql_routine_round_trip_with_begin_end_body() -> Result<()> {
    let d = Driver::Mysql;
    let db = connect(d).await?;
    run(&db, &[
        "DROP FUNCTION IF EXISTS t060_add",
        "CREATE FUNCTION t060_add(x INT) RETURNS INT DETERMINISTIC BEGIN DECLARE y INT; SET y = x + 1; RETURN y; END",
    ]).await?;
    let object = db
        .list_objects()
        .await?
        .into_iter()
        .find(|o| o.table.name == "t060_add")
        .expect("listed");
    assert_eq!(object.kind, ObjectKind::Function);
    let loaded = db.routine_definition(&object).await?;
    let original = loaded.text.clone();
    assert!(
        original.contains("SET y = x + 1") && original.ends_with(";\n"),
        "{original}"
    );
    let edited = original.replace("x + 1", "x + 10");
    let save = routine_save(d, &object, &original, &edited, &loaded.settings)?;
    db.execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
        .await?;
    assert_eq!(
        scalar(&db, "SELECT t060_add(1)").await?.as_deref(),
        Some("11")
    );
    // A broken definition is refused while trying it out under a temporary name: nothing was
    // dropped, and no temporary routine is left behind.
    let broken = routine_save(
        d,
        &object,
        &edited,
        &edited.replace("RETURN y;", "RETURN y +;"),
        &loaded.settings,
    )?;
    assert!(
        db.execute_after_review_with(&broken.statements, false, false, &broken.settings)
            .await
            .is_err()
    );
    assert!(db.routine_exists(&object).await?);
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM information_schema.routines WHERE routine_schema = 'zixma' AND routine_name LIKE 't060_add%telecat_check%'").await?.as_deref(),
        Some("0")
    );
    // If it was dropped anyway, the restore puts the saved original back.
    run(&db, &["DROP FUNCTION t060_add"]).await?;
    db.execute_after_review_with(&broken.restore, false, false, &broken.settings)
        .await?;
    assert!(db.routine_exists(&object).await?);
    assert_eq!(
        scalar(&db, "SELECT t060_add(1)").await?.as_deref(),
        Some("11")
    );
    run(&db, &["DROP FUNCTION t060_add"]).await?;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn postgres_routines_keep_overloads_apart() -> Result<()> {
    let d = Driver::Postgres;
    let db = connect(d).await?;
    run(&db, &[
        "DROP FUNCTION IF EXISTS t060_f(integer)",
        "DROP FUNCTION IF EXISTS t060_f(text)",
        "DROP AGGREGATE IF EXISTS t060_agg(integer)",
        "CREATE FUNCTION t060_f(x integer) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN x + 1; END; $$",
        "CREATE FUNCTION t060_f(x text) RETURNS text LANGUAGE sql AS $$ SELECT x || '!' $$",
        "CREATE AGGREGATE t060_agg(integer) (SFUNC = int4pl, STYPE = integer)",
    ]).await?;
    let objects = db.list_objects().await?;
    let overloads: Vec<_> = objects
        .iter()
        .filter(|o| o.table.name == "t060_f")
        .collect();
    assert_eq!(overloads.len(), 2);
    assert!(
        objects.iter().all(|o| o.table.name != "t060_agg"),
        "aggregates aren't listed"
    );
    let int = overloads
        .iter()
        .find(|o| o.signature.as_deref() == Some("x integer"))
        .unwrap();
    let original = db.routine_definition(int).await?.text;
    assert!(
        original.starts_with("CREATE OR REPLACE FUNCTION public.t060_f(x integer)"),
        "{original}"
    );
    let save = routine_save(
        d,
        int,
        &original,
        &original.replace("x + 1", "x + 5"),
        &Default::default(),
    )?;
    db.execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
        .await?;
    assert_eq!(scalar(&db, "SELECT t060_f(1)").await?.as_deref(), Some("6"));
    assert_eq!(
        scalar(&db, "SELECT t060_f('a'::text)").await?.as_deref(),
        Some("a!")
    );
    // Delete drops this overload only.
    db.execute(&[table_op_statement(d, int, false)?], true)
        .await?;
    let left: Vec<_> = db
        .list_objects()
        .await?
        .into_iter()
        .filter(|o| o.table.name == "t060_f")
        .collect();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].signature.as_deref(), Some("x text"));
    run(
        &db,
        &[
            "DROP FUNCTION t060_f(text)",
            "DROP AGGREGATE t060_agg(integer)",
        ],
    )
    .await?;
    db.close().await;
    Ok(())
}

fn trig(name: &str, timing: &str, event: &str, statement: &str) -> Trigger {
    Trigger {
        name: name.into(),
        timing: timing.into(),
        event: event.into(),
        statement: statement.into(),
        ..Default::default()
    }
}
#[tokio::test]
async fn triggers_are_listed_created_edited_and_dropped() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        let table = tref(d, "t060_trg");
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS t060_trg",
                "CREATE TABLE t060_trg (id int PRIMARY KEY, n int, note varchar(20))",
            ],
        )
        .await?;
        if d == Driver::Postgres {
            run(&db, &[
                "CREATE OR REPLACE FUNCTION t060_trg_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.n := 7; RETURN NEW; END; $$",
                // An FK adds internal triggers; they must not show up.
                "DROP TABLE IF EXISTS t060_child",
                "CREATE TABLE t060_child (id int PRIMARY KEY, parent int REFERENCES t060_trg(id))",
            ]).await?;
        }
        assert!(db.triggers(&table).await?.is_empty());
        let created = if d == Driver::Mysql {
            trig(
                "t060_bi",
                "BEFORE",
                "INSERT",
                "BEGIN\n  SET NEW.n = 7;\n  SET NEW.note = 'x';\nEND",
            )
        } else {
            trig(
                "t060_bi",
                "BEFORE",
                "INSERT",
                "FOR EACH ROW EXECUTE FUNCTION t060_trg_fn()",
            )
        };
        let edits = vec![TriggerEdit {
            key: "1".into(),
            orig: None,
            cur: created.clone(),
            deleted: false,
        }];
        for (_, save) in trigger_saves(d, &table, ObjectKind::Table, &edits)? {
            db.execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
                .await?;
        }
        run(&db, &["INSERT INTO t060_trg (id, n) VALUES (1, 0)"]).await?;
        assert_eq!(
            scalar(&db, "SELECT n FROM t060_trg WHERE id = 1")
                .await?
                .as_deref(),
            Some("7")
        );
        let listed = db.triggers(&table).await?;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(
            (
                listed[0].name.as_str(),
                listed[0].timing.as_str(),
                listed[0].event.as_str()
            ),
            ("t060_bi", "BEFORE", "INSERT")
        );
        // Edit: rename and move to UPDATE.
        let mut edited = listed[0].clone();
        edited.name = "t060_bu".into();
        edited.event = "UPDATE".into();
        let edits = vec![TriggerEdit {
            key: "1".into(),
            orig: Some(listed[0].clone()),
            cur: edited,
            deleted: false,
        }];
        for (_, save) in trigger_saves(d, &table, ObjectKind::Table, &edits)? {
            db.execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
                .await?;
        }
        let listed = db.triggers(&table).await?;
        assert_eq!(
            listed
                .iter()
                .map(|t| (t.name.as_str(), t.event.as_str()))
                .collect::<Vec<_>>(),
            [("t060_bu", "UPDATE")]
        );
        // A broken edit on PostgreSQL rolls back as a whole: the old trigger stays.
        if d == Driver::Postgres {
            let mut bad = listed[0].clone();
            bad.statement = "FOR EACH ROW EXECUTE FUNCTION t060_missing_fn()".into();
            let edits = vec![TriggerEdit {
                key: "1".into(),
                orig: Some(listed[0].clone()),
                cur: bad,
                deleted: false,
            }];
            let save = &trigger_saves(d, &table, ObjectKind::Table, &edits)?[0].1;
            assert!(
                db.execute_after_review_with(&save.statements, true, false, &save.settings)
                    .await
                    .is_err()
            );
            assert_eq!(db.triggers(&table).await?.len(), 1);
        }
        let edits = vec![TriggerEdit {
            key: "1".into(),
            orig: Some(listed[0].clone()),
            cur: listed[0].clone(),
            deleted: true,
        }];
        for (_, save) in trigger_saves(d, &table, ObjectKind::Table, &edits)? {
            db.execute_after_review_with(&save.statements, save.transaction, false, &save.settings)
                .await?;
        }
        assert!(db.triggers(&table).await?.is_empty());
        if d == Driver::Postgres {
            run(
                &db,
                &[
                    "DROP TABLE t060_child",
                    "DROP TABLE t060_trg",
                    "DROP FUNCTION t060_trg_fn()",
                ],
            )
            .await?;
        } else {
            run(&db, &["DROP TABLE t060_trg"]).await?;
        }
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn autocomplete_columns_come_in_one_batch() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS t060_cols",
                "CREATE TABLE t060_cols (id int, `ชื่อ` text, zeta int)"
                    .replace('`', if d == Driver::Mysql { "`" } else { "\"" })
                    .as_str(),
            ],
        )
        .await?;
        let columns = db
            .column_names(&[tref(d, "t060_cols"), tref(d, "t060_missing_table")])
            .await?;
        assert_eq!(
            columns.get(&(schema(d).to_owned(), "t060_cols".to_owned())),
            Some(&vec!["id".to_owned(), "ชื่อ".to_owned(), "zeta".to_owned()])
        );
        assert_eq!(columns.len(), 1);
        // On PostgreSQL a failing lookup in a user's open transaction would abort it: the
        // editor's connection keeps working after a lookup.
        db.query("BEGIN").await.ok();
        db.column_names(&[tref(d, "t060_cols")]).await?;
        assert_eq!(scalar(&db, "SELECT 1").await?.as_deref(), Some("1"));
        db.query("ROLLBACK").await.ok();
        run(&db, &["DROP TABLE t060_cols"]).await?;
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn export_streams_the_whole_filtered_sorted_table() -> Result<()> {
    for d in [Driver::Mysql, Driver::Postgres] {
        let db = connect(d).await?;
        run(
            &db,
            &[
                "DROP TABLE IF EXISTS t060_export",
                "CREATE TABLE t060_export (id int PRIMARY KEY, grp int, note text)",
            ],
        )
        .await?;
        // More rows than one page or one export chunk.
        let values: Vec<String> = (1..=7500)
            .map(|i| format!("({i}, {}, 'n{i}')", i % 3))
            .collect();
        run(
            &db,
            &[&format!(
                "INSERT INTO t060_export VALUES {}",
                values.join(", ")
            )],
        )
        .await?;
        let req = TableDataRequest {
            table: tref(d, "t060_export"),
            filters: vec![Filter {
                enabled: true,
                column: "grp".into(),
                op: FilterOp::Eq,
                value: "1".into(),
                value2: None,
            }],
            combine: Combine::And,
            order_by: vec![OrderBy {
                column: "id".into(),
                dir: Direction::Desc,
            }],
            estimate: false,
            limit: 300,
            offset: 600,
        };
        let mut writer: Option<ExportWriter> = None;
        let mut out = String::new();
        let mut chunks = 0;
        let n = db
            .export_table(&req, &mut |columns, binary, rows| {
                let w = writer.get_or_insert_with(|| {
                    let w = ExportWriter::new(
                        ExportFormat::Csv,
                        d,
                        insert_target(d, &req.table),
                        columns.to_vec(),
                        binary.to_vec(),
                    );
                    out += &w.header();
                    w
                });
                out += &w.rows(&rows);
                chunks += 1;
                Ok(())
            })
            .await?;
        assert_eq!(n, 2500);
        assert!(chunks >= 2, "{chunks}");
        let lines: Vec<&str> = out.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2501);
        assert_eq!(lines[0], "\u{feff}id,grp,note");
        assert_eq!(lines[1], "7498,1,n7498");
        assert_eq!(lines[2500], "1,1,n1");
        // A raw filter that writes is refused: exports are read-only.
        let mut raw = req.clone();
        raw.filters = vec![Filter {
            enabled: true,
            column: RAW_SQL.into(),
            op: FilterOp::Eq,
            value: "nextval('t060_export_seq') > 0".into(),
            value2: None,
        }];
        if d == Driver::Postgres {
            run(
                &db,
                &[
                    "DROP SEQUENCE IF EXISTS t060_export_seq",
                    "CREATE SEQUENCE t060_export_seq",
                ],
            )
            .await?;
            let e = db
                .export_table(&raw, &mut |_, _, _| Ok(()))
                .await
                .unwrap_err();
            assert!(format!("{e:#}").contains("read-only"), "{e:#}");
            run(&db, &["DROP SEQUENCE t060_export_seq"]).await?;
        }
        // Stopping from the sink stops the export.
        let stopped = db
            .export_table(&req, &mut |_, _, _| anyhow::bail!("Export cancelled"))
            .await;
        assert!(stopped.unwrap_err().to_string().contains("cancelled"));
        db.execute(&[Stmt::new("DROP TABLE t060_export")], false)
            .await?;
        db.close().await;
    }
    Ok(())
}
