//! Live UI tests of the 0.8 Table Data actions, against the throwaway test databases only
//! (MySQL :3307, PostgreSQL :5434), with the user's DOUBLE AUTO_INCREMENT key shape.
use crate::workspace::Workspace;
use crate::workspace::components::tests::{
    current_grid, db_rows, drop_table, focus_grid, hint, install_shortcuts, live_table, paint,
    save_and_wait,
};
use gpui::{AnyWindowHandle, TestAppContext};
use gpui_kit::{AppContext, Entity};
use telecat_core::changes::Cell as Value;
use telecat_core::types::{Driver, FilterOp};
use telecat_grid::Cell;

/// A parent table with a DOUBLE AUTO_INCREMENT key and a child with a foreign key to it, named
/// `<prefix>_parent` / `<prefix>_child` so tests running at the same time don't share them.
fn setup(prefix: &str) -> Vec<String> {
    [
        "DROP TABLE IF EXISTS {p}_child",
        "DROP TABLE IF EXISTS {p}_parent",
        "CREATE TABLE {p}_parent (id DOUBLE AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20), kind ENUM('a','b'), doc JSON, at DATETIME, gen INT AS (CHAR_LENGTH(name)))",
        "INSERT INTO {p}_parent (name, kind, doc, at) VALUES ('one', 'a', '{\"k\": 1}', '2026-01-02 03:04:05'), ('two', 'b', NULL, NULL), ('three', NULL, '[1, 2]', NULL)",
        "CREATE TABLE {p}_child (id DOUBLE AUTO_INCREMENT PRIMARY KEY, parent DOUBLE, note VARCHAR(20), FOREIGN KEY (parent) REFERENCES {p}_parent (id))",
        "INSERT INTO {p}_child (parent, note) VALUES (2, 'x'), (3, 'y'), (NULL, 'z')",
    ]
    .iter()
    .map(|s| s.replace("{p}", prefix))
    .collect()
}
fn open(
    cx: &mut TestAppContext,
    prefix: &str,
    table: &str,
) -> (AnyWindowHandle, Entity<Workspace>) {
    let sql = setup(prefix);
    let sql: Vec<&str> = sql.iter().map(String::as_str).collect();
    live_table(cx, Driver::Mysql, &sql, &format!("{prefix}_{table}"))
}
fn cleanup(prefix: &str) {
    drop_table(Driver::Mysql, &format!("{prefix}_child"));
    drop_table(Driver::Mysql, &format!("{prefix}_parent"));
}
fn settle(cx: &mut TestAppContext, w: AnyWindowHandle, s: &Entity<Workspace>) {
    for _ in 0..300 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        let done = s.read_with(cx, |s, _| {
            let tab = s.state.current();
            tab.table.as_ref().is_none_or(|t| t.data.loaded) && !tab.load(tab.slot()).is_loading()
        });
        if done {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    panic!("the table did not load");
}
fn key(s: &Entity<Workspace>, cx: &mut TestAppContext) -> (u64, u8) {
    s.read_with(cx, |s, _| (s.state.active, 0))
}
/// Selects grid rows `rows` and columns `from..=to` (data indices).
fn select(cx: &mut TestAppContext, s: &Entity<Workspace>, rows: &[usize], from: usize, to: usize) {
    let g = current_grid(cx, s);
    g.update(cx, |g, cx| {
        g.update_model(
            |m| {
                m.selection.rows = rows.iter().copied().collect();
                m.selection.anchor = rows.first().copied();
                m.column_anchor = Some(from);
                m.selected_column = Some(to);
            },
            cx,
        )
    });
}
fn cell(cx: &mut TestAppContext, s: &Entity<Workspace>, row: usize, col: usize) -> Value {
    s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().data.rows[row].cur[col].clone()
    })
}
fn act(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    s: &Entity<Workspace>,
    id: &str,
    row: usize,
    col: usize,
) {
    let k = key(s, cx);
    let row_key =
        current_grid(cx, s).read_with(cx, |g, _| g.model().row_key(row).map(str::to_owned));
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.data_menu_action(
                k,
                id,
                Cell { row, col },
                row_key,
                telecat_grid::Rect::default(),
                window,
                cx,
            )
        })
    })
    .unwrap();
    settle(cx, w, s);
}

