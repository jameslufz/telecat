//! Live workspaces behind a proxy that cuts or blackholes the link to the test servers: the
//! pill, ⌘⇧R, automatic retries, staged edits, interrupted saves and queries, and the UI
//! staying responsive while the network hangs.
use super::Link;
use crate::Services;
use crate::workspace::{Sheet, Workspace, fixtures};
use gpui::{AnyWindowHandle, TestAppContext};
use gpui_kit::{AppContext, Bounds, Entity, WindowBounds, WindowOptions, point, px, size};
use std::sync::Arc;
use std::time::{Duration, Instant};
use telecat_app::workspace_model::{INTERRUPTED, QueryApproval, QueryPlan};
use telecat_core::changes::Cell;
use telecat_core::types::*;
use telecat_db::test_proxy::TestProxy;

fn upstream(d: Driver) -> u16 {
    if d == Driver::Mysql { 3307 } else { 5434 }
}
fn config(d: Driver, port: u16) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "telecat-link-test".into();
    c.name = "Link test".into();
    c.host = "127.0.0.1".into();
    c.port = port;
    let mysql = d == Driver::Mysql;
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = if mysql { "zixma" } else { "main_db" }.into();
    c.password_mode = PasswordMode::Keychain;
    c
}
fn schema(d: Driver) -> &'static str {
    if d == Driver::Mysql {
        "zixma"
    } else {
        "public"
    }
}
/// Runs `sql` on the test server directly, not through any proxy.
fn db(d: Driver, sql: &str) -> Vec<Vec<Option<String>>> {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        telecat_db::Session::connect_test(&config(d, upstream(d)), "secret")
            .await
            .unwrap()
            .query(sql)
            .await
            .unwrap()
            .rows
    })
}
struct Live {
    w: AnyWindowHandle,
    s: Entity<Workspace>,
    proxy: TestProxy,
    profile: std::path::PathBuf,
}
impl Drop for Live {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}
/// A workspace connected through a fresh proxy. `plain` turns TLS off so the proxy can see a
/// COMMIT go by; `ask` sets the connection to ask for its password.
fn live(cx: &mut TestAppContext, d: Driver, plain: bool, ask: bool) -> Live {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let proxy = runtime.block_on(TestProxy::start(upstream(d))).unwrap();
    let mut c = config(d, proxy.port());
    if plain {
        c.ssl_mode = SslMode::Disabled;
    }
    if ask {
        c.password_mode = PasswordMode::Ask;
    }
    let session = runtime
        .block_on(telecat_db::Session::connect_test(&c, "secret"))
        .expect("test database through the proxy");
    // Where a reconnect reads the password again (an in-memory keychain, never the real one).
    let keychain = telecat_app::credentials::MemoryKeychain::default();
    telecat_app::credentials::Keychain::set(
        &keychain,
        telecat_app::credentials::NATIVE_SERVICE,
        &c.id,
        b"secret",
    )
    .unwrap();
    let profile = std::env::temp_dir().join(format!("telecat-link-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&profile).unwrap();
    let services = cx.update(|cx| {
        gpui_kit::init(cx);
        telecat_editor::init(cx);
        Services {
            profile: Arc::new(telecat_app::store::Profile::new(
                profile.clone(),
                Arc::new(keychain),
            )),
            runtime: Arc::new(runtime.into()),
            model: cx.new(|_| crate::Model {
                import_error: None,
                snapshot: StoreSnapshot {
                    connections: vec![],
                    groups: vec![],
                },
            }),
            scenario: false,
        }
    });
    let session = Arc::new(session);
    let (w, s) = cx
        .update(|cx| {
            let services = services.clone();
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1440.), px(fixtures::height(""))),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        Workspace::new(services, Some(session), c, String::new(), window, cx)
                    })
                },
            )
        })
        .unwrap();
    cx.update(|cx| crate::install_menus(services, cx));
    cx.update_window(w, |_, window, _| window.activate_window())
        .unwrap();
    s.update(cx, |s, _| s.link.ping_timeout = Duration::from_secs(1));
    settle(cx, w);
    Live {
        w,
        s,
        proxy,
        profile,
    }
}
fn settle(cx: &mut TestAppContext, w: AnyWindowHandle) {
    for _ in 0..4 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        std::thread::sleep(Duration::from_millis(30));
    }
}
fn paint(cx: &mut TestAppContext, w: AnyWindowHandle) {
    cx.update_window(w, |_, w, cx| {
        w.draw(cx).clear(cx);
    })
    .unwrap();
}
/// Runs both clocks until `done` holds: GPUI's timers advance `step` per round, the worker
/// runtime runs in real time.
fn wait(
    cx: &mut TestAppContext,
    l: &Live,
    what: &str,
    step: Duration,
    within: Duration,
    done: impl Fn(&Workspace) -> bool,
) {
    let deadline = Instant::now() + within;
    loop {
        cx.executor().allow_parking();
        if !step.is_zero() {
            cx.executor().advance_clock(step);
        }
        cx.run_until_parked();
        paint(cx, l.w);
        if l.s.read_with(cx, |s, _| done(s)) {
            return;
        }
        if Instant::now() > deadline {
            let link = l.s.read_with(cx, |s, _| {
                format!("{:?} attempts={}", s.link.link, s.link.attempts)
            });
            panic!("timed out waiting for {what}: {link}");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}
fn open_table(cx: &mut TestAppContext, l: &Live, d: Driver, name: &str) {
    let table = TableRef {
        schema: schema(d).into(),
        name: name.into(),
    };
    l.s.update(cx, |s, cx| s.open_table(table, cx));
    wait(
        cx,
        l,
        "the table to load",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| {
            s.state
                .current()
                .table
                .as_ref()
                .is_some_and(|t| t.data.loaded)
                && !s
                    .state
                    .current()
                    .load(s.state.current().slot())
                    .is_loading()
        },
    );
}
fn ping(cx: &mut TestAppContext, l: &Live) {
    l.s.update(cx, |s, cx| s.ping(cx));
}
fn wait_disconnected(cx: &mut TestAppContext, l: &Live) {
    ping(cx, l);
    wait(
        cx,
        l,
        "the drop to be noticed",
        Duration::from_millis(500),
        Duration::from_secs(15),
        |s| !s.connected(),
    );
}
fn wait_connected(cx: &mut TestAppContext, l: &Live) {
    wait(
        cx,
        l,
        "the reconnect",
        Duration::ZERO,
        Duration::from_secs(30),
        Workspace::connected,
    );
}
fn backend(cx: &mut TestAppContext, l: &Live) -> String {
    l.s.read_with(cx, |s, _| s.session.as_ref().unwrap().backend_id.clone())
}
fn stage(cx: &mut TestAppContext, l: &Live, value: &str) {
    l.s.update(cx, |s, _| {
        s.state
            .current_mut()
            .table
            .as_mut()
            .unwrap()
            .data
            .set_cell(0, 1, Cell::Text(value.into()))
    });
    assert!(l.s.read_with(cx, |s, _| s.state.has_changes()));
}
fn table(d: Driver, name: &str) {
    db(d, &format!("DROP TABLE IF EXISTS {name}"));
    db(
        d,
        &format!("CREATE TABLE {name} (id int PRIMARY KEY, v varchar(20))"),
    );
    db(d, &format!("INSERT INTO {name} VALUES (1, 'old')"));
}
fn stored(d: Driver, name: &str) -> Option<String> {
    db(d, &format!("SELECT v FROM {name} WHERE id = 1"))[0][0].clone()
}

#[gpui::test]
fn a_cut_link_turns_the_pill_red_and_cmd_shift_r_reconnects(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let name = "telecat_lk_cut";
    table(d, name);
    let l = live(cx, d, false, false);
    open_table(cx, &l, d, name);
    let before = backend(cx, &l);
    l.proxy.cut();
    let started = Instant::now();
    wait_disconnected(cx, &l);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "noticed quickly"
    );
    l.s.read_with(cx, |s, _| {
        assert!(matches!(&s.link.link, Link::Disconnected { error, .. } if !error.is_empty()));
    });
    // Not yet back: the automatic attempt fails and another is scheduled.
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    assert_eq!(
        l.s.read_with(cx, |s, _| s.link.attempts),
        1,
        "⌘⇧R tried at once"
    );
    paint(cx, l.w);
    wait(
        cx,
        &l,
        "the failed attempt",
        Duration::ZERO,
        Duration::from_secs(15),
        |s| {
            matches!(
                s.link.link,
                Link::Disconnected {
                    retry_at: Some(_),
                    ..
                }
            )
        },
    );
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    assert_ne!(backend(cx, &l), before, "a new session");
    l.s.read_with(cx, |s, _| {
        assert_eq!(s.state.database, "zixma");
        assert!(s.state.status_text().contains("zixma"));
        assert!(s.sheet.is_none(), "no modal error on the way");
    });
    // The table reloads with the new session.
    wait(
        cx,
        &l,
        "the reload",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| {
            s.state
                .current()
                .table
                .as_ref()
                .is_some_and(|t| t.data.loaded)
                && !s
                    .state
                    .current()
                    .load(s.state.current().slot())
                    .is_loading()
        },
    );
    db(d, &format!("DROP TABLE {name}"));
}

#[gpui::test]
fn after_a_drop_it_retries_by_itself_until_the_link_is_back(cx: &mut TestAppContext) {
    let d = Driver::Postgres;
    let l = live(cx, d, false, false);
    l.proxy.cut();
    wait_disconnected(cx, &l);
    // Two automatic attempts fail while the link is down.
    wait(
        cx,
        &l,
        "a second attempt",
        Duration::from_millis(500),
        Duration::from_secs(30),
        |s| s.link.attempts >= 2 && matches!(s.link.link, Link::Disconnected { .. }),
    );
    l.proxy.restore();
    wait(
        cx,
        &l,
        "an automatic reconnect",
        Duration::from_millis(500),
        Duration::from_secs(60),
        Workspace::connected,
    );
    // ⌘⇧R during a round starts the backoff over.
    l.proxy.cut();
    wait_disconnected(cx, &l);
    wait(
        cx,
        &l,
        "a failed attempt",
        Duration::from_millis(500),
        Duration::from_secs(30),
        |s| s.link.attempts >= 2 && matches!(s.link.link, Link::Disconnected { .. }),
    );
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    assert!(
        l.s.read_with(cx, |s, _| s.link.attempts == 1),
        "backoff reset"
    );
    wait_connected(cx, &l);
}

#[gpui::test]
fn a_blackhole_is_noticed_in_bounded_time_and_the_ui_never_freezes(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let l = live(cx, d, false, false);
    l.proxy.blackhole();
    let started = Instant::now();
    wait_disconnected(cx, &l);
    // Two ping timeouts of 1 s, plus scheduling.
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    // Reconnecting into the blackhole hangs on the network, never on the UI.
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    assert!(l.s.read_with(cx, |s, _| matches!(s.link.link, Link::Reconnecting { .. })));
    let sidebar = l.s.read_with(cx, |s, _| s.sidebar);
    let started = Instant::now();
    cx.simulate_keystrokes(l.w, "cmd-0");
    paint(cx, l.w);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_ne!(
        l.s.read_with(cx, |s, _| s.sidebar),
        sidebar,
        "keys still work"
    );
    // Cancel stops the attempt and the automatic retries.
    l.s.update(cx, |s, cx| s.cancel_reconnect(cx));
    assert!(l.s.read_with(cx, |s, _| matches!(
        s.link.link,
        Link::Disconnected { retry_at: None, .. }
    )));
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
}

#[gpui::test]
fn staged_edits_survive_the_drop_and_save_after_reconnecting(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let name = "telecat_lk_staged";
    table(d, name);
    let l = live(cx, d, false, false);
    open_table(cx, &l, d, name);
    stage(cx, &l, "kept");
    l.proxy.cut();
    wait_disconnected(cx, &l);
    // Quitting still asks about the staged edits.
    cx.simulate_keystrokes(l.w, "cmd-q");
    assert!(
        cx.has_pending_prompt(),
        "staged edits are never dropped silently"
    );
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    // ⌘S while disconnected doesn't try: the edits wait.
    cx.simulate_keystrokes(l.w, "cmd-s");
    l.s.read_with(cx, |s, _| {
        assert!(!s.saving && s.state.has_changes());
    });
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    settle_loads(cx, &l);
    assert!(
        l.s.read_with(cx, |s, _| s.state.has_changes()),
        "still staged"
    );
    cx.simulate_keystrokes(l.w, "cmd-s");
    wait(
        cx,
        &l,
        "the save",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| !s.saving && !s.state.has_changes(),
    );
    assert_eq!(stored(d, name).as_deref(), Some("kept"));
    db(d, &format!("DROP TABLE {name}"));
}
fn settle_loads(cx: &mut TestAppContext, l: &Live) {
    wait(
        cx,
        l,
        "loads",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| {
            !s.state
                .current()
                .load(s.state.current().slot())
                .is_loading()
        },
    );
}

/// ⌘S goes dark at its COMMIT (`deliver`: after the server got it); the save becomes "result
/// unknown", is never retried, and the check after reconnecting settles it.
fn interrupted_save(cx: &mut TestAppContext, d: Driver, deliver: bool) {
    let name = format!(
        "telecat_lk_save_{}_{}",
        if deliver { "after" } else { "before" },
        if d == Driver::Mysql { "my" } else { "pg" }
    );
    table(d, &name);
    let l = live(cx, d, true, false);
    open_table(cx, &l, d, &name);
    stage(cx, &l, "new");
    let more = if d == Driver::Postgres { 1 } else { 0 };
    l.proxy.blackhole_on(b"COMMIT", deliver.then_some(more));
    cx.simulate_keystrokes(l.w, "cmd-s");
    let started = Instant::now();
    while !l.proxy.tripped() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "COMMIT never sent"
        );
        cx.executor().allow_parking();
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        l.s.read_with(cx, |s, _| s.saving),
        "the save hangs on the dark link"
    );
    wait_disconnected(cx, &l);
    l.s.read_with(cx, |s, _| {
        assert!(s.commit_unknown.is_some(), "result unknown");
        assert!(!s.saving, "the UI is free again");
        assert!(s.state.has_changes(), "the edits stay staged");
        assert!(
            s.commit_unknown
                .as_ref()
                .unwrap()
                .file
                .as_ref()
                .is_some_and(|f| f.exists()),
            "the statements are kept on disk"
        );
    });
    // ⌘S explains instead of saving again.
    cx.simulate_keystrokes(l.w, "cmd-s");
    assert!(l.s.read_with(cx, |s, _| matches!(s.sheet, Some(Sheet::CommitUnknown))));
    l.s.update(cx, |s, cx| {
        s.sheet = None;
        cx.notify();
    });
    l.proxy.cut();
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    // The server notices the closed link in a moment; until then the check says so.
    let deadline = Instant::now() + Duration::from_secs(20);
    while l.s.read_with(cx, |s, _| s.commit_unknown.is_some()) {
        assert!(Instant::now() < deadline, "the check never settled");
        let unknown = l.s.read_with(cx, |s, _| {
            s.commit_unknown
                .as_ref()
                .is_some_and(|u| !u.checking && u.verdict.is_some())
        });
        if unknown {
            std::thread::sleep(Duration::from_millis(300));
            l.s.update(cx, |s, cx| s.verify_commit(cx));
        }
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, l.w);
        std::thread::sleep(Duration::from_millis(40));
    }
    settle_loads(cx, &l);
    if deliver {
        assert_eq!(stored(d, &name).as_deref(), Some("new"), "{d:?}");
        assert!(
            !l.s.read_with(cx, |s, _| s.state.has_changes()),
            "{d:?}: found applied, nothing left staged"
        );
    } else {
        assert_eq!(
            stored(d, &name).as_deref(),
            Some("old"),
            "{d:?}: never retried"
        );
        assert!(
            l.s.read_with(cx, |s, _| s.state.has_changes()),
            "{d:?}: found not applied, still staged"
        );
        // Said honestly: MySQL reads it from the rows, PostgreSQL from the server.
        let said = l.s.read_with(cx, |s, _| {
            s.state.logs.iter().any(|e| {
                e.sql.contains(if d == Driver::Mysql {
                    super::NOT_APPLIED_FROM_ROWS
                } else {
                    "The server rolled the interrupted save back"
                })
            })
        });
        assert!(said, "{d:?}: the verdict's wording");
        cx.simulate_keystrokes(l.w, "cmd-s");
        wait(
            cx,
            &l,
            "the save",
            Duration::ZERO,
            Duration::from_secs(20),
            |s| !s.saving && !s.state.has_changes(),
        );
        assert_eq!(stored(d, &name).as_deref(), Some("new"), "{d:?}");
    }
    db(d, &format!("DROP TABLE {name}"));
}
#[gpui::test]
fn a_save_cut_before_its_commit_arrived_is_found_not_applied_mysql(cx: &mut TestAppContext) {
    interrupted_save(cx, Driver::Mysql, false);
}
#[gpui::test]
fn a_save_whose_answer_was_lost_is_found_applied_mysql(cx: &mut TestAppContext) {
    interrupted_save(cx, Driver::Mysql, true);
}
#[gpui::test]
fn a_save_cut_before_its_commit_arrived_is_found_not_applied_postgres(cx: &mut TestAppContext) {
    interrupted_save(cx, Driver::Postgres, false);
}
#[gpui::test]
fn a_save_whose_answer_was_lost_is_found_applied_postgres(cx: &mut TestAppContext) {
    interrupted_save(cx, Driver::Postgres, true);
}

