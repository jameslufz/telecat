//! Regression tests for the data-safety audit. Live behaviour
//! is covered in db/tests/data_safety_fixes.rs.
use Driver::*;
use serde_json::json;
use telecat_core::{changes::*, sql::*, types::*};

fn table() -> TableRef {
    TableRef {
        schema: "s".into(),
        name: "t".into(),
    }
}
fn existing(orig: &[Option<&str>], rid: Option<Vec<String>>) -> EditRow {
    let orig: Vec<Option<String>> = orig.iter().map(|v| v.map(str::to_owned)).collect();
    EditRow {
        key: "k".into(),
        cur: orig.iter().cloned().map(Cell::from).collect(),
        orig: Some(orig),
        deleted: false,
        rid,
    }
}

/// Every statement QA found that writes without the danger confirmation (BUG-004 to 007,
/// 010, 011). `qa_dangerous_guard_bypasses` only prints; this asserts.
#[test]
fn every_audited_bypass_needs_confirmation() {
    let cases: &[(Driver, &str)] = &[
        (Mysql, "DELETE FROM t /*!99999 WHERE id = 1 */"),
        (Mysql, "DELETE FROM t WHERE id = 1 --1 OR 1=1"),
        (Mysql, "DESC ANALYZE DELETE t FROM t JOIN u ON 1=1"),
        (Mysql, "DESCRIBE ANALYZE UPDATE t JOIN u ON 1=1 SET t.v = 0"),
        (Postgres, "EXPLAIN ANALYZE VERBOSE DELETE FROM t"),
        (Postgres, "EXPLAIN ANALYSE VERBOSE UPDATE t SET v = 0"),
        (
            Postgres,
            "EXPLAIN (ANALYZE, VERBOSE, FORMAT JSON) DELETE FROM t",
        ),
        (Postgres, "EXPLAIN (VERBOSE, ANALYZE false) DELETE FROM t"),
        (
            Postgres,
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d WHERE false",
        ),
        (
            Postgres,
            "WITH d AS (DELETE FROM t RETURNING id) UPDATE u SET v = 1 WHERE u.id = 5",
        ),
        (
            Postgres,
            "WITH a AS (SELECT 1), d AS (UPDATE t SET v = 0 RETURNING 1) SELECT 1",
        ),
        (Mysql, "DELETE FROM t WHERE (1=1)"),
        (Mysql, "DELETE FROM t WHERE 2 > 1"),
        (Mysql, "DELETE FROM t WHERE id = id"),
        (Mysql, "DELETE FROM t WHERE TRUE OR FALSE"),
        (Mysql, "DELETE FROM t WHERE 1 = 1 AND 1"),
        (Mysql, "DELETE FROM t WHERE NOT (1 = 0)"),
        (Mysql, "UPDATE t SET v = 0 WHERE id IS NOT NULL"),
        (Mysql, "UPDATE t SET v = 0 WHERE `id` <=> `id`"),
        (Postgres, "DELETE FROM t WHERE 'a' = 'a'"),
        (Postgres, "DELETE FROM t WHERE $$x$$ = $$x$$ OR TRUE"),
        (Postgres, "DELETE FROM t WHERE id IS NOT DISTINCT FROM id"),
        (Mysql, "REPLACE INTO t SELECT * FROM t2"),
        (
            Postgres,
            "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
        ),
        (Mysql, "ALTER TABLE t MODIFY v TINYINT"),
        (Mysql, "ALTER TABLE t CHANGE v v2 INT"),
        (Postgres, "ALTER TABLE t ALTER COLUMN v TYPE int USING 0"),
        (Postgres, "ALTER TABLE t ALTER COLUMN v SET DATA TYPE int"),
        (Mysql, "RENAME TABLE t TO t_old"),
        (Mysql, "LOAD DATA INFILE '/x' REPLACE INTO TABLE t"),
        (Postgres, "COPY t FROM PROGRAM 'true'"),
        (Postgres, "COPY t FROM '/tmp/x'"),
        (Postgres, "DO $$ BEGIN DELETE FROM t; END $$"),
        (
            Mysql,
            "INSERT INTO t (id, v) SELECT id, 0 FROM t ON DUPLICATE KEY UPDATE v = 0",
        ),
        (
            Postgres,
            "INSERT INTO t SELECT * FROM s ON CONFLICT (id) DO UPDATE SET v = 0",
        ),
        (Mysql, "SET sql_safe_updates = 0"),
        (Mysql, "SET @@session.foreign_key_checks = 0"),
        (Postgres, "SET session_replication_role = replica"),
        (Mysql, "SELECT 1 --1; DELETE FROM t"),
        (Mysql, "PREPARE s FROM 'DELETE FROM t'"),
        // Without backslash escapes (NO_BACKSLASH_ESCAPES) this closes cleanly and the OR runs.
        (Mysql, "DELETE FROM t WHERE id = 'x\\' OR 1=1 -- '"),
        // With standard_conforming_strings off, the backslash escapes the quote instead.
        (Postgres, "DELETE FROM t WHERE id = 'x\\' OR 1=1 -- '"),
    ];
    let missed: Vec<_> = cases
        .iter()
        .filter(|(d, sql)| dangerous_reason(sql, *d).is_none())
        .collect();
    assert!(missed.is_empty(), "not flagged: {missed:#?}");
}

