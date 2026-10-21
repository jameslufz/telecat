use super::{Cell, Driver, GridEvent, Part, QueryPlan, SafeMode, TabKind, Value, key};
use crate::Services;
use crate::workspace::{Workspace, fixtures};
use gpui::{AnyWindowHandle, TestAppContext};
use gpui_kit::{AppContext, Bounds, Entity, WindowBounds, WindowOptions, point, px, size};
use std::sync::Arc;
use telecat_core::types::StoreSnapshot;
pub(in crate::workspace) fn setup(
    cx: &mut TestAppContext,
    scenario: &str,
) -> (AnyWindowHandle, Entity<Workspace>) {
    setup_with(
        cx,
        scenario,
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap(),
        None,
    )
}
/// A workspace on `runtime`, optionally connected through `session` (which must have been
/// opened on that runtime) instead of showing a fixture.
pub(in crate::workspace) fn setup_with(
    cx: &mut TestAppContext,
    scenario: &str,
    runtime: tokio::runtime::Runtime,
    session: Option<Arc<telecat_db::Session>>,
) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        telecat_editor::init(cx);
        // Outside the source tree: the system's temporary directory.
        let root = std::env::temp_dir()
            .join("telecat-ui-profiles")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let services = Services {
            profile: Arc::new(telecat_app::store::Profile::new(
                root,
                Arc::new(telecat_app::credentials::MemoryKeychain::default()),
            )),
            runtime: Arc::new(runtime.into()),
            model: cx.new(|_| crate::Model {
                import_error: None,
                snapshot: StoreSnapshot {
                    connections: vec![],
                    groups: vec![],
                },
            }),
            scenario: session.is_none(),
        };
        let name = scenario.to_owned();
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1440.), px(fixtures::height(scenario))),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let connection = session.as_ref().map_or_else(
                        || fixtures::state(&name).connection,
                        |s| live_config(s.driver),
                    );
                    Workspace::new(services, session, connection, name, window, cx)
                })
            },
        )
        .unwrap()
    })
}
fn paint(cx: &mut TestAppContext, w: AnyWindowHandle) {
    cx.update_window(w.into(), |_, w, cx| {
        w.draw(cx).clear(cx);
    })
    .unwrap();
}
#[gpui::test]
fn every_workspace_scenario_renders_real_components(cx: &mut TestAppContext) {
    for scenario in fixtures::SCENARIOS {
        let (w, workspace) = setup(cx, scenario);
        paint(cx, w);
        workspace.read_with(cx, |s, cx| {
            let tab = s.state.current();
            let binding = &s.components.grids[&key(tab)];
            let m = binding.view.read(cx).model();
            if matches!(
                *scenario,
                "07-table-data" | "51-data-staged" | "22-pg-table" | "06-query-all-rowselected"
            ) {
                let expected = if *scenario == "06-query-all-rowselected" {
                    telecat_grid::Rect {
                        x: 0.,
                        y: 0.,
                        width: 848.,
                        height: 233.5,
                    }
                } else {
                    telecat_grid::Rect {
                        x: 0.,
                        y: 0.,
                        width: 848.,
                        height: 511.,
                    }
                };
                assert_eq!(
                    m.viewport, expected,
                    "reference component dimensions for {scenario}"
                );
            }

            let count = tab
                .table
                .as_ref()
                .map_or(tab.grid.result.rows.len(), |t| t.buffer().rows.len());
            assert_eq!(m.rows.len(), count, "{scenario}");
            assert_eq!(
                m.columns.len(),
                tab.table
                    .as_ref()
                    .map_or(tab.grid.result.columns.len(), |t| t
                        .buffer()
                        .result
                        .columns
                        .len()),
                "{scenario}"
            );
            if tab.kind == TabKind::Query {
                assert_eq!(
                    s.components.editors[&tab.id].read(cx).text(cx).as_ref(),
                    tab.editor.text
                );
            }
            if tab.table.as_ref().is_some_and(|t| t.part != Part::Data) {
                assert!(!m.options.show_row_numbers);
                assert!(m.options.show_empty);
            }
        });
        cx.update_window(w.into(), |_, w, _| w.remove_window())
            .unwrap();
    }
}
#[gpui::test]
fn cell_engine_flush_is_in_commit_snapshot_and_undo_is_preserved(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    paint(cx, w);
    let grid = s.read_with(cx, |s, _| {
        s.components.grids[&key(s.state.current())].view.clone()
    });
    cx.update_window(w.into(), |_, window, cx| {
        grid.update(cx, |g, cx| {
            g.request_edit(Cell { row: 0, col: 3 }, 1, window, cx)
        })
    })
    .unwrap();
    paint(cx, w);
    cx.simulate_input(w.into(), "ธนาคารใหม่ 😺");
    cx.update_window(w.into(), |_, window, cx| {
        s.update(cx, |s, cx| {
            s.flush_grid_edits(window, cx);
            assert_eq!(
                s.state.current().table.as_ref().unwrap().data.rows[0].cur[3].value(),
                Some("ธนาคารใหม่ 😺")
            );
            let units = s.state.collect_changes().unwrap();
            assert_eq!(units.len(), 1);
            assert_eq!(units[0].statements[0].expect, Some(1));
            s.state.current_mut().table.as_mut().unwrap().data.undo();
            assert!(!s.state.has_changes());
        })
    })
    .unwrap();
}
#[gpui::test]
fn metadata_mapping_paste_defaults_and_query_editor_state(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "09-structure");
    paint(cx, w);
    cx.update_window(w.into(), |_, window, cx| {
        s.update(cx, |s, cx| {
            let input = s.table_inputs[&s.state.active].search.clone();
            input.update(cx, |i, cx| i.set_value("swift", window, cx));
            s.prepare_components(window, cx);
            let k = key(s.state.current());
            assert_eq!(s.components.grids[&k].rows, vec![9]);
            s.grid_event(
                k,
                GridEvent::Edit {
                    cell: Cell { row: 0, col: 1 },
                    value: "swift_renamed".into(),
                    key: s.components.grids[&k]
                        .view
                        .read(cx)
                        .model()
                        .row_key(0)
                        .map(str::to_owned),
                },
                window,
                cx,
            );
            assert_eq!(
                s.state.current().table.as_ref().unwrap().structure.rows[9].cur[1].value(),
                Some("swift_renamed")
            );
            s.switch_part(Part::Data, cx);
            s.prepare_table_inputs(window, cx);
            s.prepare_components(window, cx);
            let k = key(s.state.current());
            s.grid_event(
                k,
                GridEvent::DuplicateRows(vec![
                    s.components.grids[&k].view.read(cx).model().row_ref(0),
                ]),
                window,
                cx,
            );
            let row = s
                .state
                .current()
                .table
                .as_ref()
                .unwrap()
                .data
                .rows
                .last()
                .unwrap();
            assert!(matches!(row.cur[0], Value::Unset { .. }));
            assert!(row.orig.is_none());
            assert_eq!(
                s.components.grids[&k]
                    .view
                    .read(cx)
                    .model()
                    .selection
                    .anchor,
                Some(s.state.current().table.as_ref().unwrap().data.rows.len() - 1)
            );
            s.new_query("SELECT 1; SELECT 'ธนาคาร😺';", cx);
            s.prepare_components(window, cx);
            let e = s.components.editors[&s.state.active].clone();
            e.update(cx, |e, cx| {
                e.set_selections(
                    &[telecat_editor::Selection {
                        anchor: 10,
                        head: 10,
                    }],
                    cx,
                )
            });
            s.sync_editor_document(cx);
            assert_eq!(
                QueryPlan::new(
                    &s.state.current().editor,
                    false,
                    Driver::Mysql,
                    SafeMode::Silent,
                    0
                )
                .statements,
                vec!["SELECT 'ธนาคาร😺'"]
            );
        })
    })
    .unwrap();
}
#[gpui::test]
fn named_queries_history_and_file_dialogs(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "05-query-current");
    paint(cx, w);
    let root = s.read_with(cx, |s, _| s.services.profile.root.clone());
    cx.update_window(w.into(), |_, window, cx| {
        s.update(cx, |s, cx| {
            s.save_command(window, cx);
            assert!(matches!(
                s.sheet,
                Some(crate::workspace::Sheet::SaveQuery { .. })
            ));
            s.sheet_input
                .update(cx, |i, cx| i.set_value("Thai query 😺", window, cx));
            s.save_named_query(cx);
            assert_eq!(s.queries[0].name, "Thai query 😺");
            assert_eq!(s.state.current().title, "Thai query 😺");
            s.open_history("SELECT 9;", window, cx);
            s.sync_editor_document(cx);
            assert!(s.state.current().editor.text.ends_with("SELECT 9;"));
            s.open_sql_file(cx);
        })
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    let file = root.join("roundtrip.sql");
    std::fs::write(&file, "SELECT 'ธนาคารกรุงเทพ 😺';").unwrap();
    cx.update_window(w.into(), |_, _, cx| {
        s.update(cx, |s, cx| s.open_sql_file(cx))
    })
    .unwrap();
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && !options.multiple);
        Some(vec![file.clone()])
    });
    cx.run_until_parked();
    assert_eq!(
        s.read_with(cx, |s, _| s.state.current().editor.text.clone()),
        "SELECT 'ธนาคารกรุงเทพ 😺';"
    );
    paint(cx, w);
    cx.update_window(w.into(), |_, _, cx| {
        s.update(cx, |s, cx| s.save_sql_file(cx))
    })
    .unwrap();
    let saved = root.join("saved.sql");
    cx.simulate_new_path_selection(|_| Some(saved.clone()));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(saved).unwrap(),
        "SELECT 'ธนาคารกรุงเทพ 😺';"
    );
    cx.update_window(w.into(), |_, _, cx| {
        s.update(cx, |s, cx| s.open_sql_file(cx))
    })
    .unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec![root.join("missing.sql")]));
    cx.run_until_parked();
    assert!(s.read_with(cx, |s, _| matches!(
        s.sheet,
        Some(crate::workspace::Sheet::Error(_))
    )));
}
#[test]
fn export_matches_electron_null_empty_and_escaping() {
    let cols = vec!["a".into(), "b".into()];
    let rows = vec![vec![None, Some("ธนาคาร,\"😺\"\n".into())]];
    assert_eq!(
        super::export_text("x", &cols, &rows, "csv"),
        "\u{feff}a,b\r\n,\"ธนาคาร,\"\"😺\"\"\n\"\r\n"
    );
    let json: serde_json::Value =
        serde_json::from_str(&super::export_text("x", &cols, &rows, "json")).unwrap();
    assert!(json[0]["a"].is_null());
    assert_eq!(json[0]["b"], "ธนาคาร,\"😺\"\n");
    assert_eq!(
        super::export_text("x", &cols, &[vec![Some("a'b\\c".into()), None]], "sql"),
        "INSERT INTO x (a, b) VALUES ('a''b\\\\c', NULL);\n"
    );
}

