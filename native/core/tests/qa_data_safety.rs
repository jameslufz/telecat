//! QA adversarial probes (data safety). Each assertion encodes the SAFE expectation;
//! a failing assertion is a confirmed defect. Run:
//! cargo test -p telecat-core --test qa_data_safety -- --test-threads=1 --nocapture
use serde_json::json;
use telecat_core::{changes::*, sql::*, types::*};

fn row(orig: &[Option<&str>], cur: Vec<Cell>) -> EditRow {
    EditRow {
        key: "k".into(),
        orig: Some(orig.iter().map(|v| v.map(str::to_owned)).collect()),
        cur,
        deleted: false,
        rid: None,
    }
}

#[test]
fn qa_dangerous_guard_bypasses() {
    use Driver::*;
    // (driver, sql) pairs that DELETE/UPDATE every row (or execute a write) but should be flagged.
    let cases: &[(Driver, &str)] = &[
        (Mysql, "DELETE FROM t /*!99999 WHERE id = 1 */"),
        (Mysql, "DELETE FROM t WHERE id = 1 --1 OR 1=1"),
        (Mysql, "DESC ANALYZE DELETE t FROM t JOIN u ON 1=1"),
        (Mysql, "DESCRIBE ANALYZE UPDATE t JOIN u ON 1=1 SET t.v = 0"),
        (Postgres, "EXPLAIN ANALYZE VERBOSE DELETE FROM t"),
        (Postgres, "EXPLAIN ANALYSE VERBOSE UPDATE t SET v = 0"),
        (
            Postgres,
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d WHERE false",
        ),
        (
            Postgres,
            "WITH d AS (DELETE FROM t RETURNING id) UPDATE u SET v = 1 WHERE u.id = 5",
        ),
        (Mysql, "DELETE FROM t WHERE (1=1)"),
        (Mysql, "DELETE FROM t WHERE 2 > 1"),
        (Mysql, "DELETE FROM t WHERE id = id"),
        (Mysql, "DELETE FROM t WHERE TRUE OR FALSE"),
        (Mysql, "DELETE FROM t WHERE 1 = 1 AND 1"),
        (Mysql, "UPDATE t SET v = 0 WHERE id IS NOT NULL"),
        (Postgres, "DELETE FROM t WHERE 'a' = 'a'"),
        (Mysql, "TRUNCATE t"),
        (Mysql, "  (  DELETE FROM t )"),
        (Mysql, "/* x */ delete\nfrom t"),
        (Mysql, "# c\nDELETE FROM t"),
        (Mysql, "/*!DELETE FROM t*/"),
        (Mysql, "/*!50000 DELETE FROM t */"),
        (Postgres, "/* a /* nested */ b */ DELETE FROM t"),
        (Postgres, "DELETE FROM t WHERE $$x$$ = $$x$$ OR TRUE"),
        (Mysql, "REPLACE INTO t SELECT * FROM t2"),
        (
            Postgres,
            "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
        ),
        (Mysql, "ALTER TABLE t MODIFY v TINYINT"),
        (Postgres, "ALTER TABLE t ALTER COLUMN v TYPE int USING 0"),
        (Mysql, "RENAME TABLE t TO t_old"),
        (Mysql, "LOAD DATA INFILE '/x' REPLACE INTO TABLE t"),
        (Postgres, "COPY t FROM PROGRAM 'true'"),
        (Postgres, "DO $$ BEGIN DELETE FROM t; END $$"),
        (Mysql, "CALL purge_everything()"),
        (
            Mysql,
            "INSERT INTO t (id, v) SELECT id, 0 FROM t ON DUPLICATE KEY UPDATE v = 0",
        ),
        (Mysql, "SET sql_safe_updates = 0"),
    ];
    let mut missed = vec![];
    for (d, sql) in cases {
        let r = dangerous_reason(sql, *d);
        let ro = is_read_only(sql, *d);
        println!(
            "{:?}\t{:<75}\tdanger={:?}\tread_only={ro}",
            d,
            sql,
            r.as_deref().map(|s| &s[..s.len().min(30)])
        );
        if r.is_none() {
            missed.push(format!("{d:?}: {sql} (read_only={ro})"));
        }
    }
    println!("\nNOT FLAGGED ({}):\n{}", missed.len(), missed.join("\n"));
}

