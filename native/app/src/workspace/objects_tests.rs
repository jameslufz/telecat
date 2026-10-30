//! Live: function tabs, triggers and views through the real workspace, on the test servers.
use super::link::tests::{Live, db, live, open_table, paint, schema, wait};
use crate::workspace::Sheet;
use gpui::TestAppContext;
use gpui_kit::AppContext;
use std::time::Duration;
use telecat_core::objects::TriggerEdit;
use telecat_core::types::*;

fn object(cx: &mut TestAppContext, l: &Live, name: &str) -> DbObject {
    wait(
        cx,
        l,
        "the sidebar",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| s.state.objects.iter().any(|o| o.table.name == name),
    );
    l.s.read_with(cx, |s, _| {
        s.state
            .objects
            .iter()
            .find(|o| o.table.name == name)
            .cloned()
            .unwrap()
    })
}
fn set_text(cx: &mut TestAppContext, l: &Live, text: String) {
    cx.update_window(l.w, |_, window, cx| {
        l.s.update(cx, |s, cx| s.replace_editor_text(text, window, cx))
    })
    .unwrap();
}
/// ⌘S through the Alert Mode confirmation, answering MySQL's "not atomic" notice with Save.
fn save_through_the_gate(cx: &mut TestAppContext, l: &Live, mysql: bool) {
    cx.simulate_keystrokes(l.w, "cmd-s");
    paint(cx, l.w);
    assert!(
        l.s.read_with(cx, |s, _| matches!(s.sheet, Some(Sheet::Gate { .. }))),
        "Safe Mode asks before the definition is applied"
    );
    cx.update_window(l.w, |_, window, cx| {
        l.s.update(cx, |s, cx| s.approve_gate(window, cx))
    })
    .unwrap();
    if mysql {
        cx.run_until_parked();
        cx.simulate_prompt_answer("Save");
    }
    wait(
        cx,
        l,
        "the save",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| !s.saving,
    );
}

#[gpui::test]
fn mysql_function_tab_saves_through_safe_mode_and_puts_the_original_back(cx: &mut TestAppContext) {
    let d = Driver::Mysql;
    db(d, "DROP FUNCTION IF EXISTS t060_ui_fn");
    db(
        d,
        "CREATE FUNCTION t060_ui_fn() RETURNS INT DETERMINISTIC BEGIN DECLARE x INT; SET x = 1; RETURN x; END",
    );
    let l = live(cx, d, false, false);
    l.s.update(cx, |s, _| s.state.connection.safe_mode = SafeMode::Alert1);
    let f = object(cx, &l, "t060_ui_fn");
    assert_eq!(f.kind, ObjectKind::Function);
    // A query tab with text first: opening the function must not touch it.
    l.s.update(cx, |s, cx| s.new_query("SELECT 1;", cx));
    l.s.update(cx, |s, cx| s.open_object(f.clone(), cx));
    wait(
        cx,
        &l,
        "the definition",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| {
            s.state
                .current()
                .routine
                .as_ref()
                .is_some_and(|r| r.original.is_some())
        },
    );
    let (tabs, original) = l.s.read_with(cx, |s, _| {
        (s.state.tabs.len(), s.state.current().editor.text.clone())
    });
    assert!(original.contains("SET x = 1"), "{original}");
    assert!(l.s.read_with(cx, |s, _| {
        s.state.tabs.iter().any(|t| t.editor.text == "SELECT 1;")
    }));
    // Opening it again reuses the tab.
    l.s.update(cx, |s, cx| s.open_object(f.clone(), cx));
    assert_eq!(l.s.read_with(cx, |s, _| s.state.tabs.len()), tabs);
    paint(cx, l.w);
    set_text(cx, &l, original.replace("SET x = 1", "SET x = 2"));
    assert!(l.s.read_with(cx, |s, _| s.state.has_changes()));
    save_through_the_gate(cx, &l, true);
    assert_eq!(db(d, "SELECT t060_ui_fn()")[0][0].as_deref(), Some("2"));
    assert!(!l.s.read_with(cx, |s, _| s.state.current().routine_dirty()));
    // A broken definition: MySQL dropped the function before the CREATE failed, so Telecat
    // creates the previous version again and says so.
    let saved =
        l.s.read_with(cx, |s, _| s.state.current().editor.text.clone());
    set_text(cx, &l, saved.replace("RETURN x;", "RETURN x +;"));
    save_through_the_gate(cx, &l, true);
    wait(
        cx,
        &l,
        "the restore",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| matches!(&s.sheet, Some(Sheet::Error(e)) if e.contains("the original is unchanged")),
    );
    assert_eq!(db(d, "SELECT t060_ui_fn()")[0][0].as_deref(), Some("2"));
    assert!(
        l.s.read_with(cx, |s, _| s.state.current().routine_dirty()),
        "the edit stays staged"
    );
    // Delete from the sidebar stages DROP FUNCTION and closes its tab once saved.
    l.s.update(cx, |s, cx| s.cancel_sheet(cx));
    l.s.update(cx, |s, cx| {
        s.state
            .operations
            .push(telecat_app::workspace_model::StagedOperation {
                object: f.clone(),
                truncate: false,
            });
        cx.notify();
    });
    let preview = l.s.read_with(cx, |s, _| s.preview_text().unwrap());
    assert!(
        preview.contains("DROP FUNCTION `zixma`.`t060_ui_fn`;"),
        "{preview}"
    );
    db(d, "DROP FUNCTION IF EXISTS t060_ui_fn");
}

