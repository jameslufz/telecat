//! QA 0.8.0 re-verify 1 (fix round 2): paste quoting round trips, foreign TSV, layout mapping.
//! Kept from the QA probe (.claude-qa/qa080/qa080_r1_probe.rs). Rows Telecat copied are pasted
//! the way the app does once the clipboard text matches its record (`copied_from`): by column
//! name (`rows_by_names`), with the names the grid copied (`copied_row_columns`).
use telecat_core::changes::Cell as Value;
use telecat_core::paste::{PasteTarget, plan_paste, rows_by_layout, rows_by_names};
use telecat_grid::*;

fn grid_of(values: &[Vec<Option<String>>]) -> GridModel {
    let n = values[0].len();
    GridModel::new(
        (0..n).map(|i| Column::new(format!("c{i}"))).collect(),
        values.to_vec(),
    )
}
fn select_all_block(m: &mut GridModel) {
    let last_row = m.rows.len() - 1;
    let last_col = m.columns.len() - 1;
    m.click(0, Some(0), Modifiers::default());
    m.click(
        last_row,
        Some(last_col),
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
}

#[test]
fn r1_round_trip_hostile_values() {
    let big = "ก".repeat(300_000) + "\"\t\r\n" + &"x".repeat(300_000);
    let cases: Vec<&str> = vec![
        "\"",
        "\"\"",
        "\"a",
        "a\"",
        "\"a\"",
        "say \"hi\"",
        "\"quoted\" tail",
        "a\r\nb",
        "a\rb",
        "\r",
        "\n",
        "\t",
        "",
        " lead",
        "trail ",
        "ไทย\tภาษา\nบรรทัด",
        "😺\"😺",
        "NULL x",
        "\"\t\"",
        "a\"\tb",
        "\"\"\"",
        "x\n\"",
        &big,
    ];
    for (i, v) in cases.iter().enumerate() {
        // 2x2 block where every cell carries the hostile value, plus a plain neighbour.
        let rows = vec![
            vec![Some(v.to_string()), Some("p".into())],
            vec![Some("q".into()), Some(v.to_string())],
        ];
        let mut m = grid_of(&rows);
        select_all_block(&mut m);
        let clip = m.selected_cells_tsv();
        let parsed = parse_tsv(&clip);
        let want: Vec<Vec<String>> = rows
            .iter()
            .map(|r| r.iter().map(|c| c.clone().unwrap()).collect())
            .collect();
        assert_eq!(
            parsed,
            want,
            "case {i}: {:?}",
            if v.len() > 40 { "<big>" } else { v }
        );
    }
}

#[test]
fn r1_single_hostile_cell_fills_multi_selection_only() {
    for v in ["a\nb", "\"x\"", "a\tb", ""] {
        let mut m = grid_of(&[vec![Some(v.into()), Some("k".into())]]);
        m.click(0, Some(0), Modifiers::default());
        let p = plan_paste(
            &parse_tsv(&m.selected_cells_tsv()),
            &PasteTarget {
                rows: &[0, 1, 2],
                columns: &[0, 1],
                all_rows: &[0, 1, 2, 3],
                all_columns: &[0, 1],
            },
            |_, _| true,
            |_| false,
            |_| false,
        );
        // "" copied gives empty clipboard text: the view refuses to paste it at all.
        if v.is_empty() {
            continue;
        }
        let p = p.unwrap();
        assert_eq!(p.cells.len(), 6, "{v:?}");
        assert!(
            p.cells.iter().all(|c| c.2 == Value::Text(v.into())),
            "{v:?}"
        );
    }
}

#[test]
fn r1_foreign_tsv() {
    // Excel for Mac: quoted where needed, quotes doubled, CRLF rows, trailing CRLF.
    let excel = "id\tnote\r\n1\t\"line1\nline2\"\r\n2\t\"say \"\"hi\"\"\"\r\n3\t\r\n";
    assert_eq!(
        parse_tsv(excel),
        vec![
            vec!["id", "note"],
            vec!["1", "line1\nline2"],
            vec!["2", "say \"hi\""],
            vec!["3", ""],
        ]
    );
    // Numbers: LF rows, no trailing newline.
    assert_eq!(
        parse_tsv("a\tb\nc\td"),
        vec![vec!["a", "b"], vec!["c", "d"]]
    );
    // Plain TSV with an inch mark / stray quotes (TablePlus-style: never quoted).
    assert_eq!(
        parse_tsv("12\" pipe\tx\n\"open\tend"),
        vec![vec!["12\" pipe", "x"], vec!["\"open", "end"]]
    );
    // Thai, CRLF.
    assert_eq!(parse_tsv("ไทย\tภาษา\r\n"), vec![vec!["ไทย", "ภาษา"]]);
}

#[test]
fn r1_two_cell_column_with_empty_last_value() {
    // ⇧⌘C on a column block [a, ''] then ⇧⌘V onto two cells.
    let mut m = grid_of(&[
        vec![Some("a".into())],
        vec![Some("".into())],
        vec![Some("z".into())],
        vec![Some("w".into())],
    ]);
    m.click(0, Some(0), Modifiers::default());
    m.click(
        1,
        Some(0),
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    let clip = m.selected_cells_tsv();
    let parsed = parse_tsv(&clip);
    println!("clip={clip:?} parsed={parsed:?}");
    let p = plan_paste(
        &parsed,
        &PasteTarget {
            rows: &[2, 3],
            columns: &[0],
            all_rows: &[0, 1, 2, 3],
            all_columns: &[0],
        },
        |_, _| true,
        |_| false,
        |_| false,
    )
    .unwrap();
    println!("staged={:?}", p.cells);
    assert_eq!(
        p.cells,
        vec![
            (2, 0, Value::Text("a".into())),
            (3, 0, Value::Text("".into()))
        ]
    );
}

#[test]
fn r1_copy_rows_from_reordered_tab_paste_rows_into_natural_tab() {
    // Same schema (id, name, email). Tab A: user dragged email first (persisted layout).
    let row = vec![Some("7".into()), Some("bob".into()), Some("b@x.io".into())];
    let mut a = GridModel::new(
        vec![Column::new("id"), Column::new("name"), Column::new("email")],
        vec![row.clone()],
    );
    a.set_order(vec![2, 0, 1]);
    a.click(0, Some(2), Modifiers::default());
    let clip = a.selected_rows_tsv();
    // Tab B (another database's copy of the table), natural order: ⌘V paste rows.
    let names = a.copied_row_columns();
    let table_b: Vec<String> = ["id", "name", "email"].map(String::from).to_vec();
    let rows = rows_by_names(&parse_tsv(&clip), &names, &table_b, "zixma.users").unwrap();
    println!("clip={clip:?} new row={:?}", rows[0]);
    assert_eq!(rows[0][1], Value::Text("bob".into()), "name");
    assert_eq!(rows[0][2], Value::Text("b@x.io".into()), "email");
}

#[test]
fn r1_copy_rows_then_reorder_then_paste_rows_same_tab() {
    let mut a = GridModel::new(
        vec![Column::new("id"), Column::new("name"), Column::new("email")],
        vec![vec![
            Some("7".into()),
            Some("bob".into()),
            Some("b@x.io".into()),
        ]],
    );
    a.click(0, Some(0), Modifiers::default());
    let clip = a.selected_rows_tsv();
    let names = a.copied_row_columns();
    a.set_order(vec![2, 0, 1]); // drag email to the front after copying
    let table: Vec<String> = a.columns.iter().map(|c| c.name.clone()).collect();
    let rows = rows_by_names(&parse_tsv(&clip), &names, &table, "").unwrap();
    println!("clip={clip:?} new row={:?}", rows[0]);
    assert_eq!(rows[0][1], Value::Text("bob".into()), "name");
    assert_eq!(rows[0][2], Value::Text("b@x.io".into()), "email");
}

#[test]
fn r1_hidden_column_paste_rows_leaves_default() {
    let mut a = GridModel::new(
        vec![Column::new("id"), Column::new("name"), Column::new("email")],
        vec![vec![
            Some("7".into()),
            Some("bob".into()),
            Some("b@x.io".into()),
        ]],
    );
    a.columns[1].hidden = true;
    a.click(0, Some(0), Modifiers::default());
    let clip = a.selected_rows_tsv();
    let shown: Vec<usize> = a.visible_columns().collect();
    let rows = rows_by_layout(&parse_tsv(&clip), &shown, 3).unwrap();
    assert_eq!(rows[0][0], Value::Text("7".into()));
    assert!(
        matches!(rows[0][1], Value::Unset { .. }),
        "{:?}",
        rows[0][1]
    );
    assert_eq!(rows[0][2], Value::Text("b@x.io".into()));
}

#[test]
fn r1_refusal_is_all_or_nothing() {
    let block = vec![
        vec!["a".to_string(), "b".into()],
        vec!["c".into(), "d".into()],
    ];
    // Last cell not editable -> nothing.
    let r = plan_paste(
        &block,
        &PasteTarget {
            rows: &[0],
            columns: &[0],
            all_rows: &[0, 1],
            all_columns: &[0, 1],
        },
        |r, c| !(r == 1 && c == 1),
        |_| false,
        |_| false,
    );
    assert!(r.is_err());
    // Second row deleted -> nothing.
    let r = plan_paste(
        &block,
        &PasteTarget {
            rows: &[0],
            columns: &[0],
            all_rows: &[0, 1],
            all_columns: &[0, 1],
        },
        |_, _| true,
        |r| r == 1,
        |_| false,
    );
    assert!(r.is_err());
    // Taller than the remaining rows -> DoesNotFit.
    let r = plan_paste(
        &block,
        &PasteTarget {
            rows: &[1],
            columns: &[0],
            all_rows: &[0, 1],
            all_columns: &[0, 1],
        },
        |_, _| true,
        |_| false,
        |_| false,
    );
    assert!(r.is_err());
}

#[test]
fn r1_parse_speed_large_clipboard() {
    let line = "\"open quote never closes\tvalue ไทย\n";
    let text = line.repeat(200_000); // ~8 MB
    let t = std::time::Instant::now();
    let rows = parse_tsv(&text);
    let ms = t.elapsed().as_millis();
    println!("rows={} ms={ms}", rows.len());
    assert_eq!(rows.len(), 200_000);
    assert!(ms < 5000, "parse took {ms} ms");
}

#[test]
fn r1_copied_column_missing_in_the_target_is_refused() {
    let mut a = GridModel::new(
        vec![Column::new("id"), Column::new("nick")],
        vec![vec![Some("7".into()), Some("bo".into())]],
    );
    a.click(0, Some(0), Modifiers::default());
    let table_b: Vec<String> = ["id", "name"].map(String::from).to_vec();
    let refused = rows_by_names(
        &parse_tsv(&a.selected_rows_tsv()),
        &a.copied_row_columns(),
        &table_b,
        "zixma.people",
    );
    assert!(refused.unwrap_err().contains("nick"));
}
