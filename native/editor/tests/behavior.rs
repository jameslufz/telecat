use gpui::{
    AppContext, Bounds, Context, Entity, EntityInputHandler, TestAppContext, Window, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use telecat_editor::{Driver, EditorOptions, Schema, Selection, SqlEditor, search::SearchQuery};
fn setup(cx: &mut TestAppContext, text: &str) -> (WindowHandle<SqlEditor>, Entity<SqlEditor>) {
    let result = cx.update(|cx| {
        gpui_kit::init(cx);
        telecat_editor::init(cx);
        let mut view = None;
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(848.), px(237.)),
                    ))),
                    ..Default::default()
                },
                |w, cx| {
                    let editor = cx.new(|cx| {
                        SqlEditor::new(
                            EditorOptions {
                                text: text.into(),
                                ..Default::default()
                            },
                            w,
                            cx,
                        )
                    });
                    editor.update(cx, |s, cx| s.focus(w, cx));
                    view = Some(editor.clone());
                    editor
                },
            )
            .unwrap();
        (window, view.unwrap())
    });
    cx.update_window(result.0.into(), |_, w, _| w.activate_window())
        .unwrap();
    result
}
fn edit<R>(
    cx: &mut TestAppContext,
    w: WindowHandle<SqlEditor>,
    v: &Entity<SqlEditor>,
    f: impl FnOnce(&mut SqlEditor, &mut Window, &mut Context<SqlEditor>) -> R,
) -> R {
    cx.update_window(w.into(), |_, w, cx| v.update(cx, |v, cx| f(v, w, cx)))
        .unwrap()
}
fn text(cx: &TestAppContext, v: &Entity<SqlEditor>) -> String {
    v.read_with(cx, |v, cx| v.text(cx).to_string())
}
fn paint(cx: &mut TestAppContext, w: WindowHandle<SqlEditor>) {
    cx.update_window(w.into(), |_, w, cx| {
        w.draw(cx).clear(cx);
    })
    .unwrap();
}
#[gpui::test]
fn shell_offsets_thai_emoji_and_undo(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "SELECT 'น้ำ😺';");
    edit(cx, w, &v, |v, _, cx| {
        v.set_selections(
            &[Selection {
                anchor: 11,
                head: 13,
            }],
            cx,
        );
        assert_eq!(v.selected_text(cx).as_deref(), Some("😺"));
    });
    edit(cx, w, &v, |v, w, cx| v.insert("🚀", w, cx));
    assert_eq!(text(cx, &v), "SELECT 'น้ำ🚀';");
    edit(cx, w, &v, |v, w, cx| v.undo(w, cx));
    assert_eq!(text(cx, &v), "SELECT 'น้ำ😺';");
    edit(cx, w, &v, |v, w, cx| v.redo(w, cx));
    assert_eq!(text(cx, &v), "SELECT 'น้ำ🚀';");
    edit(cx, w, &v, |v, w, cx| v.set_text("select 2", w, cx));
    edit(cx, w, &v, |v, w, cx| v.undo(w, cx));
    assert_eq!(text(cx, &v), "SELECT 'น้ำ🚀';");
}
#[gpui::test]
fn multicursor_comment_indent_clipboard(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "a a\na a");
    paint(cx, w);
    edit(cx, w, &v, |v, _, cx| {
        v.select_next_occurrence(true, cx);
        assert_eq!(v.selections(cx).len(), 4);
    });
    cx.simulate_input(w.into(), "bank");
    assert_eq!(text(cx, &v), "bank bank\nbank bank");
    edit(cx, w, &v, |v, w, cx| {
        v.undo(w, cx);
        v.undo(w, cx);
    });
    assert_eq!(text(cx, &v), "a a\na a");
    edit(cx, w, &v, |v, w, cx| v.toggle_comment(w, cx));
    assert_eq!(text(cx, &v), "-- a a\n-- a a");
    edit(cx, w, &v, |v, w, cx| v.toggle_comment(w, cx));
    assert_eq!(text(cx, &v), "a a\na a");
    cx.simulate_keystrokes(w.into(), "cmd-a tab");
    assert_eq!(text(cx, &v), "  a a\n  a a");
    cx.simulate_keystrokes(w.into(), "shift-tab cmd-c cmd-down enter cmd-v");
    assert_eq!(text(cx, &v), "a a\na a\na a\na a");
}
#[gpui::test]
fn thai_grapheme_and_ime(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "กิ้😺");
    paint(cx, w);
    edit(cx, w, &v, |v, _, cx| {
        v.set_selections(&[Selection { anchor: 5, head: 5 }], cx)
    });
    cx.simulate_keystrokes(w.into(), "left");
    assert_eq!(v.read_with(cx, |v, cx| v.cursor(cx)), 3);
    cx.simulate_keystrokes(w.into(), "left");
    assert_eq!(v.read_with(cx, |v, cx| v.cursor(cx)), 0);
    cx.simulate_keystrokes(w.into(), "right");
    assert_eq!(v.read_with(cx, |v, cx| v.cursor(cx)), 3);
    edit(cx, w, &v, |v, w, cx| {
        v.input_state().update(cx, |s, cx| {
            s.replace_and_mark_text_in_range(Some(0..3), "น้ำ", Some(3..3), w, cx);
            assert_eq!(s.marked_text_range(w, cx), Some(0..3));
            s.replace_text_in_range(None, "น้ำ", w, cx);
            assert_eq!(s.marked_text_range(w, cx), None);
        });
    });
    assert_eq!(text(cx, &v), "น้ำ😺");
}
#[gpui::test]
fn pairing_indent_search_replace_and_shortcuts(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "");
    paint(cx, w);
    cx.simulate_input(w.into(), "(");
    assert_eq!(text(cx, &v), "()");
    cx.simulate_keystrokes(w.into(), "enter");
    assert_eq!(text(cx, &v), "(\n  \n)");
    edit(cx, w, &v, |v, w, cx| {
        v.set_text("SELECT a, a, a1, A;", w, cx);
        v.set_search(
            SearchQuery {
                query: "a".into(),
                replacement: "น้ำ😺".into(),
                whole_word: true,
                ..Default::default()
            },
            w,
            cx,
        );
        v.replace_matches(true, w, cx);
    });
    assert_eq!(text(cx, &v), "SELECT น้ำ😺, น้ำ😺, a1, น้ำ😺;");
    edit(cx, w, &v, |v, w, cx| v.undo(w, cx));
    assert_eq!(text(cx, &v), "SELECT a, a, a1, A;");
    cx.simulate_keystrokes(w.into(), "cmd-f");
    paint(cx, w);
    cx.simulate_input(w.into(), "SELECT");
    assert_eq!(v.read_with(cx, |v, cx| v.search_matches(cx).len()), 1);
}
#[test]
fn utf16_boundary_conversion() {
    let s = "กิ้😺x";
    for (u, b) in [
        (0, 0),
        (1, 3),
        (2, 6),
        (3, 9),
        (4, 9),
        (5, 13),
        (6, 14),
        (999, 14),
    ] {
        assert_eq!(telecat_editor::utf16::to_byte(s, u), b);
    }
    assert_eq!(telecat_editor::utf16::from_byte(s, 13), 5);
}
#[test]
fn indexed_completion_and_search() {
    let schema = Schema::from([(
        "sa_banks".into(),
        vec!["bank_name_th".into(), "bank_code".into()],
    )]);
    let index = telecat_editor::completion::CompletionIndex::new(Driver::Mysql, &schema);
    let result = index.query("s", 1, false);
    assert!(result.items.iter().any(|i| &*i.label == "SELECT"));
    assert!(result.items.iter().any(|i| &*i.label == "sa_banks"));
    let result = index.query("sa_banks.bank_", 14, false);
    assert_eq!(result.items.len(), 2);
    assert!(
        result
            .items
            .iter()
            .all(|i| i.kind.badge() == "C" && i.kind.badge().is_ascii())
    );
    let q = SearchQuery {
        query: "(bank)_(\\w+)".into(),
        replacement: "$2_$1".into(),
        regex: true,
        ..Default::default()
    };
    assert_eq!(
        q.edits("bank_name bank_code").unwrap(),
        vec![(0..9, "name_bank".into()), (10..19, "code_bank".into())]
    );
}
#[gpui::test]
fn keystroke_to_paint_latency(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "SELECT * FROM members;\n\ns");
    let schema: Schema = (0..10_000)
        .map(|i| {
            (
                format!("schema_table_{i:05}"),
                vec!["id".into(), "bank_name_th".into(), "created_at".into()],
            )
        })
        .collect();
    let probe = telecat_editor::latency::LatencyProbe::default();
    edit(cx, w, &v, |v, _, cx| {
        v.set_schema(&schema, cx);
        v.set_selections(
            &[Selection {
                anchor: 25,
                head: 25,
            }],
            cx,
        );
        v.set_latency_probe(probe.clone(), cx);
    });
    paint(cx, w);
    // Includes dispatch, edit, asynchronous completion, layout, text runs and paint.
    // GPUI's headless test renderer supplies synthetic font metrics, not CoreText.
    for n in 0..220 {
        probe.begin();
        cx.simulate_keystrokes(w.into(), if n % 2 == 0 { "e" } else { "backspace" });
        cx.run_until_parked();
        paint(cx, w);
        if n == 19 {
            probe.clear();
        }
    }
    let s = probe.summary();
    println!(
        "LATENCY headless n={} p50={:.3}ms p95={:.3}ms max={:.3}ms schema=10000",
        s.count, s.p50_ms, s.p95_ms, s.max_ms
    );
    assert_eq!(s.count, 200);
    assert!(
        s.p95_ms < 100.,
        "regression: keystroke paint should not approach the Electron 350ms stall"
    );
}
#[gpui::test]
fn shell_shortcut_events_and_completion_accept(cx: &mut TestAppContext) {
    use std::{cell::RefCell, rc::Rc};
    let (w, v) = setup(cx, "SELECT 1;");
    let events = Rc::new(RefCell::new(Vec::new()));
    let copy = events.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&v, move |_, event: &telecat_editor::EditorEvent, _| {
            copy.borrow_mut().push(format!("{event:?}"))
        })
    });
    paint(cx, w);
    cx.simulate_keystrokes(
        w.into(),
        "cmd-enter shift-cmd-enter cmd-i shift-cmd-i cmd-[ cmd-] shift-cmd-k",
    );
    for expected in [
        "RunCurrent",
        "RunAll",
        "Beautify",
        "Uglify",
        "PreviousTab",
        "NextTab",
        "SwitchConnection",
    ] {
        assert!(
            events.borrow().iter().any(|e| e == expected),
            "missing {expected}: {:?}",
            events.borrow()
        );
    }
    assert_eq!(text(cx, &v), "SELECT 1;");
    edit(cx, w, &v, |v, w, cx| {
        v.set_text("SEL", w, cx);
        v.set_selections(&[Selection { anchor: 3, head: 3 }], cx);
    });
    // Programmatic edits settle before the explicit completion request.
    cx.run_until_parked();
    edit(cx, w, &v, |v, w, cx| v.request_completion(true, w, cx));
    cx.run_until_parked();
    assert!(v.read_with(cx, |v, _| v.completion_count()) > 0);
    cx.simulate_keystrokes(w.into(), "enter");
    assert_eq!(text(cx, &v), "SELECT");
    cx.simulate_keystrokes(w.into(), "cmd-/");
    assert_eq!(text(cx, &v), "-- SELECT");
    cx.simulate_keystrokes(w.into(), "cmd-z");
    assert_eq!(text(cx, &v), "SELECT");
}
#[gpui::test]
fn blink_600ms_and_unfocused_idle(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc, time::Duration};
    let (w, v) = setup(cx, "SELECT 1;");
    paint(cx, w);
    cx.run_until_parked();
    let state = v.read_with(cx, |v, _| v.input_state().clone());
    let notifications = Rc::new(Cell::new(0));
    let count = notifications.clone();
    let _observer = cx.update(|cx| cx.observe(&state, move |_, _| count.set(count.get() + 1)));
    cx.executor().advance_clock(Duration::from_millis(599));
    cx.run_until_parked();
    assert_eq!(notifications.get(), 0);
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert!(notifications.get() > 0);
    cx.update_window(w.into(), |_, w, cx| w.blur(cx)).unwrap();
    paint(cx, w);
    cx.run_until_parked();
    notifications.set(0);
    cx.executor().advance_clock(Duration::from_secs(4));
    cx.run_until_parked();
    assert_eq!(
        notifications.get(),
        0,
        "unfocused editor must have no blink timer repaint"
    );
}