#[gpui::test]
fn a_query_cut_off_is_reported_interrupted_and_not_run_again(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let l = live(cx, d, false, false);
    let marker = format!("telecat_lk_marker_{}", std::process::id());
    let sql = format!("SELECT SLEEP(20), '{marker}'");
    let id = l.s.read_with(cx, |s, _| s.state.active);
    l.s.update(cx, |s, cx| {
        let mut doc = s.state.current().editor.clone();
        doc.text = sql.clone();
        let plan = QueryPlan::new(&doc, true, d, SafeMode::Silent, 1000);
        s.execute_query(id, plan, QueryApproval::default(), cx);
    });
    assert!(l.s.read_with(cx, |s, _| s.busy));
    std::thread::sleep(Duration::from_millis(300));
    l.proxy.blackhole();
    wait_disconnected(cx, &l);
    l.s.read_with(cx, |s, _| {
        assert!(!s.busy, "the editor is usable again");
        let tab = s.state.tabs.iter().find(|t| t.id == id).unwrap();
        assert!(
            tab.results[0]
                .error
                .as_deref()
                .unwrap()
                .starts_with(INTERRUPTED)
        );
        assert!(tab.messages_text().contains("It was not run again"));
    });
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    settle(cx, l.w);
    l.s.read_with(cx, |s, _| {
        let ran = s
            .state
            .logs
            .iter()
            .filter(|e| e.sql.contains(&marker))
            .count();
        assert_eq!(ran, 1, "sent once, never again");
        let tab = s.state.tabs.iter().find(|t| t.id == id).unwrap();
        assert!(
            tab.messages_text().contains("Reconnected at"),
            "the reset is said"
        );
        assert!(
            tab.messages_text()
                .contains("session variables, temporary tables")
        );
    });
}
#[gpui::test]
fn the_new_session_is_set_up_like_the_first(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let l = live(cx, d, false, false);
    l.proxy.cut();
    wait_disconnected(cx, &l);
    l.proxy.restore();
    // Connection ▸ Reconnect in the menu bar.
    cx.update(|cx| cx.dispatch_action(&crate::workspace::commands::Command::new("reconnect")));
    wait_connected(cx, &l);
    let session = l.s.read_with(cx, |s, _| s.session.clone().unwrap());
    assert_eq!(session.database, "zixma");
    assert_eq!(session.config().port, l.proxy.port(), "same endpoint");
    let mode = tokio::runtime::Runtime::new().unwrap().block_on(async {
        session
            .execute(
                &[telecat_core::changes::Stmt::new(
                    "SELECT @@SESSION.sql_mode",
                )],
                false,
            )
            .await
            .unwrap()[0]
            .rows[0][0]
            .clone()
            .unwrap()
    });
    assert!(
        mode.contains("STRICT_ALL_TABLES"),
        "commit connection: {mode}"
    );
    // Safe Mode is untouched.
    assert_eq!(
        l.s.read_with(cx, |s, _| s.state.connection.safe_mode),
        config(d, 0).safe_mode
    );
}

