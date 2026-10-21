//! QA final-round probes for the Round 2 guard. Printing only; findings go to the report.
//! cargo test -p telecat-core --test qa_final -- --nocapture
use Driver::*;
use telecat_core::{sql::*, types::*};

fn run(title: &str, cases: &[(Driver, &str)], expect_flag: bool) {
    println!("== {title}");
    let mut odd = vec![];
    for (d, s) in cases {
        let r = dangerous_reason(s, *d);
        let c = caution_reason(s, *d);
        println!(
            "{:<8} {:<72} danger={:<5} caution={:<5} ro={}",
            format!("{d:?}"),
            s,
            r.is_some(),
            c.is_some(),
            is_read_only(s, *d)
        );
        if r.is_some() != expect_flag {
            odd.push(format!("{d:?}: {s}  ->  {:?}", r));
        }
    }
    println!("-- {} unexpected:\n{}\n", odd.len(), odd.join("\n"));
}

#[test]
fn qa3_tautologies_with_columns() {
    run(
        "always-true but mentions a column (should flag)",
        &[
            (Mysql, "DELETE FROM t WHERE id = id + 0"),
            (Mysql, "DELETE FROM t WHERE id - id = 0"),
            (Mysql, "DELETE FROM t WHERE id * 0 = 0"),
            (Mysql, "DELETE FROM t WHERE id IS NOT NULL OR TRUE"),
            (Mysql, "DELETE FROM t WHERE TRUE OR id = 5"),
            (
                Mysql,
                "DELETE FROM t WHERE COALESCE(id, 0) = COALESCE(id, 0)",
            ),
            (Mysql, "DELETE FROM t WHERE LENGTH(name) >= 0"),
            (Mysql, "DELETE FROM t WHERE id IN (SELECT id FROM t)"),
            (
                Postgres,
                "DELETE FROM t WHERE EXISTS (SELECT 1 FROM t AS x WHERE x.id > 0)",
            ),
            (
                Postgres,
                "DELETE FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.flag)",
            ),
            (Mysql, "DELETE FROM t WHERE t.id = id"),
            (Mysql, "DELETE FROM t WHERE `id` = id"),
            (Postgres, "DELETE FROM t WHERE \"id\" = id"),
            (Mysql, "DELETE FROM t WHERE id LIKE '%'"),
            (Mysql, "DELETE FROM t WHERE NOT (id <> id)"),
            (Mysql, "DELETE FROM t WHERE id = ANY (SELECT id FROM t)"),
            (Postgres, "DELETE FROM t WHERE id IS NOT DISTINCT FROM id"),
            (Mysql, "DELETE FROM t WHERE (id = 1) IS NOT NULL"),
            (Mysql, "DELETE FROM t WHERE id = 1 OR 1"),
            (Mysql, "DELETE FROM t WHERE id = 1 || 1"),
            (Mysql, "DELETE FROM t WHERE IFNULL(id, 1)"),
            (Postgres, "DELETE FROM t WHERE id::text = id::text"),
            (Postgres, "UPDATE t SET v = 0 WHERE v = v"),
            (Mysql, "UPDATE t SET v = 0 WHERE v <=> v"),
        ],
        true,
    );
}