#[gpui::test]
fn multicursor_pair_wrap_skip_delete_and_undo(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "a b");
    paint(cx, w);
    edit(cx, w, &v, |v, _, cx| {
        v.set_selections(
            &[
                Selection { anchor: 0, head: 1 },
                Selection { anchor: 2, head: 3 },
            ],
            cx,
        )
    });
    cx.simulate_input(w.into(), "'");
    assert_eq!(text(cx, &v), "'a' 'b'");
    assert_eq!(
        v.read_with(cx, |v, cx| v.selections(cx)),
        vec![
            Selection { anchor: 1, head: 2 },
            Selection { anchor: 5, head: 6 }
        ]
    );
    cx.simulate_keystrokes(w.into(), "cmd-z");
    assert_eq!(text(cx, &v), "a b");
    edit(cx, w, &v, |v, w, cx| {
        v.set_text("\n", w, cx);
        v.set_selections(
            &[
                Selection { anchor: 0, head: 0 },
                Selection { anchor: 1, head: 1 },
            ],
            cx,
        );
    });
    cx.simulate_input(w.into(), "(");
    assert_eq!(text(cx, &v), "()\n()");
    cx.simulate_keystrokes(w.into(), "backspace");
    assert_eq!(text(cx, &v), "\n");
    cx.simulate_keystrokes(w.into(), "cmd-z");
    assert_eq!(text(cx, &v), "()\n()");
    cx.simulate_input(w.into(), ")");
    assert_eq!(text(cx, &v), "()\n()");
    assert_eq!(
        v.read_with(cx, |v, cx| v.selections(cx)),
        vec![
            Selection { anchor: 2, head: 2 },
            Selection { anchor: 5, head: 5 }
        ]
    );
}
#[test]
fn bracket_context_and_mismatch() {
    use telecat_editor::syntax::brackets;
    assert_eq!(
        brackets("(')')", 0, Driver::Mysql),
        vec![(0..1, true), (4..5, true)]
    );
    assert_eq!(
        brackets("([)]", 0, Driver::Mysql),
        vec![(0..1, false), (2..3, false)]
    );
    assert!(brackets("'('", 1, Driver::Mysql).is_empty());
}