#[gpui::test]
fn a_connection_that_asks_for_its_password_asks_again(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let l = live(cx, d, false, true);
    l.proxy.cut();
    wait_disconnected(cx, &l);
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    assert!(l.s.read_with(cx, |s, _| matches!(s.sheet, Some(Sheet::ReconnectPassword))));
    let type_password = |cx: &mut TestAppContext, pw: &str| {
        cx.update_window(l.w, |_, window, cx| {
            l.s.update(cx, |s, cx| {
                s.gate_password
                    .update(cx, |i, cx| i.set_value(pw, window, cx));
            })
        })
        .unwrap();
    };
    type_password(cx, "wrong-password");
    cx.simulate_keystrokes(l.w, "enter");
    wait(
        cx,
        &l,
        "the refusal",
        Duration::ZERO,
        Duration::from_secs(15),
        |s| matches!(s.link.link, Link::Disconnected { .. }),
    );
    l.s.read_with(cx, |s, _| {
        assert!(
            matches!(&s.link.link, Link::Disconnected { retry_at: None, .. }),
            "a refused password is not retried"
        );
    });
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    type_password(cx, "secret");
    cx.simulate_keystrokes(l.w, "enter");
    wait_connected(cx, &l);
    assert!(l.s.read_with(cx, |s, cx| s.gate_password.read(cx).value().is_empty()));
}

