//! Views, routines and triggers in the workspace model: what ⌘S may generate for each.
use telecat_app::workspace_model::{
    CommitTarget, RoutineEditor, StagedOperation, TabKind, WorkspaceState,
};
use telecat_core::changes::Cell;
use telecat_core::objects::{Trigger, TriggerEdit};
use telecat_core::types::*;

fn state(d: Driver) -> WorkspaceState {
    let mut c = empty_connection(d);
    c.database = "zixma".into();
    WorkspaceState::new(c)
}
fn object(schema: &str, name: &str, kind: ObjectKind) -> DbObject {
    DbObject::new(
        TableRef {
            schema: schema.into(),
            name: name.into(),
        },
        kind,
    )
}
#[test]
fn views_open_read_only_and_never_produce_structure_index_or_row_sql() {
    let mut s = state(Driver::Postgres);
    let mv = object("public", "totals", ObjectKind::MaterializedView);
    s.objects = vec![mv.clone()];
    s.open_table(mv.table.clone());
    let t = s.current_mut().table.as_mut().unwrap();
    assert_eq!(t.kind, ObjectKind::MaterializedView);
    assert!(t.read_only());
    let rows = t.structure.rows.len();
    t.part = telecat_app::table_model::Part::Structure;
    t.structure.loaded = true;
    t.add_row(Driver::Postgres);
    assert_eq!(t.structure.rows.len(), rows, "no column added to a view");
    // Even if an edit got staged some other way, nothing is generated for it.
    t.name = "renamed".into();
    assert!(
        s.collect_changes()
            .unwrap_err()
            .to_string()
            .contains("can't be edited")
    );
    let t = s.current_mut().table.as_mut().unwrap();
    t.name = t.table.name.clone();
    t.data.reset(
        QueryResult {
            columns: vec!["id".into()],
            rows: vec![vec![Some("1".into())]],
            ..Default::default()
        },
        None,
    );
    t.data.set_cell(0, 0, Cell::Text("2".into()));
    assert!(
        s.collect_changes()
            .unwrap_err()
            .to_string()
            .contains("rows can't be edited")
    );
}
#[test]
fn deleting_objects_is_keyed_by_the_whole_object() {
    let mut s = state(Driver::Postgres);
    let table = object("public", "same", ObjectKind::Table);
    let mut function = object("public", "same", ObjectKind::Function);
    function.signature = Some("a integer".into());
    let mv = object("public", "mv", ObjectKind::MaterializedView);
    s.objects = vec![table.clone(), function.clone(), mv.clone()];
    for o in [&function, &mv] {
        s.operations.push(StagedOperation {
            object: o.clone(),
            truncate: false,
        });
    }
    let sql: Vec<String> = s
        .collect_changes()
        .unwrap()
        .into_iter()
        .flat_map(|u| u.statements.into_iter().map(|s| s.sql))
        .collect();
    assert_eq!(
        sql,
        [
            r#"DROP FUNCTION "public"."same"(a integer);"#,
            r#"DROP MATERIALIZED VIEW "public"."mv";"#
        ]
    );
    // Saving the function's drop leaves the table of the same name alone.
    s.committed(&CommitTarget::Operation(StagedOperation {
        object: function.clone(),
        truncate: false,
    }));
    assert!(s.objects.contains(&table) && !s.objects.contains(&function));
    assert_eq!(s.operations.len(), 1);
}
#[test]
fn a_routine_tab_saves_its_edited_definition_and_is_clean_afterwards() {
    for d in [Driver::Mysql, Driver::Postgres] {
        let mut s = state(d);
        let schema = if d == Driver::Mysql {
            "zixma"
        } else {
            "public"
        };
        let mut f = object(schema, "f", ObjectKind::Function);
        if d == Driver::Postgres {
            f.signature = Some(String::new());
        }
        let id = s.open_routine(f.clone());
        assert_eq!(s.open_routine(f.clone()), id, "the open tab is reused");
        assert_eq!(s.current().kind, TabKind::Routine(f.clone()));
        let original = if d == Driver::Mysql {
            "CREATE FUNCTION `f`() RETURNS int RETURN 1;\n"
        } else {
            "CREATE OR REPLACE FUNCTION public.f()\n RETURNS integer\n LANGUAGE sql\nAS $function$select 1$function$;\n"
        };
        // Not loaded yet: nothing to save.
        assert!(!s.has_changes());
        let tab = s.current_mut();
        tab.routine = Some(RoutineEditor {
            original: Some(original.into()),
            settings: Default::default(),
        });
        tab.editor.text = original.replace('1', "2");
        assert!(s.has_changes());
        let units = s.collect_changes().unwrap();
        assert_eq!(units.len(), 1);
        assert!(matches!(units[0].target, CommitTarget::Routine(t) if t == id));
        if d == Driver::Mysql {
            // Tried under a temporary name first, then dropped and created.
            assert!(
                units[0].statements[0]
                    .sql
                    .starts_with("CREATE FUNCTION `f__telecat_check_")
            );
            assert_eq!(
                units[0].statements[2].sql,
                "DROP FUNCTION IF EXISTS `zixma`.`f`;"
            );
            assert!(!units[0].transaction);
            assert_eq!(units[0].restore.len(), 1);
        } else {
            assert_eq!(units[0].statements.len(), 1);
            assert!(units[0].transaction && units[0].restore.is_empty());
        }
        // A definition for something else is refused before anything is sent.
        let keep = s.current().editor.text.clone();
        s.current_mut().editor.text = "DROP TABLE users".into();
        assert!(s.collect_changes().is_err());
        s.current_mut().editor.text = keep;
        s.committed(&CommitTarget::Routine(id));
        assert!(!s.has_changes());
    }
}
#[test]
fn trigger_edits_save_one_by_one_and_settle() {
    let mut s = state(Driver::Mysql);
    let table = object("zixma", "orders", ObjectKind::Table);
    s.objects = vec![table.clone()];
    let id = s.open_table(table.table.clone());
    let old = Trigger {
        name: "a".into(),
        timing: "BEFORE".into(),
        event: "INSERT".into(),
        statement: "SET NEW.n = 1".into(),
        ..Default::default()
    };
    let mut new = old.clone();
    new.statement = "SET NEW.n = 2".into();
    s.current_mut().table.as_mut().unwrap().triggers.edits = Some(vec![TriggerEdit {
        key: "k1".into(),
        orig: Some(old),
        cur: new,
        deleted: false,
    }]);
    assert!(s.has_changes());
    let units = s.collect_changes().unwrap();
    assert_eq!(units.len(), 1);
    assert!(matches!(&units[0].target, CommitTarget::Trigger(t, k) if *t == id && k == "k1"));
    assert_eq!(units[0].statements[0].sql, "DROP TRIGGER `zixma`.`a`;");
    assert!(!units[0].restore.is_empty());
    s.committed(&units[0].target);
    assert!(!s.has_changes());
}
#[test]
fn a_rename_and_a_trigger_edit_in_one_save_target_the_new_name_after_the_rename() {
    let mut s = state(Driver::Mysql);
    let table = object("zixma", "orders", ObjectKind::Table);
    s.objects = vec![table.clone()];
    s.open_table(table.table.clone());
    let old = Trigger {
        name: "a".into(),
        timing: "BEFORE".into(),
        event: "INSERT".into(),
        statement: "SET NEW.n = 1".into(),
        ..Default::default()
    };
    let mut new = old.clone();
    new.event = "UPDATE".into();
    let editor = s.current_mut().table.as_mut().unwrap();
    editor.name = "orders_v2".into();
    editor.triggers.edits = Some(vec![TriggerEdit {
        key: "k".into(),
        orig: Some(old),
        cur: new,
        deleted: false,
    }]);
    let units = s.collect_changes().unwrap();
    let rename = units
        .iter()
        .position(|u| u.statements.iter().any(|st| st.sql.contains("RENAME")))
        .expect("rename");
    let trigger = units
        .iter()
        .position(|u| matches!(u.target, CommitTarget::Trigger(..)))
        .expect("trigger");
    assert!(rename < trigger, "the rename runs first");
    let t = &units[trigger];
    assert!(
        t.statements[1].sql.contains("ON `zixma`.`orders_v2`"),
        "{}",
        t.statements[1].sql
    );
    assert!(
        t.restore[0].sql.contains("ON `zixma`.`orders_v2`"),
        "{}",
        t.restore[0].sql
    );
}
