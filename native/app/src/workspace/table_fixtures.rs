//! Fixed capture data. Metadata was read only from the throwaway test containers.
use super::fixtures;
use telecat_app::{table_model::*, workspace_model::*};
use telecat_core::{changes::Cell, filters::build_where, types::*};
fn log(time: &str, sql: impl Into<String>) -> LogEntry {
    LogEntry {
        time: format!("2026-10-17 {time}"),
        sql: sql.into(),
    }
}
fn metadata(key: &str, part: Part) -> QueryResult {
    let value: serde_json::Value = serde_json::from_str(include_str!("metadata.json")).unwrap();
    serde_json::from_value(
        value[key][if part == Part::Structure {
            "structure"
        } else {
            "indexes"
        }]
        .clone(),
    )
    .unwrap()
}
fn fill_metadata(s: &mut WorkspaceState, key: &str, part: Part, time: &str) {
    let structure = metadata(key, Part::Structure);
    let index = metadata(key, Part::Index);
    let sql = if part == Part::Structure {
        structure.sql.clone()
    } else {
        index.sql.clone()
    };
    let t = s.current_mut().table.as_mut().unwrap();
    t.structure.reset(structure, None);
    t.index.reset(index, None);
    t.part = part;
    s.logs.push(log(time, sql));
}
pub fn state(name: &str) -> WorkspaceState {
    let pg = name.starts_with('2');
    let mut s = fixtures::state(if pg { "95-rail" } else { "07-table-data" });
    s.connection = fixtures::connection(name);
    if pg {
        if name == "21-safe-prompt" {
            let query = "select current_setting('application_name') as app, true as b, now()::date as d, 1.50::numeric as n, '{\"x\":1}'::jsonb as j\nLIMIT 1000";
            s.current_mut().editor = EditorDocument {
                text: "UPDATE sa_banks SET is_disabled = false WHERE id = 1".into(),
                cursor_utf16: 50,
                selection: None,
                selection_from: 0,
            };
            s.current_mut().set_results(vec![QueryResult {
                columns: ["app", "b", "d", "n", "j"].map(str::to_owned).to_vec(),
                rows: vec![
                    ["Telecat", "true", "2026-10-17", "1.50", "{\"x\": 1}"]
                        .map(|s| Some(s.into()))
                        .to_vec(),
                ],
                duration_ms: 6,
                sql: query.into(),
                ..Default::default()
            }]);
            s.logs.push(log("08:34:52.4850", query));
            return s;
        }
        s.open_table(TableRef {
            schema: "billing".into(),
            name: "invoices".into(),
        });
        s.current_mut().total = 500;
        s.current_mut().grid.result = QueryResult {
            columns: ["id", "bank_id", "amount", "note"]
                .map(str::to_owned)
                .to_vec(),
            numeric: Some(vec![true, true, true, false]),
            rows: (1..=500)
                .map(|i| {
                    vec![
                        Some(i.to_string()),
                        Some((i % 3 + 1).to_string()),
                        Some(format!("{:.2}", i as f64 * 1.5)),
                        Some(format!("note {i}")),
                    ]
                })
                .collect(),
            ..Default::default()
        };
        s.current_mut().info = Some(TableInfo {
            data_size: "32 kB".into(),
            index_size: "32 kB".into(),
            total_size: "96 kB".into(),
            comment: String::new(),
        });
        s.logs = vec![
            log(
                "08:34:54.7660",
                "SELECT *, tableoid::oid::text AS \"__telecat_tableoid\", ctid::text AS \"__telecat_ctid\" FROM \"billing\".\"invoices\" ORDER BY \"id\" LIMIT 1000 OFFSET 0;",
            ),
            log(
                "08:34:54.7800",
                "SELECT COUNT(*) AS count FROM \"billing\".\"invoices\";",
            ),
        ];
        let data = TableDataResult {
            result: s.current().grid.result.clone(),
            total: 500,
            primary_key: vec!["id".into()],
            row_ids: None,
        };
        s.current_mut().table.as_mut().unwrap().load_data(&data);
        if name == "23-pg-structure" {
            fill_metadata(&mut s, "invoices", Part::Structure, "08:34:55.4510");
        }
        if name == "24-pg-index" {
            s.open_table(TableRef {
                schema: "public".into(),
                name: "sa_banks".into(),
            });
            s.current_mut().info = Some(TableInfo {
                data_size: "8192 bytes".into(),
                index_size: "32 kB".into(),
                total_size: "48 kB".into(),
                comment: String::new(),
            });
            fill_metadata(&mut s, "postgres", Part::Index, "08:34:56.1100");
            // Electron loaded Structure last to fill the index column-name picker.
            s.logs.push(log(
                "08:34:56.1140",
                metadata("postgres", Part::Structure).sql,
            ));
        }
        return s;
    }
    if name.starts_with('5') {
        s.logs[1].time = "2026-10-17 08:37:05.0470".into();
        s.logs[2].time = "2026-10-17 08:37:05.0510".into();
    }
    match name {
        "08-table-filter" | "52-filter" => {
            let t = s.current_mut().table.as_mut().unwrap();
            t.show_filters = true;
            t.filters = if name.starts_with("08") {
                vec![Filter {
                    value: "KBZ".into(),
                    ..new_filter()
                }]
            } else {
                vec![
                    Filter {
                        column: "bank_acronym".into(),
                        op: FilterOp::Prefix,
                        value: "K".into(),
                        ..new_filter()
                    },
                    Filter {
                        column: "bank_acronym".into(),
                        op: FilterOp::Eq,
                        value: "BBL".into(),
                        ..new_filter()
                    },
                ]
            };
            t.apply_filters(
                if name.starts_with("08") {
                    Combine::And
                } else {
                    Combine::Or
                },
                None,
            );
            let w = build_where(&t.applied, &t.data.result.columns, Driver::Mysql, t.combine);
            if name.starts_with("08") {
                t.data.result.rows.clear();
            } else {
                t.data.result.rows.truncate(3);
            }
            t.data.reset(t.data.result.clone(), None);
            let result = t.data.result.clone();
            let params = format!("\n-- params: {}", serde_json::to_string(&w.params).unwrap());
            s.current_mut().total = result.rows.len() as u64;
            s.current_mut().grid.result = result;
            s.logs = vec![
                log(
                    if name.starts_with("08") {
                        "08:34:41.8090"
                    } else {
                        "08:37:10.9000"
                    },
                    format!(
                        "SELECT * FROM `zixma`.`sa_banks`{} ORDER BY `id` LIMIT 1000 OFFSET 0;{params}",
                        w.sql
                    ),
                ),
                log(
                    if name.starts_with("08") {
                        "08:34:41.8110"
                    } else {
                        "08:37:10.9020"
                    },
                    format!(
                        "SELECT COUNT(*) as count FROM `zixma`.`sa_banks`{};{params}",
                        w.sql
                    ),
                ),
            ];
        }
        "09-structure" | "53-structure-staged" => {
            fill_metadata(
                &mut s,
                "mysql",
                Part::Structure,
                if name.starts_with("09") {
                    "08:34:42.6670"
                } else {
                    "08:37:12.5300"
                },
            );
            if name.starts_with("53") {
                let t = s.current_mut().table.as_mut().unwrap();
                let mut row = t.structure.rows.last().unwrap().cur.clone();
                for (i, c) in t.structure.result.columns.iter().enumerate() {
                    if !matches!(c.as_str(), "column_name" | "data_type" | "is_nullable") {
                        row[i] = Cell::Null;
                    }
                }
                t.structure.add(row);
            }
        }
        "10-index" => {
            s.logs
                .push(log("08:34:42.6670", metadata("mysql", Part::Structure).sql));
            fill_metadata(&mut s, "mysql", Part::Index, "08:34:43.5510");
        }
        "51-data-staged" => {
            let tab = s.current_mut();
            let t = tab.table.as_mut().unwrap();
            t.data.rows[5].cur[8] = Cell::Text("2026-10-17 00:38:40".into());
            t.data.rows[5].orig.as_mut().unwrap()[8] = Some("2026-10-17 00:38:40".into());
            t.data.delete(&[5]);
            let mut row = t.data.rows[5].cur.clone();
            for i in [0, 5, 7, 8, 9] {
                row[i] = Cell::unset();
            }
            t.data.add(row.clone());
            tab.grid.result.rows = t
                .data
                .rows
                .iter()
                .map(|r| r.cur.iter().map(|c| c.value().map(str::to_owned)).collect())
                .collect();
            tab.grid.selected_row = Some(6);
        }
        "54-index-staged" | "55-new-table" | "56-drop-truncate" => {
            s.logs.push(log("08:37:18.1000","SELECT column_name FROM information_schema.columns WHERE table_schema='zixma' AND table_name='sa_banks' AND column_name IN ('logo','swift_code')"));
            fill_metadata(&mut s, "mysql", Part::Index, "08:37:18.1060");
            let t = s.current_mut().table.as_mut().unwrap();
            t.index.set_cell(4, 3, Cell::Null);
            t.add_row(Driver::Mysql);
            let i = t.index.rows.len() - 1;
            t.index.set_cell(i, 0, Cell::Text("INDEX_swift".into()));
            if name != "54-index-staged" {
                s.logs.push(log("08:37:21.7770","SELECT index_name, non_unique, column_name FROM information_schema.statistics WHERE table_schema='zixma' AND table_name='sa_banks' AND index_name='INDEX_swift'"));
                let table = TableRef {
                    schema: "zixma".into(),
                    name: "untitled_table_337".into(),
                };
                s.open_table(table.clone());
                let mut t = TableEditor::new(table.clone(), Driver::Mysql, true);
                t.add_row(Driver::Mysql);
                t.structure.set_cell(1, 1, Cell::Text("title".into()));
                s.current_mut().table = Some(t);
                s.objects.push(DbObject {
                    table,
                    kind: ObjectKind::Table,
                });
                s.objects.sort_by_key(|o| o.table.name.to_lowercase());
            }
            if name == "56-drop-truncate" {
                let table = TableRef {
                    schema: "zixma".into(),
                    name: "nopk".into(),
                };
                s.open_table(table.clone());
                s.current_mut().info = Some(TableInfo {
                    data_size: "16 KB".into(),
                    index_size: "0 B".into(),
                    total_size: "16 KB".into(),
                    comment: String::new(),
                });
                let result = QueryResult {
                    columns: vec!["a".into(), "b".into()],
                    ..Default::default()
                };
                s.current_mut().grid.result = result.clone();
                s.current_mut()
                    .table
                    .as_mut()
                    .unwrap()
                    .data
                    .reset(result, None);
                s.operations.push(StagedOperation {
                    object: DbObject {
                        table,
                        kind: ObjectKind::Table,
                    },
                    truncate: true,
                });
                s.logs = vec![
                    log(
                        "08:37:27.7850",
                        "SELECT column_name FROM information_schema.key_column_usage WHERE table_schema='zixma' AND table_name='nopk' AND constraint_name='PRIMARY' ORDER BY ordinal_position",
                    ),
                    log(
                        "08:37:27.7870",
                        "SELECT * FROM `zixma`.`nopk` LIMIT 1000 OFFSET 0;",
                    ),
                    log(
                        "08:37:27.8040",
                        "SELECT COUNT(*) as count FROM `zixma`.`nopk`;",
                    ),
                ];
            }
        }
        _ => {}
    }
    // Metadata snapshots may include later QA changes; the captured structure had ten columns.
    if let Some(t) = &mut s.current_mut().table {
        if t.structure.result.columns.is_empty() {
            t.structure.result.columns = structure_columns(Driver::Mysql);
        }
    }
    s
}
