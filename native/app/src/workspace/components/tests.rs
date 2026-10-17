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
    cx.update(|cx| {
        gpui_kit::init(cx);
        telecat_editor::init(cx);
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../.cache/integration-ui-profiles")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let services = Services {
            profile: Arc::new(telecat_app::store::Profile::new(
                root,
                Arc::new(telecat_app::credentials::MemoryKeychain::default()),
            )),
            runtime: Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .unwrap()
                    .into(),
            ),
            model: cx.new(|_| crate::Model {
                import_error: None,
                snapshot: StoreSnapshot {
                    connections: vec![],
                    groups: vec![],
                },
            }),
            scenario: true,
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
                    Workspace::new(
                        services,
                        None,
                        fixtures::state(&name).connection,
                        name,
                        window,
                        cx,
                    )
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
            s.grid_event(k, GridEvent::DuplicateRows(vec![0]), window, cx);
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
