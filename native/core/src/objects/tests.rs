use super::*;
use crate::changes::table_op_statement;

fn routine(schema: &str, name: &str, kind: ObjectKind) -> DbObject {
    DbObject::new(
        TableRef {
            schema: schema.into(),
            name: name.into(),
        },
        kind,
    )
}
fn sqls(s: &ObjectSave) -> Vec<&str> {
    s.statements.iter().map(|s| s.sql.as_str()).collect()
}
const MY_FN: &str = "CREATE DEFINER=`root`@`%` FUNCTION `add_one`(x INT) RETURNS int\n    DETERMINISTIC\nBEGIN\n  DECLARE y INT;\n  SET y = x + 1;\n  RETURN y;\nEND";
const PG_FN: &str = "CREATE OR REPLACE FUNCTION public.add_one(x integer)\n RETURNS integer\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n  RETURN x + 1;\nEND;\n$function$\n";

#[test]
fn mysql_routine_save_tries_the_new_definition_first_then_drops_and_creates() {
    let f = routine("zixma", "add_one", ObjectKind::Function);
    let edited = MY_FN.replace("x + 1", "x + 2");
    let settings = CreateSettings {
        sql_mode: Some("NO_ENGINE_SUBSTITUTION".into()),
        collation_connection: Some("utf8mb4_0900_ai_ci".into()),
    };
    let save = routine_save(Driver::Mysql, &f, MY_FN, &format!("{edited};\n"), &settings).unwrap();
    let sql = sqls(&save);
    assert_eq!(sql.len(), 4, "{sql:?}");
    // 1–2: the new definition under a temporary name, dropped again: nothing is touched if the
    // server refuses it (syntax, DEFINER privileges, binary-log safety).
    let temp = sql[0]
        .strip_prefix("CREATE DEFINER=`root`@`%` FUNCTION `")
        .and_then(|r| r.split_once('`'))
        .map(|(name, _)| name.to_owned())
        .expect("temporary CREATE");
    assert!(
        temp.starts_with("add_one__telecat_check_") && temp.len() <= 64,
        "{temp}"
    );
    assert!(
        sql[0].contains("x + 2") && !sql[0].contains("`add_one`"),
        "{}",
        sql[0]
    );
    assert_eq!(sql[1], format!("DROP FUNCTION `zixma`.`{temp}`;"));
    // 3–4: the real replace.
    assert_eq!(sql[2], "DROP FUNCTION IF EXISTS `zixma`.`add_one`;");
    assert_eq!(sql[3], format!("{edited};"));
    assert!(!save.transaction);
    assert_eq!(save.restore, vec![Stmt::new(format!("{MY_FN};"))]);
    // Both run under the routine's own settings.
    assert_eq!(
        save.settings,
        vec![
            ("sql_mode".to_owned(), "NO_ENGINE_SUBSTITUTION".to_owned()),
            (
                "collation_connection".to_owned(),
                "utf8mb4_0900_ai_ci".to_owned()
            )
        ]
    );
    // Two saves never share a temporary name.
    let again = routine_save(Driver::Mysql, &f, MY_FN, MY_FN, &settings).unwrap();
    assert_ne!(again.statements[0].sql, save.statements[0].sql);
    // Comments in the header, a schema and a long name: the temporary name lands on the name.
    let long = routine("zixma", &"n".repeat(64), ObjectKind::Procedure);
    let def = format!(
        "CREATE /* c */ DEFINER = 'a b'@'%' PROCEDURE -- x\n `zixma` . `{}`() SELECT 1",
        "n".repeat(64)
    );
    let save = routine_save(Driver::Mysql, &long, &def, &def, &CreateSettings::default()).unwrap();
    assert!(
        save.statements[0]
            .sql
            .starts_with("CREATE /* c */ DEFINER = 'a b'@'%' PROCEDURE -- x\n `zixma` . `nnn")
    );
    assert!(save.settings.is_empty());
    // Without a readable original nothing could be put back: refused.
    let e = routine_save(Driver::Mysql, &f, "garbage; more", MY_FN, &settings).unwrap_err();
    assert!(e.to_string().contains("won't replace it"), "{e}");
}
#[test]
fn mysql_trigger_bodies_come_from_show_create_as_written() {
    for (create, body) in [
        (
            r"CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE UPDATE ON `x` FOR EACH ROW SET NEW.note = CONCAT(NEW.note, ' it\'s')",
            r"SET NEW.note = CONCAT(NEW.note, ' it\'s')",
        ),
        (
            r"CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW FOLLOWS `a b` SET NEW.p = REPLACE(NEW.p, '/', '\\');",
            r"SET NEW.p = REPLACE(NEW.p, '/', '\\')",
        ),
        (
            "CREATE TRIGGER zixma.t AFTER DELETE ON zixma.`for each row` FOR EACH ROW PRECEDES other BEGIN\n  SET @x = 'FOR EACH ROW';\nEND",
            "BEGIN\n  SET @x = 'FOR EACH ROW';\nEND",
        ),
        (
            "CREATE DEFINER='u'@'h' TRIGGER `ไทย` BEFORE INSERT ON t FOR EACH ROW SET NEW.a = 'ก'",
            "SET NEW.a = 'ก'",
        ),
    ] {
        assert_eq!(
            parse_mysql_trigger_body(create).as_deref(),
            Some(body),
            "{create}"
        );
    }
    assert_eq!(parse_mysql_trigger_body("garbage"), None);
}
#[test]
fn mysql_trigger_edits_keep_definer_order_and_settings() {
    let table = TableRef {
        schema: "zixma".into(),
        name: "orders".into(),
    };
    let mk = |name: &str, order: u32, body: &str| Trigger {
        name: name.into(),
        timing: "BEFORE".into(),
        event: "INSERT".into(),
        statement: body.into(),
        definer: Some("app@%".into()),
        settings: CreateSettings {
            sql_mode: Some("NO_ENGINE_SUBSTITUTION".into()),
            collation_connection: None,
        },
        order: Some(order),
        neighbours: (None, None),
    };
    let (a, b, c) = (
        mk("a", 1, "SET NEW.t = 1"),
        mk("b", 2, "SET NEW.t = 2"),
        mk("c", 3, "SET NEW.t = 3"),
    );
    let edit = |o: &Trigger, cur: Trigger, key: &str| TriggerEdit {
        key: key.into(),
        orig: Some(o.clone()),
        cur,
        deleted: false,
    };
    let mut a2 = a.clone();
    a2.statement = "SET NEW.t = 10".into();
    let mut c2 = c.clone();
    c2.name = "c_renamed".into();
    // Listed out of order: the saves still go a, then c.
    let edits = vec![
        edit(&c, c2, "c"),
        edit(&b, b.clone(), "b"),
        edit(&a, a2, "a"),
    ];
    let saves = trigger_saves(Driver::Mysql, &table, ObjectKind::Table, &edits).unwrap();
    assert_eq!(
        saves.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        ["a", "c"]
    );
    let a_save = &saves[0].1;
    assert_eq!(
        a_save.statements[1].sql,
        "CREATE DEFINER=`app`@`%` TRIGGER `zixma`.`a` BEFORE INSERT ON `zixma`.`orders` FOR EACH ROW PRECEDES `b` SET NEW.t = 10;"
    );
    assert!(
        a_save.restore[0]
            .sql
            .contains("PRECEDES `b` SET NEW.t = 1;")
    );
    assert_eq!(
        a_save.settings,
        vec![("sql_mode".to_owned(), "NO_ENGINE_SUBSTITUTION".to_owned())]
    );
    // The DROP is refused first unless this account may create it again as app@%.
    let check = a_save.statements[0].check.as_ref().expect("create check");
    assert_eq!(check.params, vec![serde_json::json!("app@%"); 4]);
    assert!(check.sql.contains("SET_USER_ID") && check.sql.contains("'SYSTEM_USER'"));
    assert!(check.sql.contains("log_bin_trust_function_creators"));
    // One reason per result, each naming what is missing and how to edit it instead.
    for (n, missing) in [(2, "SET_USER_ID"), (3, "SYSTEM_USER"), (4, "SUPER")] {
        let line = check
            .message
            .lines()
            .find(|l| l.starts_with(&format!("when {n}: ")))
            .unwrap();
        assert!(line.contains(missing), "{line}");
    }
    assert!(check.message.contains("Edit it while connected as app@%"));
    let c_save = &saves[1].1;
    assert!(
        c_save.statements[1]
            .sql
            .contains("TRIGGER `zixma`.`c_renamed`")
    );
    assert!(
        c_save.statements[1]
            .sql
            .contains("FOR EACH ROW FOLLOWS `b` SET"),
        "{}",
        c_save.statements[1].sql
    );
    // A new trigger goes last, under the connection's settings and as the current account.
    let new = TriggerEdit {
        key: "n".into(),
        orig: None,
        cur: t("n", "BEFORE", "INSERT", "SET NEW.t = 0"),
        deleted: false,
    };
    let saves = trigger_saves(Driver::Mysql, &table, ObjectKind::Table, &[new]).unwrap();
    assert!(saves[0].1.settings.is_empty());
    assert_eq!(
        saves[0].1.statements[0].sql,
        "CREATE TRIGGER `zixma`.`n` BEFORE INSERT ON `zixma`.`orders` FOR EACH ROW SET NEW.t = 0;"
    );
    // Only the edited trigger in the save: its neighbours as read put it back in place,
    // unless the save drops or renames them.
    let mut lone = b.clone();
    lone.neighbours = (Some("a".into()), Some("c".into()));
    let mut lone2 = lone.clone();
    lone2.statement = "SET NEW.t = 20".into();
    let saves = trigger_saves(
        Driver::Mysql,
        &table,
        ObjectKind::Table,
        &[edit(&lone, lone2.clone(), "b")],
    )
    .unwrap();
    assert!(
        saves[0].1.statements[1].sql.contains("FOLLOWS `a` SET"),
        "{}",
        saves[0].1.statements[1].sql
    );
    let dropped_a = TriggerEdit {
        key: "a".into(),
        orig: Some(a.clone()),
        cur: a.clone(),
        deleted: true,
    };
    let saves = trigger_saves(
        Driver::Mysql,
        &table,
        ObjectKind::Table,
        &[dropped_a, edit(&lone, lone2, "b")],
    )
    .unwrap();
    assert!(
        saves[1].1.statements[1].sql.contains("PRECEDES `c` SET"),
        "{}",
        saves[1].1.statements[1].sql
    );
    // Moved to another event: no neighbours to keep.
    let mut moved = b.clone();
    moved.event = "UPDATE".into();
    let saves = trigger_saves(
        Driver::Mysql,
        &table,
        ObjectKind::Table,
        &[edit(&a, a.clone(), "a"), edit(&b, moved, "b")],
    )
    .unwrap();
    assert!(
        !saves[0].1.statements[1].sql.contains("FOLLOWS")
            && !saves[0].1.statements[1].sql.contains("PRECEDES")
    );
}
#[test]
fn mysql_routine_header_forms() {
    for (sql, kind, schema, name) in [
        (
            "CREATE FUNCTION f() RETURNS INT RETURN 1",
            ObjectKind::Function,
            None,
            "f",
        ),
        (
            "create procedure `db`.`p q`() select 1",
            ObjectKind::Procedure,
            Some("db"),
            "p q",
        ),
        (
            "CREATE DEFINER = 'a b'@'localhost' PROCEDURE p() SELECT 1",
            ObjectKind::Procedure,
            None,
            "p",
        ),
        (
            "CREATE DEFINER=CURRENT_USER() FUNCTION f() RETURNS INT RETURN 1",
            ObjectKind::Function,
            None,
            "f",
        ),
        (
            "CREATE DEFINER=root@localhost FUNCTION `a``b`() RETURNS INT RETURN 1",
            ObjectKind::Function,
            None,
            "a`b",
        ),
        (
            "-- note\n/* c */ CREATE FUNCTION ฟังก์ชัน() RETURNS INT RETURN 1",
            ObjectKind::Function,
            None,
            "ฟังก์ชัน",
        ),
    ] {
        let h = parse_routine_header(Driver::Mysql, sql).unwrap();
        assert_eq!(
            (h.kind, h.schema.as_deref(), h.name.as_str()),
            (kind, schema, name),
            "{sql}"
        );
    }
}
#[test]
fn routine_save_refuses_anything_but_one_create_of_the_same_routine() {
    let f = routine("zixma", "add_one", ObjectKind::Function);
    for (bad, why) in [
        ("", "empty"),
        ("DROP TABLE users", "must start with CREATE"),
        ("UPDATE users SET a = 1", "must start with CREATE"),
        (
            "CREATE FUNCTION add_one() RETURNS INT RETURN 1; DROP TABLE users",
            "single statement",
        ),
        ("CREATE PROCEDURE add_one() SELECT 1", "not a procedure"),
        (
            "CREATE FUNCTION other() RETURNS INT RETURN 1",
            "another name",
        ),
        (
            "CREATE FUNCTION elsewhere.add_one() RETURNS INT RETURN 1",
            "another name",
        ),
        (
            "DELIMITER $$\nCREATE FUNCTION add_one() RETURNS INT RETURN 1$$\nDELIMITER ;",
            "DELIMITER",
        ),
        ("CREATE TABLE add_one (id int)", "must start with CREATE"),
    ] {
        let e = routine_save(Driver::Mysql, &f, MY_FN, bad, &CreateSettings::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{bad:?}: {e}");
    }
    // Names are case-insensitive on MySQL.
    assert!(
        routine_save(
            Driver::Mysql,
            &f,
            MY_FN,
            "CREATE FUNCTION ZIXMA.ADD_ONE() RETURNS INT RETURN 1",
            &CreateSettings::default()
        )
        .is_ok()
    );
}
#[test]
fn postgres_routine_save_replaces_in_place_and_refuses_new_overloads() {
    let mut f = routine("public", "add_one", ObjectKind::Function);
    f.signature = Some("x integer".into());
    let edited = PG_FN.replace("x + 1", "x + 2");
    let save = routine_save(
        Driver::Postgres,
        &f,
        PG_FN,
        &edited,
        &CreateSettings::default(),
    )
    .unwrap();
    assert_eq!(sqls(&save), [format!("{};", edited.trim_end()).as_str()]);
    assert!(save.transaction && save.restore.is_empty());
    let reformatted = edited.replace("(x integer)", "( x   INTEGER )");
    assert!(
        routine_save(
            Driver::Postgres,
            &f,
            PG_FN,
            &reformatted,
            &CreateSettings::default()
        )
        .is_ok()
    );
    for (bad, why) in [
        (
            edited.replace("(x integer)", "(x bigint)"),
            "arguments changed",
        ),
        (edited.replace("OR REPLACE ", ""), "Keep CREATE OR REPLACE"),
        (
            edited.replace("public.add_one", "other.add_one"),
            "another name",
        ),
        (
            edited.replace("public.add_one", "public.\"Add_One\""),
            "another name",
        ),
        (format!("{edited}; DROP TABLE users"), "single statement"),
        (edited.replace("FUNCTION", "PROCEDURE"), "not a procedure"),
    ] {
        let e = routine_save(
            Driver::Postgres,
            &f,
            PG_FN,
            &bad,
            &CreateSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains(why), "{bad:?}: {e}");
    }
    // Unquoted names fold to lower case; quoted ones keep theirs.
    let h = parse_routine_header(
        Driver::Postgres,
        "CREATE OR REPLACE PROCEDURE S.\"Do It\"(a text DEFAULT ')') LANGUAGE sql AS 'select 1'",
    )
    .unwrap();
    assert_eq!(h.schema.as_deref(), Some("s"));
    assert_eq!(h.name, "Do It");
    assert_eq!(h.args.as_deref(), Some("a text DEFAULT ')'"));
}
#[test]
fn drop_and_truncate_follow_the_object_kind() {
    let table = TableRef {
        schema: "public".into(),
        name: "v".into(),
    };
    let op = |kind, truncate| {
        table_op_statement(
            Driver::Postgres,
            &DbObject::new(table.clone(), kind),
            truncate,
        )
        .map(|s| s.sql)
    };
    assert_eq!(
        op(ObjectKind::View, false).unwrap(),
        r#"DROP VIEW "public"."v";"#
    );
    assert_eq!(
        op(ObjectKind::MaterializedView, false).unwrap(),
        r#"DROP MATERIALIZED VIEW "public"."v";"#
    );
    assert_eq!(
        op(ObjectKind::Table, true).unwrap(),
        r#"TRUNCATE TABLE "public"."v";"#
    );
    for kind in [
        ObjectKind::View,
        ObjectKind::MaterializedView,
        ObjectKind::Function,
    ] {
        assert!(op(kind, true).is_err(), "{kind:?}");
    }
    // A PostgreSQL routine is dropped by its arguments; without them nothing is generated.
    assert!(op(ObjectKind::Function, false).is_err());
    let mut f = DbObject::new(table.clone(), ObjectKind::Function);
    f.signature = Some("a integer, b text".into());
    assert_eq!(
        table_op_statement(Driver::Postgres, &f, false).unwrap().sql,
        r#"DROP FUNCTION "public"."v"(a integer, b text);"#
    );
    f.signature = Some(String::new());
    f.kind = ObjectKind::Procedure;
    assert_eq!(
        table_op_statement(Driver::Postgres, &f, false).unwrap().sql,
        r#"DROP PROCEDURE "public"."v"();"#
    );
    f.kind = ObjectKind::Procedure;
    assert_eq!(
        table_op_statement(Driver::Mysql, &f, false).unwrap().sql,
        "DROP PROCEDURE `public`.`v`;"
    );
}
fn t(name: &str, timing: &str, event: &str, statement: &str) -> Trigger {
    Trigger {
        name: name.into(),
        timing: timing.into(),
        event: event.into(),
        statement: statement.into(),
        ..Default::default()
    }
}
#[test]
fn trigger_sql_for_both_drivers() {
    let table = TableRef {
        schema: "zixma".into(),
        name: "orders".into(),
    };
    assert_eq!(
        create_trigger_sql(
            Driver::Mysql,
            &table,
            &t(
                "orders_bi",
                "before",
                "insert",
                "BEGIN\n  SET NEW.total = 0;\n  SET NEW.n = 1;\nEND;"
            )
        )
        .unwrap(),
        "CREATE TRIGGER `zixma`.`orders_bi` BEFORE INSERT ON `zixma`.`orders` FOR EACH ROW BEGIN\n  SET NEW.total = 0;\n  SET NEW.n = 1;\nEND;"
    );
    assert_eq!(
        drop_trigger_sql(Driver::Mysql, &table, "orders_bi"),
        "DROP TRIGGER `zixma`.`orders_bi`;"
    );
    let pg = TableRef {
        schema: "public".into(),
        name: "orders".into(),
    };
    assert_eq!(
        create_trigger_sql(
            Driver::Postgres,
            &pg,
            &t(
                "Audit",
                "AFTER",
                "INSERT OR UPDATE OF total",
                "FOR EACH ROW EXECUTE FUNCTION audit()"
            )
        )
        .unwrap(),
        r#"CREATE TRIGGER "Audit" AFTER INSERT OR UPDATE OF total ON "public"."orders" FOR EACH ROW EXECUTE FUNCTION audit();"#
    );
    assert_eq!(
        drop_trigger_sql(Driver::Postgres, &pg, "Audit"),
        r#"DROP TRIGGER "Audit" ON "public"."orders";"#
    );
}
#[test]
fn trigger_statements_cannot_smuggle_a_second_statement() {
    let table = TableRef {
        schema: "zixma".into(),
        name: "orders".into(),
    };
    for (d, trigger, why) in [
        (
            Driver::Mysql,
            t("x", "BEFORE", "INSERT", "SET NEW.a = 1; DROP TABLE users"),
            "single statement",
        ),
        (
            Driver::Postgres,
            t(
                "x",
                "BEFORE",
                "INSERT",
                "FOR EACH ROW EXECUTE FUNCTION f(); DROP TABLE users",
            ),
            "single statement",
        ),
        (
            Driver::Mysql,
            t("x", "BEFORE", "TRUNCATE", "SET NEW.a = 1"),
            "choose the event",
        ),
        (
            Driver::Mysql,
            t("x", "INSTEAD OF", "INSERT", "SET NEW.a = 1"),
            "choose when",
        ),
        (
            Driver::Postgres,
            t(
                "x",
                "AFTER",
                "INSERT; DROP TABLE users; --",
                "FOR EACH ROW EXECUTE FUNCTION f()",
            ),
            "choose the event",
        ),
        (
            Driver::Mysql,
            t(" ", "BEFORE", "INSERT", "SET NEW.a = 1"),
            "needs a name",
        ),
        (
            Driver::Mysql,
            t("x", "BEFORE", "INSERT", "  "),
            "statement is empty",
        ),
    ] {
        let e = create_trigger_sql(d, &table, &trigger)
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{trigger:?}: {e}");
    }
}
#[test]
fn trigger_saves_order_drops_first_and_restore_only_mysql_edits() {
    let table = TableRef {
        schema: "zixma".into(),
        name: "orders".into(),
    };
    let old = t("a", "BEFORE", "INSERT", "SET NEW.n = 1");
    let edits = vec![
        TriggerEdit {
            key: "1".into(),
            orig: None,
            cur: t("a", "AFTER", "DELETE", "SET @x = 1"),
            deleted: false,
        },
        TriggerEdit {
            key: "2".into(),
            orig: Some(old.clone()),
            cur: t("b", "BEFORE", "INSERT", "SET NEW.n = 2"),
            deleted: false,
        },
        TriggerEdit {
            key: "3".into(),
            orig: Some(t("c", "BEFORE", "UPDATE", "SET NEW.n = 3")),
            cur: t("c", "BEFORE", "UPDATE", "SET NEW.n = 3"),
            deleted: true,
        },
        TriggerEdit {
            key: "4".into(),
            orig: Some(t("same", "BEFORE", "UPDATE", "SET NEW.n = 4")),
            cur: t("same", "BEFORE", "UPDATE", "SET NEW.n = 4"),
            deleted: false,
        },
        TriggerEdit {
            key: "5".into(),
            orig: None,
            cur: t("gone", "BEFORE", "UPDATE", "SET NEW.n = 5"),
            deleted: true,
        },
    ];
    let saves = trigger_saves(Driver::Mysql, &table, ObjectKind::Table, &edits).unwrap();
    assert_eq!(
        saves.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        ["3", "2", "1"]
    );
    let saves: Vec<ObjectSave> = saves.into_iter().map(|s| s.1).collect();
    assert_eq!(saves.len(), 3);
    assert_eq!(sqls(&saves[0]), ["DROP TRIGGER `zixma`.`c`;"]);
    assert!(saves[0].restore.is_empty());
    assert_eq!(sqls(&saves[1])[0], "DROP TRIGGER `zixma`.`a`;");
    assert!(sqls(&saves[1])[1].starts_with("CREATE TRIGGER `zixma`.`b` BEFORE INSERT"));
    assert_eq!(
        saves[1].restore,
        vec![Stmt::new(
            create_trigger_sql(Driver::Mysql, &table, &old).unwrap()
        )]
    );
    assert!(sqls(&saves[2])[0].starts_with("CREATE TRIGGER `zixma`.`a` AFTER DELETE"));
    assert!(saves.iter().all(|s| !s.transaction));
    let pg = trigger_saves(Driver::Postgres, &table, ObjectKind::Table, &edits).unwrap();
    assert!(pg.iter().all(|s| s.1.transaction && s.1.restore.is_empty()));
    assert!(trigger_saves(Driver::Mysql, &table, ObjectKind::View, &edits).is_err());
    assert!(
        trigger_saves(Driver::Mysql, &table, ObjectKind::View, &edits[3..4])
            .unwrap()
            .is_empty()
    );
}
#[test]
fn postgres_triggerdef_is_split_into_fields() {
    assert_eq!(
        parse_pg_triggerdef(
            "audit",
            "CREATE TRIGGER audit AFTER INSERT OR UPDATE OF total, \"on\" ON public.\"order items\" FOR EACH ROW WHEN ((new.total > 0)) EXECUTE FUNCTION audit('x')"
        ),
        Some(t(
            "audit",
            "AFTER",
            "INSERT OR UPDATE OF total, \"on\"",
            "FOR EACH ROW WHEN ((new.total > 0)) EXECUTE FUNCTION audit('x')"
        ))
    );
    assert_eq!(
        parse_pg_triggerdef(
            "x",
            "CREATE TRIGGER x INSTEAD OF DELETE ON v FOR EACH ROW EXECUTE FUNCTION f()"
        )
        .unwrap()
        .timing,
        "INSTEAD OF"
    );
    assert_eq!(parse_pg_triggerdef("x", "garbage"), None);
}
#[test]
fn new_trigger_names_are_free() {
    let n = new_trigger(
        Driver::Mysql,
        "orders",
        &["orders_trigger_1", "ORDERS_TRIGGER_2"],
    );
    assert_eq!(n.name, "orders_trigger_3");
    assert!(
        new_trigger(Driver::Postgres, "t", &[])
            .statement
            .starts_with("FOR EACH ROW")
    );
}
#[test]
fn views_never_get_structure_or_index_statements() {
    use crate::changes::*;
    let table = TableRef {
        schema: "zixma".into(),
        name: "v".into(),
    };
    let columns: Vec<String> = ["column_name", "data_type", "is_nullable"]
        .map(String::from)
        .to_vec();
    let added = EditRow {
        key: row_key(),
        orig: None,
        cur: vec![
            Cell::Text("x".into()),
            Cell::Text("int".into()),
            Cell::Text("YES".into()),
        ],
        deleted: false,
        rid: None,
    };
    for relation in [ObjectKind::View, ObjectKind::MaterializedView] {
        let change = StructureChange {
            table: table.clone(),
            new_name: "renamed".into(),
            is_new: false,
            primary_key: vec![],
            columns: columns.clone(),
            rows: vec![added.clone()],
            relation,
        };
        let e = structure_statements(Driver::Mysql, &change)
            .unwrap_err()
            .to_string();
        assert!(e.contains("can't be edited"), "{e}");
        let index_columns: Vec<String> =
            ["index_name", "index_algorithm", "is_unique", "column_name"]
                .map(String::from)
                .to_vec();
        let index = EditRow {
            key: row_key(),
            orig: None,
            cur: vec![
                Cell::Text("i".into()),
                Cell::Text("BTREE".into()),
                Cell::Text("FALSE".into()),
                Cell::Text("x".into()),
            ],
            deleted: false,
            rid: None,
        };
        assert!(
            index_statements(Driver::Postgres, &table, relation, &index_columns, &[index]).is_err()
        );
        assert_eq!(
            index_statements(Driver::Postgres, &table, relation, &index_columns, &[]).unwrap(),
            vec![]
        );
    }
}
#[test]
fn a_saved_column_without_a_name_gets_a_plain_message() {
    use crate::changes::*;
    let columns: Vec<String> = ["column_name", "data_type", "is_nullable"]
        .map(String::from)
        .to_vec();
    let change = StructureChange {
        table: TableRef {
            schema: "s".into(),
            name: "t".into(),
        },
        new_name: "t".into(),
        is_new: false,
        primary_key: vec![],
        columns,
        rows: vec![EditRow {
            key: row_key(),
            orig: Some(vec![
                Some("total".into()),
                Some("int".into()),
                Some("YES".into()),
            ]),
            cur: vec![
                Cell::Null,
                Cell::Text("int".into()),
                Cell::Text("YES".into()),
            ],
            deleted: false,
            rid: None,
        }],
        relation: ObjectKind::Table,
    };
    let e = structure_statements(Driver::Postgres, &change)
        .unwrap_err()
        .to_string();
    assert_eq!(
        e,
        "Column total has no name. Type a name in column_name, or delete the column instead."
    );
    assert!(!e.contains("Cannot read"));
}