#[gpui::test]
fn data_actions_stage_paste_set_value_and_refuse_like_tableplus(cx: &mut TestAppContext) {
    let (w, s) = open(cx, "t080a", "parent");
    let k = key(&s, cx);
    // Column details arrived with the rows: the generated column is read-only.
    let editable = current_grid(cx, &s).read_with(cx, |g, _| {
        g.model()
            .columns
            .iter()
            .map(|c| c.editable)
            .collect::<Vec<_>>()
    });
    assert_eq!(editable, [true, true, true, true, true, false]);
    assert!(current_grid(cx, &s).read_with(cx, |g, _| g.model().columns[2].chevron));
    // Paste to Selected Cells: a 2x1 block from row 0, column 1; one ⌘Z undoes it.
    select(cx, &s, &[0], 1, 1);
    s.update(cx, |s, cx| {
        s.paste_cells(k, vec![vec!["p1".into()], vec!["NULL".into()]], None, cx)
    });
    assert_eq!(cell(cx, &s, 0, 1), Value::Text("p1".into()));
    assert_eq!(cell(cx, &s, 1, 1), Value::Null);
    s.update(cx, |s, _| {
        assert!(s.state.current_mut().table.as_mut().unwrap().data.undo());
    });
    assert_eq!(cell(cx, &s, 0, 1), Value::Text("one".into()));
    assert_eq!(cell(cx, &s, 1, 1), Value::Text("two".into()));
    // Onto the generated column: refused whole, with TablePlus's message.
    select(cx, &s, &[0], 4, 5);
    s.update(cx, |s, cx| {
        s.paste_cells(
            k,
            vec![vec!["2026-01-01 00:00:00".into(), "9".into()]],
            None,
            cx,
        )
    });
    assert_eq!(
        hint(cx, &s).as_deref(),
        Some("A selected cell cannot be edited")
    );
    assert!(!s.read_with(cx, |s, _| s.state.has_changes()));
    // Set Value ▸ NULL on a block of two rows.
    select(cx, &s, &[0, 1], 1, 1);
    act(cx, w, &s, "sv-null", 0, 1);
    assert_eq!(cell(cx, &s, 0, 1), Value::Null);
    assert_eq!(cell(cx, &s, 1, 1), Value::Null);
    // DEFAULT is for new rows only.
    act(cx, w, &s, "sv-default", 0, 1);
    assert_eq!(
        hint(cx, &s).as_deref(),
        Some("DEFAULT can only be set on a new row.")
    );
    // PRETTY on JSON, refused on text that isn't JSON.
    select(cx, &s, &[0], 3, 3);
    act(cx, w, &s, "sv-pretty", 0, 3);
    assert_eq!(cell(cx, &s, 0, 3), Value::Text("{\n  \"k\": 1\n}".into()));
    // NOW() reads the server's clock in the column's format.
    select(cx, &s, &[1], 4, 4);
    act(cx, w, &s, "sv-now", 1, 4);
    for _ in 0..100 {
        settle(cx, w, &s);
        if matches!(cell(cx, &s, 1, 4), Value::Text(_)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    let Value::Text(now) = cell(cx, &s, 1, 4) else {
        panic!("NOW() was not staged");
    };
    assert!(
        telecat_core::datetime::parse(&now).is_some() && now.len() == 19,
        "{now}"
    );
    // Deleted rows refuse a paste.
    s.update(cx, |s, _| {
        s.state.current_mut().table.as_mut().unwrap().data.rows[2].deleted = true;
    });
    select(cx, &s, &[2], 1, 1);
    s.update(cx, |s, cx| {
        s.paste_cells(k, vec![vec!["x".into()]], None, cx)
    });
    assert_eq!(
        hint(cx, &s).as_deref(),
        Some("Deleted rows cannot be edited")
    );
    s.update(cx, |s, _| {
        s.state.current_mut().table.as_mut().unwrap().data.rows[2].deleted = false;
    });
    // ⌘S saves the staged values through the guarded path.
    save_and_wait(cx, w, &s);
    let rows = db_rows(
        Driver::Mysql,
        "SELECT name, CAST(at AS CHAR) FROM t080a_parent ORDER BY id",
    );
    assert_eq!(rows[0][0], None);
    assert_eq!(rows[1][0], None);
    assert_eq!(rows[1][1].as_deref(), Some(now.as_str()));
    assert_eq!(rows[2][0].as_deref(), Some("three"));
    cleanup("t080a");
}

#[gpui::test]
fn foreign_keys_quick_filters_sorts_and_go_back(cx: &mut TestAppContext) {
    let (w, s) = open(cx, "t080b", "child");
    let k = key(&s, cx);
    let child = k.0;
    assert!(current_grid(cx, &s).read_with(cx, |g, _| g.model().columns[1].foreign_key));
    // The arrow of row 1 (parent 3) opens the parent table filtered to that row.
    let row_key =
        current_grid(cx, &s).read_with(cx, |g, _| g.model().row_key(1).map(str::to_owned));
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.follow_foreign_key(k, Cell { row: 1, col: 1 }, row_key, window, cx)
        })
    })
    .unwrap();
    settle(cx, w, &s);
    let (title, rows, applied) = s.read_with(cx, |s, _| {
        let tab = s.state.current();
        let t = tab.table.as_ref().unwrap();
        (tab.title.clone(), t.data.rows.len(), t.applied.clone())
    });
    assert!(title.contains("t080b_parent"), "{title}");
    assert_eq!(rows, 1);
    assert_eq!(
        (
            applied[0].column.as_str(),
            applied[0].op,
            applied[0].value.as_str()
        ),
        ("id", FilterOp::Eq, "3")
    );
    assert_eq!(cell(cx, &s, 0, 1), Value::Text("three".into()));
    // Go Back returns to the child table.
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.go(false, window, cx))
    })
    .unwrap();
    settle(cx, w, &s);
    assert_eq!(s.read_with(cx, |s, _| s.state.active), child);
    // A NULL key references nothing.
    let row_key =
        current_grid(cx, &s).read_with(cx, |g, _| g.model().row_key(2).map(str::to_owned));
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.follow_foreign_key(k, Cell { row: 2, col: 1 }, row_key, window, cx)
        })
    })
    .unwrap();
    assert_eq!(s.read_with(cx, |s, _| s.state.active), child);
    // Quick Filter ▸ note = 'y' (item 0 of the submenu) shows one row.
    act(cx, w, &s, "quick:0", 1, 2);
    settle(cx, w, &s);
    let (rows, shown) = s.read_with(cx, |s, _| {
        let t = s.state.current().table.as_ref().unwrap();
        (t.data.rows.len(), t.show_filters)
    });
    assert_eq!((rows, shown), (1, true));
    // Sort multiple Columns from the header menu.
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.header_action(k, 2, "note", "h-desc", window, cx);
        })
    })
    .unwrap();
    settle(cx, w, &s);
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.header_action(k, 0, "id", "h-add-asc", window, cx)
        })
    })
    .unwrap();
    settle(cx, w, &s);
    let order = s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().order_by.clone()
    });
    assert_eq!(
        order.iter().map(|o| o.column.as_str()).collect::<Vec<_>>(),
        ["note", "id"]
    );
    let sort = current_grid(cx, &s).read_with(cx, |g, _| g.model().sort.len());
    assert_eq!(sort, 2);
    cleanup("t080b");
}

