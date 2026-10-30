//! QA re-verify 2: can a query-tab export write a stale MySQL JSON value after a saved edit or
//! a saved delete? Mirrors app/src/workspace/export.rs (export_result, query branch), which picks the JSON text
//! with `export_json_text`:
//!   - shown rows come from the grid, which is built from `result_edit.data.result`
//!     (components.rs make_grid) with `real` = identity mapping here (no filter);
//!   - the JSON text comes from `tab.grid.result.json_text`, used when its column lengths equal
//!     `tab.grid.result.rows.len()`;
//!   - a cell keeps its shown value only when `result_edit.data.rows[row]` is missing, new, or
//!     `cell_modified`.
//! After a save, the workspace runs `r.data.accepted(); r.data.loaded = true` (workspace_model
//! committed, CommitTarget::Result) and nothing touches `tab.grid.result`.
use std::collections::{BTreeMap, BTreeSet};
use telecat_app::workspace_model::{ResultEditor, export_json_text};
use telecat_core::{
    changes::{Cell, IdentityKind, cell_modified},
    result_edit::{ResultEditPlan, ResultSources, SourceTable},
    types::*,
};

fn server_result() -> QueryResult {
    QueryResult {
        columns: vec!["id".into(), "j".into()],
        rows: vec![
            vec![Some("1".into()), Some("{\"v\": 1}".into())],
            vec![Some("2".into()), Some("{\"v\": 2}".into())],
            vec![Some("3".into()), Some("{\"v\": 3}".into())],
        ],
        json_text: Some(vec![(
            1,
            vec![
                Some("{\"v\": 1}".into()),
                Some("{\"v\": 2}".into()),
                Some("{\"v\": 3}".into()),
            ],
        )]),
        ..Default::default()
    }
}
fn plan() -> ResultEditPlan {
    ResultEditPlan::new(
        Driver::Mysql,
        ResultSources {
            tables: vec![SourceTable {
                table: TableRef {
                    schema: "s".into(),
                    name: "t".into(),
                },
                not_a_table: None,
                key: vec!["id".into()],
                key_kinds: vec![IdentityKind::Signed],
                read_only_columns: BTreeSet::new(),
                repeated: false,
                columns: BTreeMap::new(),
            }],
            columns: vec![Some((0, "id".into())), Some((0, "j".into()))],
        },
        &[false, false],
    )
}
/// The JSON column exactly as export_result would write it.
fn exported(grid: &QueryResult, r: &ResultEditor) -> Vec<Option<String>> {
    // The grid shows the staged rows' current values (components.rs make_grid / staged).
    let shown: Vec<Vec<Option<String>>> = r
        .data
        .rows
        .iter()
        .map(|x| x.cur.iter().map(|v| v.value().map(str::to_owned)).collect())
        .collect();
    let real: Vec<usize> = (0..shown.len()).collect();
    let json = export_json_text(grid, Some(r));
    shown
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let rr = real[i];
            let edited = r
                .data
                .rows
                .get(rr)
                .is_none_or(|x| x.orig.is_none() || cell_modified(x, 1));
            match json.and_then(|j| j[0].1.get(rr)) {
                Some(text) if !edited => text.clone(),
                _ => row[1].clone(),
            }
        })
        .collect()
}

#[test]
fn qa060r2_query_export_after_saved_edit_is_not_stale() {
    let grid = server_result();
    let mut r = ResultEditor::new(plan(), grid.clone(), 1);
    r.stage(1, 1, Cell::Text("{\"v\": 20}".into())).unwrap();
    assert_eq!(
        exported(&grid, &r)[1].as_deref(),
        Some("{\"v\": 20}"),
        "staged edit exports its value"
    );
    // ⌘S succeeded: workspace_model::committed(CommitTarget::Result)
    r.data.accepted();
    r.data.loaded = true;
    let shown: Vec<_> = r.data.result.rows.iter().map(|x| x[1].clone()).collect();
    let out = exported(&grid, &r);
    eprintln!("grid shows {shown:?}\nexport writes {out:?}");
    assert_eq!(
        out, shown,
        "export after a saved edit differs from what the grid shows"
    );
}

#[test]
fn qa060r2_query_export_after_saved_delete_is_not_shifted() {
    let grid = server_result();
    let mut r = ResultEditor::new(plan(), grid.clone(), 1);
    r.delete(&[0]).unwrap();
    r.data.accepted();
    r.data.loaded = true;
    let shown: Vec<_> = r.data.result.rows.iter().map(|x| x.clone()).collect();
    let out = exported(&grid, &r);
    eprintln!("grid shows {shown:?}\nexport writes j = {out:?}");
    let expected: Vec<_> = shown.iter().map(|x| x[1].clone()).collect();
    assert_eq!(
        out, expected,
        "export after a saved delete writes other rows' JSON"
    );
}