#[test]
fn reasons_name_the_cause() {
    assert_eq!(
        dangerous_reason("DELETE FROM t /*!99999 WHERE id = 1 */", Mysql).as_deref(),
        Some(
            "DELETE without WHERE affects every row of the table (if the server skips a /*!…*/ versioned comment)"
        )
    );
    assert_eq!(
        dangerous_reason("EXPLAIN ANALYZE VERBOSE DELETE FROM t", Postgres).as_deref(),
        Some(
            "EXPLAIN ANALYZE runs the statement: DELETE without WHERE affects every row of the table"
        )
    );
    assert!(
        dangerous_reason("DELETE FROM t WHERE id = 1 --1 OR 1=1", Mysql)
            .unwrap()
            .contains("always-true WHERE")
    );
}

#[test]
fn ordinary_statements_stay_unflagged() {
    for (d, sql) in [
        (Mysql, "DELETE FROM t WHERE id = 1 -- remove one"),
        (Mysql, "DELETE FROM t WHERE id = 1 /*!99999 AND v = 2 */"),
        (Mysql, "UPDATE t SET v = v + 1 WHERE id BETWEEN 1 AND 10"),
        (
            Mysql,
            "UPDATE t JOIN u ON u.id = t.uid SET t.v = 1 WHERE u.flag = 1",
        ),
        (Mysql, "INSERT INTO t SELECT * FROM s"),
        (
            Mysql,
            "INSERT INTO t (a) VALUES (1) ON DUPLICATE KEY UPDATE a = 2",
        ),
        (Mysql, "ALTER TABLE t ADD COLUMN type INT"),
        (Mysql, "SELECT * FROM t WHERE a = a"),
        (Mysql, "EXPLAIN DELETE FROM t"),
        (Mysql, "SET time_zone = '+00:00'"),
        (Mysql, "UPDATE t SET name = 'O\\'Brien' WHERE id = 5"),
        (Mysql, "DELETE FROM t WHERE path = 'C:\\\\' AND id = 1"),
        (Postgres, "UPDATE t SET p = 'C:\\' WHERE id = 1"),
        (Postgres, "UPDATE t SET p = E'it\\'s' WHERE id = 1"),
        (Postgres, "UPDATE t SET v = 1 WHERE \"id\" = \"parent_id\""),
        (
            Postgres,
            "DELETE FROM t WHERE created < now() - interval '1 day'",
        ),
        (
            Postgres,
            "WITH x AS (SELECT id FROM s) DELETE FROM t WHERE id IN (SELECT id FROM x)",
        ),
        (
            Postgres,
            "INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE SET v = 2",
        ),
        (Postgres, "COPY t TO STDOUT"),
        (Postgres, "SELECT * FROM t FOR UPDATE"),
        (
            Postgres,
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1; $$",
        ),
    ] {
        assert_eq!(dangerous_reason(sql, d), None, "{d:?}: {sql}");
    }
}

#[test]
fn read_only_means_the_server_cannot_write() {
    for (d, sql) in [
        (Mysql, "DESC ANALYZE DELETE t FROM t JOIN u ON 1=1"),
        (Mysql, "EXPLAIN ANALYZE UPDATE t SET v = 1 WHERE id = 1"),
        (Mysql, "SELECT 1 --1; DELETE FROM t"),
        (Postgres, "EXPLAIN (ANALYZE) DELETE FROM t WHERE id = 1"),
        (
            Postgres,
            "WITH d AS (DELETE FROM t WHERE id = 1 RETURNING *) SELECT * FROM d",
        ),
    ] {
        assert!(!is_read_only(sql, d), "{d:?}: {sql}");
    }
    for (d, sql) in [
        (Mysql, "SELECT * FROM t WHERE note = '--1; DELETE'"),
        (Mysql, "EXPLAIN ANALYZE SELECT * FROM t"),
        (Postgres, "EXPLAIN DELETE FROM t"),
        (Postgres, "WITH x AS (SELECT 1) SELECT * FROM x"),
    ] {
        assert!(is_read_only(sql, d), "{d:?}: {sql}");
    }
}