#[gpui::test]
fn only_the_affected_workspace_shows_the_drop(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let a = live(cx, d, false, false);
    let b = live(cx, d, false, false);
    a.proxy.cut();
    wait_disconnected(cx, &a);
    b.s.update(cx, |s, cx| s.ping(cx));
    for _ in 0..20 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(b.s.read_with(cx, |s, _| s.connected()));
    a.proxy.restore();
    cx.update_window(a.w, |_, window, _| window.activate_window())
        .unwrap();
    cx.simulate_keystrokes(a.w, "cmd-shift-r");
    wait_connected(cx, &a);
}

#[gpui::test]
fn cmd_shift_r_during_a_hung_save_settles_it_before_reconnecting(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let name = "telecat_lk_hung";
    table(d, name);
    let l = live(cx, d, true, false);
    open_table(cx, &l, d, name);
    stage(cx, &l, "new");
    l.proxy.blackhole_on(b"COMMIT", None);
    cx.simulate_keystrokes(l.w, "cmd-s");
    let started = Instant::now();
    while !l.proxy.tripped() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "COMMIT never sent"
        );
        cx.executor().allow_parking();
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(l.s.read_with(cx, |s, _| s.saving && s.connected()));
    // The user doesn't wait for the keepalive.
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    l.s.read_with(cx, |s, _| {
        assert!(!s.saving, "the overlay is gone");
        assert!(s.commit_unknown.is_some(), "the save is result unknown");
        assert!(s.state.has_changes());
    });
    l.proxy.cut();
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    let deadline = Instant::now() + Duration::from_secs(20);
    while l.s.read_with(cx, |s, _| s.commit_unknown.is_some()) {
        assert!(Instant::now() < deadline, "the check never settled");
        if l.s.read_with(cx, |s, _| {
            s.commit_unknown
                .as_ref()
                .is_some_and(|u| !u.checking && u.verdict.is_some())
        }) {
            std::thread::sleep(Duration::from_millis(300));
            l.s.update(cx, |s, cx| s.verify_commit(cx));
        }
        cx.executor().allow_parking();
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(
        l.s.read_with(cx, |s, _| s.connected()),
        "no spurious drop afterwards"
    );
    assert_eq!(stored(d, name).as_deref(), Some("old"));
    assert!(l.s.read_with(cx, |s, _| s.state.has_changes()));
    db(d, &format!("DROP TABLE {name}"));
}

