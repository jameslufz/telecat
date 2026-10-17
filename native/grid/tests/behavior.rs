use serde_json::{Value, json};
use telecat_grid::*;
fn model(n: usize) -> GridModel {
    let mut columns = vec![Column::new("id"), Column::new("name"), Column::new("other")];
    for c in &mut columns {
        c.editable = true;
    }
    let mut m = GridModel::new(
        columns,
        (0..n)
            .map(|r| vec![Some(r.to_string()), Some(format!("row {r}")), None])
            .collect(),
    );
    m.options.hooks = Hooks {
        edit: true,
        sort: true,
        paste: true,
        delete: true,
        insert: true,
        duplicate: true,
        detail: true,
        quick_look: true,
        ..Default::default()
    };
    m.viewport = Rect {
        x: 0.,
        y: 0.,
        width: 848.,
        height: 511.,
    };
    m
}
fn mods(shift: bool, meta: bool) -> Modifiers {
    Modifiers {
        shift,
        meta,
        ..Default::default()
    }
}
fn sel(s: &Selection) -> Value {
    json!({"rows":s.rows,"anchor":s.anchor})
}
fn edit(m: &GridModel) -> Value {
    m.editing.as_ref().map_or(
        Value::Null,
        |e| json!({"row":e.cell.row,"col":e.cell.col,"value":e.value}),
    )
}
#[test]
fn actual_electron_oracle() {
    let data: Value = serde_json::from_str(include_str!("oracle.json")).unwrap();
    for (n, case) in data["cases"].as_array().unwrap().iter().enumerate() {
        let i = &case["input"];
        let expected = &case["expected"];
        let op = i["op"].as_str().unwrap();
        let columns: Vec<String> = i
            .get("columns")
            .map(|v| serde_json::from_value(v.clone()).unwrap())
            .unwrap_or(vec!["a".into(), "b".into(), "c".into()]);
        let mut cols: Vec<_> = columns.iter().map(Column::new).collect();
        let rows: Vec<Vec<Option<String>>> = i
            .get("rows")
            .map(|v| serde_json::from_value(v.clone()).unwrap())
            .unwrap_or(vec![vec![Some("a".into()), None, Some("".into())]; 20]);
        for (c, col) in cols.iter_mut().enumerate() {
            col.editable = i
                .get("editable")
                .is_none_or(|v| v.as_array().unwrap().contains(&json!(c)));
            col.hidden = i
                .get("hidden")
                .is_some_and(|v| v.as_array().unwrap().contains(&json!(c)));
            col.chevron = i
                .get("chevrons")
                .is_some_and(|v| v.as_array().unwrap().contains(&json!(col.name)));
        }
        let mut m = GridModel::new(cols, rows);
        m.options.hooks.edit = true;
        m.options.hooks.paste = true;
        m.selected_column = i["col"].as_u64().map(|v| v as usize);
        if let Some(s) = i.get("selection") {
            m.selection.rows = serde_json::from_value(s["rows"].clone()).unwrap();
            m.selection.anchor = s["anchor"].as_u64().map(|v| v as usize);
        }
        let shift = i["shift"].as_bool().unwrap_or(false);
        let meta = i["meta"].as_bool().unwrap_or(false);
        let mut events = vec![];
        match op {
            "select" => {
                events = m.click(i["row"].as_u64().unwrap() as usize, None, mods(shift, meta));
            }
            "key" => {
                let k = match i["key"].as_str().unwrap() {
                    "ArrowUp" => Key::Up,
                    "ArrowDown" => Key::Down,
                    "ArrowLeft" => Key::Left,
                    "ArrowRight" => Key::Right,
                    "Enter" => Key::Enter,
                    "Escape" => Key::Escape,
                    _ => unreachable!(),
                };
                events = m.navigate(k, mods(shift, meta));
            }
            "width" => {
                let widths: Vec<_> = columns
                    .iter()
                    .map(|c| expected["widths"][c].as_f64().unwrap() as f32)
                    .collect();
                assert_eq!(m.widths, widths, "case {n}");
            }
            "edit" => {
                m.begin_edit(Cell {
                    row: i["row"].as_u64().unwrap() as usize,
                    col: i["editcol"].as_u64().unwrap() as usize,
                });
                if let Some(e) = &mut m.editing {
                    e.value = i["value"].as_str().unwrap().to_owned();
                }
                events = m.finish_edit(
                    i["commit"].as_bool().unwrap(),
                    i["move"].as_i64().unwrap() as i8,
                );
            }
            "copy" => {
                assert_eq!(json!(m.selected_tsv()), *expected, "case {n}");
                continue;
            }
            "paste" => {
                let got = m.paste(i["text"].as_str().unwrap());
                let v = got.first().map_or(Value::Null, |e| {
                    if let GridEvent::Paste(rows) = e {
                        json!(rows)
                    } else {
                        panic!()
                    }
                });
                assert_eq!(v, *expected, "case {n}");
                continue;
            }
            _ => unreachable!(),
        }
        assert_eq!(
            sel(&m.selection),
            expected["selection"],
            "selection case {n}"
        );
        assert_eq!(json!(m.selected_column), expected["col"], "column case {n}");
        assert_eq!(edit(&m), expected["editing"], "editor case {n}");
        let edits: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let GridEvent::Edit { cell, value } = e {
                    Some(json!({"row":cell.row,"col":cell.col,"value":value}))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(json!(edits), expected["edits"], "edits case {n}");
    }
}
#[test]
fn selection_anchor_and_row_number_semantics() {
    let mut m = model(10);
    m.click(3, Some(1), mods(false, false));
    m.click(7, None, mods(true, false));
    assert_eq!(m.selection.rows, (3..=7).collect());
    assert_eq!(m.selection.anchor, Some(3));
    assert_eq!(m.selected_column, Some(1));
    m.click(5, Some(2), mods(false, true));
    assert!(!m.selection.rows.contains(&5));
    assert_eq!(m.selection.anchor, Some(5));
    m.navigate(Key::Down, mods(true, false));
    assert_eq!(m.selection.rows, [5, 6].into());
    m.navigate(Key::Down, mods(true, false));
    assert_eq!(m.selection.rows, [5, 6].into()); // Electron retains anchor.
    let events = m.context_menu(Cell { row: 6, col: 1 }, Rect::default());
    assert_eq!(m.selection.rows, [5, 6].into());
    assert!(matches!(events[1], GridEvent::ContextMenu { .. }));
    m.context_menu(Cell { row: 9, col: 0 }, Rect::default());
    assert_eq!(m.selection, Selection::single(9));
}
#[test]
fn navigation_extensions_and_boundaries() {
    let mut m = model(1000);
    m.click(0, Some(0), Modifiers::default());
    m.navigate(Key::PageDown, Modifiers::default());
    assert_eq!(m.selection.anchor, Some(18));
    m.navigate(Key::End, Modifiers::default());
    assert_eq!(m.selection.anchor, Some(999));
    assert!(m.scroll_y > 25000.);
    m.navigate(Key::Down, Modifiers::default());
    assert_eq!(m.selection.anchor, Some(999));
    m.navigate(Key::Home, Modifiers::default());
    assert_eq!(m.scroll_y, 0.);
    m.columns[1].hidden = true;
    m.navigate(Key::Tab, Modifiers::default());
    assert_eq!(m.selected_column, Some(2));
    m.navigate(Key::Tab, Modifiers::default());
    assert_eq!(m.selection.anchor, Some(1));
    assert_eq!(m.selected_column, Some(0));
    m.navigate(Key::Tab, mods(true, false));
    assert_eq!(m.selection.anchor, Some(0));
    assert_eq!(m.selected_column, Some(2));
    m.navigate(Key::Left, Modifiers::default());
    assert_eq!(m.selected_column, Some(1)); // TS arrows include hidden columns.
    let mut empty = model(0);
    for key in [
        Key::Up,
        Key::Down,
        Key::Home,
        Key::End,
        Key::Tab,
        Key::Enter,
    ] {
        assert!(empty.navigate(key, Modifiers::default()).is_empty());
    }
}
#[test]
fn editor_commit_cancel_null_default_tab_and_nonce() {
    let mut m = model(2);
    let cell = Cell { row: 0, col: 2 };
    m.begin_edit(cell);
    assert_eq!(m.editing.as_ref().unwrap().value, "");
    assert!(m.finish_edit(true, 0).is_empty());
    m.edits.defaults.insert(cell);
    m.begin_edit(cell);
    assert!(m.finish_edit(true, 0).is_empty());
    m.begin_edit(cell);
    m.editing.as_mut().unwrap().value = "NULL".into();
    assert_eq!(
        m.finish_edit(true, 0),
        vec![GridEvent::Edit {
            cell,
            value: "NULL".into()
        }]
    );
    assert_eq!(m.rows.cell(0, 2), None);
    m.begin_edit(cell);
    m.editing.as_mut().unwrap().value = "cancel\n😺".into();
    assert!(m.finish_edit(false, 0).is_empty());
    m.columns[1].editable = false;
    m.begin_edit(Cell { row: 0, col: 0 });
    m.editing.as_mut().unwrap().value = "".into();
    let events = m.finish_edit(true, 1);
    assert!(matches!(&events[0],GridEvent::Edit{value,..} if value.is_empty()));
    assert_eq!(m.editing.as_ref().unwrap().cell, cell);
    m.finish_edit(true, 1);
    assert!(m.editing.is_none());
    m.request_edit(Cell { row: 1, col: 1 }, 42);
    assert_eq!(m.editing.as_ref().unwrap().cell, Cell { row: 1, col: 1 });
    m.finish_edit(false, 0);
    m.request_edit(Cell { row: 1, col: 1 }, 42);
    assert!(m.editing.is_none());
    m.options.hooks.escape = true;
    m.selection = Selection::single(1);
    assert_eq!(
        m.navigate(Key::Escape, Modifiers::default()),
        vec![GridEvent::Escape]
    );
    assert_eq!(m.selection.anchor, Some(1));
}
#[test]
fn tsv_is_deliberately_unquoted_and_hidden_columns_are_copied() {
    let mut m = model(2);
    m.columns[1].hidden = true;
    m.rows
        .replace_row(0, vec![Some("a\tb".into()), Some("line\n2".into()), None]);
    m.edits.defaults.insert(Cell { row: 1, col: 2 });
    m.selection.rows = [0, 1].into();
    assert_eq!(m.selected_tsv(), "a\tb\tline\n2\tNULL\n1\trow 1\tDEFAULT");
    assert_eq!(
        parse_tsv("a\r\nb\r\t\t\n\n"),
        vec![vec!["a"], vec!["b", "", ""], vec![""]]
    );
    assert_eq!(parse_tsv("\"a\tb\""), vec![vec!["\"a", "b\""]]);
    assert_eq!(
        m.copy(CopyKind::AllTsv, Cell { row: 0, col: 0 }),
        format!("id\tname\tother\n{}", m.selected_tsv())
    );
    let json: Value =
        serde_json::from_str(&m.copy(CopyKind::RowJson, Cell { row: 0, col: 0 })).unwrap();
    assert!(json["other"].is_null());
    m.columns = vec![Column::new("10"), Column::new("a"), Column::new("2")];
    assert!(
        m.copy(CopyKind::RowJson, Cell { row: 0, col: 0 })
            .starts_with("{\n  \"2\": null,")
    );
}
#[test]
fn widths_sample_utf16_clamp_resize_and_reset() {
    let mut columns = vec![
        Column::new("x"),
        Column::new("x".repeat(100)),
        Column::new("emoji"),
    ];
    columns[0].chevron = true;
    let mut rows = vec![vec![None, None, Some("😺".repeat(10))]; 61];
    rows[60][2] = Some("x".repeat(100));
    assert_eq!(default_widths(&columns, &rows), vec![91., 420., 174.]);
    let mut m = GridModel::new(columns.clone(), rows.clone());
    m.resize(0, 1.);
    assert_eq!(m.widths[0], 40.);
    m.replace_data(columns.clone(), rows.clone());
    assert_eq!(m.widths[0], 40.);
    columns[0].name = "changed".into();
    m.replace_data(columns, rows);
    assert_ne!(m.widths[0], 40.);
    assert_eq!(model(500).number_width(), 49.);
    assert_eq!(model(100000).number_width(), 76.);
}
#[test]
fn sort_is_controlled_and_cycles_ascending_descending_none() {
    let mut m = model(1);
    for direction in [Some(Direction::Asc), Some(Direction::Desc), None] {
        let old = m.sort.clone();
        let events = m.click_header(1);
        assert_eq!(m.sort, old);
        let GridEvent::Sort(next) = &events[0] else {
            panic!()
        };
        assert_eq!(next.as_ref().map(|s| s.direction), direction);
        m.sort = next.clone();
    }
}
#[test]
fn virtualization_and_scroll_gutters_are_bounded() {
    for count in [0, 1, 6, 1000, 100000] {
        for h in [0., 1., 233.5, 511., 800.] {
            for scroll in [0., 1., 26., 400., 2600000.] {
                let r = virtual_range(count, scroll, h, 20);
                assert!(r.start <= r.end && r.end <= count);
                assert!(r.len() <= (h / 26.).ceil() as usize + 41);
            }
        }
    }
    assert_eq!(filler_count(6, 501.), 13);
    assert_eq!(filler_count(0, 233.5), 8);
    let mut m = model(100000);
    m.widths = vec![420.; 3];
    m.scroll_x = f32::INFINITY;
    m.scroll_y = f32::NAN;
    m.clamp_scroll();
    assert_eq!((m.scroll_x, m.scroll_y), (0., 0.));
    let l = m.layout();
    assert!(l.horizontal && l.vertical);
    assert_eq!(l.width, 838.);
    assert_eq!(l.height, 501.);
    m.scroll_to_cell(Cell { row: 99999, col: 2 });
    assert_eq!(m.scroll_y, l.content_height - l.height);
    assert!(m.cell_rect(Cell { row: 99999, col: 2 }).y >= 30.);
}
#[test]
fn filler_rows_never_enlarge_scroll_ranges_or_create_gutters() {
    for rows in [0, 1, 6, 18, 19, 100] {
        for height in [511., 233.5, 524., 525., 56., 31., 30., 29., 0.] {
            for total_width in [320., 843., 848., 849., 1304.] {
                let mut m = model(rows);
                m.viewport.height = height;
                m.widths = vec![100., 100., total_width - m.number_width() - 200.];
                let l = m.layout();
                let actual_height = if rows == 0 {
                    0.
                } else {
                    30. + rows as f32 * 26.
                };
                assert_eq!(
                    l.content_height,
                    actual_height.max(l.height),
                    "rows={rows}, height={height}, width={total_width}"
                );
                assert_eq!(l.vertical, actual_height > l.height);
                assert_eq!(l.horizontal, total_width > l.width);
                if actual_height <= l.height {
                    assert_eq!(l.width, 848., "filler must not reserve a vertical gutter");
                }
                for offset in [-100., 0.125, 13., 100000.] {
                    m.scroll_y = offset;
                    m.clamp_scroll();
                    assert_eq!(
                        m.scroll_y,
                        offset.clamp(0., (actual_height - l.height).max(0.))
                    );
                    let stable = (m.scroll_x, m.scroll_y);
                    m.clamp_scroll();
                    assert_eq!((m.scroll_x, m.scroll_y), stable);
                }
            }
        }
    }
}
#[test]
fn replacing_shorter_results_cannot_leave_dangling_selection_or_editor() {
    let mut m = model(100);
    m.selection = Selection::single(99);
    m.begin_edit(Cell { row: 99, col: 1 });
    m.scroll_to_row(99);
    m.replace_data(m.columns.clone(), vec![]);
    assert!(m.selection.rows.is_empty());
    assert!(m.selection.anchor.is_none());
    assert!(m.editing.is_none());
}
#[test]
fn duplicate_names_share_widths_and_editor_compares_current_caller_value() {
    let mut m = GridModel::new(
        vec![Column::new("a"), Column::new("a")],
        vec![vec![Some("x".into()), Some("long value here".into())]],
    );
    assert_eq!(m.widths, vec![136., 136.]);
    m.resize(0, 200.);
    assert_eq!(m.widths, vec![200., 200.]);
    m.columns[0].editable = true;
    m.options.hooks.edit = true;
    let cell = Cell { row: 0, col: 0 };
    m.begin_edit(cell);
    m.editing.as_mut().unwrap().value = "new".into();
    m.rows.set(0, 0, Some("new".into()));
    assert!(m.finish_edit(true, 0).is_empty());
}
#[test]
fn hooks_are_typed_and_edits_remain_caller_owned() {
    let mut m = model(10);
    m.selection.rows = [7, 2, 3].into();
    assert_eq!(
        m.navigate(Key::Delete, Modifiers::default()),
        vec![GridEvent::DeleteRows(vec![2, 3, 7])]
    );
    assert_eq!(m.rows.len(), 10);
    assert_eq!(
        m.navigate(
            Key::Duplicate,
            Modifiers {
                meta: true,
                ..Default::default()
            }
        ),
        vec![GridEvent::DuplicateRows(vec![2, 3, 7])]
    );
    assert_eq!(
        m.navigate(
            Key::Insert,
            Modifiers {
                meta: true,
                ..Default::default()
            }
        ),
        vec![GridEvent::InsertRow]
    );
    assert_eq!(
        m.navigate(Key::Space, Modifiers::default()),
        vec![GridEvent::ToggleDetail]
    );
    m.options.hooks.paste = false;
    assert!(m.paste("x\ty").is_empty());
}