#[test]
fn qa_read_only_misclassification() {
    // Safe Mode 2 / Alert Mode 2 skip the gate entirely when every statement is read-only.
    for (d, sql) in [
        (Driver::Mysql, "DESC ANALYZE DELETE t FROM t JOIN u ON 1=1"),
        (
            Driver::Mysql,
            "DESCRIBE ANALYZE UPDATE t JOIN u ON 1=1 SET t.v=0",
        ),
        (Driver::Mysql, "SELECT 1 --1; DELETE FROM t"),
        (Driver::Postgres, "SELECT setval('s', 1)"),
    ] {
        println!("{d:?} {sql:<60} read_only={}", is_read_only(sql, d));
    }
    assert!(
        !is_read_only("DESC ANALYZE DELETE t FROM t JOIN u ON 1=1", Driver::Mysql),
        "DESC ANALYZE <write> classified read-only"
    );
}

#[test]
fn qa_mysql_dashdash_lexer_mismatch() {
    // MySQL needs whitespace after `--`; the lexer treats every `--` as a comment.
    let sql = "SELECT 1 --1; DELETE FROM t";
    let parts = split_statements(sql, Driver::Mysql);
    println!("split -> {} statement(s): {:?}", parts.len(), parts);
    let sql2 = "DELETE FROM t WHERE id = 1 --1 OR 1=1";
    println!(
        "code view of `{sql2}` -> {:?}",
        strip_comments(sql2, Driver::Mysql)
    );
    assert!(
        dangerous_reason(sql2, Driver::Mysql).is_some(),
        "`--1 OR 1=1` hides an always-true OR from the guard"
    );
}

#[test]
fn qa_mysql_versioned_comment() {
    let sql = "DELETE FROM t /*!99999 WHERE id = 1 */";
    assert!(
        dangerous_reason(sql, Driver::Mysql).is_some(),
        "versioned comment above server version hides absent WHERE"
    );
}

#[test]
fn qa_inline_params_question_in_identifier() {
    // Preview / gate / log SQL built from inline_params: `?` inside a quoted identifier.
    let st = Stmt {
        sql: "UPDATE `s`.`t?` SET `a?b` = ? WHERE `id` = ?;".into(),
        params: vec![json!("NEW"), json!("1")],
        expect: Some(1),
        check: None,
    };
    let shown = inline_params(Driver::Mysql, &st);
    println!("{shown}");
    assert_eq!(
        shown, "UPDATE `s`.`t?` SET `a?b` = 'NEW' WHERE `id` = '1';",
        "confirmation preview/log shows different SQL from what executes"
    );
}

#[test]
fn qa_binary_hex_lookalike_where() {
    // A VARBINARY value whose bytes are literally the ASCII text "0x41" is displayed as "0x41"
    // (buffer_cell keeps printable UTF-8). row_where then turns it into UNHEX('41') = 'A'.
    let m = TableMeta {
        columns: vec!["k".into(), "v".into()],
        primary_key: vec!["k".into()],
        binary: Some(vec![true, false]),
    };
    let t = TableRef {
        schema: "s".into(),
        name: "t".into(),
    };
    let r = row(
        &[Some("0x41"), Some("2")],
        vec![Cell::Text("0x41".into()), Cell::Text("99".into())],
    );
    let st = data_statements(Driver::Mysql, &t, &m, &[r]).unwrap();
    println!("{:?}", st);
    assert!(
        !st[0].sql.contains("UNHEX"),
        "text '0x41' in a binary key is matched as bytes 0x41 ('A')"
    );
}

#[test]
fn qa_unset_on_existing_row_becomes_null() {
    // Not reachable from the context menu (Set DEFAULT is offered only for new rows), but
    // EditBuffer::set_cell accepts it: an existing row with an Unset cell writes NULL, not DEFAULT.
    let m = TableMeta {
        columns: vec!["id".into(), "v".into()],
        primary_key: vec!["id".into()],
        binary: None,
    };
    let t = TableRef {
        schema: "s".into(),
        name: "t".into(),
    };
    let r = row(
        &[Some("1"), Some("x")],
        vec![Cell::Text("1".into()), Cell::unset()],
    );
    let st = data_statements(Driver::Mysql, &t, &m, &[r]).unwrap();
    println!("{}", st[0].sql);
}