#[gpui::test]
fn an_import_cut_off_says_so_and_is_checked_after_reconnecting(cx: &mut TestAppContext) {
    use crate::workspace::import::{ImportRequest, ImportStage};
    let d = Driver::Postgres;
    let name = "telecat_lk_import";
    db(d, &format!("DROP TABLE IF EXISTS {name}"));
    let l = live(cx, d, true, false);
    let mut dump = format!("CREATE TABLE {name} (id int);\n");
    for i in 0..40 {
        dump += &format!("INSERT INTO {name} VALUES ({i});\n");
    }
    let path = l.profile.join("dump.sql");
    std::fs::write(&path, &dump).unwrap();
    let (scan, check) =
        telecat_db::scan_dump_checked(&path, d, "main_db", vec!["main_db".into()]).unwrap();
    let request = ImportRequest {
        path,
        file: "dump.sql".into(),
        size: dump.len() as u64,
        scan,
        database: "main_db".into(),
        tables: 1,
        allow: Default::default(),
        check,
    };
    l.proxy.blackhole_on(b"VALUES (20)", None);
    l.s.update(cx, |s, cx| s.start_import(request, cx));
    let started = Instant::now();
    while !l.proxy.tripped() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the import never got there"
        );
        cx.executor().allow_parking();
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_disconnected(cx, &l);
    let outcome = |s: &Workspace| match &s.sheet {
        Some(Sheet::Import(ImportStage::Done { result: Err(f), .. })) => Some(f.outcome),
        _ => None,
    };
    l.s.read_with(cx, |s, _| {
        assert!(!s.saving);
        assert_eq!(
            outcome(s),
            Some(telecat_db::Outcome::Unknown),
            "not claimed rolled back yet"
        );
    });
    l.proxy.cut();
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    wait(
        cx,
        &l,
        "the import check",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| outcome(s) == Some(telecat_db::Outcome::RolledBack),
    );
    assert_eq!(
        db(d, &format!("SELECT to_regclass('{name}')::text"))[0][0],
        None,
        "nothing of it stayed"
    );
}