#[gpui::test]
fn native_edit_menu_routes_to_focused_input_and_cell(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    paint(cx, w);
    cx.update_window(w.into(), |_, window, cx| {
        s.update(cx, |s, cx| {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("bank_name_th".into()));
            s.search.update(cx, |i, cx| i.focus(window, cx));
            s.edit_menu("paste", window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    s.read_with(cx, |s, cx| {
        assert_eq!(s.search.read(cx).value().as_ref(), "bank_name_th");
        assert!(!s.state.has_changes());
    });
}

fn drag(cx: &mut TestAppContext, w: AnyWindowHandle, from: (f32, f32), to: (f32, f32)) {
    use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
    cx.update_window(w.into(), |_, w, cx| {
        w.dispatch_event(
            MouseMoveEvent {
                position: point(px(from.0), px(from.1)),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseDownEvent {
                position: point(px(from.0), px(from.1)),
                button: MouseButton::Left,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseMoveEvent {
                position: point(px(to.0), px(to.1)),
                pressed_button: Some(MouseButton::Left),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseUpEvent {
                position: point(px(to.0), px(to.1)),
                button: MouseButton::Left,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    paint(cx, w);
}
#[gpui::test]
fn shell_splitters_and_scrollbar_drag_outside_their_bounds(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "05-query-current");
    paint(cx, w);
    // Electron exposes a 7 px grab area around the 1 px divider.
    drag(cx, w, (272., 350.), (372., 400.));
    assert_eq!(s.read_with(cx, |s, _| s.layout.sidebar), 370.);
    drag(cx, w, (1119., 350.), (1000., 400.));
    assert_eq!(s.read_with(cx, |s, _| s.layout.panel), 439.);
    drag(cx, w, (700., 645.), (700., 600.));
    assert_eq!(s.read_with(cx, |s, _| s.layout.console), 215.);
    let b = s.read_with(cx, |s, _| s.layout.query_bounds);
    let y = b.top().as_f32() + b.size.height.as_f32() * 0.5;
    drag(cx, w, (700., y), (700., b.bottom().as_f32() - 10.));
    assert_eq!(
        s.read_with(cx, |s, _| s.layout.query_split(s.state.active)),
        0.85
    );

    let (w, s) = setup(cx, "07-table-data");
    paint(cx, w);
    let (bounds, offset, max) = s.read_with(cx, |s, _| {
        (
            s.console_scroll.bounds(),
            s.console_scroll.offset().y,
            s.console_scroll.max_offset().y,
        )
    });
    assert_eq!(
        -offset, max,
        "console must start at the bottom after layout"
    );
    assert!(max > px(0.));
    drag(
        cx,
        w,
        (bounds.right().as_f32() - 5., bounds.bottom().as_f32() - 8.),
        (700., bounds.top().as_f32() - 200.),
    );
    assert_eq!(s.read_with(cx, |s, _| s.console_scroll.offset().y), px(0.));
}
#[gpui::test]
fn perf_fixture_completion_catalog_and_idle_scenarios(cx: &mut TestAppContext) {
    use std::{cell::Cell as Counter, rc::Rc, time::Duration};
    let (w, s) = setup(cx, "perf-1000");
    paint(cx, w);
    s.read_with(cx, |s, cx| {
        let t = s.state.current();
        assert_eq!(t.kind, TabKind::Query);
        let m = s.components.grids[&key(t)].view.read(cx).model();
        assert_eq!(m.rows.len(), 1000);
        assert_eq!(m.columns.len(), 15);
        assert_eq!(m.cell_value(Cell { row: 999, col: 14 }), Some("🐈 🏦"));
        assert_eq!(m.cell_value(Cell { row: 0, col: 2 }), Some("ธนาคารกรุงเทพ"));
        assert!(m.rows.iter().all(|r| r.len() == 15));
        assert!(m.storage_bytes() < 500_000);
        println!("perf-1000 grid model: {} bytes", m.storage_bytes());
    });
    for scenario in ["05-query-current", "07-table-data"] {
        let (w, s) = setup(cx, scenario);
        paint(cx, w);
        cx.run_until_parked();
        s.read_with(cx, |s, _| {
            assert!(s.running.timer.is_none());
            assert!(s.components.schema.contains_key("sa_banks"));
            assert!(s.components.schema.values().all(Vec::is_empty));
            assert_eq!(
                s.components.schema.len(),
                s.state
                    .objects
                    .iter()
                    .filter(|o| matches!(
                        o.kind,
                        telecat_core::types::ObjectKind::Table
                            | telecat_core::types::ObjectKind::View
                    ))
                    .count()
            );
        });
        let notifications = Rc::new(Counter::new(0));
        let count = notifications.clone();
        let mut observers =
            vec![cx.update(|cx| cx.observe(&s, move |_, _| count.set(count.get() + 1)))];
        let inputs = s.read_with(cx, |s, _| {
            vec![
                s.search.clone(),
                s.field_search.clone(),
                s.sheet_input.clone(),
                s.gate_password.clone(),
            ]
        });
        for input in inputs {
            let count = notifications.clone();
            observers
                .push(cx.update(|cx| cx.observe(&input, move |_, _| count.set(count.get() + 1))));
        }
        let editors = s.read_with(cx, |s, cx| {
            s.components
                .editors
                .values()
                .map(|e| e.read(cx).input_state().clone())
                .collect::<Vec<_>>()
        });
        for input in editors {
            let count = notifications.clone();
            observers
                .push(cx.update(|cx| cx.observe(&input, move |_, _| count.set(count.get() + 1))));
        }
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        assert_eq!(
            notifications.get(),
            0,
            "idle workspace notifications: {scenario}"
        );
    }
}
#[gpui::test]
fn open_anything_uses_fuzzy_labels_current_database_and_scrolls_selection(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "95-rail");
    paint(cx, w);
    cx.update_window(w.into(), |_, window, cx| {
        s.update(cx, |s, cx| {
            s.open_anything(window, cx);
            s.sheet_input
                .update(cx, |i, cx| i.set_value("blinv", window, cx));
            let items = s.picker_items(cx);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].0, "billing.invoices");
            s.databases = vec!["a".into(), s.state.database.clone(), "z".into()];
            s.open_database_picker(window, cx);
            assert_eq!(s.picker_selection, 1);
        })
    })
    .unwrap();
    paint(cx, w);
}

#[test]
fn metadata_widths_sample_the_loaded_rows_before_staged_edits() {
    for scenario in [
        "09-structure",
        "23-pg-structure",
        "24-pg-index",
        "53-structure-staged",
    ] {
        let state = super::super::table_fixtures::state(scenario);
        let (model, _) = super::make_grid(state.current(), state.connection.driver, "");
        let source = &state.current().table.as_ref().unwrap().buffer().result;
        assert_eq!(
            model.widths,
            telecat_grid::default_widths(&model.columns, &source.rows)
        );
        if model.columns[0].name == "#" {
            assert_eq!(model.widths[0], 60.);
        }
    }
    let before = super::super::table_fixtures::state("09-structure");
    let staged = super::super::table_fixtures::state("53-structure-staged");
    assert_eq!(
        super::make_grid(before.current(), before.connection.driver, "")
            .0
            .widths,
        super::make_grid(staged.current(), staged.connection.driver, "")
            .0
            .widths
    );
}

#[gpui::test]
fn worker_panic_surfaces_in_workspace_without_losing_edits(cx: &mut TestAppContext) {
    let (_, workspace) = setup(cx, "51-data-staged");
    let error = workspace.read_with(cx, |s, _| {
        futures::executor::block_on(s.services.runtime.spawn(async {
            panic!("test-only secret SQL parameter");
        }))
        .unwrap_err()
    });
    workspace.update(cx, |s, cx| s.error(error, cx));
    workspace.read_with(cx, |s, _| {
        assert!(matches!(&s.sheet, Some(super::super::Sheet::Error(message))
            if message.contains("Reload or reconnect") && !message.contains("secret")));
        assert!(s.state.current().table.as_ref().unwrap().data.dirty());
    });
}
#[gpui::test]
fn cmd_z_shortcut_undoes_typing_in_the_focused_sql_editor(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "04-ws-mysql");
    let services = s.read_with(cx, |s, _| s.services.clone());
    cx.update(|cx| crate::install_menus(services, cx));
    cx.update_window(w, |_, window, _| window.activate_window())
        .unwrap();
    paint(cx, w);
    let editor = s.read_with(cx, |s, _| s.components.editors[&s.state.active].clone());
    cx.update_window(w.into(), |_, window, cx| {
        editor.update(cx, |e, cx| e.focus(window, cx))
    })
    .unwrap();
    cx.simulate_input(w.into(), "select 1");
    assert_eq!(editor.read_with(cx, |e, cx| e.text(cx)), "select 1");
    cx.simulate_keystrokes(w.into(), "cmd-z");
    assert_eq!(editor.read_with(cx, |e, cx| e.text(cx)), "");
}
fn install_shortcuts(cx: &mut TestAppContext, w: AnyWindowHandle, s: &Entity<Workspace>) {
    let services = s.read_with(cx, |s, _| s.services.clone());
    cx.update(|cx| crate::install_menus(services, cx));
    cx.update_window(w, |_, window, _| window.activate_window())
        .unwrap();
    paint(cx, w);
}
#[gpui::test]
fn clipboard_shortcuts_edit_a_focused_text_input(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.search.update(cx, |i, cx| i.focus(window, cx)))
    })
    .unwrap();
    cx.simulate_input(w, "abc");
    cx.simulate_keystrokes(w, "cmd-a cmd-x");
    assert_eq!(s.read_with(cx, |s, cx| s.search.read(cx).value()), "");
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "abc"
    );
    cx.simulate_keystrokes(w, "cmd-v cmd-v");
    assert_eq!(s.read_with(cx, |s, cx| s.search.read(cx).value()), "abcabc");
    assert!(!s.read_with(cx, |s, _| s.state.has_changes()));
}
#[gpui::test]
fn clipboard_shortcuts_copy_and_paste_grid_rows(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    let grid = s.read_with(cx, |s, _| {
        s.components.grids[&key(s.state.current())].view.clone()
    });
    let select = |cx: &mut TestAppContext, row: usize| {
        grid.update(cx, |g, cx| {
            g.update_model(|m| m.selection = telecat_grid::Selection::single(row), cx)
        })
    };
    select(cx, 0);
    cx.simulate_keystrokes(w, "cmd-c");
    let copied = cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert_eq!(copied, grid.read_with(cx, |g, _| g.model().selected_tsv()));
    assert!(!s.read_with(cx, |s, _| s.state.has_changes()));
    select(cx, 1);
    cx.simulate_keystrokes(w, "cmd-v");
    assert!(
        s.read_with(cx, |s, _| s.state.has_changes()),
        "pasting row 1 over row 2 stages edits"
    );
}
fn double_click_grid_empty_area(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    s: &Entity<Workspace>,
) {
    use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseUpEvent};
    let grid = s.read_with(cx, |s, _| {
        s.components.grids[&key(s.state.current())].view.clone()
    });
    let b = grid.read_with(cx, |g, _| g.bounds());
    // Bottom-left of the body, clear of the scrollbars: below the last row of these fixtures.
    let position = point(b.origin.x + px(120.), b.origin.y + b.size.height - px(24.));
    for count in [1, 2] {
        cx.update_window(w, |_, w, cx| {
            w.dispatch_event(
                MouseDownEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: count,
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            w.dispatch_event(
                MouseUpEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: count,
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
        })
        .unwrap();
        paint(cx, w);
    }
}
fn staged_new_rows(cx: &mut TestAppContext, s: &Entity<Workspace>) -> usize {
    s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().map_or(0, |t| {
            t.data.rows.iter().filter(|r| r.orig.is_none()).count()
        })
    })
}
/// Feature: double-clicking the empty area of a table's Data grid stages one new row, selects it
/// and opens the editor on its first non-key cell, like the Insert action.
#[gpui::test]
fn double_click_empty_grid_area_adds_one_row(cx: &mut TestAppContext) {
    for scenario in [
        "07-table-data",
        "22-pg-table",
        "58-empty-table",
        "59-pg-empty-table",
    ] {
        let (w, s) = setup(cx, scenario);
        // Keep three rows so the grid has empty space below them (a "short table").
        cx.update_window(w, |_, window, cx| {
            s.update(cx, |s, cx| {
                let t = s.state.current_mut().table.as_mut().unwrap();
                let mut result = t.data.result.clone();
                result.rows.truncate(3);
                t.data.reset(result, None);
                s.prepare_components(window, cx);
            })
        })
        .unwrap();
        paint(cx, w);
        let before = s.read_with(cx, |s, _| {
            s.state.current().table.as_ref().unwrap().data.rows.len()
        });
        let (rows, b, loaded) = s.read_with(cx, |s, cx| {
            let g = s.components.grids[&key(s.state.current())].view.read(cx);
            (
                g.model().rows.len(),
                g.bounds(),
                s.state.current().table.as_ref().unwrap().data.loaded,
            )
        });
        double_click_grid_empty_area(cx, w, &s);
        assert_eq!(
            staged_new_rows(cx, &s),
            1,
            "{scenario}: rows={rows} bounds={b:?} loaded={loaded}"
        );
        let grid = s.read_with(cx, |s, _| {
            s.components.grids[&key(s.state.current())].view.clone()
        });
        grid.read_with(cx, |g, _| {
            let m = g.model();
            assert!(
                m.selection.rows.contains(&before),
                "{scenario}: new row selected"
            );
            assert_eq!(
                m.editing.as_ref().map(|e| e.cell.row),
                Some(before),
                "{scenario}: editing it"
            );
        });
        s.update(cx, |s, _| {
            s.state.current_mut().table.as_mut().unwrap().data.undo()
        });
        assert_eq!(staged_new_rows(cx, &s), 0, "{scenario}: ⌘Z removes it");
        cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
    }
}
#[gpui::test]
fn double_click_empty_area_is_ignored_in_query_results_and_while_saving(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "04-ws-mysql");
    paint(cx, w);
    let rows = s.read_with(cx, |s, cx| {
        s.components.grids[&key(s.state.current())]
            .view
            .read(cx)
            .model()
            .rows
            .len()
    });
    double_click_grid_empty_area(cx, w, &s);
    assert_eq!(
        s.read_with(cx, |s, cx| s.components.grids[&key(s.state.current())]
            .view
            .read(cx)
            .model()
            .rows
            .len()),
        rows,
        "query results never get rows"
    );
    cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
    let (w, s) = setup(cx, "58-empty-table");
    paint(cx, w);
    s.update(cx, |s, _| s.saving = true);
    double_click_grid_empty_area(cx, w, &s);
    assert_eq!(staged_new_rows(cx, &s), 0, "locked while a commit runs");
}
/// The sidebar's minimum width fits Items / Queries / History at their real text widths.
#[gpui::test]
fn sidebar_minimum_fits_the_segment_labels(cx: &mut TestAppContext) {
    let (w, _) = setup(cx, "d1-narrow-sidebar");
    cx.update_window(w, |_, window, _| {
        let font = telecat_theme::ui();
        let widths: Vec<f32> = ["Items", "Queries", "History"]
            .iter()
            .map(|label| {
                window
                    .text_system()
                    .shape_line(
                        (*label).into(),
                        px(13.5),
                        &[gpui_kit::TextRun {
                            len: label.len(),
                            font: font.clone(),
                            color: gpui_kit::black(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    )
                    .width
                    .as_f32()
            })
            .collect();
        // Equal-width segments: the widest label plus 6 pt padding each side, three of them,
        // 4 pt gaps and the control's 8 pt insets.
        let widest = widths.iter().cloned().fold(0., f32::max);
        let needed = 3. * (widest + 12.) + 2. * 4. + 16.;
        assert!(
            needed <= crate::workspace::layout::SIDEBAR_MIN,
            "labels need {needed} pt, minimum is {}",
            crate::workspace::layout::SIDEBAR_MIN
        );
    })
    .unwrap();
}
/// Loading shows the overlay over a dimmed grid without "No rows"; a finished empty load shows
/// "No rows".
#[gpui::test]
fn table_grid_loading_and_empty_states(cx: &mut TestAppContext) {
    use telecat_app::workspace_model::{LoadSlot, LoadState, Placeholder};
    let (w, s) = setup(cx, "d2-loading");
    paint(cx, w);
    let grid = s.read_with(cx, |s, _| {
        s.components.grids[&key(s.state.current())].view.clone()
    });
    assert!(
        grid.read_with(cx, |g, _| g.model().options.empty_text.is_empty()),
        "no \"No rows\" while loading"
    );
    assert_eq!(
        s.read_with(cx, |s, _| s.grid_placeholder(false)),
        Placeholder::Loading { dimmed: false }
    );
    s.update(cx, |s, _| {
        s.state.current_mut().loads.insert(
            LoadSlot::Part(telecat_app::table_model::Part::Data),
            LoadState::Loaded,
        )
    });
    paint(cx, w);
    assert_eq!(
        grid.read_with(cx, |g, _| g.model().options.empty_text.clone()),
        "No rows"
    );
    s.update(cx, |s, _| {
        s.state.current_mut().loads.insert(
            LoadSlot::Part(telecat_app::table_model::Part::Data),
            LoadState::Failed("ER_NO_SUCH_TABLE".into()),
        )
    });
    paint(cx, w);
    assert!(grid.read_with(cx, |g, _| g.model().options.empty_text.is_empty()));
}
/// The open-tabs bar scrolls the active tab into view and with the mouse wheel.
#[gpui::test]
fn open_tabs_bar_scrolls(cx: &mut TestAppContext) {
    use gpui_kit::{InputEvent as _, ScrollDelta, ScrollWheelEvent};
    let (w, s) = setup(cx, "d3-many-tabs");
    for _ in 0..3 {
        paint(cx, w);
    }
    let (offset, max) = s.read_with(cx, |s, _| {
        (s.tab_scroll.offset().x, s.tab_scroll.max_offset().x)
    });
    assert!(max > px(0.), "eleven tabs overflow the bar");
    assert!(
        offset < px(0.),
        "the last, active tab was scrolled into view"
    );
    let last = s.read_with(cx, |s, _| {
        s.tab_scroll
            .bounds_for_item(s.state.tabs.len() - 1)
            .unwrap()
    });
    let bar = s.read_with(cx, |s, _| s.tab_scroll.bounds());
    // Item bounds are in unscrolled layout coordinates.
    assert!(
        last.right() + offset <= bar.right() + px(1.) && last.left() + offset >= bar.left(),
        "{last:?} scrolled by {offset:?} within {bar:?}"
    );
    // A vertical wheel over the bar scrolls it sideways.
    let position = point(bar.left() + px(40.), bar.center().y);
    cx.update_window(w, |_, window, cx| {
        window.dispatch_event(
            gpui_kit::MouseMoveEvent {
                position,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), px(300.))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    paint(cx, w);
    let after = s.read_with(cx, |s, _| s.tab_scroll.offset().x);
    assert!(
        after > offset,
        "wheel scrolled back towards the first tab: {offset:?} -> {after:?}"
    );
    // Activating a tab brings it into view again.
    s.update(cx, |s, _| s.state.activate(s.state.tabs.last().unwrap().id));
    s.update(cx, |s, _| s.state.activate(s.state.tabs[0].id));
    for _ in 0..2 {
        paint(cx, w);
    }
    assert_eq!(s.read_with(cx, |s, _| s.tab_scroll.offset().x), px(0.));
}
fn live_config(driver: Driver) -> telecat_core::types::ConnectionConfig {
    let mut c = telecat_core::types::empty_connection(driver);
    c.id = "telecat-ui-live".into();
    c.host = "127.0.0.1".into();
    let mysql = driver == Driver::Mysql;
    c.port = if mysql { 3307 } else { 5434 };
    c.user = if mysql { "root" } else { "postgres" }.into();
    c.database = if mysql { "zixma" } else { "main_db" }.into();
    c
}
/// A workspace connected to the local test database with `table` open and loaded.
fn live_table(
    cx: &mut TestAppContext,
    driver: Driver,
    ddl: &[&str],
    table: &str,
) -> (AnyWindowHandle, Entity<Workspace>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let config = live_config(driver);
    let session = runtime
        .block_on(telecat_db::Session::connect_test(&config, "secret"))
        .expect("test database");
    for sql in ddl {
        runtime.block_on(session.query(sql)).expect(sql);
    }
    let (w, s) = setup_with(cx, "", runtime, Some(Arc::new(session)));
    let table = telecat_core::types::TableRef {
        schema: if driver == Driver::Mysql {
            "zixma"
        } else {
            "public"
        }
        .into(),
        name: table.into(),
    };
    s.update(cx, |s, cx| s.open_table(table.clone(), cx));
    for _ in 0..200 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        let loaded = s.read_with(cx, |s, _| {
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
        });
        if loaded {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(500));
            cx.run_until_parked();
            paint(cx, w);
            return (w, s);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{table:?} did not load");
}
fn current_grid(cx: &mut TestAppContext, s: &Entity<Workspace>) -> Entity<telecat_grid::DataGrid> {
    s.read_with(cx, |s, _| {
        s.components.grids[&key(s.state.current())].view.clone()
    })
}
fn double_click_at(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    position: gpui_kit::Point<gpui_kit::Pixels>,
) {
    use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
    cx.update_window(w, |_, w, cx| {
        w.dispatch_event(
            MouseMoveEvent {
                position,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        )
    })
    .unwrap();
    for count in [1, 2] {
        cx.update_window(w, |_, w, cx| {
            w.dispatch_event(
                MouseDownEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: count,
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            w.dispatch_event(
                MouseUpEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: count,
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        paint(cx, w);
    }
}
fn grid_editor(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    s: &Entity<Workspace>,
) -> (Option<Cell>, Option<String>, bool) {
    let grid = current_grid(cx, s);
    cx.update_window(w, |_, window, cx| {
        let g = grid.read(cx);
        let state = g.editor_state();
        (
            g.model().editing.as_ref().map(|e| e.cell),
            state.as_ref().map(|e| e.read(cx).value().to_string()),
            state.is_some_and(|e| {
                gpui_kit::Focusable::focus_handle(e.read(cx), cx).is_focused(window)
            }),
        )
    })
    .unwrap()
}
/// Regression: double-clicking a cell of an existing row opens the focused inline editor on
/// real MySQL and PostgreSQL tables, with and without a primary key, and it stays open.
#[gpui::test]
fn double_click_existing_cell_edits_it_on_live_tables(cx: &mut TestAppContext) {
    let tables = [
        (
            Driver::Mysql,
            "CREATE TABLE telecat_ui_dblclick (id int PRIMARY KEY AUTO_INCREMENT, name varchar(40), b blob)",
        ),
        (
            Driver::Mysql,
            "CREATE TABLE telecat_ui_dblclick (n int, name varchar(40))",
        ),
        (
            Driver::Postgres,
            "CREATE TABLE telecat_ui_dblclick (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name varchar(40), b bytea)",
        ),
        (
            Driver::Postgres,
            "CREATE TABLE telecat_ui_dblclick (n int, name varchar(40))",
        ),
    ];
    for (driver, ddl) in tables {
        let (w, s) = live_table(
            cx,
            driver,
            &[
                "DROP TABLE IF EXISTS telecat_ui_dblclick",
                ddl,
                "INSERT INTO telecat_ui_dblclick (name) VALUES ('ธนาคาร'), ('b')",
            ],
            "telecat_ui_dblclick",
        );
        let b = current_grid(cx, &s).read_with(cx, |g, _| g.bounds());
        double_click_at(cx, w, point(b.origin.x + px(120.), b.origin.y + px(40.)));
        for _ in 0..3 {
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(1));
            cx.run_until_parked();
            paint(cx, w);
        }
        assert_eq!(
            grid_editor(cx, w, &s),
            (Some(Cell { row: 0, col: 1 }), Some("ธนาคาร".into()), true),
            "{ddl}: the editor opens on the cell, focused, and stays open"
        );
        cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for driver in [Driver::Mysql, Driver::Postgres] {
        runtime.block_on(async {
            let session = telecat_db::Session::connect_test(&live_config(driver), "secret")
                .await
                .unwrap();
            session
                .query("DROP TABLE IF EXISTS telecat_ui_dblclick")
                .await
                .unwrap();
        });
    }
}
/// The same without a server: table grids edit on double-click, query results never do.
#[gpui::test]
fn double_click_existing_cell_edits_table_grids_only(cx: &mut TestAppContext) {
    for (scenario, editable) in [
        ("07-table-data", true),
        ("22-pg-table", true),
        ("04-ws-mysql", false),
    ] {
        let (w, s) = setup(cx, scenario);
        paint(cx, w);
        let b = current_grid(cx, &s).read_with(cx, |g, _| g.bounds());
        double_click_at(cx, w, point(b.origin.x + px(120.), b.origin.y + px(40.)));
        let (cell, _, focused) = grid_editor(cx, w, &s);
        assert_eq!(cell.is_some(), editable, "{scenario}");
        assert_eq!(focused, editable, "{scenario}");
        cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
    }
}
fn click_cell(cx: &mut TestAppContext, w: AnyWindowHandle, s: &Entity<Workspace>, row: usize) {
    use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseUpEvent};
    let b = current_grid(cx, s).read_with(cx, |g, _| g.bounds());
    let position = point(
        b.origin.x + px(120.),
        b.origin.y + px(40. + 26. * row as f32),
    );
    cx.update_window(w, |_, w, cx| {
        w.dispatch_event(
            MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseUpEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    paint(cx, w);
}
fn data_cell(cx: &mut TestAppContext, s: &Entity<Workspace>, row: usize, col: usize) -> Value {
    s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().data.rows[row].cur[col].clone()
    })
}
fn inspector_value(cx: &mut TestAppContext, s: &Entity<Workspace>, col: usize) -> String {
    let input = s
        .read_with(cx, |s, _| s.inspector_input(col))
        .expect("editable field");
    input.read_with(cx, |i, _| i.value().to_string())
}
fn focus_inspector(cx: &mut TestAppContext, w: AnyWindowHandle, s: &Entity<Workspace>, col: usize) {
    let input = s
        .read_with(cx, |s, _| s.inspector_input(col))
        .expect("editable field");
    cx.update_window(w, |_, window, cx| {
        input.update(cx, |i, cx| i.focus(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
/// Feature: the row inspector edits the selected row through the grid's staging path:
/// multi-line Thai text, NULL, colour coding, ⌘Z and the commit preview all match a grid edit.
#[gpui::test]
fn row_inspector_stages_edits_like_the_grid(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    click_cell(cx, w, &s, 0);
    let original = data_cell(cx, &s, 0, 1);
    assert_eq!(inspector_value(cx, &s, 1), original.value().unwrap_or(""));
    // Selecting a row never moves focus into the inspector.
    let input = s.read_with(cx, |s, _| s.inspector_input(1)).unwrap();
    assert!(
        !cx.update_window(w, |_, window, cx| gpui_kit::Focusable::focus_handle(
            input.read(cx),
            cx
        )
        .is_focused(window))
            .unwrap()
    );

    focus_inspector(cx, w, &s, 1);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "ธนาคาร");
    cx.simulate_keystrokes(w, "shift-enter");
    cx.simulate_input(w, "สาขา 2 😺");
    assert_eq!(
        data_cell(cx, &s, 0, 1),
        original,
        "typing alone stages nothing"
    );
    cx.simulate_keystrokes(w, "enter");
    paint(cx, w);
    assert_eq!(
        data_cell(cx, &s, 0, 1),
        Value::Text("ธนาคาร\nสาขา 2 😺".into())
    );
    let grid = current_grid(cx, &s);
    assert!(
        grid.read_with(cx, |g, _| g
            .model()
            .edits
            .modified
            .contains(&Cell { row: 0, col: 1 })),
        "the grid shows the staged cell as modified"
    );
    assert!(s.read_with(cx, |s, _| {
        s.preview_text().unwrap().contains("ธนาคาร\nสาขา 2 😺")
    }));

    // ⌘Z with the field unfocused undoes the staged edit, and the field follows.
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| window.focus(&s.focus, cx))
    })
    .unwrap();
    cx.simulate_keystrokes(w, "cmd-z");
    paint(cx, w);
    assert_eq!(data_cell(cx, &s, 0, 1), original);
    assert_eq!(inspector_value(cx, &s, 1), original.value().unwrap_or(""));

    // Set NULL, then an untouched empty NULL field stays NULL on blur.
    s.update(cx, |s, cx| s.set_inspector_null(1, cx));
    paint(cx, w);
    assert_eq!(data_cell(cx, &s, 0, 1), Value::Null);
    assert_eq!(inspector_value(cx, &s, 1), "");
    focus_inspector(cx, w, &s, 1);
    cx.simulate_keystrokes(w, "enter");
    assert_eq!(data_cell(cx, &s, 0, 1), Value::Null);

    // Text typed but not yet staged is staged when the selection moves to another row.
    focus_inspector(cx, w, &s, 1);
    cx.simulate_input(w, "moved");
    click_cell(cx, w, &s, 1);
    assert_eq!(data_cell(cx, &s, 0, 1), Value::Text("moved".into()));
    assert_eq!(
        inspector_value(cx, &s, 1),
        data_cell(cx, &s, 1, 1).value().unwrap_or(""),
        "the inspector now shows row 2"
    );

    // ⌘S flushes the focused field into the commit snapshot.
    focus_inspector(cx, w, &s, 1);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "flushed");
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.flush_grid_edits(window, cx))
    })
    .unwrap();
    assert_eq!(data_cell(cx, &s, 1, 1), Value::Text("flushed".into()));

    // Escape discards unstaged typing.
    cx.simulate_input(w, " and more");
    cx.simulate_keystrokes(w, "escape");
    assert_eq!(inspector_value(cx, &s, 1), "flushed");
    cx.simulate_keystrokes(w, "enter");
    assert_eq!(data_cell(cx, &s, 1, 1), Value::Text("flushed".into()));
}
/// The inspector is read-only wherever the grid is: binary columns, query results, and while a
/// commit runs.
#[gpui::test]
fn row_inspector_respects_read_only_rules(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    s.update(cx, |s, _| {
        let t = s.state.current_mut().table.as_mut().unwrap();
        let n = t.data.result.columns.len();
        t.data.result.binary = Some((0..n).map(|c| c == 2).collect());
    });
    paint(cx, w);
    click_cell(cx, w, &s, 0);
    assert!(s.read_with(cx, |s, _| s.inspector_input(1).is_some()));
    assert!(
        s.read_with(cx, |s, _| s.inspector_input(2).is_none()),
        "binary columns are shown, not edited"
    );
    s.update(cx, |s, cx| {
        s.stage_inspector_text(s.state.active, "missing", 1, "x".into(), cx)
    });
    let key = s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().data.rows[0]
            .key
            .clone()
    });
    let before = data_cell(cx, &s, 0, 2);
    s.update(cx, |s, cx| {
        s.stage_inspector_text(s.state.active, &key, 2, "x".into(), cx)
    });
    assert_eq!(
        data_cell(cx, &s, 0, 2),
        before,
        "a binary cell is never staged as text"
    );

    let before = data_cell(cx, &s, 0, 1);
    s.update(cx, |s, cx| {
        s.saving = true;
        cx.notify();
    });
    paint(cx, w);
    focus_inspector(cx, w, &s, 1);
    cx.simulate_input(w, "while saving");
    cx.simulate_keystrokes(w, "enter");
    s.update(cx, |s, cx| s.set_inspector_null(1, cx));
    assert_eq!(
        data_cell(cx, &s, 0, 1),
        before,
        "locked while a commit runs"
    );
    cx.update_window(w, |_, w, _| w.remove_window()).unwrap();

    let (w, s) = setup(cx, "06-query-all-rowselected");
    paint(cx, w);
    assert!(
        s.read_with(cx, |s, _| s.inspector.is_none()
            && s.inspector_input(0).is_none()),
        "query results are read-only"
    );
}
/// Enter stages the field and hands focus back to the grid, so the next ⌘Z undoes the staged
/// edit, exactly as after a grid edit.
#[gpui::test]
fn row_inspector_enter_then_cmd_z_undoes_the_staged_edit(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    click_cell(cx, w, &s, 0);
    let original = data_cell(cx, &s, 0, 3);
    focus_inspector(cx, w, &s, 3);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "typed");
    cx.simulate_keystrokes(w, "enter");
    assert_eq!(data_cell(cx, &s, 0, 3), Value::Text("typed".into()));
    let grid = current_grid(cx, &s);
    assert!(
        cx.update_window(w, |_, window, cx| gpui_kit::Focusable::focus_handle(
            grid.read(cx),
            cx
        )
        .is_focused(window))
            .unwrap()
    );
    cx.simulate_keystrokes(w, "cmd-z");
    paint(cx, w);
    assert_eq!(data_cell(cx, &s, 0, 3), original);
    assert_eq!(inspector_value(cx, &s, 3), original.value().unwrap_or(""));
    assert!(!s.read_with(cx, |s, _| s.state.has_changes()));
}
fn focus_grid(cx: &mut TestAppContext, w: AnyWindowHandle, s: &Entity<Workspace>) {
    let g = current_grid(cx, s);
    cx.update_window(w, |_, window, cx| {
        let h = gpui_kit::Focusable::focus_handle(g.read(cx), cx);
        window.focus(&h, cx)
    })
    .unwrap();
    cx.run_until_parked();
    paint(cx, w);
}
fn dirty(cx: &mut TestAppContext, s: &Entity<Workspace>) -> bool {
    s.read_with(cx, |s, _| s.state.has_changes())
}
/// QA BUG-002: Discard All throws away typing still in an inspector field; it never comes back
/// when the field loses focus.
#[gpui::test]
fn discard_all_drops_unstaged_inspector_typing(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    click_cell(cx, w, &s, 0);
    let original = data_cell(cx, &s, 0, 1);
    focus_inspector(cx, w, &s, 3);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "staged");
    cx.simulate_keystrokes(w, "enter");
    assert!(dirty(cx, &s));
    focus_inspector(cx, w, &s, 1);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "UNSTAGED");
    cx.simulate_keystrokes(w, "cmd-shift-backspace");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();
    paint(cx, w);
    assert!(!dirty(cx, &s));
    assert_eq!(inspector_value(cx, &s, 1), original.value().unwrap_or(""));
    focus_grid(cx, w, &s);
    assert_eq!(data_cell(cx, &s, 0, 1), original);
    assert!(!dirty(cx, &s), "discarded typing came back on blur");
}
/// QA BUG-001: a cell editor left open while the rows are reloaded (here in another order) is
/// closed; Enter afterwards stages nothing, on any row.
#[gpui::test]
fn reload_closes_an_open_cell_editor_without_staging(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    let b = current_grid(cx, &s).read_with(cx, |g, _| g.bounds());
    double_click_at(cx, w, point(b.origin.x + px(120.), b.origin.y + px(40.)));
    assert!(grid_editor(cx, w, &s).0.is_some());
    s.update(cx, |s, cx| {
        let t = s.state.current_mut().table.as_mut().unwrap();
        let mut result = t.data.result.clone();
        result.rows.reverse();
        t.data.reset(result, None);
        cx.notify();
    });
    paint(cx, w);
    assert_eq!(grid_editor(cx, w, &s), (None, None, false));
    cx.simulate_keystrokes(w, "enter");
    cx.run_until_parked();
    paint(cx, w);
    assert!(!dirty(cx, &s));
    // QA BUG-003: the grid's highlight and the inspector agree after the reload.
    let (selected, highlighted) = s.read_with(cx, |s, cx| {
        let g = s.components.grids[&key(s.state.current())].view.read(cx);
        (
            s.state.current().grid.selected_row,
            g.model().selection.anchor,
        )
    });
    assert_eq!((selected, highlighted), (None, None));
}
/// QA BUG-005: a new row's editor opens on the first visible column, never on a hidden one, so
/// it is always on screen; with no visible column there is no editor, and the grid keeps working.
#[gpui::test]
fn new_row_editor_skips_hidden_columns(cx: &mut TestAppContext) {
    for how in ["hidden", "column_filter"] {
        let (w, s) = setup(cx, "07-table-data");
        install_shortcuts(cx, w, &s);
        let first = s.read_with(cx, |s, _| {
            let t = s.state.current().table.as_ref().unwrap();
            t.data
                .result
                .columns
                .iter()
                .position(|c| !t.primary_key.contains(c))
                .unwrap()
        });
        s.update(cx, |s, cx| {
            let t = s.state.current_mut().table.as_mut().unwrap();
            if how == "hidden" {
                let name = t.data.result.columns[first].clone();
                t.hidden.insert(name);
            } else {
                t.column_filter = Some("zzz_no_match".into());
            }
            cx.notify();
        });
        paint(cx, w);
        let before = s.read_with(cx, |s, _| {
            s.state.current().table.as_ref().unwrap().data.rows.len()
        });
        double_click_grid_empty_area(cx, w, &s);
        let g = current_grid(cx, &s);
        let (editing, rect, scroll_x, width) = g.read_with(cx, |g, _| {
            let m = g.model();
            (
                m.editing.as_ref().map(|e| e.cell),
                m.editing.as_ref().map(|e| m.cell_rect(e.cell)),
                m.scroll_x,
                m.layout().width,
            )
        });
        let new_row = s.read_with(cx, |s, _| {
            s.state.current().table.as_ref().unwrap().data.rows.len()
        });
        assert_eq!(new_row, before + 1, "{how}: the row is added");
        if how == "hidden" {
            let cell = editing.expect("an editor on a visible column");
            assert_ne!(cell.col, first, "{how}");
            let r = rect.unwrap();
            assert!(
                r.width > 0. && r.x >= 0. && r.x + r.width <= width,
                "{how}: on screen {r:?}"
            );
            assert_eq!(scroll_x, 0., "{how}");
            cx.simulate_input(w, "typed");
            cx.simulate_keystrokes(w, "enter");
            let row = s.read_with(cx, |s, _| {
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
            assert!(
                matches!(row[first], Value::Unset { .. }),
                "{how}: the hidden column is untouched"
            );
            assert_eq!(row[cell.col], Value::Text("typed".into()));
        } else {
            assert_eq!(editing, None, "{how}: no visible column, no editor");
        }
        // The grid is never stuck: the arrow keys still move the selection.
        focus_grid(cx, w, &s);
        cx.simulate_keystrokes(w, "up");
        paint(cx, w);
        let anchor = current_grid(cx, &s).read_with(cx, |g, _| g.model().selection.anchor);
        assert_eq!(anchor, Some(before - 1), "{how}: arrows move the selection");
        cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
    }
}
/// QA BUG-001 end to end on PostgreSQL without a primary key: a reload that returns the rows in
/// another order while a cell editor is open never writes the old row's value into another row.
#[gpui::test]
fn live_pg_reload_under_an_open_editor_never_writes_another_row(cx: &mut TestAppContext) {
    let (w, s) = live_table(
        cx,
        Driver::Postgres,
        &[
            "DROP TABLE IF EXISTS telecat_ui_race",
            "CREATE TABLE telecat_ui_race (n int, name text)",
            "INSERT INTO telecat_ui_race VALUES (1,'alpha'),(2,'bravo'),(3,'charlie')",
        ],
        "telecat_ui_race",
    );
    install_shortcuts(cx, w, &s);
    // Another client updates row 1 (PostgreSQL appends the new tuple, so the next read returns
    // it last) inside a transaction that holds our reload back for a moment.
    struct DropTable;
    impl Drop for DropTable {
        fn drop(&mut self) {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                if let Ok(s) =
                    telecat_db::Session::connect_test(&live_config(Driver::Postgres), "secret")
                        .await
                {
                    let _ = s.query("DROP TABLE IF EXISTS telecat_ui_race").await;
                }
            });
        }
    }
    let _drop_table = DropTable;
    let (locked, wait_locked) = std::sync::mpsc::channel();
    let locker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let other = telecat_db::Session::connect_test(&live_config(Driver::Postgres), "secret")
                .await
                .unwrap();
            for sql in [
                "BEGIN",
                "LOCK TABLE telecat_ui_race IN ACCESS EXCLUSIVE MODE",
                "UPDATE telecat_ui_race SET n = n WHERE n = 1",
            ] {
                other.query(sql).await.unwrap();
            }
            locked.send(()).unwrap();
            for sql in ["SELECT pg_sleep(1.5)", "COMMIT"] {
                other.query(sql).await.unwrap();
            }
        });
    });
    wait_locked.recv().unwrap();
    s.update(cx, |s, cx| s.refresh_tab(cx));
    cx.run_until_parked();
    paint(cx, w);
    let b = current_grid(cx, &s).read_with(cx, |g, _| g.bounds());
    double_click_at(cx, w, point(b.origin.x + px(120.), b.origin.y + px(40.)));
    locker.join().unwrap();
    for _ in 0..200 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        if !s.read_with(cx, |s, _| {
            s.state
                .current()
                .load(s.state.current().slot())
                .is_loading()
        }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let names = s.read_with(cx, |s, _| {
        s.state
            .current()
            .table
            .as_ref()
            .unwrap()
            .data
            .rows
            .iter()
            .map(|r| r.cur[1].clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        names[0],
        Value::Text("bravo".into()),
        "the reload reordered the rows"
    );
    cx.simulate_keystrokes(w, "enter");
    cx.run_until_parked();
    assert!(
        !dirty(cx, &s),
        "{:?}",
        s.read_with(cx, |s, _| s.preview_text())
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let session = telecat_db::Session::connect_test(&live_config(Driver::Postgres), "secret")
            .await
            .unwrap();
        let rows = session
            .query("SELECT n || '=' || name FROM telecat_ui_race ORDER BY n")
            .await
            .unwrap();
        let rows: Vec<_> = rows.rows.iter().map(|r| r[0].clone().unwrap()).collect();
        assert_eq!(rows, ["1=alpha", "2=bravo", "3=charlie"]);
    });
}
/// A megabyte-sized value is shown read-only and truncated in the inspector, so selecting its row
/// stays fast; nothing is staged, and the full value is untouched.
#[gpui::test]
fn row_inspector_previews_large_values(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    let big = format!("{}end", "ธนาคาร ".repeat(1024 * 1024 / 19));
    s.update(cx, |s, cx| {
        let t = s.state.current_mut().table.as_mut().unwrap();
        let mut result = t.data.result.clone();
        result.rows[0][2] = Some(big.clone());
        t.data.reset(result, None);
        cx.notify();
    });
    paint(cx, w);
    let started = std::time::Instant::now();
    click_cell(cx, w, &s, 0);
    paint(cx, w);
    eprintln!(
        "selecting a row with a {} KB value: {:?}",
        big.len() / 1024,
        started.elapsed()
    );
    assert!(s.read_with(cx, |s, _| s.inspector_input(2).is_none()));
    assert!(s.read_with(cx, |s, _| s.inspector_input(1).is_some()));
    focus_inspector(cx, w, &s, 1);
    focus_grid(cx, w, &s);
    assert!(!dirty(cx, &s));
    assert_eq!(data_cell(cx, &s, 0, 2), Value::Text(big));
}
fn names_in(cx: &mut TestAppContext, s: &Entity<Workspace>, col: usize) -> Vec<Value> {
    s.read_with(cx, |s, _| {
        s.state
            .current()
            .table
            .as_ref()
            .unwrap()
            .data
            .rows
            .iter()
            .map(|r| r.cur[col].clone())
            .collect()
    })
}
/// Rows replaced in the same frame as a keystroke, before any render: what the reload does when
/// its result lands while the user is typing.
fn reverse_rows_without_render(cx: &mut TestAppContext, s: &Entity<Workspace>) {
    s.update(cx, |s, _| {
        let t = s.state.current_mut().table.as_mut().unwrap();
        let mut result = t.data.result.clone();
        result.rows.reverse();
        t.data.reset(result, None);
    });
}
/// QA BUG-006: Enter in a cell editor after the rows were replaced but before the next render
/// stages nothing, because the edit carries its row's identity and that row is gone.
#[gpui::test]
fn edit_after_rows_replaced_in_the_same_frame_never_lands_on_another_row(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    let b = current_grid(cx, &s).read_with(cx, |g, _| g.bounds());
    double_click_at(cx, w, point(b.origin.x + px(120.), b.origin.y + px(40.)));
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "TYPED");
    reverse_rows_without_render(cx, &s);
    cx.simulate_keystrokes(w, "enter");
    assert!(
        !names_in(cx, &s, 1).contains(&Value::Text("TYPED".into())),
        "{:?}",
        names_in(cx, &s, 1)
    );
    assert!(!dirty(cx, &s));
    // ⌘S's flush takes the same path.
    paint(cx, w);
    double_click_at(cx, w, point(b.origin.x + px(120.), b.origin.y + px(40.)));
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "FLUSHED");
    reverse_rows_without_render(cx, &s);
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.flush_grid_edits(window, cx))
    })
    .unwrap();
    assert!(!dirty(cx, &s));
}
/// QA BUG-006: delete and duplicate act on the rows the user selected, by identity; when those
/// rows were replaced before the keystroke, nothing happens to whatever rows are there now.
#[gpui::test]
fn row_actions_after_rows_replaced_in_the_same_frame_do_nothing(cx: &mut TestAppContext) {
    let (w, s) = setup(cx, "07-table-data");
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    click_cell(cx, w, &s, 0);
    focus_grid(cx, w, &s);
    let rows = s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().data.rows.len()
    });
    let k = s.read_with(cx, |s, _| key(s.state.current()));
    let selected = current_grid(cx, &s).read_with(cx, |g, _| vec![g.model().row_ref(0)]);
    reverse_rows_without_render(cx, &s);
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.grid_event(k, GridEvent::DeleteRows(selected.clone()), window, cx);
            s.grid_event(k, GridEvent::DuplicateRows(selected.clone()), window, cx);
            s.grid_event(
                k,
                GridEvent::Menu {
                    id: "null".into(),
                    cell: Cell { row: 0, col: 1 },
                    key: selected[0].key.clone(),
                },
                window,
                cx,
            );
        })
    })
    .unwrap();
    let t = s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().data.rows.clone()
    });
    assert_eq!(t.len(), rows);
    assert!(t.iter().all(|r| !r.deleted && r.orig.is_some()));
    assert!(!dirty(cx, &s));
    // The same actions on the rows as shown now do act.
    paint(cx, w);
    let selected = current_grid(cx, &s).read_with(cx, |g, _| vec![g.model().row_ref(0)]);
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.grid_event(k, GridEvent::DeleteRows(selected), window, cx)
        })
    })
    .unwrap();
    assert!(s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().unwrap().data.rows[0].deleted
    }));
}
/// QA BUG-007: after ⌘S the selected row is selected again once the table reloads (by primary
/// key, or by position without one), so the inspector, Enter and the arrow keys keep working.
#[gpui::test]
fn save_keeps_the_selection_on_live_tables(cx: &mut TestAppContext) {
    for (driver, ddl) in [
        (
            Driver::Mysql,
            "CREATE TABLE telecat_ui_save (id int PRIMARY KEY, name varchar(40))",
        ),
        (
            Driver::Postgres,
            "CREATE TABLE telecat_ui_save (n int, name text)",
        ),
    ] {
        let (w, s) = live_table(
            cx,
            driver,
            &[
                "DROP TABLE IF EXISTS telecat_ui_save",
                ddl,
                "INSERT INTO telecat_ui_save VALUES (1,'alpha'),(2,'bravo'),(3,'charlie')",
            ],
            "telecat_ui_save",
        );
        install_shortcuts(cx, w, &s);
        click_cell(cx, w, &s, 1);
        focus_inspector(cx, w, &s, 1);
        cx.simulate_keystrokes(w, "cmd-a");
        cx.simulate_input(w, "ธนาคาร");
        cx.simulate_keystrokes(w, "enter");
        assert!(dirty(cx, &s));
        cx.update_window(w, |_, window, cx| {
            s.update(cx, |s, cx| s.request_commit(window, cx))
        })
        .unwrap();
        for _ in 0..400 {
            cx.executor().allow_parking();
            cx.run_until_parked();
            paint(cx, w);
            let done = s.read_with(cx, |s, _| {
                !s.saving
                    && s.state
                        .current()
                        .table
                        .as_ref()
                        .is_some_and(|t| t.data.loaded)
                    && !s
                        .state
                        .current()
                        .load(s.state.current().slot())
                        .is_loading()
            });
            if done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!dirty(cx, &s), "{driver:?}: saved");
        // PostgreSQL returns the updated row last, MySQL keeps it in key order.
        let saved = names_in(cx, &s, 1)
            .iter()
            .position(|v| *v == Value::Text("ธนาคาร".into()))
            .expect("saved");
        let anchor = current_grid(cx, &s).read_with(cx, |g, _| g.model().selection.anchor);
        assert_eq!(
            anchor,
            Some(saved),
            "{driver:?}: the saved row is still selected"
        );
        assert_eq!(
            s.read_with(cx, |s, _| s.state.current().grid.selected_row),
            Some(saved)
        );
        assert_eq!(
            inspector_value(cx, &s, 1),
            "ธนาคาร",
            "{driver:?}: inspector shows it"
        );
        focus_grid(cx, w, &s);
        cx.simulate_keystrokes(w, "up");
        paint(cx, w);
        let anchor = current_grid(cx, &s).read_with(cx, |g, _| g.model().selection.anchor);
        assert_eq!(anchor, Some(saved - 1), "{driver:?}: arrows work");
        cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let session = telecat_db::Session::connect_test(&live_config(driver), "secret")
                .await
                .unwrap();
            session
                .query("DROP TABLE IF EXISTS telecat_ui_save")
                .await
                .unwrap();
        });
    }
}
fn save_and_wait(cx: &mut TestAppContext, w: AnyWindowHandle, s: &Entity<Workspace>) {
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.request_commit(window, cx))
    })
    .unwrap();
    for _ in 0..400 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        let done = s.read_with(cx, |s, _| {
            !s.saving
                && s.state
                    .current()
                    .table
                    .as_ref()
                    .is_some_and(|t| t.data.loaded)
                && !s
                    .state
                    .current()
                    .load(s.state.current().slot())
                    .is_loading()
        });
        if done {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("save did not finish");
}
fn drop_mysql_table(name: &str) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let session = telecat_db::Session::connect_test(&live_config(Driver::Mysql), "secret")
            .await
            .unwrap();
        session
            .query(&format!("DROP TABLE IF EXISTS {name}"))
            .await
            .unwrap();
    });
}
/// After a save, a selected row is found again only by primary key or by an exact, unique match
/// of its values, and the inspector shows nothing otherwise.
fn assert_nothing_selected(cx: &mut TestAppContext, s: &Entity<Workspace>, what: &str) {
    let anchor = current_grid(cx, s).read_with(cx, |g, _| g.model().selection.anchor);
    assert_eq!(anchor, None, "{what}: grid selection");
    assert_eq!(
        s.read_with(cx, |s, _| s.state.current().grid.selected_row),
        None,
        "{what}"
    );
    assert!(
        s.read_with(cx, |s, _| s.inspector.is_none()),
        "{what}: inspector is empty"
    );
}
/// QA BUG-008: a no-PK row edited out of the filter and saved is not replaced, in the selection,
/// by the row that took its position; the user's next edit can't land on that row.
#[gpui::test]
fn save_never_selects_another_row_in_its_place(cx: &mut TestAppContext) {
    let (w, s) = live_table(
        cx,
        Driver::Mysql,
        &[
            "DROP TABLE IF EXISTS telecat_ui_filter",
            "CREATE TABLE telecat_ui_filter (n int, name varchar(40))",
            "INSERT INTO telecat_ui_filter VALUES (1,'n1'),(2,'n2'),(3,'n3'),(4,'n4'),(5,'n5')",
        ],
        "telecat_ui_filter",
    );
    install_shortcuts(cx, w, &s);
    s.update(cx, |s, cx| {
        let t = s.state.current_mut().table.as_mut().unwrap();
        let filter = telecat_core::types::Filter {
            enabled: true,
            column: "name".into(),
            op: telecat_core::types::FilterOp::Contains,
            value: "n".into(),
            value2: None,
        };
        t.filters = vec![filter.clone()];
        t.applied = vec![filter];
        s.refresh_tab(cx);
    });
    save_and_wait(cx, w, &s);
    click_cell(cx, w, &s, 1);
    assert_eq!(inspector_value(cx, &s, 1), "n2");
    focus_inspector(cx, w, &s, 1);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "zzz");
    cx.simulate_keystrokes(w, "enter");
    save_and_wait(cx, w, &s);
    assert_eq!(
        names_in(cx, &s, 1),
        ["n1", "n3", "n4", "n5"].map(|n| Value::Text(n.into())),
        "the saved row left the filter"
    );
    assert_nothing_selected(cx, &s, "row left the filter");
    drop_mysql_table("telecat_ui_filter");
}
/// A no-PK row the server changes while saving it (a trigger, an ON UPDATE column) can't be
/// found again by its values, so nothing is selected rather than a guess.
#[gpui::test]
fn save_does_not_guess_when_the_server_changed_the_row(cx: &mut TestAppContext) {
    let (w, s) = live_table(
        cx,
        Driver::Mysql,
        &[
            "DROP TABLE IF EXISTS telecat_ui_trig",
            "CREATE TABLE telecat_ui_trig (n int, name varchar(40), u int)",
            "CREATE TRIGGER telecat_ui_trig_bu BEFORE UPDATE ON telecat_ui_trig FOR EACH ROW SET NEW.u = OLD.u + 1",
            "INSERT INTO telecat_ui_trig VALUES (1,'a',0),(2,'b',0),(3,'c',0)",
        ],
        "telecat_ui_trig",
    );
    install_shortcuts(cx, w, &s);
    click_cell(cx, w, &s, 1);
    focus_inspector(cx, w, &s, 1);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "bb");
    cx.simulate_keystrokes(w, "enter");
    save_and_wait(cx, w, &s);
    let u = s.read_with(cx, |s, _| {
        s.state
            .current()
            .table
            .as_ref()
            .unwrap()
            .data
            .rows
            .iter()
            .map(|r| r.cur[2].clone())
            .collect::<Vec<_>>()
    });
    assert!(
        u.contains(&Value::Text("1".into())),
        "the trigger changed the saved row: {u:?}"
    );
    assert_nothing_selected(cx, &s, "server-changed row");
    // An unchanged no-PK row is still found by its exact values.
    click_cell(cx, w, &s, 0);
    focus_inspector(cx, w, &s, 0);
    focus_grid(cx, w, &s);
    s.update(cx, |s, cx| s.refresh_tab(cx));
    save_and_wait(cx, w, &s);
    assert_eq!(
        s.read_with(cx, |s, _| s.state.current().grid.selected_row),
        Some(0)
    );
    drop_mysql_table("telecat_ui_trig");
}
/// A connected workspace on its SQL Query tab, with `ddl` run first.
fn live_query_tab(
    cx: &mut TestAppContext,
    driver: Driver,
    ddl: &[&str],
) -> (AnyWindowHandle, Entity<Workspace>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let session = runtime
        .block_on(telecat_db::Session::connect_test(
            &live_config(driver),
            "secret",
        ))
        .expect("test database");
    for sql in ddl {
        runtime.block_on(session.query(sql)).expect(sql);
    }
    let (w, s) = setup_with(cx, "", runtime, Some(Arc::new(session)));
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    (w, s)
}
/// Runs `sql` in the query tab and waits until its result's editability is known.
fn run_query(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    s: &Entity<Workspace>,
    sql: &str,
    all: bool,
) {
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.replace_editor_text(sql.into(), window, cx);
            s.run(all, window, cx);
        })
    })
    .unwrap();
    for _ in 0..400 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        if s.read_with(cx, |s, _| {
            !s.busy && s.state.current().result_edit.is_some()
        }) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("query did not finish: {sql}");
}
fn badge(cx: &mut TestAppContext, s: &Entity<Workspace>) -> String {
    s.read_with(cx, |s, _| {
        s.state.current().result_edit.as_ref().unwrap().plan.badge()
    })
}
fn hint(cx: &mut TestAppContext, s: &Entity<Workspace>) -> Option<String> {
    s.read_with(cx, |s, _| s.hint.as_ref().map(|h| h.0.to_string()))
}
fn result_cells(cx: &mut TestAppContext, s: &Entity<Workspace>, col: usize) -> Vec<Value> {
    s.read_with(cx, |s, _| {
        s.state
            .current()
            .result_edit
            .as_ref()
            .unwrap()
            .data
            .rows
            .iter()
            .map(|r| r.cur[col].clone())
            .collect()
    })
}
fn db_rows(driver: Driver, sql: &str) -> Vec<Vec<Option<String>>> {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let s = telecat_db::Session::connect_test(&live_config(driver), "secret")
            .await
            .unwrap();
        s.query(sql).await.unwrap().rows
    })
}
fn cell_at(
    cx: &mut TestAppContext,
    s: &Entity<Workspace>,
    row: usize,
    col: usize,
) -> gpui_kit::Point<gpui_kit::Pixels> {
    let g = current_grid(cx, s);
    g.read_with(cx, |g, _| {
        let r = g.model().cell_rect(Cell { row, col });
        let b = g.bounds();
        point(b.origin.x + px(r.x + 8.), b.origin.y + px(r.y + 10.))
    })
}
/// Feature: a single table's SELECT result with its key is edited like a table: double-click,
/// the inspector, Set NULL, Delete, ⌘S; the saved values stay on screen with the selection.
#[gpui::test]
fn query_result_edits_save_exactly_the_rows_shown(cx: &mut TestAppContext) {
    for driver in [Driver::Mysql, Driver::Postgres] {
        let (w, s) = live_query_tab(
            cx,
            driver,
            &[
                "DROP TABLE IF EXISTS telecat_ui_q",
                "CREATE TABLE telecat_ui_q (id int PRIMARY KEY, name varchar(40))",
                "INSERT INTO telecat_ui_q VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')",
            ],
        );
        run_query(
            cx,
            w,
            &s,
            "SELECT id, name FROM telecat_ui_q ORDER BY id",
            false,
        );
        assert_eq!(badge(cx, &s), "Editable · telecat_ui_q", "{driver:?}");
        // Double-click a cell: the inline editor opens.
        let at = cell_at(cx, &s, 0, 1);
        double_click_at(cx, w, at);
        cx.simulate_keystrokes(w, "cmd-a");
        cx.simulate_input(w, "ธนาคาร");
        cx.simulate_keystrokes(w, "enter");
        // The inspector edits row 2; Set NULL on row 3.
        click_cell(cx, w, &s, 1);
        let at = cell_at(cx, &s, 1, 1);
        let g = current_grid(cx, &s);
        let _ = g;
        let _ = at;
        focus_inspector(cx, w, &s, 1);
        cx.simulate_keystrokes(w, "cmd-a");
        cx.simulate_input(w, "two");
        cx.simulate_keystrokes(w, "enter");
        click_cell(cx, w, &s, 2);
        s.update(cx, |s, cx| s.set_inspector_null(1, cx));
        // Delete row 4.
        click_cell(cx, w, &s, 3);
        focus_grid(cx, w, &s);
        cx.simulate_keystrokes(w, "backspace");
        paint(cx, w);
        assert_eq!(
            result_cells(cx, &s, 1),
            [
                Value::Text("ธนาคาร".into()),
                Value::Text("two".into()),
                Value::Null,
                Value::Text("d".into())
            ],
            "{driver:?}"
        );
        assert!(s.read_with(cx, |s, _| {
            s.state.current().result_edit.as_ref().unwrap().data.rows[3].deleted
        }));
        let preview = s.read_with(cx, |s, _| s.preview_text().unwrap());
        assert_eq!(preview.matches("UPDATE").count(), 3, "{preview}");
        assert_eq!(preview.matches("DELETE").count(), 1, "{preview}");
        // The SQL can't change under staged edits.
        let locked = s.read_with(cx, |s, cx| {
            !s.components.editors[&s.state.active]
                .read(cx)
                .input_state()
                .read(cx)
                .is_editable()
        });
        assert!(locked, "{driver:?}: SQL locked");
        save_and_wait_query(cx, w, &s);
        assert_eq!(
            db_rows(driver, "SELECT id, name FROM telecat_ui_q ORDER BY id"),
            [
                vec![Some("1".to_owned()), Some("ธนาคาร".to_owned())],
                vec![Some("2".into()), Some("two".into())],
                vec![Some("3".into()), None],
            ],
            "{driver:?}"
        );
        // Shown in place, nothing left staged, the selection kept.
        assert_eq!(
            result_cells(cx, &s, 1),
            [
                Value::Text("ธนาคาร".into()),
                Value::Text("two".into()),
                Value::Null
            ],
            "{driver:?}"
        );
        assert!(!dirty(cx, &s));
        drop_table(driver, "telecat_ui_q");
        cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
    }
}
fn save_and_wait_query(cx: &mut TestAppContext, w: AnyWindowHandle, s: &Entity<Workspace>) {
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.request_commit(window, cx))
    })
    .unwrap();
    for _ in 0..400 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        if s.read_with(cx, |s, _| !s.saving) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("save did not finish");
}
fn drop_table(driver: Driver, name: &str) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let s = telecat_db::Session::connect_test(&live_config(driver), "secret")
            .await
            .unwrap();
        s.query(&format!("DROP TABLE IF EXISTS {name}"))
            .await
            .unwrap();
    });
}
/// Read-only results say why on double-click instead of doing nothing; Delete is refused with
/// its reason; a run of several statements is read-only.
#[gpui::test]
fn read_only_query_results_explain_why(cx: &mut TestAppContext) {
    let (w, s) = live_query_tab(
        cx,
        Driver::Mysql,
        &[
            "DROP TABLE IF EXISTS telecat_ui_qa",
            "DROP TABLE IF EXISTS telecat_ui_qb",
            "CREATE TABLE telecat_ui_qa (id int PRIMARY KEY, b_id int, note varchar(20))",
            "CREATE TABLE telecat_ui_qb (id int PRIMARY KEY, name varchar(20))",
            "INSERT INTO telecat_ui_qa VALUES (1, 1, 'x')",
            "INSERT INTO telecat_ui_qb VALUES (1, 'y')",
        ],
    );
    run_query(cx, w, &s, "SELECT name FROM telecat_ui_qb", false);
    assert_eq!(
        badge(cx, &s),
        "Read-only: telecat_ui_qb's key column id is not in the result"
    );
    let at = cell_at(cx, &s, 0, 0);
    double_click_at(cx, w, at);
    assert_eq!(grid_editor(cx, w, &s).0, None);
    assert_eq!(
        hint(cx, &s).as_deref(),
        Some("Read-only: telecat_ui_qb's key column id is not in the result")
    );
    // A JOIN: the keyed side edits, the other side and Delete explain themselves.
    run_query(
        cx,
        w,
        &s,
        "SELECT a.id, a.note, b.name FROM telecat_ui_qa a JOIN telecat_ui_qb b ON b.id = a.b_id",
        false,
    );
    assert_eq!(badge(cx, &s), "Editable · telecat_ui_qa");
    let at = cell_at(cx, &s, 0, 2);
    double_click_at(cx, w, at);
    assert_eq!(grid_editor(cx, w, &s).0, None);
    assert_eq!(
        hint(cx, &s).as_deref(),
        Some("Read-only: telecat_ui_qb's key column id is not in the result")
    );
    let at = cell_at(cx, &s, 0, 1);
    double_click_at(cx, w, at);
    assert_eq!(grid_editor(cx, w, &s).0, Some(Cell { row: 0, col: 1 }));
    cx.simulate_keystrokes(w, "escape");
    focus_grid(cx, w, &s);
    cx.simulate_keystrokes(w, "backspace");
    assert!(!dirty(cx, &s));
    // Several statements.
    run_query(
        cx,
        w,
        &s,
        "SELECT id, name FROM telecat_ui_qb; SELECT id, note FROM telecat_ui_qa",
        true,
    );
    assert_eq!(badge(cx, &s), "Read-only: the run had several statements");
    for t in ["telecat_ui_qa", "telecat_ui_qb"] {
        drop_table(Driver::Mysql, t);
    }
}
/// Staged result edits: re-running asks first, and an edit that lands after the result was
/// replaced (same frame, before a render) never reaches another row.
#[gpui::test]
fn staged_result_edits_survive_nothing_unexpected(cx: &mut TestAppContext) {
    let (w, s) = live_query_tab(
        cx,
        Driver::Postgres,
        &[
            "DROP TABLE IF EXISTS telecat_ui_qr",
            "CREATE TABLE telecat_ui_qr (id int PRIMARY KEY, name text)",
            "INSERT INTO telecat_ui_qr VALUES (1, 'a'), (2, 'b')",
        ],
    );
    run_query(
        cx,
        w,
        &s,
        "SELECT id, name FROM telecat_ui_qr ORDER BY id",
        false,
    );
    // Same-frame race: the editor is open on row 1 when the result is replaced.
    let at = cell_at(cx, &s, 0, 1);
    double_click_at(cx, w, at);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "TYPED");
    s.update(cx, |s, _| {
        let t = s.state.current_mut();
        let mut result = t.grid.result.clone();
        result.rows.reverse();
        // A new run's result: a new grid revision, rows with new identities.
        t.grid.revision += 1;
        let revision = t.grid.revision;
        let plan = t.result_edit.as_ref().unwrap().plan.clone();
        t.grid.result = result;
        t.result_edit = None;
        t.attach_result_plan(revision, plan);
    });
    cx.simulate_keystrokes(w, "enter");
    assert!(
        !dirty(cx, &s),
        "the edit's row is gone, so nothing is staged"
    );
    // Re-running with staged edits asks first.
    paint(cx, w);
    let at = cell_at(cx, &s, 0, 1);
    double_click_at(cx, w, at);
    cx.simulate_keystrokes(w, "cmd-a");
    cx.simulate_input(w, "staged");
    cx.simulate_keystrokes(w, "enter");
    assert!(dirty(cx, &s));
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.run(false, window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(dirty(cx, &s), "Cancel keeps the staged edit");
    // Esc discards.
    focus_grid(cx, w, &s);
    cx.simulate_keystrokes(w, "escape");
    assert!(!dirty(cx, &s));
    assert_eq!(
        db_rows(
            Driver::Postgres,
            "SELECT name FROM telecat_ui_qr ORDER BY id"
        ),
        [[Some("a".to_owned())], [Some("b".to_owned())]]
    );
    drop_table(Driver::Postgres, "telecat_ui_qr");
}
fn run_and_wait(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    s: &Entity<Workspace>,
    sql: &str,
    all: bool,
) {
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.replace_editor_text(sql.into(), window, cx);
            s.run(all, window, cx);
        })
    })
    .unwrap();
    for _ in 0..400 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        if s.read_with(cx, |s, _| !s.busy && !s.state.current().results.is_empty()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("query did not finish: {sql}");
}
/// Errors say where they are in the editor's text, across statements, CRLF and Thai; the
/// Messages text is selectable and copyable; "Go to" puts the caret on the error.
#[gpui::test]
fn query_errors_point_to_line_and_column(cx: &mut TestAppContext) {
    for (driver, sql, line, column) in [
        (
            Driver::Postgres,
            "SELECT 1;\r\n-- ธนาคาร 😺\r\nSELECT 'ชื่อ' AS x\r\n  FRM t;",
            4,
            3,
        ),
        (
            Driver::Mysql,
            "SELECT 1;\n-- ธนาคาร 😺\nSELECT\n  'ไทย',\n  FROM t",
            5,
            3,
        ),
    ] {
        let (w, s) = live_query_tab(cx, driver, &[]);
        run_and_wait(cx, w, &s, sql, true);
        let at = s.read_with(cx, |s, _| s.state.current().error_locations.clone());
        let at = at.into_iter().flatten().next().expect("a located error");
        assert_eq!((at.line, at.column), (line, column), "{driver:?}");
        let text = s.read_with(cx, |s, _| s.state.current().messages_text());
        assert!(
            text.contains(&format!("Error at line {line}, column {column}: ")),
            "{driver:?}: {text}"
        );
        assert!(text.contains("^"), "{driver:?}: caret line");
        // The Messages panel: read-only, ⌘A selects everything and ⌘C copies it.
        s.update(cx, |s, cx| {
            s.state.current_mut().message = true;
            cx.notify();
        });
        paint(cx, w);
        let field = s.read_with(cx, |s, _| s.messages[&s.state.active].1.clone());
        assert!(!field.read_with(cx, |f, _| f.is_editable()));
        cx.update_window(w, |_, window, cx| {
            field.update(cx, |f, cx| f.focus(window, cx))
        })
        .unwrap();
        cx.simulate_keystrokes(w, "cmd-a cmd-c");
        let copied = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .unwrap_or_default();
        assert_eq!(copied, text, "{driver:?}");
        cx.simulate_input(w, "typing does nothing");
        assert_eq!(field.read_with(cx, |f, _| f.value().to_string()), text);
        // Jump to the error.
        cx.update_window(w, |_, window, cx| {
            s.update(cx, |s, cx| s.jump_to_location(at, window, cx))
        })
        .unwrap();
        let caret = s.read_with(cx, |s, cx| {
            s.components.editors[&s.state.active]
                .read(cx)
                .selections(cx)[0]
                .clone()
        });
        assert_eq!(caret.anchor, at.utf16, "{driver:?}");
        assert_eq!(
            caret.head,
            at.utf16 + 1,
            "{driver:?}: the character is selected"
        );
        // After the SQL changes, the old location could name another character: no jump.
        cx.update_window(w, |_, window, cx| {
            s.update(cx, |s, cx| {
                s.replace_editor_text(format!("-- edited\n{sql}"), window, cx);
                s.jump_to_location(at, window, cx)
            })
        })
        .unwrap();
        let caret = s.read_with(cx, |s, cx| {
            s.components.editors[&s.state.active]
                .read(cx)
                .selections(cx)[0]
                .clone()
        });
        assert_ne!(
            caret.anchor, at.utf16,
            "{driver:?}: stale location not used"
        );
        assert!(hint(cx, &s).is_some_and(|h| h.contains("changed since it ran")));
        cx.update_window(w, |_, w, _| w.remove_window()).unwrap();
    }
}
/// A connected workspace on a fresh throwaway database, dropped by the returned guard.
struct ScratchDb {
    driver: Driver,
    name: String,
}
impl Drop for ScratchDb {
    fn drop(&mut self) {
        let (driver, name) = (self.driver, self.name.clone());
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                if let Ok(s) = telecat_db::Session::connect_test(&live_config(driver), "secret").await {
                    if driver == Driver::Postgres {
                        let _ = s
                            .query(&format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{name}' AND pid <> pg_backend_pid()"))
                            .await;
                    }
                    let _ = s.query(&format!("DROP DATABASE IF EXISTS {name}")).await;
                }
            })
        })
        .join()
        .unwrap();
    }
}
fn scratch_workspace(
    cx: &mut TestAppContext,
    driver: Driver,
    tag: &str,
) -> (AnyWindowHandle, Entity<Workspace>, ScratchDb) {
    let name = format!("telecat_import_ui_{tag}_{}", std::process::id());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut config = live_config(driver);
    let session = runtime.block_on(async {
        let admin = telecat_db::Session::connect_test(&config, "secret")
            .await
            .unwrap();
        admin
            .query(&format!("DROP DATABASE IF EXISTS {name}"))
            .await
            .unwrap();
        admin
            .query(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        config.database = name.clone();
        telecat_db::Session::connect_test(&config, "secret")
            .await
            .unwrap()
    });
    let guard = ScratchDb { driver, name };
    let (w, s) = setup_with(cx, "", runtime, Some(Arc::new(session)));
    install_shortcuts(cx, w, &s);
    paint(cx, w);
    (w, s, guard)
}
fn wait_for(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    s: &Entity<Workspace>,
    done: impl Fn(&Workspace) -> bool,
) {
    for _ in 0..600 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        paint(cx, w);
        if s.read_with(cx, |s, _| done(s)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out");
}
/// Feature: Import ▸ From SQL Dump… into an empty database: the confirmation shows what the dump
/// does, Safe modes ask as for dangerous SQL, the import runs, the sidebar and History update.
#[gpui::test]
fn import_sql_dump_into_an_empty_database(cx: &mut TestAppContext) {
    use super::super::Sheet;
    use super::super::import::{ImportStage, items_menu_items};
    let (w, s, db) = scratch_workspace(cx, Driver::Mysql, "m");
    wait_for(cx, w, &s, |s| !s.state.objects_load.is_loading());
    assert!(
        s.read_with(cx, |s, _| s.state.objects.is_empty()),
        "a brand-new database"
    );
    // The empty-area menu: only items that work.
    let menu = items_menu_items().to_string();
    assert!(
        menu.contains("From SQL Dump") && menu.contains("\"Table\""),
        "{menu}"
    );
    let dir = std::env::temp_dir().join(&db.name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shop.sql");
    std::fs::write(
        &path,
        "DROP TABLE IF EXISTS `item`;\nCREATE TABLE `item` (`id` int PRIMARY KEY, `name` varchar(20));\nINSERT INTO `item` VALUES (1,'ธนาคาร'),(2,'b');\nUSE zixma;\n",
    )
    .unwrap();
    s.update(cx, |s, cx| s.scan_dump_file(path.clone(), cx));
    wait_for(cx, w, &s, |s| {
        matches!(s.sheet, Some(Sheet::Import(ImportStage::Confirm(_))))
    });
    let (statements, blocked, can_run) = s.read_with(cx, |s, _| match &s.sheet {
        Some(Sheet::Import(ImportStage::Confirm(r))) => (
            r.scan.statements,
            r.scan.blocked.clone(),
            r.blocked_reason(Driver::Mysql).is_none(),
        ),
        _ => unreachable!(),
    });
    assert_eq!(statements, 4);
    assert_eq!(blocked, [(4, "uses another database: ZIXMA".to_owned())]);
    assert!(!can_run, "blocked until allowed");
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.confirm_import(window, cx))
    })
    .unwrap();
    assert!(
        s.read_with(cx, |s, _| matches!(
            s.sheet,
            Some(Sheet::Import(ImportStage::Confirm(_)))
        )),
        "Import does nothing while blocked"
    );
    // Without the USE line it runs; Safe mode asks for the password first.
    std::fs::write(
        &path,
        "DROP TABLE IF EXISTS `item`;\nCREATE TABLE `item` (`id` int PRIMARY KEY, `name` varchar(20));\nINSERT INTO `item` VALUES (1,'ธนาคาร'),(2,'b');\n",
    )
    .unwrap();
    s.update(cx, |s, cx| {
        s.state.connection.safe_mode = SafeMode::Safe1;
        s.scan_dump_file(path.clone(), cx)
    });
    wait_for(cx, w, &s, |s| {
        matches!(s.sheet, Some(Sheet::Import(ImportStage::Confirm(_))))
    });
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.confirm_import(window, cx))
    })
    .unwrap();
    assert!(s.read_with(cx, |s, _| matches!(
        s.sheet,
        Some(Sheet::Gate {
            danger: Some(_),
            ..
        })
    )));
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| s.approve_gate(window, cx))
    })
    .unwrap();
    assert!(s.read_with(cx, |s, _| matches!(
        s.sheet,
        Some(Sheet::Gate { password: true, .. })
    )));
    cx.update_window(w, |_, window, cx| {
        s.update(cx, |s, cx| {
            s.gate_password
                .update(cx, |i, cx| i.set_value("secret", window, cx));
            s.approve_gate(window, cx)
        })
    })
    .unwrap();
    wait_for(cx, w, &s, |s| {
        matches!(s.sheet, Some(Sheet::Import(ImportStage::Done { .. })))
    });
    let ok = s.read_with(cx, |s, _| match &s.sheet {
        Some(Sheet::Import(ImportStage::Done { result, .. })) => {
            result.clone().map(|r| r.statements)
        }
        _ => unreachable!(),
    });
    assert_eq!(ok.map_err(|f| f.to_string()), Ok(3));
    wait_for(cx, w, &s, |s| {
        s.state.objects.iter().any(|o| o.table.name == "item")
    });
    assert!(s.read_with(cx, |s, _| {
        s.history
            .iter()
            .any(|h| h.sql.contains("Imported shop.sql"))
    }));
    // New ▸ Table works here too.
    s.update(cx, |s, cx| s.items_menu_action("new-table", cx));
    assert!(s.read_with(cx, |s, _| {
        s.state.current().table.as_ref().is_some_and(|t| t.is_new)
    }));
    std::fs::remove_dir_all(dir).unwrap();
}
