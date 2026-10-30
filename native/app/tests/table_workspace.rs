use anyhow::Result;
use telecat_app::{commit::execute_reviewed_part, table_model::*, workspace_model::*};
use telecat_core::{changes::*, types::*};
use telecat_db::Session;
fn config(d: Driver) -> ConnectionConfig {
    let mut c = empty_connection(d);
    c.id = "shell-m4".into();
    c.host = "127.0.0.1".into();
    c.port = if d == Driver::Mysql { 3307 } else { 5434 };
    c.user = if d == Driver::Mysql {
        "root"
    } else {
        "postgres"
    }
    .into();
    c.database = if d == Driver::Mysql {
        "zixma"
    } else {
        "main_db"
    }
    .into();
    c
}
fn table(d: Driver) -> TableRef {
    TableRef {
        schema: if d == Driver::Mysql {
            "zixma"
        } else {
            "public"
        }
        .into(),
        name: "telecat_shell_m4".into(),
    }
}
#[test]
fn edit_buffer_undo_delete_default_null_and_redo() {
    let mut edit = EditBuffer::default();
    edit.reset(
        QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![Some("1".into()), Some("ไทย 😺".into())]],
            ..Default::default()
        },
        Some(&[vec!["11".into(), "(0,1)".into()]]),
    );
    edit.set_cell(0, 1, Cell::Null);
    assert!(edit.dirty());
    assert!(edit.undo());
    assert!(!edit.dirty());
    assert!(edit.redo());
    edit.add(vec![Cell::unset(), Cell::Text("".into())]);
    edit.delete(&[0, 1]);
    assert_eq!(edit.rows.len(), 1);
    assert!(edit.rows[0].deleted);
    assert_eq!(edit.rows[0].rid.as_ref().unwrap()[1], "(0,1)");
    edit.discard();
    assert!(!edit.dirty());
    assert!(edit.undo());
    assert!(edit.dirty());
    for _ in 0..205 {
        edit.set_cell(0, 1, Cell::Text("value".into()));
    }
    let mut undos = 0;
    while edit.undo() {
        undos += 1;
    }
    assert_eq!(undos, 200);
}
#[test]
fn filters_keep_drafts_separate_and_order_new_tables_before_operations() -> Result<()> {
    let d = Driver::Mysql;
    let mut t = TableEditor::new(table(d), d, false);
    t.filters = vec![
        Filter {
            enabled: false,
            column: "name".into(),
            value: "ธนาคาร😺".into(),
            ..new_filter()
        },
        Filter {
            value: "second".into(),
            ..new_filter()
        },
    ];
    t.apply_filters(Combine::Or, None);
    assert_eq!(t.applied.len(), 1);
    assert_eq!(t.combine, Combine::Or);
    t.apply_filters(Combine::Or, Some(0));
    assert!(t.applied[0].enabled);
    assert_eq!(t.combine, Combine::And);
    assert!(!t.filters[0].enabled);
    t.filters[0].value = "draft".into();
    assert_eq!(t.applied[0].value, "ธนาคาร😺");
    let mut state = WorkspaceState::new(config(d));
    let mut renamed = t.clone();
    renamed.name = "renamed".into();
    state.open_table(t.table.clone());
    state.current_mut().table = Some(renamed);
    let new = TableRef {
        schema: "zixma".into(),
        name: "fresh".into(),
    };
    let fresh_id = state.open_table(new.clone());
    state.current_mut().table = Some(TableEditor::new(new, d, true));
    let object = DbObject {
        table: t.table,
        kind: ObjectKind::Table,
        signature: None,
    };
    state.operations = vec![
        StagedOperation {
            object: object.clone(),
            truncate: false,
        },
        StagedOperation {
            object,
            truncate: true,
        },
    ];
    let units = state.collect_changes()?;
    assert!(matches!(units[0].target,CommitTarget::Table(id,Part::Structure) if id==fresh_id));
    assert!(!units[0].transaction);
    assert!(
        units[units.len() - 2].statements[0]
            .sql
            .starts_with("TRUNCATE")
    );
    assert!(units.last().unwrap().statements[0].sql.starts_with("DROP"));
    Ok(())
}
async fn commit(state: &mut WorkspaceState, s: &Session) -> Result<()> {
    for unit in state.collect_changes()? {
        execute_reviewed_part(s, &unit.statements, unit.transaction, |_| async { false }).await?;
        state.committed(&unit.target);
    }
    Ok(())
}
async fn live(d: Driver) -> Result<()> {
    let s = Session::connect_test(&config(d), "secret").await?;
    assert!(s.verify_password("secret"));
    assert!(!s.verify_password(""));
    assert!(!s.verify_password("Secret"));
    assert!(!s.verify_password("secret\0"));
    let table = table(d);
    let qualified = qualified(d, &table);
    s.query(&format!("DROP TABLE IF EXISTS {qualified}"))
        .await?;
    let mut state = WorkspaceState::new(config(d));
    state.open_table(table.clone());
    let mut editor = TableEditor::new(table.clone(), d, true);
    editor.add_row(d);
    editor.structure.set_cell(1, 1, Cell::Text("name".into()));
    state.current_mut().table = Some(editor);
    let units = state.collect_changes()?;
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].transaction, d == Driver::Postgres);
    commit(&mut state, &s).await?;
    assert_eq!(s.structure(&table).await?.rows.len(), 2);
    s.query(&format!(
        "INSERT INTO {qualified} (name) VALUES ('ธนาคารกรุงเทพ 😺')"
    ))
    .await?;
    let req = TableDataRequest {
        table: table.clone(),
        filters: vec![],
        combine: Combine::And,
        order_by: None,
        limit: 1000,
        offset: 0,
    };
    let data = s.table_data(&req).await?;
    let t = state.current_mut().table.as_mut().unwrap();
    t.load_data(&data);
    t.structure.reset(s.structure(&table).await?, None);
    t.index.reset(s.indexes(&table).await?, None);
    t.data.set_cell(0, 1, Cell::Text("edited".into()));
    t.part = Part::Structure;
    t.add_row(d);
    t.structure.set_cell(2, 1, Cell::Text("extra".into()));
    t.part = Part::Index;
    t.add_row(d);
    let last = t.index.rows.len() - 1;
    t.index
        .set_cell(last, 0, Cell::Text("shell_name_index".into()));
    t.index.set_cell(last, 3, Cell::Text("name".into()));
    let units = state.collect_changes()?;
    assert_eq!(units.len(), 3);
    assert!(units[0].transaction);
    assert_eq!(units[1].transaction, d == Driver::Postgres);
    // Commit data then structure, fail the next part, and retain only the failed index edits.
    for unit in units.iter().take(2) {
        execute_reviewed_part(&s, &unit.statements, unit.transaction, |_| async { false }).await?;
        state.committed(&unit.target);
    }
    let mut invalid = units[2].statements.clone();
    invalid[0].sql = invalid[0]
        .sql
        .replace("name)", "no_such_column)")
        .replace("`name`", "`no_such_column`")
        .replace("\"name\"", "\"no_such_column\"");
    assert!(
        execute_reviewed_part(&s, &invalid, units[2].transaction, |_| async { false })
            .await
            .is_err()
    );
    let t = state.current().table.as_ref().unwrap();
    assert!(!t.data.dirty());
    assert!(!t.structure.dirty());
    assert!(t.index.dirty());
    assert_eq!(s.structure(&table).await?.rows.len(), 3);
    assert_eq!(
        s.query(&format!("SELECT name FROM {qualified}"))
            .await?
            .rows[0][0]
            .as_deref(),
        Some("edited")
    );
    commit(&mut state, &s).await?;
    assert!(!state.has_changes());
    assert!(
        s.indexes(&table)
            .await?
            .rows
            .iter()
            .any(|r| r[0].as_deref() == Some("shell_name_index"))
    );
    // Data guard mismatch must still roll back through the same controller used by ⌘S.
    let t = state.current_mut().table.as_mut().unwrap();
    t.load_data(&s.table_data(&req).await?);
    t.data.set_cell(0, 1, Cell::Text("must roll back".into()));
    let mut guarded = state.collect_changes()?[0].statements.clone();
    guarded[0].expect = Some(2);
    assert!(
        execute_reviewed_part(&s, &guarded, true, |_| async { false })
            .await
            .is_err()
    );
    assert_eq!(
        s.query(&format!("SELECT name FROM {qualified}"))
            .await?
            .rows[0][0]
            .as_deref(),
        Some("edited")
    );
    state.current_mut().table.as_mut().unwrap().data.discard();
    state.operations.push(StagedOperation {
        object: DbObject {
            table: table.clone(),
            kind: ObjectKind::Table,
            signature: None,
        },
        truncate: true,
    });
    commit(&mut state, &s).await?;
    assert_eq!(s.table_data(&req).await?.total, 0);
    state.operations.push(StagedOperation {
        object: DbObject {
            table: table.clone(),
            kind: ObjectKind::Table,
            signature: None,
        },
        truncate: false,
    });
    commit(&mut state, &s).await?;
    assert!(!s.list_objects().await?.iter().any(|o| o.table == table));
    assert!(!state.has_changes());
    if d == Driver::Mysql {
        let name = "telecat_shell_m4_myisam";
        s.query(&format!("DROP TABLE IF EXISTS {name}")).await?;
        s.query(&format!(
            "CREATE TABLE {name} (id INT PRIMARY KEY, value INT) ENGINE=MyISAM"
        ))
        .await?;
        s.query(&format!("INSERT INTO {name} VALUES (1,1)")).await?;
        let mut st = Stmt::new(format!("UPDATE {name} SET value=2 WHERE id=1"));
        st.expect = Some(1);
        assert!(
            execute_reviewed_part(&s, &[st.clone()], true, |review| async move {
                assert!(!review.non_transactional.is_empty());
                false
            })
            .await
            .is_err()
        );
        assert_eq!(
            s.query(&format!("SELECT value FROM {name}")).await?.rows[0][0].as_deref(),
            Some("1")
        );
        st.expect = Some(2);
        assert!(
            execute_reviewed_part(&s, &[st.clone()], true, |_| async { true })
                .await
                .is_err()
        );
        assert_eq!(
            s.query(&format!("SELECT value FROM {name}")).await?.rows[0][0].as_deref(),
            Some("1")
        );
        st.expect = Some(1);
        execute_reviewed_part(&s, &[st], true, |_| async { true }).await?;
        assert_eq!(
            s.query(&format!("SELECT value FROM {name}")).await?.rows[0][0].as_deref(),
            Some("2")
        );
        s.query(&format!("DROP TABLE {name}")).await?;
    }
    s.close().await;
    Ok(())
}
#[tokio::test]
async fn mysql_table_commits_and_partial_failure() -> Result<()> {
    live(Driver::Mysql).await
}
#[tokio::test]
async fn postgres_table_commits_and_partial_failure() -> Result<()> {
    live(Driver::Postgres).await
}
#[test]
fn safe_mode_changes_never_touch_credentials() -> Result<()> {
    use std::sync::Arc;
    struct NoKeychain;
    impl telecat_app::credentials::Keychain for NoKeychain {
        fn get(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>> {
            panic!("mode changes must not read Keychain")
        }
        fn set(&self, _: &str, _: &str, _: &[u8]) -> Result<()> {
            panic!("mode changes must not write Keychain")
        }
        fn delete(&self, _: &str, _: &str) -> Result<()> {
            panic!("mode changes must not delete Keychain")
        }
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.cache/table-profile-tests")
        .join(uuid::Uuid::new_v4().to_string());
    let profile = telecat_app::store::Profile::new(dir.clone(), Arc::new(NoKeychain));
    profile.seed(&StoreSnapshot {
        connections: vec![config(Driver::Mysql)],
        groups: vec![],
    })?;
    profile.set_safe_mode("shell-m4", SafeMode::Safe2)?;
    let reopened = telecat_app::store::Profile::new(dir.clone(), Arc::new(NoKeychain));
    assert_eq!(
        reopened.connection("shell-m4").unwrap().safe_mode,
        SafeMode::Safe2
    );
    assert!(reopened.set_safe_mode("missing", SafeMode::Silent).is_err());
    std::fs::remove_dir_all(dir)?;
    Ok(())
}
#[test]
fn only_new_columns_move_and_postgres_only_in_new_tables() {
    let mut pg = TableEditor::new(table(Driver::Postgres), Driver::Postgres, true);
    pg.add_row(Driver::Postgres);
    assert!(pg.column_movable(Driver::Postgres, 1));
    pg.move_column(Driver::Postgres, 1, None);
    let name = |t: &TableEditor, i: usize| {
        let col = t
            .structure
            .result
            .columns
            .iter()
            .position(|c| c == "column_name")
            .unwrap();
        t.structure.rows[i].cur[col].value().unwrap().to_owned()
    };
    assert_eq!(name(&pg, 0), "column_2");
    pg.is_new = false;
    assert!(!pg.column_movable(Driver::Postgres, 0));
    let mut my = TableEditor::new(table(Driver::Mysql), Driver::Mysql, false);
    my.structure.reset(
        QueryResult {
            columns: structure_columns(Driver::Mysql),
            rows: vec![
                structure_columns(Driver::Mysql)
                    .iter()
                    .map(|c| Some(if c == "column_name" { "id" } else { "" }.into()))
                    .collect(),
            ],
            ..Default::default()
        },
        None,
    );
    my.part = Part::Structure;
    my.add_row(Driver::Mysql);
    assert!(
        !my.column_movable(Driver::Mysql, 0),
        "a saved column stays put"
    );
    my.move_column(Driver::Mysql, 0, Some(1));
    assert_eq!(name(&my, 0), "id");
    my.move_column(Driver::Mysql, 1, None);
    assert_eq!(name(&my, 0), "column_2");
    assert!(my.structure.undo());
    assert_eq!(name(&my, 1), "column_2");
}
/// New MySQL columns land where they sit in the list, including after a column renamed in
/// the same save.
#[tokio::test]
async fn mysql_new_columns_are_added_where_they_were_moved() -> Result<()> {
    let d = Driver::Mysql;
    let s = Session::connect_test(&config(d), "secret").await?;
    let table = TableRef {
        name: "telecat_column_order".into(),
        ..table(d)
    };
    let q = qualified(d, &table);
    s.query(&format!("DROP TABLE IF EXISTS {q}")).await?;
    s.query(&format!(
        "CREATE TABLE {q} (id int PRIMARY KEY, a int, b int)"
    ))
    .await?;
    let mut state = WorkspaceState::new(config(d));
    state.open_table(table.clone());
    let mut t = TableEditor::new(table.clone(), d, false);
    t.structure.reset(s.structure(&table).await?, None);
    let col = t
        .structure
        .result
        .columns
        .iter()
        .position(|c| c == "column_name")
        .unwrap();
    t.structure.set_cell(2, col, Cell::Text("b2".into()));
    t.part = Part::Structure;
    for name in ["first_one", "after_a", "after_b2"] {
        t.add_row(d);
        let last = t.structure.rows.len() - 1;
        t.structure.set_cell(last, col, Cell::Text(name.into()));
    }
    // id a b2 first_one after_a after_b2 → first_one id a after_a b2 after_b2
    t.move_column(d, 3, None);
    t.move_column(d, 4, Some(2));
    state.current_mut().table = Some(t);
    let units = state.collect_changes()?;
    let sql = &units[0].statements[0].sql;
    assert!(sql.contains("`first_one` varchar(255) NULL FIRST"), "{sql}");
    assert!(
        sql.contains("AFTER `a`") && sql.contains("AFTER `b2`"),
        "{sql}"
    );
    commit(&mut state, &s).await?;
    let names: Vec<_> = s
        .structure(&table)
        .await?
        .rows
        .iter()
        .map(|r| r[col].clone().unwrap())
        .collect();
    assert_eq!(names, ["first_one", "id", "a", "after_a", "b2", "after_b2"]);
    s.query(&format!("DROP TABLE {q}")).await?;
    Ok(())
}