#[gpui::test]
fn command_click_and_option_rectangle(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton, VisualTestContext};
    let (w, v) = setup(cx, "abcd\nabcd");
    paint(cx, w);
    let mut position = |offset| {
        edit(cx, w, &v, |v, _, cx| {
            v.set_selections(
                &[Selection {
                    anchor: offset,
                    head: offset,
                }],
                cx,
            )
        });
        paint(cx, w);
        v.read_with(cx, |v, cx| {
            let b = v.input_state().read(cx).cursor_layout().unwrap().0;
            point(b.left(), b.center().y)
        })
    };
    let start = position(1);
    let end = position(8);
    edit(cx, w, &v, |v, _, cx| {
        v.set_selections(&[Selection { anchor: 1, head: 1 }], cx)
    });
    paint(cx, w);
    {
        let mut visual = VisualTestContext::from_window(w.into(), cx);
        visual.simulate_click(
            end,
            Modifiers {
                platform: true,
                ..Default::default()
            },
        );
    }
    assert_eq!(
        v.read_with(cx, |v, cx| v.selections(cx)),
        vec![
            Selection { anchor: 8, head: 8 },
            Selection { anchor: 1, head: 1 }
        ]
    );
    {
        let mut visual = VisualTestContext::from_window(w.into(), cx);
        let mods = Modifiers {
            alt: true,
            ..Default::default()
        };
        visual.simulate_mouse_down(start, MouseButton::Left, mods);
        visual.simulate_mouse_move(end, MouseButton::Left, mods);
        visual.simulate_mouse_up(end, MouseButton::Left, mods);
    }
    let mut ranges = v.read_with(cx, |v, cx| v.selections(cx));
    ranges.sort_by_key(|s| s.anchor);
    assert_eq!(
        ranges,
        vec![
            Selection { anchor: 1, head: 3 },
            Selection { anchor: 6, head: 8 }
        ]
    );
    cx.simulate_input(w.into(), "X");
    assert_eq!(text(cx, &v), "aXd\naXd");
}

