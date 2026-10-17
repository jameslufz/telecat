use telecat_core::{changes::*, filters::build_where, sql::*, types::*};
#[test]
fn arbitrary_unicode_never_panics() {
    let mut state = 0x5eedfaceu64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens = [
        '\0', '\'', '"', '`', '\\', '/', '*', '!', '$', ';', '(', ')', '\n', '\r', '\u{feff}', 'ธ',
        '😺', '\u{2028}', 'A', '1',
    ];
    for _ in 0..10000 {
        let mut s = String::new();
        for _ in 0..next() % 150 {
            let n = next();
            s.push(if n % 3 == 0 {
                char::from_u32((n % 0x110000) as u32).unwrap_or('�')
            } else {
                tokens[n as usize % tokens.len()]
            });
        }
        for d in [Driver::Mysql, Driver::Postgres] {
            let _ = split_statements(&s, d);
            let _ = strip_comments(&s, d);
            let _ = statement_at(&s, next() as usize, d);
            let _ = first_keyword(&s, d);
            let _ = is_read_only(&s, d);
            let _ = dangerous_reason(&s, d);
            let _ = apply_limit(&s, Some(100), d);
            let _ = uglify(&s, d);
            let _ = quote_ident(d, &s);
            let _ = literal(d, &s);
            let table = TableRef {
                schema: s.clone(),
                name: s.clone(),
            };
            let columns = vec![
                "column_name".into(),
                "data_type".into(),
                "index_name".into(),
                s.clone(),
            ];
            let meta = TableMeta {
                columns: columns.clone(),
                primary_key: vec![s.clone()],
                binary: Some(vec![true]),
            };
            let r = EditRow {
                key: "fuzz".into(),
                orig: (next() % 2 == 0).then(|| vec![Some(s.clone()); (next() % 6) as usize]),
                cur: (0..next() % 6)
                    .map(|i| match i % 3 {
                        0 => Cell::Text(s.clone()),
                        1 => Cell::Null,
                        _ => Cell::unset(),
                    })
                    .collect(),
                deleted: next() % 3 == 0,
                rid: Some(vec![s.clone(); (next() % 4) as usize]),
            };
            let _ = data_statements(d, &table, &meta, std::slice::from_ref(&r));
            let _ = index_statements(d, &table, &columns, std::slice::from_ref(&r));
            let _ = structure_statements(
                d,
                &StructureChange {
                    table: table.clone(),
                    new_name: s.clone(),
                    is_new: next() % 2 == 0,
                    primary_key: vec![s.clone()],
                    columns: columns.clone(),
                    rows: vec![r],
                },
            );
            let _ = build_where(
                &[Filter {
                    enabled: true,
                    column: s.clone(),
                    op: FilterOp::Contains,
                    value: s.clone(),
                    value2: None,
                }],
                &columns,
                d,
                Combine::Or,
            );
        }
    }
    let nested = "/*!".repeat(20000);
    let _ = dangerous_reason(&nested, Driver::Mysql);
}