#[gpui::test]
fn copy_shortcuts_beat_the_console_toggle_and_filters_are_remembered(cx: &mut TestAppContext) {
    let (w, s) = open(cx, "t080c", "child");
    install_shortcuts(cx, w, &s);
    focus_grid(cx, w, &s);
    select(cx, &s, &[0], 1, 2);
    let console = s.read_with(cx, |s, _| s.console);
    cx.simulate_keystrokes(w, "cmd-shift-c");
    assert_eq!(
        s.read_with(cx, |s, _| s.console),
        console,
        "⇧⌘C in the grid is Copy Selected Cells"
    );
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some("2\tx"));
    // Copy Rows As ▸ JSON.
    act(cx, w, &s, "copy-rows-as:json", 0, 1);
    let json: serde_json::Value =
        serde_json::from_str(&cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap()))
            .unwrap();
    assert_eq!(json[0]["note"], "x");
    // A rule is remembered for this table (shown, not applied, when the table opens again).
    s.update(cx, |s, cx| {
        let id = s.state.active;
        let t = s.table_of(id).unwrap();
        t.filters[0].column = "note".into();
        t.filters[0].value = "y".into();
        s.queue_table_state(id, cx);
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    settle(cx, w, &s);
    // Close the tab and open the table again from the same profile.
    let table = s.update(cx, |s, cx| {
        let id = s.state.active;
        let table = s.table_of(id).unwrap().table.clone();
        s.state.open_query("q", "");
        s.state.close_tab(id);
        s.open_table(table.clone(), cx);
        table
    });
    settle(cx, w, &s);
    let (filters, applied, rows, name) = s.read_with(cx, |s, _| {
        let t = s.state.current().table.as_ref().unwrap();
        (
            t.filters.clone(),
            t.applied.clone(),
            t.data.rows.len(),
            t.table.name.clone(),
        )
    });
    assert_eq!(name, table.name);
    assert_eq!(
        (filters[0].column.as_str(), filters[0].value.as_str()),
        ("note", "y")
    );
    assert!(applied.is_empty(), "restored rules are not applied");
    assert_eq!(rows, 3);
    cleanup("t080c");
}

#[gpui::test]
fn a_large_selection_keeps_rendering_fast(cx: &mut TestAppContext) {
    let (w, s) = live_table(
        cx,
        Driver::Mysql,
        &[
            "DROP TABLE IF EXISTS t080d_big",
            "CREATE TABLE t080d_big (id DOUBLE AUTO_INCREMENT PRIMARY KEY, v VARCHAR(10))",
            "SET SESSION cte_max_recursion_depth = 10000",
            "INSERT INTO t080d_big (v) WITH RECURSIVE n (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000) SELECT CONCAT('v', i) FROM n",
        ],
        "t080d_big",
    );
    // A page of 5,000 rows, all selected (⌘A), with the row detail showing them.
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.navigate_table(super::TableNavigation::Page(0, 5000), window, cx)
        })
    })
    .unwrap();
    settle(cx, w, &s);
    let n = current_grid(cx, &s).read_with(cx, |g, _| g.model().rows.len());
    assert_eq!(n, 5000);
    let all: Vec<usize> = (0..n).collect();
    select(cx, &s, &all, 1, 1);
    let started = std::time::Instant::now();
    for _ in 0..10 {
        cx.update_window(w, |_, window, cx| {
            s.update(cx, |s, cx| s.prepare_inspector(window, cx))
        })
        .unwrap();
        paint(cx, w);
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "{:?} for 10 frames",
        started.elapsed()
    );
    assert_eq!(s.read_with(cx, |s, _| s.inspected_count()), 5000);
    drop_table(Driver::Mysql, "t080d_big");
}