#[gpui::test]
fn comments_align_preserve_selection_and_skip_empty_lines(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "  a\n    b\n\n  c");
    paint(cx, w);
    cx.simulate_keystrokes(w.into(), "cmd-a cmd-/");
    assert_eq!(text(cx, &v), "  -- a\n  --   b\n\n  -- c");
    cx.simulate_keystrokes(w.into(), "cmd-/");
    assert_eq!(text(cx, &v), "  a\n    b\n\n  c");
    edit(cx, w, &v, |v, w, cx| {
        v.set_text("", w, cx);
        v.toggle_comment(w, cx);
    });
    assert_eq!(text(cx, &v), "-- ");
}

#[gpui::test]
fn replace_one_advances_and_all_selects_matches(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "a a a");
    paint(cx, w);
    edit(cx, w, &v, |v, w, cx| {
        v.set_search(
            SearchQuery {
                query: "a".into(),
                replacement: "น้ำ😺".into(),
                ..Default::default()
            },
            w,
            cx,
        );
        v.replace_matches(false, w, cx);
    });
    assert_eq!(
        v.read_with(cx, |v, cx| v.selected_text(cx)),
        Some("a".into())
    );
    edit(cx, w, &v, |v, w, cx| v.replace_matches(false, w, cx));
    assert_eq!(text(cx, &v), "น้ำ😺 a a");
    edit(cx, w, &v, |v, w, cx| v.replace_matches(false, w, cx));
    assert_eq!(text(cx, &v), "น้ำ😺 น้ำ😺 a");
    edit(cx, w, &v, |v, _, cx| v.select_search_matches(cx));
    assert_eq!(v.read_with(cx, |v, cx| v.selections(cx).len()), 1);
}

