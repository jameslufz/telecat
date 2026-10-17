//! These tests are intentionally live and never ignored: unavailable containers fail the test.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::time::Duration;
use telecat_core::{changes::*, sql::dangerous_reason, types::*};
use telecat_db::{Session, validate_test_endpoint};
fn fixture(d: Driver) -> Value {
    serde_json::from_str(include_str!("fixtures/mysql.json"))
        .ok()
        .filter(|_| d == Driver::Mysql)
        .unwrap_or_else(|| serde_json::from_str(include_str!("fixtures/postgres.json")).unwrap())
}
fn normalize(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("durationMs");
        o.remove("sql");
        if let Some(ids) = o.get_mut("rowIds") {
            for id in ids.as_array_mut().unwrap() {
                *id = json!(["OID", "CTID"]);
            }
        }
    }
    v
}
fn st(sql: impl Into<String>, params: Vec<Value>, expect: Option<u64>) -> Stmt {
    Stmt {
        sql: sql.into(),
        params,
        expect,
        check: None,
    }
}
async fn count(db: &Session, table: &str) -> Result<String> {
    Ok(db
        .query(&format!("SELECT COUNT(*) FROM {table}"))
        .await?
        .rows[0][0]
        .clone()
        .unwrap())
}
async fn contract(d: Driver) -> Result<()> {
    let fx = fixture(d);
    let config: ConnectionConfig = serde_json::from_value(fx["config"].clone())?;
    let db = Session::connect_test(&config, "secret")
        .await
        .context("Start the specified test container first")?;
    let pg = d == Driver::Postgres;
    for s in fx["setup"].as_array().unwrap() {
        db.query(s.as_str().unwrap()).await?;
    }
    for case in fx["cases"].as_array().unwrap() {
        let arg = &case["arg"];
        let got = match case["op"].as_str().unwrap() {
            "query" => json!(db.query(arg.as_str().unwrap()).await?),
            "execute" => json!(
                db.execute(&[serde_json::from_value(arg.clone())?], false)
                    .await?[0]
            ),
            "queryError" => json!(
                db.query(arg.as_str().unwrap())
                    .await
                    .unwrap_err()
                    .to_string()
            ),
            "structure" => json!(db.structure(&serde_json::from_value(arg.clone())?).await?),
            "indexes" => json!(db.indexes(&serde_json::from_value(arg.clone())?).await?),
            "ddl" => json!(db.ddl(&serde_json::from_value(arg.clone())?).await?),
            "tableData" => json!(db.table_data(&serde_json::from_value(arg.clone())?).await?),
            "tableDataSql" => json!(
                db.table_data_sql(&serde_json::from_value(arg.clone())?)
                    .await?
            ),
            "listObjects" => json!(
                db.list_objects()
                    .await?
                    .into_iter()
                    .filter(|o| o.table.name.starts_with("telecat_m1_values"))
                    .collect::<Vec<_>>()
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            normalize(got),
            case["out"],
            "{d:?} {} {:?}",
            case["op"],
            arg
        );
    }
    assert!(db.list_databases().await?.contains(&config.database));
    let table = TableRef {
        schema: if pg { "public" } else { "zixma" }.into(),
        name: "telecat_m1_values".into(),
    };
    let info = db.table_info(&table).await?;
    assert!(!info.total_size.is_empty());
    let req = TableDataRequest {
        table: table.clone(),
        filters: vec![],
        combine: Combine::And,
        order_by: None,
        limit: 100,
        offset: 0,
    };
    let result = db.table_data(&req).await?;
    assert_eq!(result.primary_key, ["id"]);
    assert_eq!(result.total, 2);
    if pg {
        assert!(
            result
                .row_ids
                .as_ref()
                .unwrap()
                .iter()
                .all(|r| r.len() == 2 && !r[0].is_empty() && r[1].starts_with('('))
        );
    }
    assert!(db.table_data_sql(&req).await?.contains("LIMIT 100"));
    // Filters are bound, and SQL-shaped values remain literal text.
    let mut req = req.clone();
    req.filters = vec![Filter {
        enabled: true,
        column: "txt".into(),
        op: FilterOp::Eq,
        value: "abc' OR 1=1; DELETE FROM telecat_m1_values; --".into(),
        value2: None,
    }];
    assert_eq!(db.table_data(&req).await?.total, 0);
    assert_eq!(count(&db, "telecat_m1_values").await?, "2");
    req.filters[0] = Filter {
        enabled: true,
        column: RAW_SQL.into(),
        op: FilterOp::Eq,
        value: "1=1); DELETE FROM telecat_m1_values; --".into(),
        value2: None,
    };
    assert!(db.table_data(&req).await.is_err());
    assert_eq!(count(&db, "telecat_m1_values").await?, "2");
    for t in ["telecat_m1_guard", "telecat_m1_dup"] {
        db.query(&format!("DROP TABLE IF EXISTS {t}")).await?;
    }
    db.query("CREATE TABLE telecat_m1_guard (id int PRIMARY KEY, v varchar(40))")
        .await?;
    db.query("INSERT INTO telecat_m1_guard VALUES (1,'a'),(2,'b')")
        .await?;
    let p = if pg { "$1" } else { "?" };
    // Found-rows: unchanged values still match exactly one row.
    db.execute(
        &[st(
            format!("UPDATE telecat_m1_guard SET v={p} WHERE id=1"),
            vec![json!("a")],
            Some(1),
        )],
        true,
    )
    .await?;
    // Wrong count, including zero, rolls back. A previous statement in the same batch also rolls back.
    for expect in [0, 1, 3] {
        let before = db
            .query("SELECT * FROM telecat_m1_guard ORDER BY id")
            .await?
            .rows;
        let statements = [
            st(
                "INSERT INTO telecat_m1_guard VALUES (3,'c')",
                vec![],
                Some(1),
            ),
            st(
                "UPDATE telecat_m1_guard SET v='changed' WHERE id<=2",
                vec![],
                Some(expect),
            ),
        ];
        assert!(db.execute(&statements, true).await.is_err());
        assert_eq!(
            db.query("SELECT * FROM telecat_m1_guard ORDER BY id")
                .await?
                .rows,
            before
        );
    }
    assert!(
        db.execute(
            &[st(
                "UPDATE telecat_m1_guard SET v='stale' WHERE id=999",
                vec![],
                Some(1)
            )],
            true
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("row was not found")
    );
    // Guarded writes cannot accidentally opt out of rollback via transaction=false.
    assert!(
        db.execute(
            &[st(
                "DELETE FROM telecat_m1_guard WHERE id>0",
                vec![],
                Some(1)
            )],
            false
        )
        .await
        .is_err()
    );
    assert_eq!(count(&db, "telecat_m1_guard").await?, "2");
    // SQL errors roll back the whole batch and leave the admin connection usable.
    assert!(
        db.execute(
            &[
                st(
                    "INSERT INTO telecat_m1_guard VALUES (3,'c')",
                    vec![],
                    Some(1)
                ),
                st(
                    "INSERT INTO telecat_m1_guard VALUES (1,'duplicate')",
                    vec![],
                    Some(1)
                )
            ],
            true
        )
        .await
        .is_err()
    );
    assert_eq!(count(&db, "telecat_m1_guard").await?, "2");
    let mut guarded = st(
        "UPDATE telecat_m1_guard SET v='checked' WHERE id=1",
        vec![],
        Some(1),
    );
    guarded.check = Some(Check {
        sql: format!("SELECT COUNT(*) FROM telecat_m1_guard WHERE v={p}"),
        params: vec![json!("missing")],
        expect: 1,
        message: "check blocked".into(),
    });
    assert!(
        db.execute(&[guarded.clone()], true)
            .await
            .unwrap_err()
            .to_string()
            .contains("check blocked (matched 0)")
    );
    guarded.check.as_mut().unwrap().params = vec![json!("a")];
    db.execute(&[guarded], true).await?;
    // A successful prior batch stays committed when the next part fails.
    assert!(
        db.execute(
            &[st(
                "DELETE FROM telecat_m1_guard WHERE id>0",
                vec![],
                Some(1)
            )],
            true
        )
        .await
        .is_err()
    );
    assert_eq!(
        db.query("SELECT v FROM telecat_m1_guard WHERE id=1")
            .await?
            .rows[0][0]
            .as_deref(),
        Some("checked")
    );
    // The commit connection is distinct from the foreground query connection.
    let admin_id = db
        .execute(
            &[Stmt::new(if pg {
                "SELECT pg_backend_pid()"
            } else {
                "SELECT CONNECTION_ID()"
            })],
            false,
        )
        .await?[0]
        .rows[0][0]
        .clone()
        .unwrap();
    assert_ne!(admin_id, db.backend_id);
    // Every public path rejects a second statement, including checks and executable MySQL comments.
    assert!(
        db.query("SELECT 1; DELETE FROM telecat_m1_guard")
            .await
            .is_err()
    );
    assert!(
        db.execute(
            &[Stmt::new("SELECT 1; DELETE FROM telecat_m1_guard")],
            false
        )
        .await
        .is_err()
    );
    if !pg {
        assert!(
            db.query("SELECT 1 /*!; DELETE FROM telecat_m1_guard */")
                .await
                .is_err()
        );
    }
    let mut bad = st("DELETE FROM telecat_m1_guard WHERE id=1", vec![], Some(1));
    bad.check = Some(Check {
        sql: "SELECT 1; DELETE FROM telecat_m1_guard".into(),
        params: vec![],
        expect: 1,
        message: "bad".into(),
    });
    assert!(db.execute(&[bad], true).await.is_err());
    assert_eq!(count(&db, "telecat_m1_guard").await?, "2");
    assert!(db.execute(&[Stmt::new("COMMIT")], true).await.is_err());
    if !pg {
        assert!(
            db.execute(
                &[st("TRUNCATE TABLE telecat_m1_guard", vec![], Some(0))],
                true
            )
            .await
            .is_err()
        );
    }
    if !pg {
        db.query("DROP TABLE IF EXISTS telecat_m1_nontransactional")
            .await?;
        db.query("CREATE TABLE telecat_m1_nontransactional (v int) ENGINE=MyISAM")
            .await?;
        assert!(
            db.execute(
                &[st(
                    "INSERT INTO telecat_m1_nontransactional VALUES (1)",
                    vec![],
                    Some(2)
                )],
                true
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot roll back")
        );
        assert_eq!(count(&db, "telecat_m1_nontransactional").await?, "0");
        let wrong = [st(
            "INSERT INTO telecat_m1_nontransactional VALUES (1)",
            vec![],
            Some(2),
        )];
        let review = db.review_commit(&wrong).await?;
        assert_eq!(review.non_transactional[0].engine, "MyISAM");
        assert!(db.execute_after_review(&wrong, true, true).await.is_err());
        assert_eq!(count(&db, "telecat_m1_nontransactional").await?, "0");
        db.execute_after_review(
            &[st(
                "INSERT INTO telecat_m1_nontransactional VALUES (1),(1),(2)",
                vec![],
                Some(3),
            )],
            true,
            true,
        )
        .await?;
        for sql in [
            "UPDATE telecat_m1_nontransactional SET v=9 WHERE v=1 LIMIT 1",
            "DELETE FROM telecat_m1_nontransactional WHERE v=1 LIMIT 1",
            "UPDATE telecat_m1_nontransactional SET v=9",
            "DELETE FROM telecat_m1_nontransactional",
        ] {
            assert!(
                db.execute_after_review(&[st(sql, vec![], Some(1))], true, true)
                    .await
                    .is_err()
            );
        }
        assert_eq!(count(&db, "telecat_m1_nontransactional").await?, "3");
        db.execute_after_review(
            &[st(
                "UPDATE telecat_m1_nontransactional SET v=? WHERE v=?",
                vec![json!(3), json!(2)],
                Some(1),
            )],
            true,
            true,
        )
        .await?;
        db.execute_after_review(
            &[st(
                "DELETE FROM telecat_m1_nontransactional WHERE v=3",
                vec![],
                Some(1),
            )],
            true,
            true,
        )
        .await?;
        assert_eq!(count(&db, "telecat_m1_nontransactional").await?, "2");
        assert!(db.execute_after_review(&[st("UPDATE telecat_m1_nontransactional a JOIN telecat_m1_guard b ON a.v=b.id SET a.v=0",vec![],Some(1))],true,true).await.is_err());
        db.query("DROP TABLE telecat_m1_nontransactional").await?;
    }
    // Dangerous-statement detection is a required boundary before user SQL runs.
    for sql in [
        "UPDATE telecat_m1_guard SET v='x'",
        "DELETE FROM telecat_m1_guard",
        "DELETE FROM telecat_m1_guard WHERE TRUE",
        "UPDATE telecat_m1_guard SET v=(SELECT 'x' WHERE TRUE)",
    ] {
        assert!(dangerous_reason(sql, d).is_some());
        assert!(db.query_checked(sql, false).await.is_err());
    }
    assert_eq!(count(&db, "telecat_m1_guard").await?, "2");
    // No-PK duplicate rows refuse both update and delete, even with MySQL LIMIT 1.
    db.query("CREATE TABLE telecat_m1_dup (v varchar(40), n int)")
        .await?;
    db.query("INSERT INTO telecat_m1_dup VALUES ('same',1),('same',1),('unique',2)")
        .await?;
    let t = TableRef {
        schema: table.schema.clone(),
        name: "telecat_m1_dup".into(),
    };
    let meta = TableMeta {
        columns: vec!["v".into(), "n".into()],
        primary_key: vec![],
        binary: None,
    };
    let mut r = EditRow {
        key: "r1".into(),
        orig: Some(vec![Some("same".into()), Some("1".into())]),
        cur: vec![Cell::Text("changed".into()), Cell::Text("1".into())],
        deleted: false,
        rid: None,
    };
    for deleted in [false, true] {
        r.deleted = deleted;
        let sql = data_statements(d, &t, &meta, &[r.clone()])?;
        assert!(sql[0].check.is_some());
        assert!(
            db.execute(&sql, true)
                .await
                .unwrap_err()
                .to_string()
                .contains("can't be identified uniquely")
        );
        assert_eq!(count(&db, "telecat_m1_dup").await?, "3");
    }
    r.orig = Some(vec![Some("unique".into()), Some("2".into())]);
    r.cur = vec![Cell::Text("edited".into()), Cell::Text("2".into())];
    r.deleted = false;
    db.execute(&data_statements(d, &t, &meta, &[r])?, true)
        .await?;
    if pg {
        // Physical identity targets one duplicate and must become stale after it moves.
        let req = TableDataRequest {
            table: t.clone(),
            filters: vec![],
            combine: Combine::And,
            order_by: None,
            limit: 100,
            offset: 0,
        };
        let data = db.table_data(&req).await?;
        let mut rows = to_edit_rows(&data.result.rows, data.row_ids.as_deref());
        let i = rows
            .iter()
            .position(|r| r.cur[0].value() == Some("same"))
            .unwrap();
        rows[i].cur[0] = Cell::Text("physical".into());
        let changes = data_statements(d, &t, &meta, &[rows[i].clone()])?;
        db.execute(&changes, true).await?;
        assert!(db.execute(&changes, true).await.is_err());
        // The guarded type conversion refuses value truncation.
        let mut alter = Stmt::new(
            "ALTER TABLE telecat_m1_guard ALTER COLUMN v TYPE varchar(2) USING v::varchar(2)",
        );
        alter.check=Some(Check{sql:"SELECT COUNT(*) FROM telecat_m1_guard WHERE v IS NOT NULL AND (v::varchar(2))::text IS DISTINCT FROM v::text".into(),params:vec![],expect:0,message:"conversion would truncate".into()});
        assert!(
            db.execute(&[alter], true)
                .await
                .unwrap_err()
                .to_string()
                .contains("conversion would truncate")
        );
        assert_eq!(
            db.query("SELECT v FROM telecat_m1_guard WHERE id=1")
                .await?
                .rows[0][0]
                .as_deref(),
            Some("checked")
        );
    }
    // A commit does not block foreground reads or land in the foreground transaction.
    let sleep = if pg {
        "SELECT pg_sleep(0.2)"
    } else {
        "SELECT SLEEP(0.2)"
    };
    let statements = [Stmt::new(sleep)];
    let (a, b) = tokio::join!(db.execute(&statements, true), async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        tokio::time::timeout(Duration::from_secs(2), db.query("SELECT 42")).await?
    });
    a?;
    assert_eq!(b?.rows[0][0].as_deref(), Some("42"));
    // Process list and cancellation: wait until the actual slow query is visible before killing it.
    let sleep = if pg {
        "SELECT pg_sleep(15) AS telecat_m1_cancel"
    } else {
        "SELECT id FROM telecat_m1_guard WHERE SLEEP(15)=0 /* telecat_m1_cancel */"
    };
    let (run, cancel) = tokio::join!(db.query(sleep), async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let processes = db.process_list(Some(&config.database)).await?;
                if processes
                    .rows
                    .iter()
                    .flatten()
                    .flatten()
                    .any(|s| s.contains("telecat_m1_cancel"))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        db.cancel().await
    });
    cancel?;
    assert!(run.is_err(), "{d:?} cancellation must stop the query");
    assert_eq!(
        db.query("SELECT 42").await?.rows[0][0].as_deref(),
        Some("42")
    );
    assert!(db.kill("1; DELETE FROM t", false).await.is_err());
    // Connection kill is exercised only on a new test session.
    let victim = Session::connect_test(&config, "secret").await?;
    db.kill(&victim.backend_id, true).await?;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(victim.query("SELECT 1").await.is_err());
    victim.close().await;
    for name in ["telecat_m1_values", "telecat_m1_guard", "telecat_m1_dup"] {
        db.query(&format!("DROP TABLE {name}")).await?;
    }
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn mysql_live_contract() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), contract(Driver::Mysql)).await?
}
#[tokio::test]
async fn postgres_live_contract() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), contract(Driver::Postgres)).await?
}
#[test]
fn endpoint_fence() {
    for d in [Driver::Mysql, Driver::Postgres] {
        let mut c = empty_connection(d);
        for port in [3306, 5432, 5433] {
            c.port = port;
            assert!(validate_test_endpoint(&c).is_err());
        }
        c.port = if d == Driver::Mysql { 3307 } else { 5434 };
        assert!(validate_test_endpoint(&c).is_ok());
        for host in ["localhost", "example.com", "::1", "/tmp/mysql.sock"] {
            c.host = host.into();
            assert!(validate_test_endpoint(&c).is_err());
        }
    }
}

#[test]
fn endpoint_fence_is_opt_in() {
    if let Ok(mode) = std::env::var("TELECAT_FENCE_CHILD") {
        let mut c = empty_connection(Driver::Mysql);
        c.host = "user-configured.example".into();
        c.port = 3306;
        assert_eq!(telecat_db::validate_endpoint(&c).is_err(), mode == "on");
        return;
    }
    for (mode, value) in [("on", "1"), ("off", "0")] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "endpoint_fence_is_opt_in"])
            .env("TELECAT_FENCE_CHILD", mode)
            .env("TELECAT_TEST_DB_ONLY", value)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