/// QA 0.8.0 fix round 1: expression-default columns stay editable (BUG-001); a copied
/// multi-line cell pastes into exactly one cell (BUG-002); copied rows follow the shown columns
/// after a reorder and a hidden column (BUG-003).
#[gpui::test]
fn qa080_defaults_stay_editable_and_copies_paste_back_cell_for_cell(cx: &mut TestAppContext) {
    let (w, s) = live_table(
        cx,
        Driver::Mysql,
        &[
            "DROP TABLE IF EXISTS t080e_round",
            "CREATE TABLE t080e_round (id DOUBLE AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20), note TEXT, created_at TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP, uid VARCHAR(36) DEFAULT (uuid()))",
            "INSERT INTO t080e_round (name, note) VALUES ('alice', 'line1\\nline2\\tx'), ('bob', 'keep-2'), ('carol', 'keep-3')",
        ],
        "t080e_round",
    );
    let k = key(&s, cx);
    let editable = current_grid(cx, &s).read_with(cx, |g, _| {
        g.model()
            .columns
            .iter()
            .map(|c| c.editable)
            .collect::<Vec<_>>()
    });
    assert_eq!(
        editable, [true; 5],
        "DEFAULT CURRENT_TIMESTAMP / DEFAULT (uuid()) stay editable"
    );
    // ⇧⌘C on the multi-line note of row 0, ⇧⌘V onto row 1's note: one staged cell.
    select(cx, &s, &[0], 2, 2);
    let clip = current_grid(cx, &s).read_with(cx, |g, _| g.model().selected_cells_tsv());
    select(cx, &s, &[1], 2, 2);
    s.update(cx, |s, cx| {
        s.paste_cells(k, telecat_grid::parse_tsv(&clip), None, cx)
    });
    assert_eq!(cell(cx, &s, 1, 2), Value::Text("line1\nline2\tx".into()));
    assert_eq!(cell(cx, &s, 2, 2), Value::Text("keep-3".into()));
    s.update(cx, |s, _| {
        assert!(s.state.current_mut().table.as_mut().unwrap().data.undo());
        assert!(!s.state.has_changes());
    });
    // Shown as note, id, name (uid hidden): ⌘C row 1, ⇧⌘V at row 2's first shown cell.
    // The tab's layout, as a header drag and Hide this column leave it.
    s.update(cx, |s, cx| {
        let t = s.state.current_mut().table.as_mut().unwrap();
        t.column_order = ["note", "id", "name", "created_at", "uid"]
            .map(String::from)
            .to_vec();
        t.hidden.insert("uid".into());
        cx.notify();
    });
    settle(cx, w, &s);
    select(cx, &s, &[1], 2, 2);
    let rows = current_grid(cx, &s).read_with(cx, |g, _| g.model().selected_rows_tsv());
    assert!(rows.starts_with("keep-2\t2\tbob\t"), "{rows}");
    select(cx, &s, &[2], 2, 2);
    s.update(cx, |s, cx| {
        s.paste_cells(k, telecat_grid::parse_tsv(&rows), None, cx)
    });
    assert_eq!(cell(cx, &s, 2, 0), Value::Text("2".into()));
    assert_eq!(cell(cx, &s, 2, 1), Value::Text("bob".into()));
    assert_eq!(cell(cx, &s, 2, 2), Value::Text("keep-2".into()));
    // From a later column the row doesn't fit: refused, nothing more staged.
    s.update(cx, |s, _| {
        s.state.current_mut().table.as_mut().unwrap().data.undo();
    });
    select(cx, &s, &[2], 0, 0);
    s.update(cx, |s, cx| {
        s.paste_cells(k, telecat_grid::parse_tsv(&rows), None, cx)
    });
    assert!(hint(cx, &s).is_some_and(|h| h.contains("don't fit")));
    assert!(!s.read_with(cx, |s, _| s.state.has_changes()));
    // A whole row pasted with ⌘V maps back through the same layout.
    s.update(cx, |s, cx| {
        let g = s.components.grid(k).unwrap().view.clone();
        g.update(cx, |g, cx| {
            for event in g.model().paste(&rows) {
                cx.emit(event);
            }
        });
    });
    settle(cx, w, &s);
    let added = s.read_with(cx, |s, _| {
        s.state
            .current()
            .table
            .as_ref()
            .unwrap()
            .data
            .rows
            .last()
            .unwrap()
            .cur
            .clone()
    });
    assert_eq!(added[1], Value::Text("bob".into()));
    assert_eq!(added[2], Value::Text("keep-2".into()));
    assert_eq!(added[0], Value::unset(), "the key is the server's");
    drop_table(Driver::Mysql, "t080e_round");
}

