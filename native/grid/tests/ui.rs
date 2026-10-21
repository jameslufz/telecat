use gpui::{
    AnyWindowHandle, AppContext, Bounds, Context, Entity, Focusable, InputEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, TestAppContext, Window, WindowBounds,
    WindowOptions, point, px, size,
};
use std::{cell::RefCell, rc::Rc};
use telecat_grid::{Cell, Column, DataGrid, GridEvent, GridModel, Hooks};
fn setup(
    cx: &mut TestAppContext,
) -> (
    AnyWindowHandle,
    Entity<DataGrid>,
    Rc<RefCell<Vec<GridEvent>>>,
) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        let events = Rc::new(RefCell::new(vec![]));
        let captured = events.clone();
        let (w, grid) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(848.), px(511.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let mut columns = ["id", "name", "value"].map(Column::new).to_vec();
                for c in &mut columns {
                    c.editable = true;
                }
                let mut model = GridModel::new(
                    columns,
                    (0..100)
                        .map(|i| vec![Some(i.to_string()), Some("ธนาคาร😺".into()), None])
                        .collect(),
                );
                model.options.hooks = Hooks {
                    edit: true,
                    sort: true,
                    paste: true,
                    quick_look: true,
                    ..Default::default()
                };
                let grid = cx.new(|cx| DataGrid::new(model, cx));
                grid.focus_handle(cx).focus(window, cx);
                grid
            },
        )
        .unwrap();
        cx.subscribe(&grid, move |_, event: &GridEvent, _| {
            captured.borrow_mut().push(event.clone())
        })
        .detach();
        (w, grid, events)
    })
}
fn update<R>(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    g: &Entity<DataGrid>,
    f: impl FnOnce(&mut DataGrid, &mut Window, &mut Context<DataGrid>) -> R,
) -> R {
    cx.update_window(w.into(), |_, w, cx| g.update(cx, |g, cx| f(g, w, cx)))
        .unwrap()
}
fn paint(cx: &mut TestAppContext, w: AnyWindowHandle) {
    cx.update_window(w.into(), |_, w, cx| {
        w.draw(cx).clear(cx);
    })
    .unwrap();
}
#[gpui::test]
fn empty_table_has_one_pill_after_redraw_resize_and_reload(cx: &mut TestAppContext) {
    let (w, g, _) = setup(cx);
    let columns = [
        "org_fingerprint",
        "user_fingerprint",
        "account_id",
        "created_at",
    ];
    update(cx, w, &g, |g, _, cx| {
        g.update_model(
            |m| {
                m.replace_data(columns.map(Column::new).to_vec(), Vec::new());
                m.options.empty_text = "No rows".into();
            },
            cx,
        );
    });
    for width in [848., 848., 420., 1100., 848.] {
        cx.update_window(w.into(), |_, window, _| {
            window.resize(size(px(width), px(511.)));
        })
        .unwrap();
        update(cx, w, &g, |g, _, cx| {
            g.update_model(
                |m| {
                    m.scroll_x = 150.;
                    m.scroll_y = 100.;
                },
                cx,
            );
        });
        paint(cx, w);
        assert_eq!(empty_pills(cx, w), 1, "one visible pill at width {width}");
    }
    for rows in [vec![vec![Some("value".into()); 4]], vec![]] {
        let expected = usize::from(rows.is_empty());
        update(cx, w, &g, |g, _, cx| {
            g.update_model(
                |m| {
                    m.replace_data(columns.map(Column::new).to_vec(), rows);
                },
                cx,
            );
        });
        paint(cx, w);
        assert_eq!(empty_pills(cx, w), expected);
    }
}
fn empty_pills(cx: &mut TestAppContext, w: AnyWindowHandle) -> usize {
    cx.update_window(w.into(), |_, window, _| {
        window
            .painted_quads()
            .iter()
            .filter(|q| {
                q.background == gpui::rgba(0x2a2a2aee).into()
                    && q.border_color == gpui::rgb(0x3a3a3a).into()
            })
            .count()
    })
    .unwrap()
}