#[test]
fn completion_alias_and_search_escapes() {
    let schema = Schema::from([(
        "sa_banks".into(),
        vec!["bank_name_th".into(), "bank_code".into()],
    )]);
    let index = telecat_editor::completion::CompletionIndex::new(Driver::Mysql, &schema);
    let text = "select b.bank_ from sa_banks as b";
    assert_eq!(
        index
            .query(text, text.find("bank_").unwrap() + 5, false)
            .items
            .len(),
        2
    );
    let q = SearchQuery {
        query: r"a\nb".into(),
        replacement: r"x\ty".into(),
        ..Default::default()
    };
    assert_eq!(q.edits("a\nb").unwrap(), vec![(0..3, "x\ty".into())]);
    let q = SearchQuery {
        query: "^b".into(),
        regex: true,
        ..Default::default()
    };
    assert_eq!(q.matches("a\nb").unwrap(), vec![2..3]);
}

#[gpui::test]
fn backtick_pair_and_occurrences_inside_strings(cx: &mut TestAppContext) {
    let (w, v) = setup(cx, "");
    paint(cx, w);
    cx.simulate_input(w.into(), "`");
    assert_eq!(text(cx, &v), "``");
    edit(cx, w, &v, |v, w, cx| {
        v.set_text("'bank' bank", w, cx);
        v.set_selections(&[Selection { anchor: 3, head: 3 }], cx);
    });
    cx.simulate_keystrokes(w.into(), "cmd-d");
    assert_eq!(
        v.read_with(cx, |v, cx| v.selected_text(cx)),
        Some("bank".into())
    );
    cx.simulate_keystrokes(w.into(), "cmd-d");
    assert_eq!(v.read_with(cx, |v, cx| v.selections(cx).len()), 2);
}

#[gpui::test]
fn frozen_scenario_caret_stays_idle_after_window_reactivation(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc, time::Duration};
    let (w, v) = setup(cx, "SELECT 'ธนาคารกรุงเทพ 😺';");
    paint(cx, w);
    edit(cx, w, &v, |v, _, cx| v.freeze_caret(true, cx));
    let state = v.read_with(cx, |v, _| v.input_state().clone());
    let (other, _) = setup(cx, "SELECT 2;");
    paint(cx, other);
    cx.run_until_parked();
    cx.update_window(w.into(), |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    let count = Rc::new(Cell::new(0));
    let observed = count.clone();
    let _observer =
        cx.update(|cx| cx.observe(&state, move |_, _| observed.set(observed.get() + 1)));
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert_eq!(
        count.get(),
        0,
        "reactivating a frozen caret restarted its timer"
    );
    cx.simulate_input(w.into(), "x");
    assert!(
        text(cx, &v).contains('x'),
        "idle suspension must not stop input"
    );
}
