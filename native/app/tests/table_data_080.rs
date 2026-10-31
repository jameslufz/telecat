//! Telecat 0.8 Table Data model: multi-cell staging, Go Back / Go Forward with table views,
//! remembered table state and data-column editability.
use std::sync::Arc;
use telecat_app::{
    credentials::MemoryKeychain, store::Profile, table_model::TableEditor, workspace_model::*,
};
use telecat_core::{
    changes::Cell,
    columns::{ColumnDetail, TableColumns, ValueKind},
    filters::DEFAULT_PAGE_ROWS,
    table_state::{SavedTable, table_key},
    types::*,
};

fn config() -> ConnectionConfig {
    let mut c = empty_connection(Driver::Mysql);
    c.id = "t080".into();
    c.database = "zixma".into();
    c
}
fn table(name: &str) -> TableRef {
    TableRef {
        schema: "zixma".into(),
        name: name.into(),
    }
}
fn rule(column: &str, value: &str) -> Filter {
    Filter {
        enabled: true,
        column: column.into(),
        op: FilterOp::Eq,
        value: value.into(),
        value2: None,
    }
}
fn loaded(name: &str) -> TableEditor {
    let mut t = TableEditor::new(table(name), Driver::Mysql, false);
    t.load_data(&TableDataResult {
        result: QueryResult {
            columns: vec!["id".into(), "name".into(), "twice".into(), "blob".into()],
            binary: Some(vec![false, false, false, true]),
            rows: vec![
                vec![Some("1".into()), Some("a".into()), Some("2".into()), None],
                vec![Some("2".into()), Some("b".into()), Some("4".into()), None],
            ],
            ..Default::default()
        },
        total: 2,
        estimated: false,
        primary_key: vec!["id".into()],
        row_ids: None,
    });
    t
}

#[test]
fn several_cells_are_one_undo_step() {
    let mut t = loaded("t");
    t.data.set_cells(vec![
        (0, 1, Cell::Text("x".into())),
        (1, 1, Cell::Null),
        (9, 1, Cell::Null),
    ]);
    assert_eq!(t.data.rows[0].cur[1], Cell::Text("x".into()));
    assert_eq!(t.data.rows[1].cur[1], Cell::Null);
    assert!(t.data.undo());
    assert_eq!(t.data.rows[0].cur[1], Cell::Text("a".into()));
    assert_eq!(t.data.rows[1].cur[1], Cell::Text("b".into()));
    assert!(!t.data.undo());
    // Nothing that exists: no undo step.
    t.data.set_cells(vec![(5, 0, Cell::Null)]);
    assert!(!t.data.undo());
}

#[test]
fn generated_binary_and_view_columns_are_not_editable() {
    let mut t = loaded("t");
    assert!(t.data_column_editable(1));
    assert!(!t.data_column_editable(3));
    assert!(t.data_column_editable(2));
    t.columns = Some(TableColumns {
        columns: vec![ColumnDetail {
            name: "twice".into(),
            data_type: "int".into(),
            kind: ValueKind::Other,
            values: vec![],
            nullable: true,
            generated: true,
        }],
        foreign_keys: vec![],
    });
    assert!(!t.data_column_editable(2));
    assert!(t.data_column_refusal(2).contains("generated"));
    assert!(t.data_column_refusal(3).contains("binary"));
    t.kind = ObjectKind::View;
    assert!(!t.data_column_editable(1));
    assert!(!t.data_column_editable(99));
}

#[test]
fn table_tabs_page_300_rows_and_new_rules_use_equals() {
    let mut s = WorkspaceState::new(config());
    s.open_table(table("t"));
    assert_eq!(s.current().limit, DEFAULT_PAGE_ROWS);
    assert_eq!(
        s.current().table.as_ref().unwrap().filters[0].op,
        FilterOp::Eq
    );
    // Query tabs keep their own row limit.
    s.open_query("q", "");
    assert_eq!(s.current().limit, 1000);
}

#[test]
fn go_back_restores_the_tab_and_its_filters() {
    let mut s = WorkspaceState::new(config());
    let child = s.open_table(table("child"));
    let parent = s.open_table(table("parent"));
    // A foreign key filtered the parent table.
    s.remember_current_view();
    {
        let t = s.current_mut().table.as_mut().unwrap();
        t.filters = vec![rule("id", "5")];
        t.apply_filters(Combine::And, None);
    }
    s.visit_current_view();
    // Another key to the same table: a new step in the same tab.
    s.remember_current_view();
    {
        let t = s.current_mut().table.as_mut().unwrap();
        t.filters = vec![rule("id", "7")];
        t.apply_filters(Combine::And, None);
    }
    s.visit_current_view();
    assert!(s.can_back());
    let back = s.navigate(false).expect("the earlier filters");
    assert_eq!(s.active, parent);
    assert_eq!(back.applied, vec![rule("id", "5")]);
    s.current_mut().table.as_mut().unwrap().restore_view(&back);
    // Back to the parent as first opened (no filters), then the child.
    let first = s.navigate(false).expect("unfiltered");
    assert!(first.applied.is_empty());
    s.current_mut().table.as_mut().unwrap().restore_view(&first);
    assert_eq!(s.navigate(false), None);
    assert_eq!(s.active, child);
    // Forward returns to the parent, which still shows the view it was left with.
    assert_eq!(s.navigate(true), None);
    assert_eq!(s.active, parent);
    assert!(s.current().table.as_ref().unwrap().applied.is_empty());
    // Closing the parent forgets it; the history never points at a closed tab.
    assert!(s.close_tab(parent));
    while s.can_back() {
        s.navigate(false);
        assert!(s.tabs.iter().any(|t| t.id == s.active));
    }
}

#[test]
fn table_state_is_remembered_per_connection_database_and_table() {
    let root = std::env::temp_dir()
        .join("telecat-ui-profiles")
        .join(format!("t080-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let profile = Profile::new(root.clone(), Arc::new(MemoryKeychain::default()));
    let key = table_key("conn", "zixma", &table("users")).unwrap();
    assert!(profile.saved_table(&key).is_none());
    let saved = SavedTable {
        filters: vec![rule("name", "bob")],
        combine: Combine::Or,
        show_filters: true,
        widths: [("name".to_owned(), 240.)].into(),
        order: vec!["name".into(), "id".into()],
        used: 0,
    };
    profile.save_table(&key, saved.clone()).unwrap();
    let back = profile.saved_table(&key).unwrap();
    assert_eq!(back.filters, saved.filters);
    assert_eq!(back.combine, Combine::Or);
    assert_eq!(back.widths, saved.widths);
    assert!(back.used > 0);
    let other = table_key("conn", "other_db", &table("users")).unwrap();
    assert!(profile.saved_table(&other).is_none());
    // A damaged file never blocks saving again.
    std::fs::write(root.join(telecat_app::store::TABLES_FILE), b"{not json").unwrap();
    assert!(profile.saved_table(&key).is_none());
    profile.save_table(&key, saved).unwrap();
    assert!(profile.saved_table(&key).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn result_edits_stage_several_cells_or_none() {
    use telecat_core::result_edit::ResultEditPlan;
    let result = QueryResult {
        columns: vec!["a".into()],
        rows: vec![vec![Some("1".into())]],
        ..Default::default()
    };
    let mut r = ResultEditor::new(
        ResultEditPlan::read_only(Driver::Mysql, 1, "no key"),
        result,
        1,
    );
    assert!(r.stage_cells(vec![(0, 0, Cell::Null)]).is_err());
    assert!(!r.data.dirty());
    assert!(!r.data.undo());
}