#[test]
fn qa3_column_free_where() {
    run(
        "WHERE without any column (should flag)",
        &[
            (Mysql, "DELETE FROM t WHERE 'a' <> 'b'"),
            (Mysql, "DELETE FROM t WHERE NOW() > '2000-01-01'"),
            (Mysql, "DELETE FROM t WHERE @x IS NULL"),
            (Mysql, "DELETE FROM t WHERE ? = ?"),
            (Postgres, "DELETE FROM t WHERE $1 = $1"),
            (Mysql, "DELETE FROM t WHERE RAND() < 2"),
            (Mysql, "DELETE FROM t WHERE CAST('1' AS SIGNED)"),
            (Mysql, "DELETE FROM t WHERE DATE('2024-01-01') IS NOT NULL"),
            (Postgres, "DELETE FROM t WHERE current_user = 'postgres'"),
            (
                Postgres,
                "DELETE FROM t WHERE current_date > DATE '2000-01-01'",
            ),
            (Postgres, "DELETE FROM t WHERE 'x'::text = 'x'"),
            (Mysql, "DELETE FROM t WHERE (SELECT COUNT(*) FROM u) >= 0"),
            (Mysql, "DELETE FROM t WHERE EXISTS (SELECT 1 FROM u)"),
            (Mysql, "DELETE FROM t WHERE 1 IN (SELECT 1 FROM u)"),
            (Mysql, "DELETE FROM t WHERE CURRENT_TIMESTAMP"),
            (
                Mysql,
                "DELETE FROM t WHERE INTERVAL 1 DAY + '2020-01-01' > '2000-01-01'",
            ),
            (Mysql, "DELETE FROM t WHERE ROW(1,2) = ROW(1,2)"),
            (Mysql, "DELETE FROM t WHERE 'a' COLLATE utf8mb4_bin = 'a'"),
            (
                Mysql,
                "DELETE FROM t WHERE CONVERT('a' USING utf8mb4) = 'a'",
            ),
            (Mysql, "DELETE FROM t WHERE CASE WHEN 1 THEN 1 END"),
            (Mysql, "DELETE FROM t WHERE JSON_VALID('{}')"),
        ],
        true,
    );
}

#[test]
fn qa3_common_column_names_not_flagged() {
    run(
        "ordinary WHEREs on keyword-like column names (should NOT flag)",
        &[
            (Mysql, "DELETE FROM t WHERE date < '2020-01-01'"),
            (Mysql, "UPDATE t SET x = 1 WHERE status = 'new'"),
            (Postgres, "UPDATE t SET x = 1 WHERE \"user\" = 'bob'"),
            (Mysql, "DELETE FROM t WHERE level > 3"),
            (Mysql, "DELETE FROM t WHERE year = 2024"),
            (Mysql, "DELETE FROM t WHERE month = 1"),
            (Mysql, "DELETE FROM t WHERE value IS NULL"),
            (Mysql, "DELETE FROM t WHERE name = 'x'"),
            (Mysql, "DELETE FROM t WHERE type = 'a'"),
            (Mysql, "DELETE FROM t WHERE position = 1"),
            (Mysql, "DELETE FROM t WHERE count = 0"),
            (Mysql, "DELETE FROM t WHERE time > 5"),
            (Mysql, "DELETE FROM t WHERE timestamp < NOW()"),
            (Mysql, "DELETE FROM t WHERE text = ''"),
            (Mysql, "DELETE FROM t WHERE action = 'x'"),
            (Mysql, "DELETE FROM t WHERE language = 'th'"),
            (Mysql, "DELETE FROM t WHERE data IS NULL"),
            (Mysql, "DELETE FROM t WHERE source = 'x'"),
            (Mysql, "DELETE FROM t WHERE format = 'x'"),
            (Mysql, "DELETE FROM t WHERE day = 1"),
            (Mysql, "DELETE FROM t WHERE hour = 1"),
            (Mysql, "DELETE FROM t WHERE second = 1"),
            (Mysql, "DELETE FROM t WHERE password = ''"),
            (Mysql, "DELETE FROM t WHERE comment = ''"),
            (Mysql, "DELETE FROM t WHERE role = 'x'"),
            (Mysql, "DELETE FROM t WHERE state = 'x'"),
            (Mysql, "DELETE FROM t WHERE `key` = 'x'"),
            (Mysql, "DELETE FROM t WHERE `order` = 1"),
            (Postgres, "DELETE FROM t WHERE \"order\" = 1"),
            (Postgres, "DELETE FROM t WHERE ctid = '(0,1)'"),
            (Postgres, "DELETE FROM t WHERE CURRENT OF cur"),
            (Mysql, "DELETE FROM t WHERE json->'$.a' = 1"),
            (Postgres, "DELETE FROM t WHERE data->>'a' = '1'"),
            (Postgres, "DELETE FROM t WHERE tags @> ARRAY['x']"),
            (
                Mysql,
                "DELETE FROM t WHERE created_at < NOW() - INTERVAL 30 DAY",
            ),
            (
                Mysql,
                "DELETE FROM t WHERE id IN (SELECT t_id FROM u WHERE u.flag = 1)",
            ),
            (Postgres, "DELETE FROM t USING u WHERE t.id = u.id"),
            (Mysql, "DELETE t FROM t JOIN u ON t.id = u.id WHERE u.x = 1"),
            (Mysql, "UPDATE t SET a = 1 WHERE id = ?"),
            (Postgres, "UPDATE t SET a = 1 WHERE id = $1"),
            (Mysql, "UPDATE t SET a = 1 WHERE MATCH(body) AGAINST('x')"),
            (Mysql, "DELETE FROM t WHERE ST_Contains(g, POINT(1,1))"),
            (Mysql, "DELETE FROM t WHERE BINARY name = 'x'"),
            (Mysql, "DELETE FROM t WHERE name REGEXP '^a'"),
            (Postgres, "DELETE FROM t WHERE name ~ '^a'"),
            (Postgres, "DELETE FROM t WHERE name ILIKE 'a%'"),
            (Postgres, "DELETE FROM t WHERE tsv @@ to_tsquery('x')"),
            (Mysql, "DELETE FROM t WHERE ไทย = 1"),
            (Mysql, "DELETE FROM t WHERE `ชื่อ` = 'x'"),
        ],
        false,
    );
}