/// BUG-012: what the user approves is what runs.
#[test]
fn inlined_sql_leaves_quoted_text_alone() {
    let st = Stmt {
        sql: "UPDATE `s`.`t?` SET `a?b` = ? /* ? */ WHERE `id` = ? AND c = '?';".into(),
        params: vec![json!("NEW"), json!("1")],
        expect: Some(1),
        check: None,
    };
    assert_eq!(
        inline_params(Mysql, &st),
        "UPDATE `s`.`t?` SET `a?b` = 'NEW' /* ? */ WHERE `id` = '1' AND c = '?';"
    );
    let st = Stmt {
        sql: "UPDATE \"t$1.\" SET \"v\" = $1 WHERE note = 'costs $2' AND id = $2 AND b = $$ $1 $$;"
            .into(),
        params: vec![json!("changed"), json!("7")],
        expect: Some(1),
        check: None,
    };
    assert_eq!(
        inline_params(Postgres, &st),
        "UPDATE \"t$1.\" SET \"v\" = 'changed' WHERE note = 'costs $2' AND id = '7' AND b = $$ $1 $$;"
    );
    let st = Stmt {
        sql: "SELECT /*!50000 ? */ 1".into(),
        params: vec![json!("x")],
        expect: None,
        check: None,
    };
    assert_eq!(
        inline_params(Mysql, &st),
        "SELECT /*!50000 'x' */ 1",
        "versioned comments are code"
    );
}

/// BUG-001: without a key, a ctid must also match the original values, under the COUNT check.
#[test]
fn postgres_ctid_without_key_also_matches_values() {
    let m = TableMeta {
        columns: vec!["name".into(), "balance".into()],
        primary_key: vec![],
        binary: None,
    };
    let mut r = existing(
        &[Some("alice"), None],
        Some(vec!["16384".into(), "(0,1)".into()]),
    );
    r.deleted = true;
    let st = &data_statements(Postgres, &table(), &m, &[r]).unwrap()[0];
    let predicate =
        "tableoid = $1::oid AND ctid = $2::tid AND \"name\"::text = $3 AND \"balance\" IS NULL";
    assert_eq!(
        st.sql,
        format!("DELETE FROM \"s\".\"t\" WHERE {predicate};")
    );
    let check = st.check.as_ref().expect("COUNT check");
    assert_eq!(
        check.sql,
        format!("SELECT COUNT(*) FROM \"s\".\"t\" WHERE {predicate}")
    );
    assert_eq!(check.params, st.params);
    // With a key the ctid plus key stays the whole predicate.
    let keyed = TableMeta {
        primary_key: vec!["name".into()],
        ..m
    };
    let mut r = existing(
        &[Some("alice"), None],
        Some(vec!["16384".into(), "(0,1)".into()]),
    );
    r.deleted = true;
    let st = &data_statements(Postgres, &table(), &keyed, &[r]).unwrap()[0];
    assert_eq!(
        st.sql,
        "DELETE FROM \"s\".\"t\" WHERE tableoid = $1::oid AND ctid = $2::tid AND \"name\" = $3;"
    );
    assert!(st.check.is_none());
}

