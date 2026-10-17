use anyhow::Result;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use telecat_app::{credentials::MemoryKeychain, store::Profile, workspace_model::*};
use telecat_core::{changes::Stmt, types::*};
use telecat_db::Session;
fn config(driver: Driver) -> ConnectionConfig {
    let mut c = empty_connection(driver);
    c.id = "shell-tests".into();
    c.host = "127.0.0.1".into();
    c.port = if driver == Driver::Mysql { 3307 } else { 5434 };
    c.user = if driver == Driver::Mysql {
        "root"
    } else {
        "postgres"
    }
    .into();
    c.database = if driver == Driver::Mysql {
        "zixma"
    } else {
        "main_db"
    }
    .into();
    c
}
#[test]
fn navigation_close_restore_and_status() {
    let mut s = WorkspaceState::new(config(Driver::Mysql));
    s.server_version = "MySQL 8.0.46".into();
    s.tls = Some("TLSv1.3".into());
    assert_eq!(
        s.status_text(),
        "NONE | MySQL 8.0.46 : TLSv1.3 : root : zixma : SQL Query"
    );
    let a = s.open_query("Saved", "SELECT 'ธนาคารกรุงเทพ 😺'");
    let table = TableRef {
        schema: "zixma".into(),
        name: "a.b".into(),
    };
    let b = s.open_table(table.clone());
    assert_eq!(s.open_table(table), b);
    assert_eq!(s.tabs.len(), 3);
    s.navigate(false);
    assert_eq!(s.active, a);
    s.navigate(false);
    assert_eq!(s.active, 1);
    s.navigate(true);
    assert_eq!(s.active, a);
    assert!(s.close_tab(a));
    s.navigate(true);
    assert_eq!(s.active, b);
    assert!(s.close_tab(b));
    assert!(!s.close_tab(1));
    s.restore_drafts(&[
        SqlDraft {
            title: "Thai".into(),
            sql: "SELECT '😺'".into(),
        },
        SqlDraft {
            title: "Two".into(),
            sql: "SELECT 2".into(),
        },
    ]);
    assert_eq!(s.current().title, "Thai");
    assert_eq!(s.drafts().len(), 2);
    s.step(-1);
    assert_eq!(s.current().title, "Two");
    // Identifiers with dots stay distinct; public PG names are shown without a prefix.
    assert_eq!(
        display_name(
            &TableRef {
                schema: "QA Schema".into(),
                name: "Mixed Tbl".into()
            },
            Driver::Postgres
        ),
        "QA Schema.Mixed Tbl"
    );
}
#[test]
fn query_selection_utf16_limits_and_both_gates() {
    let mut doc = EditorDocument {
        text: "SELECT '😺';\nUPDATE t SET name = 'ไทย' WHERE id = 1;\nDELETE FROM t".into(),
        cursor_utf16: 15,
        selection: None,
    };
    let p = QueryPlan::new(&doc, false, Driver::Mysql, SafeMode::Alert2, 1000);
    assert_eq!(p.original.len(), 1);
    assert!(p.original[0].starts_with("UPDATE"));
    assert_eq!(p.gate, Gate::Alert);
    assert!(p.dangers.is_empty());
    let p = QueryPlan::new(&doc, true, Driver::Mysql, SafeMode::Silent, 1000);
    assert_eq!(p.original.len(), 3);
    assert_eq!(p.dangers.len(), 1);
    assert_eq!(p.gate, Gate::None);
    assert!(p.statements[0].ends_with("LIMIT 1000"));
    doc.selection = Some("SELECT 1; SELECT 2".into());
    for mode in [
        SafeMode::Silent,
        SafeMode::Alert1,
        SafeMode::Alert2,
        SafeMode::Safe1,
        SafeMode::Safe2,
    ] {
        let p = QueryPlan::new(&doc, false, Driver::Postgres, mode, 0);
        assert_eq!(p.original.len(), 2);
        assert_eq!(p.statements, p.original);
        assert_eq!(
            p.gate,
            match mode {
                SafeMode::Alert1 => Gate::Alert,
                SafeMode::Safe1 => Gate::Password,
                _ => Gate::None,
            }
        );
    }
    doc.selection = Some("/* hidden */ DELETE FROM t".into());
    let p = QueryPlan::new(&doc, true, Driver::Postgres, SafeMode::Safe2, 1000);
    assert_eq!(p.gate, Gate::Password);
    assert_eq!(p.dangers.len(), 1);
}
#[test]
fn history_round_trip_import_shape_dedup_and_limit() -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.cache/shell-tests")
        .join(uuid::Uuid::new_v4().to_string());
    let p = Profile::new(root.clone(), Arc::new(MemoryKeychain::default()));
    for n in 0..505 {
        p.add_history(
            "one",
            LogEntry {
                time: n.to_string(),
                sql: format!("SELECT {n} -- ไทย 😺"),
            },
        )?;
    }
    p.add_history(
        "two",
        LogEntry {
            time: "now".into(),
            sql: "SELECT 2".into(),
        },
    )?;
    p.add_history(
        "one",
        LogEntry {
            time: "new".into(),
            sql: "SELECT 500 -- ไทย 😺".into(),
        },
    )?;
    let reopened = Profile::new(root.clone(), Arc::new(MemoryKeychain::default()));
    let history = reopened.history("one");
    assert_eq!(history.len(), 500);
    assert_eq!(history[0].time, "new");
    assert_eq!(
        history.iter().filter(|e| e.sql == history[0].sql).count(),
        1
    );
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("history.json"))?)?;
    assert_eq!(
        json["one"][0],
        serde_json::json!({"time":"new","sql":"SELECT 500 -- ไทย 😺"})
    );
    reopened.clear_history("one")?;
    assert!(reopened.history("one").is_empty());
    assert_eq!(reopened.history("two").len(), 1);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