/// QA 0.8.0 BUG-004: rows Telecat copied paste into the columns of their names, whatever the
/// layouts of the copy and the paste (a reorder in between, or another tab of the same columns
/// laid out differently); a copied column the target lacks is refused.
#[gpui::test]
fn qa080_copied_rows_paste_by_column_name(cx: &mut TestAppContext) {
    let (w, s) = live_table(
        cx,
        Driver::Mysql,
        &[
            "DROP TABLE IF EXISTS t080f_a",
            "DROP TABLE IF EXISTS t080f_b",
            "DROP TABLE IF EXISTS t080f_c",
            "CREATE TABLE t080f_a (id DOUBLE AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20), email VARCHAR(40))",
            "CREATE TABLE t080f_b (id DOUBLE AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20), email VARCHAR(40))",
            "CREATE TABLE t080f_c (id DOUBLE AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20))",
            "INSERT INTO t080f_a (name, email) VALUES ('bob', 'b@x.io'), ('eve', 'e@x.io')",
            "INSERT INTO t080f_b (name, email) VALUES ('zed', 'z@x.io')",
            "INSERT INTO t080f_c (name) VALUES ('kim')",
        ],
        "t080f_a",
    );
    let layout = |cx: &mut TestAppContext, order: &[&str]| {
        s.update(cx, |s, cx| {
            let t = s.state.current_mut().table.as_mut().unwrap();
            t.column_order = order.iter().map(|c| c.to_string()).collect();
            cx.notify();
        });
        settle(cx, w, &s);
    };
    let grid_do =
        |cx: &mut TestAppContext,
         f: fn(&mut telecat_grid::DataGrid, &mut gpui::Context<telecat_grid::DataGrid>)| {
            let g = current_grid(cx, &s);
            g.update(cx, f);
            settle(cx, w, &s);
        };
    let last_row = |cx: &mut TestAppContext| {
        s.read_with(cx, |s, _| {
            s.state
                .current()
                .table
                .as_ref()
                .unwrap()
                .data
                .rows
                .last()
                .unwrap()
                .cur
                .clone()
        })
    };
    // ⌘C with email shown first, then the user drags it back, then ⌘V.
    layout(cx, &["email", "id", "name"]);
    select(cx, &s, &[0], 0, 0);
    grid_do(cx, |g, cx| g.copy_selection(cx));
    layout(cx, &["id", "name", "email"]);
    grid_do(cx, |g, cx| g.paste_clipboard(cx));
    let row = last_row(cx);
    assert_eq!(
        (row[1].clone(), row[2].clone()),
        (Value::Text("bob".into()), Value::Text("b@x.io".into()))
    );
    assert_eq!(row[0], Value::unset());
    s.update(cx, |s, _| {
        s.state.current_mut().table.as_mut().unwrap().data.discard()
    });
    // Copy from that layout, paste into another table with the same columns in its own order.
    layout(cx, &["email", "id", "name"]);
    select(cx, &s, &[1], 0, 0);
    grid_do(cx, |g, cx| g.copy_selection(cx));
    let schema = s.read_with(cx, |s, _| s.state.database.clone());
    s.update(cx, |s, cx| {
        s.open_table(
            telecat_core::types::TableRef {
                schema: schema.clone(),
                name: "t080f_b".into(),
            },
            cx,
        )
    });
    settle(cx, w, &s);
    grid_do(cx, |g, cx| g.paste_clipboard(cx));
    let row = last_row(cx);
    assert_eq!(
        (row[1].clone(), row[2].clone()),
        (Value::Text("eve".into()), Value::Text("e@x.io".into()))
    );
    // ⇧⌘V of those whole rows onto an existing row also goes by name.
    s.update(cx, |s, _| {
        s.state.current_mut().table.as_mut().unwrap().data.discard()
    });
    select(cx, &s, &[0], 1, 1);
    grid_do(cx, |g, cx| g.paste_cells(cx));
    assert_eq!(cell(cx, &s, 0, 1), Value::Text("eve".into()));
    assert_eq!(cell(cx, &s, 0, 2), Value::Text("e@x.io".into()));
    assert_eq!(
        cell(cx, &s, 0, 0),
        Value::Text("2".into()),
        "id came along by name"
    );
    s.update(cx, |s, _| {
        s.state.current_mut().table.as_mut().unwrap().data.discard()
    });
    // A table without `email` refuses, naming the column; nothing is staged.
    s.update(cx, |s, cx| {
        s.open_table(
            telecat_core::types::TableRef {
                schema,
                name: "t080f_c".into(),
            },
            cx,
        )
    });
    settle(cx, w, &s);
    grid_do(cx, |g, cx| g.paste_clipboard(cx));
    assert!(!s.read_with(cx, |s, _| s.state.current().dirty()));
    assert!(s.read_with(cx, |s, _| matches!(
        &s.sheet,
        Some(super::Sheet::Error(e)) if e.contains("email") && e.contains("t080f_a")
    )));
    for name in ["t080f_a", "t080f_b", "t080f_c"] {
        drop_table(Driver::Mysql, name);
    }
}