fn mysql_meta(pk: &[&str]) -> (TableMeta, Vec<IdentityKind>) {
    let columns: Vec<String> = ["k", "ts", "f", "v"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let kinds = vec![
        IdentityKind::Binary,
        IdentityKind::Timestamp,
        IdentityKind::Float,
        IdentityKind::Other,
    ];
    (
        TableMeta {
            columns,
            primary_key: pk.iter().map(|s| s.to_string()).collect(),
            binary: Some(vec![true, false, false, false]),
        },
        kinds,
    )
}
fn deleted(orig: &[Option<&str>], rid: Vec<String>) -> EditRow {
    let mut r = existing(orig, Some(rid));
    r.deleted = true;
    r
}

/// BUG-002, 003, 016: MySQL rows are found by what the server stored, and a row someone else
/// changed no longer matches.
#[test]
fn mysql_rows_are_identified_exactly() {
    let (m, kinds) = mysql_meta(&["k", "ts"]);
    let keys = [
        ("k".to_owned(), IdentityKind::Binary),
        ("ts".to_owned(), IdentityKind::Timestamp),
    ];
    let exprs = mysql_identity_columns(&m.columns, &kinds, &keys);
    assert_eq!(exprs.len(), 3, "fingerprint, HEX(k), UNIX_TIMESTAMP(ts)");
    assert!(exprs[0].contains("CAST(`f` + 0e0 AS BINARY)") && exprs[0].contains("`v`"));
    assert!(
        !exprs[0].contains("`k`") && !exprs[0].contains("`ts`"),
        "keys are compared, not hashed"
    );
    assert_eq!(exprs[1], "HEX(`k`)");
    assert_eq!(exprs[2], "UNIX_TIMESTAMP(`ts`)");
    let rid = mysql_row_id(
        &kinds,
        &keys,
        &[
            Some("abc123".into()),
            Some("30783431".into()),
            Some("1730615400.000000".into()),
        ],
    )
    .unwrap();
    // The grid shows the text "0x41"; the stored bytes are 30 78 34 31.
    let mut r = existing(
        &[
            Some("0x41"),
            Some("2024-11-03 01:30:00"),
            Some("1.23457"),
            Some("2"),
        ],
        Some(rid),
    );
    r.cur[3] = Cell::Text("99".into());
    let st = &data_statements(Mysql, &table(), &m, &[r]).unwrap()[0];
    assert!(
        st.sql.starts_with(
            "UPDATE `s`.`t` SET `v` = ? WHERE `k` = UNHEX(?) AND UNIX_TIMESTAMP(`ts`) = ? AND SHA2(CONCAT_WS("
        ),
        "{}",
        st.sql
    );
    assert_eq!(
        st.params,
        [
            json!("99"),
            json!("30783431"),
            json!("1730615400.000000"),
            json!("abc123")
        ]
    );
    assert!(st.check.is_none());
}

/// Integer and DECIMAL keys compare as their own type: compared with a string, MySQL uses
/// DOUBLE, which can't tell 9007199254740993 from 9007199254740992.
#[test]
fn mysql_numeric_keys_compare_exactly() {
    let m = TableMeta {
        columns: vec!["id".into(), "d".into(), "n".into(), "v".into()],
        primary_key: vec!["id".into(), "d".into(), "n".into()],
        binary: None,
    };
    let kinds = [
        IdentityKind::Unsigned,
        IdentityKind::Decimal,
        IdentityKind::Signed,
        IdentityKind::Other,
    ];
    let keys = [
        ("id".to_owned(), IdentityKind::Unsigned),
        ("d".to_owned(), IdentityKind::Decimal),
        ("n".to_owned(), IdentityKind::Signed),
    ];
    assert_eq!(
        mysql_identity_columns(&m.columns, &kinds, &keys)[1..],
        [
            "CAST(`id` AS CHAR)",
            "CAST(`d` AS CHAR)",
            "CAST(`n` AS CHAR)"
        ]
    );
    let rid = mysql_row_id(
        &kinds,
        &keys,
        &[
            Some("fp".into()),
            Some("18446744073709551615".into()),
            Some("12.50".into()),
            Some("-9223372036854775808".into()),
        ],
    )
    .unwrap();
    let r = deleted(&[Some("1"), Some("12.5"), Some("-1"), Some("x")], rid);
    let st = &data_statements(Mysql, &table(), &m, &[r]).unwrap()[0];
    assert!(st.sql.starts_with(
        "DELETE FROM `s`.`t` WHERE `id` = CAST(? AS DECIMAL(20, 0)) AND `d` = ? AND BINARY CAST(`d` AS CHAR) = ? AND `n` = CAST(? AS DECIMAL(20, 0)) AND SHA2(CONCAT_WS(',', '', IFNULL(SHA2(CAST(`v` AS BINARY), 256), '-')), 256) = ?;"
    ), "{}", st.sql);
    assert_eq!(
        st.params,
        [
            json!("18446744073709551615"),
            json!("12.50"),
            json!("12.50"),
            json!("-9223372036854775808"),
            json!("fp")
        ],
        "the loaded exact values, not the display"
    );
}

#[test]
fn mysql_rows_without_a_key_match_the_fingerprint_under_a_count() {
    let (m, kinds) = mysql_meta(&[]);
    let rid = mysql_row_id(&kinds, &[], &[Some("fp".into())]).unwrap();
    let r = deleted(&[Some("A"), None, Some("1e300"), Some("{\"a\": 1}")], rid);
    let st = &data_statements(Mysql, &table(), &m, &[r]).unwrap()[0];
    assert!(
        st.sql.starts_with("DELETE FROM `s`.`t` WHERE SHA2(") && st.sql.ends_with(" = ? LIMIT 1;"),
        "{}",
        st.sql
    );
    for c in ["`k`", "`ts`", "`f`", "`v`"] {
        assert!(st.sql.contains(c), "every column is hashed: {c}");
    }
    let check = st.check.as_ref().unwrap();
    assert_eq!(check.expect, 1);
    assert!(
        check
            .sql
            .ends_with(&st.sql["DELETE FROM `s`.`t`".len()..st.sql.len() - " LIMIT 1;".len()])
    );
}

/// NEW-8: FLOAT and BIT keys and an invisible primary key are matched by exact values.
#[test]
fn mysql_float_bit_and_invisible_keys_are_exact() {
    let (m, kinds) = mysql_meta(&["f"]);
    let keys = [("f".to_owned(), IdentityKind::Float)];
    assert_eq!(
        mysql_identity_columns(&m.columns, &kinds, &keys)[1],
        "CAST(`f` + 0e0 AS CHAR)"
    );
    let rid = mysql_row_id(
        &kinds,
        &keys,
        &[Some("fp".into()), Some("1.2345677614212036".into())],
    )
    .unwrap();
    let st = &data_statements(
        Mysql,
        &table(),
        &m,
        &[deleted(&[Some("A"), None, Some("1.23457"), Some("2")], rid)],
    )
    .unwrap()[0];
    assert!(
        st.sql
            .starts_with("DELETE FROM `s`.`t` WHERE `f` = ? + 0e0 AND SHA2("),
        "{}",
        st.sql
    );
    assert_eq!(st.params[0], json!("1.2345677614212036"));
    assert!(st.check.is_none());

    let bit = TableMeta {
        columns: vec!["b".into(), "v".into()],
        primary_key: vec!["b".into()],
        binary: Some(vec![true, false]),
    };
    let bit_kinds = [IdentityKind::Bit, IdentityKind::Other];
    let bit_keys = [("b".to_owned(), IdentityKind::Bit)];
    assert_eq!(
        mysql_identity_columns(&bit.columns, &bit_kinds, &bit_keys)[1],
        "CAST(`b` AS UNSIGNED)"
    );
    let rid = mysql_row_id(
        &bit_kinds,
        &bit_keys,
        &[Some("fp".into()), Some("65".into())],
    )
    .unwrap();
    let st = &data_statements(
        Mysql,
        &table(),
        &bit,
        &[deleted(&[Some("A"), Some("1")], rid)],
    )
    .unwrap()[0];
    assert!(
        st.sql
            .starts_with("DELETE FROM `s`.`t` WHERE `b` = CAST(? AS DECIMAL(20, 0)) AND SHA2("),
        "{}",
        st.sql
    );

    let invisible = TableMeta {
        primary_key: vec!["my_row_id".into()],
        ..m.clone()
    };
    let keys = [("my_row_id".to_owned(), IdentityKind::Unsigned)];
    assert_eq!(
        mysql_identity_columns(&m.columns, &kinds, &keys)[1],
        "CAST(`my_row_id` AS CHAR)"
    );
    let rid = mysql_row_id(&kinds, &keys, &[Some("fp".into()), Some("7".into())]).unwrap();
    let st = &data_statements(
        Mysql,
        &table(),
        &invisible,
        &[deleted(&[Some("A"), None, None, None], rid)],
    )
    .unwrap()[0];
    assert!(
        st.sql.starts_with(
            "DELETE FROM `s`.`t` WHERE `my_row_id` = CAST(? AS DECIMAL(20, 0)) AND SHA2("
        ),
        "{}",
        st.sql
    );
    assert!(
        st.check.is_none(),
        "keyed: no NOT_UNIQUE refusal for visible duplicates"
    );
}

#[test]
fn malformed_mysql_identity_is_refused() {
    let (m, _) = mysql_meta(&["k"]);
    for rid in [
        vec!["mysql".to_owned()],
        vec![
            "mysql".into(),
            "kinds=bc".into(),
            "fp=x".into(),
            "keys=b".into(),
        ],
        vec![
            "mysql".into(),
            "kinds=btfc".into(),
            "keys=b".into(),
            "k0=41".into(),
        ],
        vec![
            "mysql".into(),
            "kinds=btfc".into(),
            "fp=x".into(),
            "keys=b".into(),
        ],
        vec![
            "mysql".into(),
            "kinds=btfc".into(),
            "fp=x".into(),
            "keys=".into(),
        ],
        vec![
            "mysql".into(),
            "kinds=btfc".into(),
            "fp=x".into(),
            "k0=41".into(),
        ],
        vec!["16384".into(), "(0,1)".into()],
    ] {
        let r = deleted(&[Some("A"), None, None, None], rid.clone());
        let e = data_statements(Mysql, &table(), &m, &[r])
            .unwrap_err()
            .to_string();
        assert!(e.contains("couldn't be identified exactly"), "{rid:?}: {e}");
    }
}

/// BUG-002 without an identity: text that decodes to readable bytes can only be the stored text;
/// text that decodes to unreadable bytes is ambiguous and refused.
#[test]
fn binary_display_text_is_never_reinterpreted_as_bytes() {
    let m = TableMeta {
        columns: vec!["k".into(), "v".into()],
        primary_key: vec!["k".into()],
        binary: Some(vec![true, false]),
    };
    let mut r = existing(&[Some("0x41"), Some("1")], None);
    r.deleted = true;
    let st = &data_statements(Mysql, &table(), &m, &[r]).unwrap()[0];
    assert_eq!(st.sql, "DELETE FROM `s`.`t` WHERE `k` = ?;");
    assert_eq!(st.params, [json!("0x41")]);
    let mut r = existing(&[Some("0x00FF"), Some("1")], None);
    r.deleted = true;
    let e = data_statements(Mysql, &table(), &m, &[r])
        .unwrap_err()
        .to_string();
    assert!(e.contains(BINARY_AMBIGUOUS), "{e}");
    assert!(
        binary_shown_as_text(b"0x41")
            && !binary_shown_as_text(&[0, 0xff])
            && !binary_shown_as_text("\u{fffd}".as_bytes())
    );
}

fn structure(d: Driver, old: &str, new: &str) -> Vec<Stmt> {
    let cols: Vec<String> = [
        "#",
        "column_name",
        "data_type",
        "is_nullable",
        "column_default",
        "extra",
        "comment",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let orig = vec![
        Some("1"),
        Some("d"),
        Some(old),
        Some("YES"),
        None,
        Some(""),
        Some(""),
    ];
    let mut cur: Vec<Cell> = orig
        .iter()
        .map(|v| Cell::from(v.map(str::to_owned)))
        .collect();
    cur[2] = Cell::Text(new.into());
    structure_statements(
        d,
        &StructureChange {
            table: table(),
            new_name: "t".into(),
            is_new: false,
            primary_key: vec![],
            columns: cols,
            rows: vec![existing(&orig, None).tap(|r| r.cur = cur)],
        },
    )
    .unwrap()
}
trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&mut Self)) -> Self;
}
impl Tap for EditRow {
    fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
        f(&mut self);
        self
    }
}