#[gpui::test]
fn postgres_triggers_save_with_cmd_s_and_views_stay_read_only(cx: &mut TestAppContext) {
    let d = Driver::Postgres;
    db(d, "DROP VIEW IF EXISTS t060_ui_v");
    db(d, "DROP TABLE IF EXISTS t060_ui_t");
    db(d, "CREATE TABLE t060_ui_t (id int PRIMARY KEY, n int)");
    db(
        d,
        "CREATE OR REPLACE FUNCTION t060_ui_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.n := 5; RETURN NEW; END; $$",
    );
    let l = live(cx, d, false, false);
    open_table(cx, &l, d, "t060_ui_t");
    cx.update_window(l.w, |_, window, cx| {
        l.s.update(cx, |s, cx| {
            s.switch_part(telecat_app::table_model::Part::Structure, cx);
            s.open_triggers(window, cx);
        })
    })
    .unwrap();
    assert!(l.s.read_with(cx, |s, _| matches!(s.sheet, Some(Sheet::Triggers))));
    wait(
        cx,
        &l,
        "the triggers",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| {
            s.state
                .current()
                .table
                .as_ref()
                .is_some_and(|t| t.triggers.edits.is_some())
        },
    );
    cx.update_window(l.w, |_, window, cx| {
        l.s.update(cx, |s, cx| s.add_trigger(window, cx))
    })
    .unwrap();
    l.s.update(cx, |s, _| {
        let edits = s
            .state
            .current_mut()
            .table
            .as_mut()
            .unwrap()
            .triggers
            .edits
            .as_mut()
            .unwrap();
        let e: &mut TriggerEdit = edits.last_mut().unwrap();
        e.cur.name = "t060_ui_bi".into();
        e.cur.statement = "FOR EACH ROW EXECUTE FUNCTION t060_ui_fn()".into();
    });
    l.s.update(cx, |s, cx| s.cancel_sheet(cx));
    assert!(
        l.s.read_with(cx, |s, _| s.state.has_changes()),
        "the trigger is staged"
    );
    let preview = l.s.read_with(cx, |s, _| s.preview_text().unwrap());
    assert!(
        preview.contains("CREATE TRIGGER \"t060_ui_bi\" BEFORE INSERT ON \"public\".\"t060_ui_t\""),
        "{preview}"
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
    db(d, "INSERT INTO t060_ui_t (id, n) VALUES (1, 0)");
    assert_eq!(db(d, "SELECT n FROM t060_ui_t")[0][0].as_deref(), Some("5"));
    // A view created after the sidebar loaded: the server says what it is when it opens.
    db(d, "CREATE VIEW t060_ui_v AS SELECT id, n FROM t060_ui_t");
    let view = TableRef {
        schema: schema(d).into(),
        name: "t060_ui_v".into(),
    };
    l.s.update(cx, |s, cx| s.open_table(view.clone(), cx));
    wait(
        cx,
        &l,
        "the view",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| {
            s.state
                .current()
                .table
                .as_ref()
                .is_some_and(|t| t.data.loaded && t.kind == ObjectKind::View)
        },
    );
    let rows = l.s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().data.rows.len()
    });
    cx.update_window(l.w, |_, window, cx| {
        l.s.update(cx, |s, cx| s.add_grid_row(window, cx))
    })
    .unwrap();
    l.s.read_with(cx, |s, _| {
        let t = s.state.current().table.as_ref().unwrap();
        assert!(t.read_only());
        assert_eq!(t.data.rows.len(), rows, "no row added to a view");
        assert!(super::triggers::triggers_unavailable(t).is_some());
    });
    l.s.update(cx, |s, cx| {
        let t = s.state.current_mut().table.as_mut().unwrap();
        t.name = "renamed_view".into();
        cx.notify();
    });
    let e = l.s.read_with(cx, |s, _| {
        s.state.collect_changes().unwrap_err().to_string()
    });
    assert!(e.contains("can't be edited here"), "{e}");
    db(d, "DROP VIEW t060_ui_v");
    db(d, "DROP TABLE t060_ui_t");
    db(d, "DROP FUNCTION t060_ui_fn()");
}

