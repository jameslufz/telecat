//! QA 0.8.0 fix round 1 (BUG-002, BUG-003): ⇧⌘C / ⌘C clipboard text fed back into Paste to Selected Cells (⇧⌘V),
//! exactly as the app does: GridModel::selected_*_tsv -> parse_tsv -> plan_paste.
//! Kept from the QA probe (.claude-qa/qa080/qa080_paste_probe.rs).
use telecat_core::changes::Cell as Value;
use telecat_core::paste::{PasteTarget, plan_paste};
use telecat_grid::*;

fn model() -> GridModel {
    GridModel::new(
        vec![Column::new("id"), Column::new("name"), Column::new("note")],
        vec![
            vec![
                Some("1".into()),
                Some("alice".into()),
                Some("line1\nline2".into()),
            ],
            vec![Some("2".into()), Some("bob".into()), Some("keep-2".into())],
            vec![
                Some("3".into()),
                Some("carol".into()),
                Some("keep-3".into()),
            ],
        ],
    )
}
fn plan(m: &GridModel, text: &str, rows: &[usize], cols: &[usize]) -> Vec<(usize, usize, Value)> {
    let all_rows: Vec<usize> = (0..m.rows.len()).collect();
    let all_cols: Vec<usize> = m.visible_columns().collect();
    plan_paste(
        &parse_tsv(text),
        &PasteTarget {
            rows,
            columns: cols,
            all_rows: &all_rows,
            all_columns: &all_cols,
        },
        |_, _| true,
        |_| false,
        |_| false,
    )
    .map(|p| p.cells)
    .unwrap_or_else(|e| panic!("refused: {e:?}"))
}

#[test]
fn qa080_one_multiline_cell_pasted_into_one_cell() {
    let mut m = model();
    // ⇧⌘C on row 0 "note" (a multi-line value).
    m.click(0, Some(2), Modifiers::default());
    let clip = m.selected_cells_tsv();
    println!("clipboard = {clip:?}");
    // Select row 1 "note" only and ⇧⌘V.
    let cells = plan(&m, &clip, &[1], &[2]);
    println!("staged = {cells:?}");
    // Expected: exactly one cell, row 1 note = "line1\nline2".
    assert_eq!(cells, vec![(1, 2, Value::Text("line1\nline2".into()))]);
}

#[test]
fn qa080_one_tab_cell_pasted_into_one_cell() {
    let mut m = GridModel::new(
        vec![Column::new("id"), Column::new("a"), Column::new("b")],
        vec![
            vec![Some("1".into()), Some("x\ty".into()), Some("b1".into())],
            vec![Some("2".into()), Some("a2".into()), Some("b2".into())],
        ],
    );
    m.click(0, Some(1), Modifiers::default());
    let clip = m.selected_cells_tsv();
    let cells = plan(&m, &clip, &[1], &[1]);
    println!("staged = {cells:?}");
    assert_eq!(cells, vec![(1, 1, Value::Text("x\ty".into()))]);
}

#[test]
fn qa080_copy_row_then_paste_cells_after_reorder() {
    let mut m = model();
    // User dragged "note" to the front: display order note, id, name.
    m.set_order(vec![2, 0, 1]);
    // ⌘C (Copy Rows) on row 1, then select row 2's first shown cell and ⇧⌘V.
    m.click(1, Some(2), Modifiers::default());
    let clip = m.selected_rows_tsv();
    println!("clipboard = {clip:?}");
    let cells = plan(&m, &clip, &[2], &[2]);
    println!("staged = {cells:?}");
    // Each value must land in its own column: id->0, name->1, note->2.
    for (_, col, v) in &cells {
        let want = match col {
            0 => "2",
            1 => "bob",
            _ => "keep-2",
        };
        assert_eq!(v, &Value::Text(want.into()), "column {col}");
    }
}

#[test]
fn qa080_copy_row_then_paste_cells_with_hidden_column() {
    let mut m = model();
    m.columns[1].hidden = true;
    m.click(1, Some(0), Modifiers::default());
    let clip = m.selected_rows_tsv();
    let all_rows: Vec<usize> = (0..m.rows.len()).collect();
    let all_cols: Vec<usize> = m.visible_columns().collect();
    let p = plan_paste(
        &parse_tsv(&clip),
        &PasteTarget {
            rows: &[2],
            columns: &[0],
            all_rows: &all_rows,
            all_columns: &all_cols,
        },
        |_, _| true,
        |_| false,
        |_| false,
    )
    .unwrap();
    println!("clipboard = {clip:?} staged = {:?}", p.cells);
    assert_eq!(
        p.cells.len(),
        2,
        "the hidden column is neither copied nor pasted"
    );
    for (_, col, v) in &p.cells {
        let want = match col {
            0 => "2",
            1 => "bob",
            _ => "keep-2",
        };
        assert_eq!(v, &Value::Text(want.into()), "column {col}");
    }
}

#[test]
fn qa080_a_whole_row_pasted_from_a_later_column_is_refused_not_shifted() {
    let mut m = model();
    m.click(1, Some(0), Modifiers::default());
    let clip = m.selected_rows_tsv();
    let all_rows: Vec<usize> = (0..m.rows.len()).collect();
    let all_cols: Vec<usize> = m.visible_columns().collect();
    let refused = plan_paste(
        &parse_tsv(&clip),
        &PasteTarget {
            rows: &[2],
            columns: &[1],
            all_rows: &all_rows,
            all_columns: &all_cols,
        },
        |_, _| true,
        |_| false,
        |_| false,
    );
    assert_eq!(
        refused,
        Err(telecat_core::paste::PasteRefusal::DoesNotFit {
            rows: 1,
            columns: 3
        })
    );
}

#[test]
fn qa080_copied_rows_are_the_shown_columns_quoted_where_needed() {
    let mut m = model();
    m.columns[1].hidden = true;
    m.set_order(vec![2, 1, 0]);
    m.click(0, Some(0), Modifiers::default());
    assert_eq!(m.selected_rows_tsv(), "\"line1\nline2\"\t1");
    assert_eq!(
        parse_tsv(&m.selected_rows_tsv()),
        vec![vec!["line1\nline2", "1"]]
    );
}