/// BUG-008: a MySQL type change only runs when no stored value would change.
#[test]
fn mysql_type_changes_carry_a_value_check() {
    // What each check means is verified against the server in db/tests/data_safety_fixes.rs.
    for (old, new, condition) in [
        (
            "decimal(10,4)",
            "decimal(10,1)",
            "CAST(`d` AS DECIMAL(10, 1))",
        ),
        (
            "datetime(6)",
            "datetime",
            "CAST(CAST(`d` AS DATETIME(0)) AS DATETIME(6))",
        ),
        ("double", "float", "CAST(`d` AS FLOAT)"),
        (
            "bigint",
            "tinyint unsigned",
            "NOT (`d` + 0e0 BETWEEN 0 AND 255)",
        ),
        (
            "int",
            "smallint",
            "NOT (`d` + 0e0 BETWEEN -32768 AND 32767)",
        ),
        ("text", "varchar(10)", "CAST(`d` AS CHAR(10))"),
        (
            "varchar(10)",
            "char(5)",
            "TRIM(TRAILING ' ' FROM CAST(`d` AS CHAR(5)))",
        ),
        (
            "varchar(10)",
            "int",
            "BINARY CAST(CAST(`d` AS SIGNED) AS CHAR) <=> BINARY CAST(`d` AS CHAR)",
        ),
        (
            "enum('a','b')",
            "enum('A','b')",
            "NOT (BINARY `d` IN ('A','b'))",
        ),
        ("text", "geometry", "TRUE"),
        ("bit(8)", "int", "TRUE"),
    ] {
        let st = structure(Mysql, old, new);
        let check = st[0]
            .check
            .as_ref()
            .unwrap_or_else(|| panic!("{old} -> {new}"));
        assert!(
            check
                .sql
                .starts_with("SELECT COUNT(*) FROM `s`.`t` WHERE (`d` IS NOT NULL AND (")
                && check.sql.contains(condition),
            "{old} -> {new}: {}",
            check.sql
        );
        assert_eq!(check.expect, 0);
        assert!(
            check.message.contains(&format!("d to {new}")),
            "{}",
            check.message
        );
    }
    assert!(
        structure(Mysql, "int", "INT")[0].check.is_none(),
        "same type, other case"
    );
}