#[gpui::test]
fn a_reconnect_from_a_working_session_can_be_called_off(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    // Asked for a password: Esc leaves the working session alone.
    let ask = live(cx, d, false, true);
    cx.simulate_keystrokes(ask.w, "cmd-shift-r");
    assert!(
        ask.s
            .read_with(cx, |s, _| matches!(s.sheet, Some(Sheet::ReconnectPassword)))
    );
    cx.simulate_keystrokes(ask.w, "escape");
    ask.s.read_with(cx, |s, _| {
        assert!(s.sheet.is_none());
        assert!(s.connected(), "still connected, ⌘S and Run still work");
    });
    // A wrong password on a working session keeps it too.
    cx.simulate_keystrokes(ask.w, "cmd-shift-r");
    cx.update_window(ask.w, |_, window, cx| {
        ask.s.update(cx, |s, cx| {
            s.gate_password
                .update(cx, |i, cx| i.set_value("wrong-password", window, cx));
        })
    })
    .unwrap();
    cx.simulate_keystrokes(ask.w, "enter");
    wait(
        cx,
        &ask,
        "the refusal",
        Duration::ZERO,
        Duration::from_secs(15),
        |s| s.connected(),
    );
    // Cancel while the attempt hangs goes back to the working session.
    let l = live(cx, d, false, false);
    cx.update_window(l.w, |_, window, _| window.activate_window())
        .unwrap();
    let before = backend(cx, &l);
    l.proxy.blackhole();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    assert!(l.s.read_with(cx, |s, _| matches!(s.link.link, Link::Reconnecting { .. })));
    l.s.update(cx, |s, cx| s.cancel_reconnect(cx));
    assert!(l.s.read_with(cx, |s, _| s.connected()));
    assert_eq!(backend(cx, &l), before, "the same session");
    l.proxy.restore();
}

