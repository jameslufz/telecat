//! QA 0.3.0 regression tests: Messages panel error line/column, live.
use anyhow::Result;
use telecat_app::workspace_model::*;
use telecat_core::types::*;
use telecat_db::Session;

fn config(d: Driver, db: &str) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "qa030-pos".into();
    c.host = "127.0.0.1".into();
    let mysql = d == Driver::Mysql;
    c.port = if mysql { 3307 } else { 5434 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = db.into();
    c
}
/// (line, column) of the first char of the `nth` occurrence of `marker` in `doc`, CR ignored.
fn expected(doc: &str, marker: &str, nth: usize) -> (usize, usize) {
    let byte = doc.match_indices(marker).nth(nth).unwrap().0;
    let (mut line, mut col) = (1, 1);
    for c in doc[..byte].chars() {
        if c == '\n' {
            line += 1;
            col = 1;
        } else if c != '\r' {
            col += 1;
        }
    }
    (line, col)
}
async fn check(
    s: &Session,
    d: Driver,
    name: &str,
    doc: &str,
    marker: &str,
    nth: usize,
    bad: &mut Vec<String>,
) {
    let plan = QueryPlan::new(
        &EditorDocument {
            text: doc.into(),
            cursor_utf16: 0,
            selection: None,
            ..Default::default()
        },
        true,
        d,
        SafeMode::Silent,
        1000,
    );
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let results = run_plan(
        s,
        &plan,
        QueryApproval {
            danger: true,
            safe_mode: true,
        },
        &cancelled,
    )
    .await
    .unwrap();
    let mut state = WorkspaceState::new(config(d, "x"));
    state.open_query("q", doc);
    let tab = state.current_mut();
    tab.set_run_results(results.clone(), &plan, doc);
    let err = results
        .iter()
        .find_map(|r| r.error.clone())
        .unwrap_or_default();
    let loc = tab.error_locations.iter().flatten().next().copied();
    let want = expected(doc, marker, nth);
    let got = loc.map(|l| (l.line, l.column));
    // Go-to: the UTF-16 offset must point at the marker too
    let utf16_ok = loc.map(|l| {
        let byte = doc.match_indices(marker).nth(nth).unwrap().0;
        doc[..byte].encode_utf16().count() == l.utf16
    });
    eprintln!(
        "[{d:?} {name}] error={:?}\n   got={got:?} want={want:?} utf16_ok={utf16_ok:?}",
        err.lines().next().unwrap_or("")
    );
    if got != Some(want) || utf16_ok != Some(true) {
        bad.push(format!(
            "{d:?} {name}: got {got:?}, want {want:?} (utf16 ok {utf16_ok:?}); {}",
            err.lines().next().unwrap_or("")
        ));
    }
}

#[tokio::test]
async fn qa_error_positions() -> Result<()> {
    let mut bad = vec![];
    // MySQL
    let d = Driver::Mysql;
    let admin = Session::connect_test(&config(d, "zixma"), "secret").await?;
    admin
        .query("CREATE DATABASE IF NOT EXISTS qa030pos")
        .await?;
    let s = Session::connect_test(&config(d, "qa030pos"), "secret").await?;
    for (name, doc, marker, nth) in [
        (
            "stmt3",
            "SELECT 1;\nSELECT 2;\nSELECT 3 FROM dual WHERE 1 = = 2;",
            "= 2",
            0,
        ),
        (
            "crlf-tab-thai",
            "SELECT 'ไทย😺👨‍👩‍👧';\r\n\tSELECT 1\r\n\t\tFROM dual WHERE 1 = = 2;",
            "= 2",
            0,
        ),
        (
            "near-repeats",
            "SELECT 1 FROM dual WHERE 1 = 1 FROM dual;",
            "FROM dual",
            1,
        ),
        (
            "near-repeats-multiline",
            "SELECT 1, 2\nFROM dual\nWHERE 1 = 1 FROM dual;",
            "FROM dual",
            1,
        ),
        (
            "leading-comments",
            "SELECT 1;\n\n-- comment ;\n/* block; */\n\nSELEC 2;",
            "SELEC 2",
            0,
        ),
        (
            "routine-body",
            "CREATE PROCEDURE qa030_p()\nBEGIN\n  SELECT 1;\n  SELEC 2;\nEND",
            "2;",
            0,
        ),
    ] {
        check(&s, d, name, doc, marker, nth, &mut bad).await;
    }
    admin.query("DROP DATABASE qa030pos").await?;
    // PostgreSQL
    let d = Driver::Postgres;
    let admin = Session::connect_test(&config(d, "main_db"), "secret").await?;
    admin.query("DROP DATABASE IF EXISTS qa030pos").await?;
    admin.query("CREATE DATABASE qa030pos").await?;
    let s = Session::connect_test(&config(d, "qa030pos"), "secret").await?;
    for (name, doc, marker, nth) in [
        (
            "stmt3",
            "SELECT 1;\nSELECT 2;\nSELECT 3 WHERE 1 = = 2;",
            "= 2",
            0,
        ),
        (
            "crlf-tab-thai",
            "SELECT 'ไทย😺👨‍👩‍👧';\r\n\tSELECT 1\r\n\t\tWHERE nope_col = 2;",
            "nope_col",
            0,
        ),
        (
            "leading-comments",
            "SELECT 1;\n\n-- comment ;\n/* block; */\n\nSELEC 2;",
            "SELEC 2",
            0,
        ),
        (
            "fn-body",
            "SELECT 1;\nDO $$\nBEGIN\n  PERFORM nope_fn();\nEND $$;",
            "nope_fn",
            0,
        ),
        (
            "fn-body-syntax",
            "CREATE FUNCTION pg_temp.qa030_f() RETURNS int LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN 1 +;\nEND $$;",
            ";\nEND",
            0,
        ),
        ("emoji-before", "SELECT '😺😺😺', nope_col;", "nope_col", 0),
    ] {
        check(&s, d, name, doc, marker, nth, &mut bad).await;
    }
    drop(s);
    admin.query("DROP DATABASE qa030pos WITH (FORCE)").await?;
    eprintln!("\nBAD-POS: {bad:#?}");
    assert!(bad.is_empty(), "{bad:#?}");
    Ok(())
}