// ------------------------------------------------------------------ round 2 (QA re-verification)

/// NEW-1, NEW-2: non-ASCII characters continue identifiers and dollar-quote tags, as on both
/// servers; only ASCII whitespace separates words.
#[test]
fn unicode_never_hides_a_missing_where() {
    let mut seed = 0x2545f4914f6cdd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let fixed = [
        '\u{a0}', '\u{b7}', '\u{feff}', '\u{200b}', '\u{2028}', '\u{3000}', '\u{85}', 'ä', 'ธ',
        '😺',
    ];
    for i in 0..2000 {
        let c = if i < fixed.len() {
            fixed[i]
        } else {
            match char::from_u32(0x80 + (next() % 0x10ff00) as u32) {
                Some(c) => c,
                None => continue,
            }
        };
        for d in [Mysql, Postgres] {
            for sql in [
                format!("DELETE FROM t AS a{c}WHERE{c}b"),
                format!("UPDATE t AS a{c}WHERE{c}x SET v = 0"),
            ] {
                assert!(
                    dangerous_reason(&sql, d).is_some(),
                    "{d:?} U+{:04X}: {sql}",
                    c as u32
                );
            }
        }
        if c != '$' {
            let sql = format!("DELETE FROM t WHERE ${c}$x${c}$ <> 'y'");
            assert!(
                dangerous_reason(&sql, Postgres).is_some(),
                "U+{:04X}: {sql}",
                c as u32
            );
        }
    }
    // A real ASCII separator still works.
    assert_eq!(
        dangerous_reason("DELETE FROM t AS a\tWHERE\u{b}a.id = 1", Mysql),
        None
    );
}