#[test]
fn qa3_word_boundary_edges() {
    // Server-accepted statements where the guard and the server may still split words differently.
    run(
        "word boundary edges (should flag)",
        &[
            (Mysql, "DELETE FROM t AS a\u{2028}WHERE\u{2028}b"),
            (Mysql, "DELETE FROM t AS a\u{3000}WHERE\u{3000}b"),
            (Mysql, "DELETE FROM t AS a\u{feff}WHERE\u{feff}b"),
            (Mysql, "DELETE FROM t AS a\u{200b}WHERE\u{200b}b"),
            (Postgres, "DELETE FROM t AS a\u{200b}WHERE\u{200b}b"),
            (Mysql, "DELETE FROM t AS a\u{85}WHERE\u{85}b"),
            (Mysql, "DELETE FROM t AS `a`WHERE`b`"),
            (Mysql, "DELETE FROM t AS a\x0bWHERE id = 1 --\x0bOR 1=1"),
            (Mysql, "DELETE FROM t WHERE id = 1 --\x01OR 1=1"),
            (Mysql, "DELETE FROM t WHERE id = 1 --\u{a0}OR 1=1"),
            (Postgres, "DELETE FROM t WHERE id = 1 --\u{a0}x\nOR 1=1"),
            (Postgres, "DELETE FROM t WHERE $a\u{a0}$x$a\u{a0}$ <> 'y'"),
            (Postgres, "DELETE FROM t WHERE U&'\\0061' <> 'b'"),
            (Postgres, "DELETE FROM t WHERE B'1' = B'1'"),
            (Mysql, "DELETE FROM t WHERE _utf8mb4'a' <> _utf8mb4'b'"),
            (Mysql, "DELETE FROM t WHERE N'a' <> 'b'"),
            (Mysql, "DELETE FROM t WHERE X'61' <> 'b'"),
            (Mysql, "DELETE FROM t WHERE 0x61 <> 'b'"),
            (Mysql, "DELETE FROM t WHERE 1e1 <> 2"),
            (Mysql, "DELETE FROM t WHERE .5 <> 2"),
            (Mysql, "DELETE FROM t WHERE 1.e1 <> 2"),
            (Postgres, "DELETE FROM t WHERE 1_000 <> 2"),
            (Postgres, "DELETE FROM t WHERE 0x1F <> 2"),
        ],
        true,
    );
}