/// Runs `sql` in the current query tab and waits for its result and edit plan.
fn run_query(cx: &mut TestAppContext, l: &Live, d: Driver, sql: &str) -> u64 {
    let id = l.s.read_with(cx, |s, _| s.state.active);
    l.s.update(cx, |s, cx| {
        let mut doc = s.state.current().editor.clone();
        doc.text = sql.into();
        let plan = QueryPlan::new(&doc, true, d, SafeMode::Silent, 1000);
        s.execute_query(id, plan, QueryApproval::default(), cx);
    });
    wait(
        cx,
        l,
        "the result's edit plan",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| !s.busy && s.state.current().result_edit.is_some(),
    );
    id
}

#[gpui::test]
fn results_read_before_a_reconnect_turn_read_only(cx: &mut TestAppContext) {
    use telecat_app::workspace_model::RESULT_BEFORE_RECONNECT;
    let d = Driver::Mysql;
    let name = "telecat_lk_result_ro";
    table(d, name);
    let l = live(cx, d, false, false);
    run_query(cx, &l, d, &format!("SELECT id, v FROM {name}"));
    assert!(l.s.read_with(cx, |s, _| {
        s.state
            .current()
            .result_edit
            .as_ref()
            .unwrap()
            .plan
            .read_only
            .is_none()
    }));
    l.proxy.cut();
    wait_disconnected(cx, &l);
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    l.s.update(cx, |s, _| {
        let r = s.state.current_mut().result_edit.as_mut().unwrap();
        assert_eq!(r.plan.read_only.as_deref(), Some(RESULT_BEFORE_RECONNECT));
        let refused = r.stage(0, 1, Cell::Text("new".into())).unwrap_err();
        assert!(
            refused.contains("re-run the query after reconnecting"),
            "{refused}"
        );
    });
    db(d, &format!("DROP TABLE {name}"));
}