/// BUG-010, NEW-3: constant conditions, and any WHERE that can't depend on the row.
#[test]
fn where_clauses_that_ignore_the_row_need_confirmation() {
    for (d, sql) in [
        (Mysql, "DELETE FROM t WHERE 'a' <> 'b'"),
        (Postgres, "DELETE FROM t WHERE 1 = 1 AND 'a' <> 'b'"),
        (Mysql, "DELETE FROM t WHERE 1 IN (1)"),
        (Mysql, "DELETE FROM t WHERE EXISTS (SELECT 1)"),
        (Mysql, "DELETE FROM t WHERE 1 BETWEEN 0 AND 2"),
        (Mysql, "DELETE FROM t WHERE 'x' LIKE '%'"),
        (Mysql, "DELETE FROM t WHERE NULL IS NULL"),
        (Mysql, "DELETE FROM t WHERE id IS NULL OR id IS NOT NULL"),
        (Mysql, "DELETE FROM t WHERE ~0"),
        (Mysql, "DELETE FROM t WHERE b'1'"),
        (Mysql, "DELETE FROM t WHERE ? = ?"),
        (Postgres, "DELETE FROM t WHERE now() > '2000-01-01'"),
        (
            Mysql,
            "UPDATE t SET v = 1 WHERE CURRENT_DATE > '2000-01-01'",
        ),
    ] {
        assert!(dangerous_reason(sql, d).is_some(), "{d:?}: {sql}");
    }
    for (d, sql) in [
        (
            Mysql,
            "DELETE FROM t WHERE created_at < NOW() - INTERVAL 30 DAY",
        ),
        (
            Postgres,
            "DELETE FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.tid = t.id)",
        ),
        (Postgres, "DELETE FROM t WHERE id IN (SELECT tid FROM u)"),
        (Mysql, "DELETE FROM t WHERE CAST(v AS CHAR) = 'x'"),
        (Postgres, "UPDATE t SET a = 1 WHERE v::text = 'x'"),
        (Postgres, "DELETE FROM t WHERE CURRENT OF c"),
    ] {
        assert_eq!(dangerous_reason(sql, d), None, "{d:?}: {sql}");
    }
}