#[gpui::test]
fn switch_database_shows_why_the_list_failed(cx: &mut TestAppContext) {
    let l = live(cx, Driver::Mysql, false, false);
    wait(
        cx,
        &l,
        "the database list",
        Duration::ZERO,
        Duration::from_secs(20),
        |s| !s.databases_loading,
    );
    l.proxy.cut();
    l.s.update(cx, |s, cx| s.load_databases(cx));
    wait(
        cx,
        &l,
        "the failure",
        Duration::ZERO,
        Duration::from_secs(30),
        |s| !s.databases_loading && s.databases_error.is_some(),
    );
    assert!(l.s.read_with(cx, |s, _| s.databases.is_empty()));
    l.proxy.restore();
}

#[gpui::test]
fn editor_shortcuts_reach_a_routine_tabs_editor(cx: &mut TestAppContext) {
    let (w, s) = super::components::tests::setup(cx, "22-pg-table");
    let f = s.read_with(cx, |s, _| {
        s.state
            .objects
            .iter()
            .find(|o| o.kind == ObjectKind::Function)
            .cloned()
            .unwrap()
    });
    s.update(cx, |s, _| {
        s.state.open_routine(f);
        let t = s.state.current_mut();
        t.routine = Some(telecat_app::workspace_model::RoutineEditor {
            original: Some("CREATE OR REPLACE FUNCTION public.qa_fn() RETURNS int LANGUAGE sql AS 'select 1';\n".into()),
            settings: Default::default(),
        });
        t.editor.text = t.routine.as_ref().unwrap().original.clone().unwrap();
    });
    let paint = |cx: &mut TestAppContext| {
        cx.update_window(w, |_, w, cx| {
            w.draw(cx).clear(cx);
        })
        .unwrap()
    };
    cx.update_window(w, |_, window, _| window.activate_window())
        .unwrap();
    paint(cx);
    cx.run_until_parked();
    paint(cx);
    let id = s.read_with(cx, |s, _| s.state.active);
    let editor = s
        .read_with(cx, |s, _| s.editor_of(id))
        .expect("the definition's editor");
    cx.update_window(w, |_, window, cx| {
        editor.update(cx, |e, cx| e.focus(window, cx))
    })
    .unwrap();
    // Typing goes to the definition's editor, and ⌘Z undoes it there.
    cx.simulate_input(w, "x");
    assert!(s.read_with(cx, |s, _| s.state.current().routine_dirty()));
    cx.simulate_keystrokes(w, "cmd-z");
    assert!(
        !s.read_with(cx, |s, _| s.state.current().routine_dirty()),
        "⌘Z undid it"
    );
    cx.simulate_keystrokes(w, "alt-cmd-f");
    paint(cx);
    let open = s.read_with(cx, |s, cx| {
        s.editor_of(id).map(|e| e.read(cx).replace_open())
    });
    assert_eq!(
        open,
        Some(true),
        "⌥⌘F opened the routine editor's Find and Replace"
    );
}