#[gpui::test]
fn result_edits_staged_before_a_reconnect_are_rechecked_before_saving(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    let name = "telecat_lk_result_staged";
    table(d, name);
    let l = live(cx, d, false, false);
    run_query(cx, &l, d, &format!("SELECT id, v FROM {name}"));
    l.s.update(cx, |s, _| {
        let r = s.state.current_mut().result_edit.as_mut().unwrap();
        r.stage(0, 1, Cell::Text("new".into())).unwrap();
    });
    l.proxy.cut();
    wait_disconnected(cx, &l);
    l.proxy.restore();
    cx.simulate_keystrokes(l.w, "cmd-shift-r");
    wait_connected(cx, &l);
    l.s.read_with(cx, |s, _| {
        let r = s.state.current().result_edit.as_ref().unwrap();
        assert!(r.recheck && r.data.dirty(), "kept, to be checked");
    });
    // The first ⌘S checks instead of saving.
    cx.simulate_keystrokes(l.w, "cmd-s");
    assert_eq!(stored(d, name).as_deref(), Some("old"));
    wait(
        cx,
        &l,
        "the check",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| !s.state.current().result_edit.as_ref().unwrap().recheck,
    );
    assert_eq!(
        stored(d, name).as_deref(),
        Some("old"),
        "checked, not saved"
    );
    cx.simulate_keystrokes(l.w, "cmd-s");
    wait(
        cx,
        &l,
        "the save",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| !s.saving && !s.state.has_changes(),
    );
    assert_eq!(stored(d, name).as_deref(), Some("new"));
    db(d, &format!("DROP TABLE {name}"));
}