#[test]
fn qa_where_never_empty() {
    // Every UPDATE/DELETE produced for an existing row must carry a non-empty WHERE and expect 1.
    let t = TableRef {
        schema: "s`\"x.y".into(),
        name: "t`\"\\.z".into(),
    };
    for d in [Driver::Mysql, Driver::Postgres] {
        for pk in [vec![], vec!["a`b".to_owned()]] {
            let m = TableMeta {
                columns: vec!["a`b".into(), "c\"d".into(), "e.f\\".into()],
                primary_key: pk.clone(),
                binary: Some(vec![false, true, false]),
            };
            for orig in [
                vec![None, None, None],
                vec![Some("x'\\\0"), Some("0x"), Some("ไทย😺")],
            ] {
                let mut del = row(
                    &orig,
                    orig.iter()
                        .map(|v| Cell::from(v.map(str::to_owned)))
                        .collect(),
                );
                del.deleted = true;
                let mut upd = del.clone();
                upd.deleted = false;
                upd.cur[1] = Cell::Text("new".into());
                for r in [del, upd] {
                    let st = data_statements(d, &t, &m, &[r]).unwrap();
                    assert_eq!(st.len(), 1);
                    let s = &st[0];
                    assert_eq!(s.expect, Some(1));
                    let w = s.sql.split(" WHERE ").nth(1).expect("WHERE present");
                    assert!(w.trim().len() > 3, "{}", s.sql);
                    if pk.is_empty() {
                        assert!(
                            s.check.is_some(),
                            "no-PK write without COUNT check: {}",
                            s.sql
                        );
                    }
                    assert_eq!(split_statements(&s.sql, d).len(), 1, "{}", s.sql);
                }
            }
        }
    }
    // rid with wrong arity is rejected, never silently dropped.
    let m = TableMeta {
        columns: vec!["a".into()],
        primary_key: vec![],
        binary: None,
    };
    let mut r = row(&[Some("1")], vec![Cell::Text("1".into())]);
    r.deleted = true;
    r.rid = Some(vec!["1".into()]);
    assert!(
        data_statements(
            Driver::Postgres,
            &TableRef {
                schema: "p".into(),
                name: "t".into()
            },
            &m,
            &[r]
        )
        .is_err()
    );
    // A table with zero columns and no PK must refuse.
    let m = TableMeta {
        columns: vec![],
        primary_key: vec![],
        binary: None,
    };
    let mut r = row(&[], vec![]);
    r.deleted = true;
    assert!(
        data_statements(
            Driver::Mysql,
            &TableRef {
                schema: "p".into(),
                name: "t".into()
            },
            &m,
            &[r]
        )
        .is_err()
    );
}

#[test]
fn qa_inline_params_ddl_literal() {
    // DDL statements carry no params; inline_params still rewrites `?` / `$n` inside string literals.
    let my =
        Stmt::new("ALTER TABLE `s`.`t`\n  CHANGE COLUMN `a` `a` int NULL COMMENT 'Is active?';");
    let pg = Stmt::new("COMMENT ON COLUMN \"p\".\"t\".\"c\" IS 'costs $1';");
    let (a, b) = (
        inline_params(Driver::Mysql, &my),
        inline_params(Driver::Postgres, &pg),
    );
    println!("{a}\n{b}");
    assert_eq!(a, my.sql);
    assert_eq!(b, pg.sql);
}

#[test]
fn qa_mysql_structure_lossy_change_has_no_check() {
    let cols: Vec<String> = [
        "#",
        "column_name",
        "data_type",
        "character_set",
        "collation",
        "is_nullable",
        "column_default",
        "extra",
        "foreign_key",
        "comment",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let orig = vec![
        Some("2"),
        Some("d"),
        Some("decimal(10,4)"),
        None,
        None,
        Some("YES"),
        None,
        Some(""),
        None,
        Some(""),
    ];
    let mut cur: Vec<Cell> = orig
        .iter()
        .map(|v| Cell::from(v.map(str::to_owned)))
        .collect();
    cur[2] = Cell::Text("decimal(10,1)".into());
    let c = StructureChange {
        table: TableRef {
            schema: "zixma".into(),
            name: "t".into(),
        },
        new_name: "t".into(),
        is_new: false,
        primary_key: vec![],
        columns: cols.clone(),
        rows: vec![row(&orig, cur.clone())],
    };
    let st = structure_statements(Driver::Mysql, &c).unwrap();
    println!("MySQL: {:?}", st);
    let mut pg = c.clone();
    pg.columns.retain(|c| c != "character_set");
    let mut o2 = orig.clone();
    o2.remove(3);
    let mut c2 = cur.clone();
    c2.remove(3);
    pg.rows = vec![row(&o2, c2)];
    pg.table.schema = "public".into();
    let pst = structure_statements(Driver::Postgres, &pg).unwrap();
    println!(
        "PG: {:?}",
        pst.iter()
            .map(|s| (&s.sql, s.check.is_some()))
            .collect::<Vec<_>>()
    );
    assert!(
        pst.iter().any(|s| s.check.is_some()),
        "PG has a lossy-change guard"
    );
    assert!(
        st.iter().any(|s| s.check.is_some()),
        "MySQL lossy type change has no guard check"
    );
}