async fn live(driver: Driver) -> Result<()> {
    let s = Arc::new(Session::connect_test(&config(driver), "secret").await?);
    let table = TableRef {
        schema: if driver == Driver::Mysql {
            "zixma"
        } else {
            "public"
        }
        .into(),
        name: "telecat_shell_m3".into(),
    };
    s.query("DROP TABLE IF EXISTS telecat_shell_m3").await?;
    s.query("CREATE TABLE telecat_shell_m3 (id INTEGER PRIMARY KEY, name VARCHAR(100))")
        .await?;
    for n in 1..=7 {
        s.query(&format!(
            "INSERT INTO telecat_shell_m3 VALUES ({n}, 'ธนาคารกรุงเทพ 😺')"
        ))
        .await?;
    }
    assert!(s.list_objects().await?.iter().any(|o| o.table == table));
    assert!(s.list_databases().await?.contains(&s.database));
    assert_eq!(s.structure(&table).await?.rows.len(), 2);
    assert!(!s.indexes(&table).await?.rows.is_empty());
    assert!(!s.ddl(&table).await?.is_empty());
    assert!(!s.table_info(&table).await?.total_size.is_empty());
    let req = TableDataRequest {
        table: table.clone(),
        filters: vec![Filter {
            enabled: true,
            column: "id".into(),
            op: FilterOp::Ge,
            value: "3".into(),
            value2: None,
        }],
        combine: Combine::And,
        order_by: Some(OrderBy {
            column: "id".into(),
            dir: Direction::Desc,
        }),
        limit: 2,
        offset: 2,
    };
    let data = s.table_data(&req).await?;
    assert_eq!(data.total, 5);
    assert_eq!(
        data.result
            .rows
            .iter()
            .map(|r| r[0].as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["5", "4"]
    );
    // Separate admin commit, wrong-count rollback, main connection remains usable.
    let mut wrong = Stmt::new("UPDATE telecat_shell_m3 SET name='changed' WHERE id=1");
    wrong.expect = Some(2);
    assert!(s.execute(&[wrong.clone()], true).await.is_err());
    assert_eq!(
        s.query("SELECT name FROM telecat_shell_m3 WHERE id=1")
            .await?
            .rows[0][0]
            .as_deref(),
        Some("ธนาคารกรุงเทพ 😺")
    );
    wrong.expect = Some(1);
    telecat_app::commit::execute_reviewed(&s, &[wrong], |_| async { false }).await?;
    let doc=EditorDocument{text:"SELECT 1; SELECT 2; SELECT * FROM telecat_shell_missing; UPDATE telecat_shell_m3 SET name='unsafe' WHERE id=1".into(),..Default::default()};
    let plan = QueryPlan::new(&doc, true, driver, SafeMode::Silent, 1000);
    let cancelled = AtomicBool::new(false);
    let out = run_plan(&s, &plan, QueryApproval::default(), &cancelled).await?;
    assert_eq!(out.len(), 3);
    assert!(out[2].error.is_some());
    assert_eq!(
        s.query("SELECT name FROM telecat_shell_m3 WHERE id=1")
            .await?
            .rows[0][0]
            .as_deref(),
        Some("changed")
    );
    let danger = QueryPlan::new(
        &EditorDocument {
            text: "DELETE FROM telecat_shell_m3".into(),
            ..Default::default()
        },
        true,
        driver,
        SafeMode::Silent,
        0,
    );
    assert!(
        run_plan(&s, &danger, QueryApproval::default(), &cancelled)
            .await
            .is_err()
    );
    let gate = QueryPlan::new(
        &EditorDocument {
            text: "SELECT 1".into(),
            ..Default::default()
        },
        true,
        driver,
        SafeMode::Safe1,
        0,
    );
    assert!(
        run_plan(&s, &gate, QueryApproval::default(), &cancelled)
            .await
            .is_err()
    );
    let sql = if driver == Driver::Mysql {
        "SELECT SLEEP(20)"
    } else {
        "SELECT pg_sleep(20)"
    };
    // Cancel must prevent the next statement even if MySQL's SLEEP reports success.
    let plan = QueryPlan::new(
        &EditorDocument {
            text: format!("{sql}; UPDATE telecat_shell_m3 SET name='unsafe' WHERE id=1"),
            ..Default::default()
        },
        true,
        driver,
        SafeMode::Silent,
        0,
    );
    let flag = Arc::new(AtomicBool::new(false));
    let worker = s.clone();
    let worker_flag = flag.clone();
    let job = tokio::spawn(async move {
        run_plan(&worker, &plan, QueryApproval::default(), &worker_flag).await
    });
    wait_running(&s).await?;
    flag.store(true, Ordering::SeqCst);
    s.cancel().await?;
    let out = tokio::time::timeout(Duration::from_secs(4), job).await???;
    assert!(out.iter().any(|r| r.error.is_some()));
    assert_eq!(
        s.query("SELECT name FROM telecat_shell_m3 WHERE id=1")
            .await?
            .rows[0][0]
            .as_deref(),
        Some("changed")
    );
    // Running Queries kills a separate session's active statement by backend ID.
    let other = Arc::new(Session::connect_test(&config(driver), "secret").await?);
    let worker = other.clone();
    let job = tokio::spawn(async move { worker.query(sql).await });
    wait_id_running(&s, &other.backend_id).await?;
    s.kill(&other.backend_id, false).await?;
    let _ = tokio::time::timeout(Duration::from_secs(4), job).await?;
    other.close().await;
    s.query("DROP TABLE telecat_shell_m3").await?;
    s.close().await;
    Ok(())
}
async fn wait_running(s: &Session) -> Result<()> {
    wait_id_running(s, &s.backend_id).await
}
async fn wait_id_running(s: &Session, id: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let list = s.process_list(None).await?;
            if list.rows.iter().any(|r| {
                r.first().and_then(|v| v.as_deref()) == Some(id)
                    && r.iter()
                        .flatten()
                        .any(|v| v.contains("SLEEP(20)") || v.contains("pg_sleep(20)"))
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await??;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql_shell_live() -> Result<()> {
    live(Driver::Mysql).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_shell_live() -> Result<()> {
    live(Driver::Postgres).await
}