// AppKit momentum arrives as Moved events after the finger's Ended event in
// GPUI's macOS adapter. Include fractional tails and alternating rubber-band deltas.
fn trackpad_sequence() -> Vec<(gpui::TouchPhase, f32, f32)> {
    use gpui::TouchPhase::*;
    vec![
        (Started, 0., -0.25),
        (Moved, -0.125, -0.25),
        (Moved, -3.5, -6.75),
        (Moved, -12.25, -25.5),
        (Ended, 0., 0.),
        (Moved, -9.5, -18.25),
        (Moved, -4.25, -8.125),
        (Moved, -0.5, -1.25),
        (Moved, -0.0625, -0.125),
        (Ended, 0., 0.),
        (Moved, -10000., -10000.),
        (Moved, -0.125, -0.125),
        (Moved, 0.25, 0.5),
        (Moved, -0.125, -0.25),
        (Cancelled, 0., 0.),
        (Started, 0., 0.),
        (Moved, 10000., 10000.),
        (Ended, 0., 0.),
        (Moved, 0.5, 1.25),
        (Moved, -0.0625, -0.125),
        (Moved, 0.0625, 0.125),
        (Ended, 0., 0.),
    ]
}
fn trackpad(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    phase: gpui::TouchPhase,
    dx: f32,
    dy: f32,
) {
    cx.update_window(w.into(), |_, w, cx| {
        w.dispatch_event(
            gpui::ScrollWheelEvent {
                position: point(px(200.), px(160.)),
                delta: gpui::ScrollDelta::Pixels(point(px(dx), px(dy))),
                touch_phase: phase,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
}
fn grid_paint(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
) -> (
    Vec<Bounds<gpui::ScaledPixels>>,
    Vec<Bounds<gpui::ScaledPixels>>,
) {
    cx.update_window(w.into(), |_, w, _| {
        let quads = w.painted_quads();
        let pills = quads
            .iter()
            .filter(|q| q.background == gpui::rgba(0x2a2a2aee).into())
            .map(|q| q.bounds)
            .collect();
        let stripes = quads
            .iter()
            .filter(|q| {
                q.bounds.size.height == px(26.).scale(w.scale_factor())
                    && q.bounds.size.width > px(800.).scale(w.scale_factor())
                    && (q.background == gpui::rgb(0x1e1e1e).into()
                        || q.background == gpui::rgb(0x232323).into())
            })
            .map(|q| q.bounds)
            .collect();
        (pills, stripes)
    })
    .unwrap()
}

#[gpui::test]
fn short_grids_stay_pinned_through_trackpad_momentum_and_bounce(cx: &mut TestAppContext) {
    let (w, g, _) = setup(cx);
    for scale in [1., 2.] {
        cx.simulate_window_scale_factor_change(w, scale);
        for rows in [0, 3, 6] {
            // Narrow, just fitting (a bogus vertical gutter forces X overflow), wide.
            for widths in [[100., 100., 100.], [100., 100., 599.], [420.; 3]] {
                update(cx, w, &g, |g, _, cx| {
                    g.update_model(
                        |m| {
                            m.replace_data(
                                ["id", "code_id", "expires_at"].map(Column::new).to_vec(),
                                vec![vec![Some("ธนาคาร 😺".into()); 3]; rows],
                            );
                            m.widths = widths.to_vec();
                            m.options.empty_text = "No rows".into();
                            m.scroll_x = 0.;
                            m.scroll_y = 0.;
                        },
                        cx,
                    )
                });
                paint(cx, w);
                let baseline = grid_paint(cx, w);
                assert_eq!(baseline.0.len(), usize::from(rows == 0));
                assert!(!baseline.1.is_empty());
                let max_x = g.read_with(cx, |g, _| (g.model().total_width() - 848.).max(0.));
                let mut previous_x = 0.;
                for (phase, dx, dy) in trackpad_sequence() {
                    trackpad(cx, w, phase, dx, dy);
                    let offsets = g.read_with(cx, |g, _| (g.model().scroll_x, g.model().scroll_y));
                    assert_eq!(
                        offsets.1, 0.,
                        "rows={rows}, widths={widths:?}, {phase:?}, dy={dy}"
                    );
                    assert_eq!(offsets.0, (previous_x - dx).clamp(0., max_x));
                    previous_x = offsets.0;
                    for _ in 0..2 {
                        paint(cx, w);
                        assert_eq!(
                            grid_paint(cx, w),
                            baseline,
                            "pill/stripe movement with rows={rows}, widths={widths:?}"
                        );
                        g.read_with(cx, |g, _| {
                            let m = g.model();
                            assert_eq!(
                                (m.scroll_x, m.scroll_y),
                                offsets,
                                "prepaint changed scroll offset"
                            );
                            assert_eq!(m.layout().width, 848.);
                            assert!(!m.layout().vertical);
                            assert_eq!(m.layout().horizontal, max_x > 0.);
                        });
                    }
                }
            }
        }
    }
}

#[gpui::test]
fn long_grid_momentum_is_monotonic_and_reload_clears_scroll(cx: &mut TestAppContext) {
    let (w, g, _) = setup(cx);
    update(cx, w, &g, |g, _, cx| {
        g.update_model(|m| m.widths = vec![420.; 3], cx)
    });
    paint(cx, w);
    let (max_x, max_y) = g.read_with(cx, |g, _| {
        let l = g.model().layout();
        (l.content_width - l.width, l.content_height - l.height)
    });
    assert!(max_x > 0. && max_y > 0.);
    let mut previous = (0., 0.);
    for (phase, dx, dy) in trackpad_sequence() {
        trackpad(cx, w, phase, dx, dy);
        let expected = (
            (previous.0 - dx).clamp(0., max_x),
            (previous.1 - dy).clamp(0., max_y),
        );
        for _ in 0..2 {
            paint(cx, w);
            assert_eq!(
                g.read_with(cx, |g, _| (g.model().scroll_x, g.model().scroll_y)),
                expected
            );
        }
        previous = expected;
    }
    trackpad(cx, w, gpui::TouchPhase::Moved, -10000., -10000.);
    for rows in [3, 0] {
        update(cx, w, &g, |g, _, cx| {
            g.update_model(
                |m| {
                    m.replace_data(m.columns.clone(), vec![vec![None; 3]; rows]);
                },
                cx,
            )
        });
        for (phase, dx, dy) in trackpad_sequence() {
            trackpad(cx, w, phase, dx, dy);
            paint(cx, w);
            assert_eq!(g.read_with(cx, |g, _| g.model().scroll_y), 0.);
        }
    }
}
fn click(
    cx: &mut TestAppContext,
    w: AnyWindowHandle,
    x: f32,
    y: f32,
    count: usize,
    button: MouseButton,
) {
    cx.update_window(w.into(), |_, w, cx| {
        let position = point(px(x), px(y));
        w.dispatch_event(
            MouseMoveEvent {
                position,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseDownEvent {
                position,
                button,
                click_count: count,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseUpEvent {
                position,
                button,
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
fn real_dispatch_edit_tab_enter_escape_and_clipboard(cx: &mut TestAppContext) {
    let (w, g, events) = setup(cx);
    paint(cx, w);
    click(cx, w, 70., 43., 2, MouseButton::Left);
    assert!(g.read_with(cx, |g, _| g.model().editing.is_some()));
    cx.simulate_input(w.into(), "edited 😺");
    cx.simulate_keystrokes(w.into(), "tab");
    paint(cx, w);
    assert_eq!(
        g.read_with(cx, |g, _| g.model().editing.as_ref().unwrap().cell),
        Cell { row: 0, col: 1 }
    );
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::Edit{cell,value,..} if cell.col==0&&value=="edited 😺"))
    );
    cx.simulate_input(w.into(), "discard");
    cx.simulate_keystrokes(w.into(), "escape");
    paint(cx, w);
    assert!(g.read_with(cx, |g, _| g.model().editing.is_none()));
    assert!(
        !events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::Edit{value,..} if value=="discard"))
    );
    cx.simulate_keystrokes(w.into(), "enter");
    paint(cx, w);
    cx.simulate_input(w.into(), "confirmed");
    cx.simulate_keystrokes(w.into(), "enter");
    paint(cx, w);
    assert!(g.read_with(cx, |g, _| g.model().editing.is_none()));
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::Edit{value,..} if value=="confirmed"))
    );
    cx.simulate_keystrokes(w.into(), "cmd-c");
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "ธนาคาร😺"
    );
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("a\tb\r\n".into())));
    cx.simulate_keystrokes(w.into(), "cmd-v");
    assert!(events.borrow().iter().any(
        |e| matches!(e,GridEvent::Paste(r) if r==&vec![vec!["a".to_string(),"b".to_string()]])
    ));
}
#[gpui::test]
fn real_dispatch_alt_enter_blur_and_resize(cx: &mut TestAppContext) {
    let (w, g, events) = setup(cx);
    paint(cx, w);
    click(cx, w, 70., 43., 2, MouseButton::Left);
    cx.simulate_input(w.into(), "first");
    cx.simulate_keystrokes(w.into(), "alt-enter");
    cx.simulate_input(w.into(), "second");
    paint(cx, w);
    assert_eq!(
        g.read_with(cx, |g, _| g.model().editing.as_ref().unwrap().value.clone()),
        "first\nsecond"
    );
    click(cx, w, 600., 180., 1, MouseButton::Left);
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::Edit{value,..} if value=="first\nsecond"))
    );
    let right = g.read_with(cx, |g, _| g.model().number_width() + g.model().widths[0]);
    cx.update_window(w.into(), |_, w, cx| {
        w.dispatch_event(
            MouseDownEvent {
                position: point(px(right - 1.), px(12.)),
                button: MouseButton::Left,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseMoveEvent {
                position: point(px(right + 70.), px(600.)),
                pressed_button: Some(MouseButton::Left),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            MouseUpEvent {
                position: point(px(right + 70.), px(600.)),
                button: MouseButton::Left,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    paint(cx, w);
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::Resize{width,..} if *width>100.))
    );
    assert!(
        !events
            .borrow()
            .iter()
            .any(|e| matches!(e, GridEvent::Sort(_)))
    );
}
#[gpui::test]
fn rendering_large_scroll_stays_bounded_and_emits_context_hooks(cx: &mut TestAppContext) {
    let (w, g, events) = setup(cx);
    paint(cx, w);
    click(cx, w, 70., 43., 1, MouseButton::Right);
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::ContextMenu{cell,..} if cell.row==0&&cell.col==0))
    );
    click(cx, w, 70., 43., 1, MouseButton::Middle);
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e, GridEvent::QuickLook(Cell { row: 0, col: 0 })))
    );
    update(cx, w, &g, |g, _, cx| {
        g.update_model(
            |m| {
                m.options.hooks.popup = true;
                m.columns[0].chevron = true;
                m.selection.rows = [0].into();
                m.selection.anchor = Some(2);
            },
            cx,
        )
    });
    paint(cx, w);
    let popup_x = g.read_with(cx, |g, _| {
        g.model().number_width() + g.model().widths[0] - 5.
    });
    click(cx, w, popup_x, 43., 1, MouseButton::Left);
    assert_eq!(g.read_with(cx, |g, _| g.model().selection.anchor), Some(0));
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::CellPopup{cell,..} if cell.row==0&&cell.col==0))
    );
    update(cx, w, &g, |g, _, cx| {
        g.update_model(
            |m| {
                m.columns = (0..30).map(|i| Column::new(format!("col_{i}"))).collect();
                m.widths = vec![120.; 30];
                m.rows = (0..100000)
                    .map(|_| vec![Some("ธนาคาร😺".into()); 30])
                    .collect();
                m.scroll_y = 1500000.;
            },
            cx,
        )
    });
    paint(cx, w);
    g.read_with(cx, |g, _| {
        assert!(g.frame_stats.last_cells <= 160);
        assert_eq!(g.model().columns.len(), 30);
        assert_eq!(g.model().rows.len(), 100000);
        assert!(g.model().scroll_y > 1000000.);
        assert_eq!(g.model().viewport.width, 848.);
    });
}
#[gpui::test]
fn cell_input_ime_unicode_and_native_edit_menu(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    let (w, g, events) = setup(cx);
    paint(cx, w);
    click(cx, w, 70., 43., 2, MouseButton::Left);
    update(cx, w, &g, |g, w, cx| {
        g.editor_state().unwrap().update(cx, |s, cx| {
            s.replace_and_mark_text_in_range(Some(0..1), "น้ำ😺", Some(5..5), w, cx);
            assert!(s.marked_text_range(w, cx).is_some());
            s.replace_text_in_range(None, "น้ำ😺", w, cx);
            assert!(s.marked_text_range(w, cx).is_none());
        });
    });
    paint(cx, w);
    cx.simulate_keystrokes(w.into(), "enter");
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e,GridEvent::Edit{value,..} if value=="น้ำ😺"))
    );
    paint(cx, w);
    cx.update_window(w.into(), |_, w, cx| {
        w.dispatch_action(Box::new(gpui_kit::base::input::Copy), cx)
    })
    .unwrap();
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "0"
    );
    update(cx, w, &g, |g, _, cx| {
        g.update_model(
            |m| {
                m.columns[0].name = "line\nbreak\tname".into();
                m.rows.set(0, 0, Some("😺".repeat(800)));
            },
            cx,
        )
    });
    paint(cx, w);
}
#[gpui::test]
fn table_tab_navigation_wins_over_root_focus_traversal(cx: &mut TestAppContext) {
    let (w, g, _) = setup(cx);
    paint(cx, w);
    click(cx, w, 70., 43., 1, MouseButton::Left);
    cx.simulate_keystrokes(w, "tab tab tab");
    assert_eq!(
        g.read_with(cx, |g, _| (
            g.model().selection.anchor,
            g.model().selected_column
        )),
        (Some(1), Some(0))
    );
    cx.simulate_keystrokes(w, "shift-tab");
    assert_eq!(
        g.read_with(cx, |g, _| (
            g.model().selection.anchor,
            g.model().selected_column
        )),
        (Some(0), Some(2))
    );
}
#[gpui::test]
fn paste_inside_single_and_multiline_editors_matches_html_inputs(cx: &mut TestAppContext) {
    let (w, g, _) = setup(cx);
    paint(cx, w);
    click(cx, w, 70., 43., 2, MouseButton::Left);
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("a\r\nb".into())));
    cx.simulate_keystrokes(w, "cmd-v");
    assert_eq!(
        g.read_with(cx, |g, _| g.model().editing.as_ref().unwrap().value.clone()),
        "ab"
    );
    cx.simulate_keystrokes(w, "alt-enter");
    paint(cx, w);
    cx.simulate_keystrokes(w, "cmd-v");
    assert_eq!(
        g.read_with(cx, |g, _| g.model().editing.as_ref().unwrap().value.clone()),
        "ab\na\nb"
    );
}
/// Double-clicking the body below the last row, or anywhere in an empty table's body, asks the
/// host for a new row; only where inserting is allowed, and never instead of editing a cell.
#[gpui::test]
fn double_click_empty_area_requests_a_new_row(cx: &mut TestAppContext) {
    let (w, g, events) = setup(cx);
    let inserts = |events: &Rc<RefCell<Vec<GridEvent>>>| {
        events
            .borrow()
            .iter()
            .filter(|e| matches!(e, GridEvent::InsertRow))
            .count()
    };
    let columns = || {
        let mut c = ["id", "name"].map(Column::new).to_vec();
        for c in &mut c {
            c.editable = true;
        }
        c
    };
    let set = |cx: &mut TestAppContext, rows: usize, insert: bool| {
        update(cx, w, &g, |g, _, cx| {
            g.update_model(
                |m| {
                    m.replace_data(
                        columns(),
                        (0..rows)
                            .map(|i| vec![Some(i.to_string()), Some("x".into())])
                            .collect::<Vec<_>>(),
                    );
                    m.options.hooks.insert = insert;
                    m.options.hooks.edit = true;
                },
                cx,
            )
        });
        paint(cx, w);
        events.borrow_mut().clear();
    };
    // Short table: below the two rows.
    set(cx, 2, true);
    click(cx, w, 200., 300., 1, MouseButton::Left);
    assert_eq!(
        inserts(&events),
        0,
        "a single click on empty space does nothing new"
    );
    click(cx, w, 200., 300., 2, MouseButton::Left);
    assert_eq!(inserts(&events), 1);
    // Also from the row-number gutter below the rows.
    click(cx, w, 10., 300., 2, MouseButton::Left);
    assert_eq!(inserts(&events), 2);
    // A double click on an existing cell still edits that cell.
    events.borrow_mut().clear();
    click(cx, w, 70., 43., 2, MouseButton::Left);
    assert_eq!(inserts(&events), 0);
    assert!(g.read_with(cx, |g, _| g.model().editing.is_some()));
    // Empty table: anywhere in the body, including over the "No rows" pill.
    set(cx, 0, true);
    for (x, y) in [(200., 60.), (424., 255.), (800., 480.)] {
        click(cx, w, x, y, 2, MouseButton::Left);
    }
    assert_eq!(inserts(&events), 3);
    // Not where inserting isn't allowed (query results, structure, read-only grids).
    set(cx, 0, false);
    click(cx, w, 424., 255., 2, MouseButton::Left);
    set(cx, 2, false);
    click(cx, w, 200., 300., 2, MouseButton::Left);
    assert_eq!(inserts(&events), 0);
    // The header is not empty space.
    set(cx, 2, true);
    click(cx, w, 200., 10., 2, MouseButton::Left);
    assert_eq!(inserts(&events), 0);
}
/// Double-clicking an existing cell opens a focused editor with the cell's value, but only where
/// the grid is editable: never with the edit hook off or on a read-only column.
#[gpui::test]
fn double_click_existing_cell_opens_the_editor_only_where_editable(cx: &mut TestAppContext) {
    let (w, g, events) = setup(cx);
    paint(cx, w);
    let editing = |cx: &mut TestAppContext| {
        cx.update_window(w.into(), |_, window, cx| {
            let g = g.read(cx);
            (
                g.model()
                    .editing
                    .as_ref()
                    .map(|e| (e.cell, e.value.clone())),
                g.editor_state().is_some_and(|e| {
                    gpui_kit::Focusable::focus_handle(e.read(cx), cx).is_focused(window)
                }),
            )
        })
        .unwrap()
    };
    let close = |cx: &mut TestAppContext| {
        update(cx, w, &g, |g, _, cx| {
            g.flush_edit(cx);
        });
        paint(cx, w);
    };
    let set = |cx: &mut TestAppContext, edit: bool, editable: bool| {
        update(cx, w, &g, |g, _, cx| {
            g.update_model(
                |m| {
                    m.options.hooks.edit = edit;
                    m.columns[1].editable = editable;
                },
                cx,
            )
        });
        paint(cx, w);
    };
    // Row 0, column "name".
    click(cx, w, 130., 43., 1, MouseButton::Left);
    click(cx, w, 130., 43., 2, MouseButton::Left);
    paint(cx, w);
    assert_eq!(
        editing(cx),
        (Some((Cell { row: 0, col: 1 }, "ธนาคาร😺".to_owned())), true)
    );
    assert!(
        !events
            .borrow()
            .iter()
            .any(|e| matches!(e, GridEvent::InsertRow)),
        "a cell is not empty space"
    );
    close(cx);
    set(cx, false, true);
    click(cx, w, 130., 43., 2, MouseButton::Left);
    assert_eq!(editing(cx), (None, false), "read-only grid");
    set(cx, true, false);
    click(cx, w, 130., 43., 2, MouseButton::Left);
    assert_eq!(editing(cx), (None, false), "read-only column");
}
/// The open editor and the selection follow their rows by identity when the host replaces the
/// rows, and close or clear when their row is gone: an edit can never land on whatever row now
/// has the old index.
#[gpui::test]
fn editor_and_selection_follow_row_identity_across_reloads(cx: &mut TestAppContext) {
    let (w, g, events) = setup(cx);
    let rows = |names: &[&str]| -> Vec<Vec<Option<String>>> {
        names
            .iter()
            .map(|n| vec![Some(n.to_string()), Some(n.to_string()), None])
            .collect()
    };
    let keys = |names: &[&str]| names.iter().map(|n| format!("k-{n}")).collect::<Vec<_>>();
    let load = |cx: &mut TestAppContext, names: &[&str], ks: Vec<String>| {
        let data = rows(names);
        update(cx, w, &g, |g, _, cx| {
            g.update_model(
                |m| {
                    m.set_row_keys(ks);
                    let columns = m.columns.clone();
                    m.replace_data(columns, data);
                },
                cx,
            )
        });
        paint(cx, w);
    };
    load(
        cx,
        &["alpha", "bravo", "charlie"],
        keys(&["alpha", "bravo", "charlie"]),
    );
    click(cx, w, 130., 43., 1, MouseButton::Left);
    click(cx, w, 130., 43., 2, MouseButton::Left);
    paint(cx, w);
    cx.simulate_input(w.into(), "typed");
    // Same rows in another order: the editor and the selection move with "alpha".
    load(
        cx,
        &["bravo", "charlie", "alpha"],
        keys(&["bravo", "charlie", "alpha"]),
    );
    let (cell, selection) = g.read_with(cx, |g, _| {
        (
            g.model().editing.as_ref().map(|e| e.cell),
            g.model().selection.rows.iter().copied().collect::<Vec<_>>(),
        )
    });
    assert_eq!(cell, Some(Cell { row: 2, col: 1 }));
    assert_eq!(selection, vec![2]);
    events.borrow_mut().clear();
    cx.simulate_keystrokes(w.into(), "enter");
    assert!(
        events.borrow().iter().any(|e| matches!(
            e,
            GridEvent::Edit { cell: Cell { row: 2, col: 1 }, value, key }
                if value == "typed" && key.as_deref() == Some("k-alpha")
        )),
        "the edit is applied to alpha, now at row 2: {:?}",
        events.borrow()
    );
    // A reload with new identities (the rows were re-read): nothing survives by index.
    click(cx, w, 130., 43., 2, MouseButton::Left);
    paint(cx, w);
    assert!(g.read_with(cx, |g, _| g.model().editing.is_some()));
    load(cx, &["bravo", "charlie", "alpha"], keys(&["x", "y", "z"]));
    events.borrow_mut().clear();
    cx.simulate_keystrokes(w.into(), "enter");
    paint(cx, w);
    let (editing, has_editor, selected) = g.read_with(cx, |g, _| {
        (
            g.model().editing.clone(),
            g.editor_state().is_some(),
            g.model().selection.rows.len(),
        )
    });
    assert_eq!((editing, has_editor, selected), (None, false, 0));
    assert!(
        !events
            .borrow()
            .iter()
            .any(|e| matches!(e, GridEvent::Edit { .. })),
        "nothing is staged after the row is gone: {:?}",
        events.borrow()
    );
}
/// A hidden column has no width and no editor: it can't be edited, requested or scrolled to.
#[gpui::test]
fn hidden_columns_never_get_an_editor(cx: &mut TestAppContext) {
    let (w, g, _) = setup(cx);
    paint(cx, w);
    update(cx, w, &g, |g, _, cx| {
        g.update_model(|m| m.columns[1].hidden = true, cx)
    });
    paint(cx, w);
    g.read_with(cx, |g, _| {
        let m = g.model();
        assert_eq!(m.cell_rect(Cell { row: 0, col: 1 }).width, 0.);
        assert_eq!(
            m.column_left(1),
            m.column_left(2),
            "where the hidden column would be"
        );
        assert!(m.column_left(2) < m.layout().width);
    });
    let begun = update(cx, w, &g, |g, window, cx| {
        g.request_edit(Cell { row: 0, col: 1 }, 7, window, cx);
        g.model().editing.is_some()
    });
    assert!(
        !begun,
        "an explicit request on a hidden column opens nothing"
    );
    let x = g.read_with(cx, |g, _| (g.model().scroll_x, g.model().editing.is_none()));
    assert_eq!(x, (0., true), "and doesn't scroll away");
    // Hiding the column under an open editor closes it.
    update(cx, w, &g, |g, _, cx| {
        g.update_model(|m| m.columns[1].hidden = false, cx)
    });
    paint(cx, w);
    click(cx, w, 130., 43., 2, MouseButton::Left);
    assert!(g.read_with(cx, |g, _| g.model().editing.is_some()));
    update(cx, w, &g, |g, _, cx| {
        g.update_model(|m| m.columns[1].hidden = true, cx)
    });
    assert!(g.read_with(cx, |g, _| g.model().editing.is_none()
        && g.editor_state().is_none()));
}