/// NEW-6: routine statements are not dangers; SETs that switch a protection off are.
#[test]
fn routine_statements_are_not_dangers() {
    for (d, sql) in [
        (Postgres, "CREATE OR REPLACE VIEW v AS SELECT 1"),
        (
            Postgres,
            "CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
        ),
        (Mysql, "CALL report_monthly()"),
        (Mysql, "SET SESSION sql_mode = 'STRICT_ALL_TABLES'"),
        (Mysql, "SET autocommit = 1"),
        (Mysql, "/*!40101 SET NAMES utf8mb4 */"),
        (Mysql, "/*!40103 SET TIME_ZONE='+00:00' */"),
        (
            Mysql,
            "/*!40111 SET @OLD_SQL_NOTES=@@SQL_NOTES, SQL_NOTES=0 */",
        ),
        (
            Mysql,
            "/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */",
        ),
        (
            Mysql,
            "/*!40014 SET @OLD_UNIQUE_CHECKS=@@UNIQUE_CHECKS, UNIQUE_CHECKS=0 */",
        ),
    ] {
        assert_eq!(dangerous_reason(sql, d), None, "{d:?}: {sql}");
    }
    for (d, sql) in [
        (Mysql, "SET sql_safe_updates = 0"),
        (Mysql, "SET SESSION foreign_key_checks = OFF"),
        (
            Mysql,
            "/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */",
        ),
        (Mysql, "SET sql_mode = ''"),
        (Postgres, "SET session_replication_role TO replica"),
        (Mysql, "CREATE OR REPLACE TABLE t (id int)"),
    ] {
        assert!(dangerous_reason(sql, d).is_some(), "{d:?}: {sql}");
    }
    // BUG-011's CALL now asks in every Safe Mode except Silent (the coordinator's round-2 rule).
    assert!(caution_reason("CALL report_monthly()", Mysql).is_some());
    assert!(caution_reason("CALL purge_everything()", Mysql).is_some());
    assert!(caution_reason("SET autocommit = 0", Mysql).is_some());
    assert!(caution_reason("SET autocommit = 1", Mysql).is_none());
}

/// NEW-9: classification stays linear on long inputs of unterminated versioned comments.
#[test]
fn classification_is_linear() {
    let sql = "/*!50000 ".repeat(80_000);
    let started = std::time::Instant::now();
    let _ = dangerous_reason(&sql, Mysql);
    let _ = is_read_only(&sql, Mysql);
    let _ = first_keyword(&sql, Mysql);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// FINAL-3: constants spelled with variables, casts, charsets, Unicode strings, numeric
/// underscores and typed literals, and simple self-comparisons through qualifiers or identity
/// arithmetic.
#[test]
fn remaining_guard_gaps_are_closed() {
    for (d, sql) in [
        (Mysql, "DELETE FROM t WHERE @unset_var IS NULL"),
        (Mysql, "DELETE FROM t WHERE CAST('1' AS SIGNED)"),
        (
            Mysql,
            "DELETE FROM t WHERE CONVERT('a' USING utf8mb4) = 'a'",
        ),
        (Postgres, "DELETE FROM t WHERE U&'\\0061' <> 'b'"),
        (Postgres, "DELETE FROM t WHERE 1_000 <> 2"),
        (
            Postgres,
            "DELETE FROM t WHERE current_date > DATE '2000-01-01'",
        ),
        (Mysql, "DELETE FROM t WHERE id = id + 0"),
        (Mysql, "DELETE FROM t WHERE id = id * 1"),
        (Mysql, "DELETE FROM t WHERE id - id = 0"),
        (Mysql, "DELETE FROM t WHERE id * 0 = 0"),
        (Mysql, "DELETE FROM t WHERE t.id = id"),
        (Mysql, "DELETE FROM t WHERE `id` = id"),
        (Postgres, "DELETE FROM t WHERE \"id\" = id"),
        (
            Mysql,
            "DELETE FROM t WHERE COALESCE(id, 0) = COALESCE(id, 0)",
        ),
        (Mysql, "DELETE FROM t WHERE id LIKE '%'"),
        (Postgres, "DELETE FROM t WHERE id::text = id::text"),
    ] {
        assert!(dangerous_reason(sql, d).is_some(), "{d:?}: {sql}");
    }
    for (d, sql) in [
        (Postgres, "DELETE FROM t USING u WHERE t.id = u.id"),
        (Mysql, "DELETE FROM t WHERE RAND(id) = RAND(id) + 1"),
        (Mysql, "DELETE FROM t WHERE 1_000 = 2"),
        (Mysql, "DELETE FROM t WHERE id = id + 1"),
        (Mysql, "DELETE FROM t WHERE date < DATE '2020-01-01'"),
        (Mysql, "DELETE FROM t WHERE name LIKE 'a%'"),
    ] {
        assert_eq!(dangerous_reason(sql, d), None, "{d:?}: {sql}");
    }
}
