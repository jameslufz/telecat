//! Count the submitted scene, including stale/cached views, rather than just the model.
use super::{Driver, Part, Workspace, key};
use gpui::{
    AnyWindowHandle, AppContext, Entity, TestAppContext, VisualTestContext, point, px, rgb, rgba,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use telecat_app::workspace_model::WorkspaceState;

fn paint_pills(cx: &mut TestAppContext, window: AnyWindowHandle) -> usize {
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window
            .painted_quads()
            .iter()
            .filter(|q| {
                q.background == rgba(0x2a2a2aee).into() && q.border_color == rgb(0x3a3a3a).into()
            })
            .count()
    })
    .unwrap()
}

#[gpui::test]
fn empty_table_scenarios_have_one_pill(cx: &mut TestAppContext) {
    for scenario in ["58-empty-table", "59-pg-empty-table"] {
        let (w, s) = super::tests::setup(cx, scenario);
        for _ in 0..3 {
            assert_eq!(paint_pills(cx, w), 1, "{scenario}");
        }
        s.read_with(cx, |s, cx| {
            assert_eq!(s.state.tabs.len(), 2);
            assert_eq!(s.state.tabs[0].title, "SQL Query");
            assert_eq!(s.state.current().title, "user_bindings");
            assert_eq!(s.state.current().total, 0);
            let grid = s.components.grids[&key(s.state.current())].view.read(cx);
            assert_eq!(grid.model().columns.len(), 4);
            assert!(grid.model().rows.is_empty());
        });
    }
}

fn wait_for(cx: &mut TestAppContext, s: &Entity<Workspace>, ready: impl Fn(&Workspace) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if s.read_with(cx, |s, _| {
            assert!(
                s.sheet.is_none(),
                "unexpected error sheet during live table loading"
            );
            ready(s)
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "live table loading timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui::test]
fn live_empty_tables_opened_from_sidebar_have_one_pill(cx: &mut TestAppContext) {
    // Real DB workers wake GPUI from Tokio threads; opt out of deterministic scheduling.
    cx.executor().allow_parking();
    for scenario in ["58-empty-table", "59-pg-empty-table"] {
        let (w, s) = super::tests::setup(cx, scenario);
        let config = super::super::fixtures::connection(scenario);
        let sql = if config.driver == Driver::Mysql {
            include_str!("../../../tests/fixtures/empty-table-mysql.sql")
        } else {
            include_str!("../../../tests/fixtures/empty-table-pg.sql")
        };
        let runtime = s.read_with(cx, |s, _| s.services.runtime.clone());
        let connection = config.clone();
        let session = futures::executor::block_on(runtime.spawn(async move {
            let session = telecat_db::Session::connect_test(&connection, "secret")
                .await
                .unwrap();
            // Leave this empty fixture available for manual sidebar reproduction. Never
            // truncate/drop an existing table: an unexpected fixture shape fails below.
            session.query(sql).await.unwrap();
            Arc::new(session)
        }))
        .unwrap();
        cx.update_window(w.into(), |_, window, cx| {
            s.update(cx, |s, cx| {
                s.state = WorkspaceState::new(config);
                s.scenario.clear();
                s.services.scenario = false;
                s.session = Some(session.clone());
                // Keep an empty result mounted in the SQL tab before opening the table.
                s.state
                    .current_mut()
                    .set_results(vec![telecat_core::types::QueryResult {
                        columns: super::super::fixtures::EMPTY_TABLE_COLUMNS
                            .map(str::to_owned)
                            .to_vec(),
                        ..Default::default()
                    }]);
                s.search
                    .update(cx, |input, cx| input.set_value("user_bindings", window, cx));
                s.refresh_objects(cx);
                cx.notify();
            })
        })
        .unwrap();
        wait_for(cx, &s, |s| {
            s.state
                .objects
                .iter()
                .any(|o| o.table.name == "user_bindings")
        });
        assert_eq!(
            paint_pills(cx, w),
            1,
            "empty SQL result before sidebar click"
        );
        let query = s.read_with(cx, |s, _| s.state.active);
        // Actual Items row click: toolbar 54 + segment/search 80 + Tables 24.
        VisualTestContext::from_window(w, cx)
            .simulate_click(point(px(120.), px(170.)), Default::default());
        wait_for(cx, &s, |s| {
            s.state
                .current()
                .table
                .as_ref()
                .is_some_and(|t| t.data.loaded)
        });
        let table = s.read_with(cx, |s, _| {
            let t = s.state.current();
            assert_eq!(t.title, "user_bindings");
            assert_eq!(
                t.grid.result.columns,
                super::super::fixtures::EMPTY_TABLE_COLUMNS
            );
            assert_eq!(
                t.total, 0,
                "fixture must be empty; do not erase existing data"
            );
            assert!(t.grid.result.rows.is_empty());
            assert_eq!(s.state.tabs.len(), 2);
            t.id
        });
        for _ in 0..3 {
            assert_eq!(paint_pills(cx, w), 1, "sidebar-opened table {scenario}");
        }
        for part in [Part::Structure, Part::Index, Part::Data] {
            cx.update_window(w.into(), |_, _, cx| {
                s.update(cx, |s, cx| s.switch_part(part, cx))
            })
            .unwrap();
            wait_for(cx, &s, |s| {
                s.state.current().table.as_ref().unwrap().buffer().loaded
            });
            assert_eq!(
                paint_pills(cx, w),
                usize::from(part != Part::Structure),
                "{scenario} {part:?}"
            );
        }
        for active in [query, table, query, table] {
            cx.update_window(w.into(), |_, _, cx| {
                s.update(cx, |s, cx| {
                    s.state.activate(active);
                    cx.notify();
                })
            })
            .unwrap();
            assert_eq!(paint_pills(cx, w), 1, "tab switch {scenario}");
        }
        cx.update_window(w.into(), |_, _, cx| {
            s.update(cx, |s, cx| {
                assert!(s.state.close_tab(table));
                cx.notify();
            })
        })
        .unwrap();
        assert_eq!(paint_pills(cx, w), 1, "after closing table {scenario}");
        VisualTestContext::from_window(w, cx)
            .simulate_click(point(px(120.), px(170.)), Default::default());
        wait_for(cx, &s, |s| {
            s.state
                .current()
                .table
                .as_ref()
                .is_some_and(|t| t.data.loaded)
        });
        assert_eq!(paint_pills(cx, w), 1, "reopened table {scenario}");
        // Close all DB workers while their runtime is still alive.
        cx.update_window(w.into(), |_, window, cx| {
            s.update(cx, |s, _| s.session = None);
            window.remove_window();
        })
        .unwrap();
        futures::executor::block_on(runtime.spawn(async move { session.close().await })).unwrap();
    }
}
