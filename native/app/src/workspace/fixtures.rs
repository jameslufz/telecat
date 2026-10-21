use telecat_app::workspace_model::*;
use telecat_core::types::*;
pub const EMPTY_TABLE_COLUMNS: [&str; 4] = [
    "org_fingerprint",
    "user_fingerprint",
    "account_id",
    "created_at",
];
pub const SCENARIOS: &[&str] = &[
    "04-ws-mysql",
    "05-query-current",
    "06-query-all-rowselected",
    "07-table-data",
    "08-table-filter",
    "09-structure",
    "10-index",
    "21-safe-prompt",
    "22-pg-table",
    "23-pg-structure",
    "24-pg-index",
    "51-data-staged",
    "52-filter",
    "53-structure-staged",
    "54-index-staged",
    "55-new-table",
    "56-drop-truncate",
    "11-two-tables",
    "57-running",
    "58-empty-table",
    "59-pg-empty-table",
    "95-rail",
    "a1-preview",
    "a2-switched",
    "a3-shortcuts",
    "c1-lock",
    "d1-narrow-sidebar",
    "d2-loading",
    "d3-many-tabs",
    "d4-reconnecting",
    "d5-disconnected",
];
pub fn is_workspace_scenario(name: &str) -> bool {
    SCENARIOS.contains(&name) || name == "perf-1000"
}
pub fn height(name: &str) -> f32 {
    // The polish scenarios have no Electron reference, so they fit smaller displays too.
    if name.starts_with("d") && name.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
        return 760.;
    }
    if [
        "95-rail",
        "a1-preview",
        "a2-switched",
        "a3-shortcuts",
        "c1-lock",
    ]
    .contains(&name)
    {
        815.
    } else {
        816.
    }
}
pub fn connection(name: &str) -> ConnectionConfig {
    let pg = matches!(
        name,
        "95-rail"
            | "a2-switched"
            | "21-safe-prompt"
            | "22-pg-table"
            | "23-pg-structure"
            | "24-pg-index"
            | "59-pg-empty-table"
    );
    let mut c = empty_connection(if pg { Driver::Postgres } else { Driver::Mysql });
    c.id = if pg { "shell-pg" } else { "shell-mysql" }.into();
    c.name = if pg { "Local PG" } else { "Local" }.into();
    c.user = if pg { "postgres" } else { "root" }.into();
    c.database = if pg { "main_db" } else { "zixma" }.into();
    c.port = if pg { 5434 } else { 3307 };
    if matches!(
        name,
        "57-running" | "a1-preview" | "a3-shortcuts" | "c1-lock"
    ) {
        c.color = "#ff3ad9".into();
    }
    if matches!(
        name,
        "57-running"
            | "95-rail"
            | "a1-preview"
            | "a2-switched"
            | "a3-shortcuts"
            | "c1-lock"
            | "d4-reconnecting"
            | "d5-disconnected"
    ) {
        c.tag = "local".into();
    }
    if name == "c1-lock" {
        c.safe_mode = SafeMode::Safe2;
    }
    if matches!(
        name,
        "21-safe-prompt"
            | "22-pg-table"
            | "23-pg-structure"
            | "24-pg-index"
            | "51-data-staged"
            | "52-filter"
            | "53-structure-staged"
            | "54-index-staged"
            | "55-new-table"
            | "56-drop-truncate"
    ) {
        c.color = "#ff3ad9".into();
        c.tag = "local".into();
    }
    if name.starts_with('2') {
        c.safe_mode = SafeMode::Safe2;
        c.name = "Mochi".into();
    }
    c
}
const SQL: &str = "SELECT * FROM members\nWHERE 1\n-- AND MEMBER_TYPE = 'x;y'\nAND MEMBER_TYPE = 'member'\nORDER BY MEMBER_ID DESC;\n\nSELECT bank_name_th FROM sa_banks;";
const TABLES: &[&str] = &[
    "QA_Mixed", "members", "nopk", "qa_bit", "qa_blob", "qa_char", "qa_ci", "qa_cpk", "qa_dt",
    "qa_dup", "qa_dup2", "qa_float", "qa_g", "qa_json", "qa_pad", "sa_banks",
];
const PG_TABLES: &[&str] = &[
    "Mixed Tbl",
    "qa_atom",
    "qa_atom2",
    "qa_bit",
    "qa_bytea",
    "qa_char",
    "qa_cpk",
    "qa_dup",
    "qa_dup2",
    "qa_float",
    "qa_inh",
    "qa_inh_c",
    "qa_json",
    "qa_jsonb",
    "qa_part",
    "qa_point",
    "qa_s8a",
    "qa_s8c",
    "qa_ts",
    "qa_victim",
    "qa_victim2",
    "sa_banks",
    "QA Schema.Mixed Tbl",
    "billing.invoices",
];
const BANKS: &[&str] = &[
    "ธนาคารกรุงเทพ",
    "ธนาคารกสิกรไทย",
    "ธนาคารกรุงไทย",
    "ธนาคารไทยพาณิชย์",
    "ธนาคารกรุงศรีอยุธยา",
    "ธนาคารทดสอบ",
];
const ROUTINES: &str = "SELECT routine_name, routine_type FROM information_schema.routines WHERE routine_schema='zixma' ORDER BY routine_name";
const PG_OBJECTS: &str = "SELECT n.nspname, c.relname, c.relkind FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r','p','v','m','f') AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' AND NOT c.relispartition ORDER BY n.nspname <> 'public', n.nspname, c.relname";
const PG_FUNCTIONS: &str = "SELECT n.nspname, p.proname, p.prokind FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') ORDER BY n.nspname, p.proname";
fn log(time: &str, sql: &str) -> LogEntry {
    LogEntry {
        time: format!("2026-10-17 {time}"),
        sql: sql.into(),
    }
}
pub fn state(name: &str) -> WorkspaceState {
    if matches!(
        name,
        "08-table-filter"
            | "09-structure"
            | "10-index"
            | "21-safe-prompt"
            | "22-pg-table"
            | "23-pg-structure"
            | "24-pg-index"
            | "51-data-staged"
            | "52-filter"
            | "53-structure-staged"
            | "54-index-staged"
            | "55-new-table"
            | "56-drop-truncate"
    ) {
        return super::table_fixtures::state(name);
    }
    let mut s = WorkspaceState::new(connection(name));
    let pg = s.connection.driver == Driver::Postgres;
    s.server_version = if pg {
        "PostgreSQL 17.11 (Debian 17.11-1.pgdg13+2)"
    } else {
        "MySQL 8.0.46"
    }
    .into();
    s.tls = (!pg).then(|| "TLSv1.3".into());
    s.objects = if pg { PG_TABLES } else { TABLES }
        .iter()
        .map(|name| {
            let (schema, name) = if pg {
                name.rsplit_once('.').unwrap_or(("public", name))
            } else {
                ("zixma", *name)
            };
            DbObject {
                table: TableRef {
                    schema: schema.into(),
                    name: name.into(),
                },
                kind: ObjectKind::Table,
            }
        })
        .collect();
    if pg {
        s.objects.push(DbObject {
            table: TableRef {
                schema: "public".into(),
                name: "qa_fn".into(),
            },
            kind: ObjectKind::Function,
        });
    }
    s.logs = if pg {
        vec![
            log("08:35:27.2200", PG_OBJECTS),
            log(
                if name == "a2-switched" {
                    "08:35:13.5860"
                } else {
                    "08:35:27.2250"
                },
                PG_FUNCTIONS,
            ),
        ]
    } else {
        vec![
            log(
                "08:34:38.6380",
                "SELECT table_name, table_type FROM information_schema.tables WHERE table_schema='zixma' ORDER BY table_name",
            ),
            log("08:34:38.6770", ROUTINES),
        ]
    };
    if matches!(name, "05-query-current" | "06-query-all-rowselected") {
        s.current_mut().editor = EditorDocument {
            text: SQL.into(),
            cursor_utf16: SQL.encode_utf16().count(),
            selection: None,
            selection_from: 0,
        };
        let result = QueryResult {
            columns: vec!["bank_name_th".into()],
            rows: BANKS.iter().map(|s| vec![Some((*s).into())]).collect(),
            duration_ms: if name.starts_with("05") { 22 } else { 8 },
            sql: "SELECT bank_name_th FROM sa_banks\nLIMIT 1000".into(),
            ..Default::default()
        };
        if name.starts_with("06") {
            s.current_mut().set_results(vec![
                QueryResult {
                    columns: vec!["MEMBER_ID".into()],
                    duration_ms: 9,
                    ..Default::default()
                },
                result,
            ]);
            s.current_mut().grid.selected_row = Some(0);
            s.logs = vec![
                log(
                    "08:34:39.9820",
                    "SELECT * FROM members\nWHERE 1\n-- AND MEMBER_TYPE = 'x;y'\nAND MEMBER_TYPE = 'member'\nORDER BY MEMBER_ID DESC\nLIMIT 1000",
                ),
                log(
                    "08:34:39.9920",
                    "SELECT bank_name_th FROM sa_banks\nLIMIT 1000",
                ),
            ];
        } else {
            s.current_mut().set_results(vec![result]);
            s.logs.push(log(
                "08:34:39.6140",
                "SELECT bank_name_th FROM sa_banks\nLIMIT 1000",
            ));
        }
    }
    if matches!(name, "58-empty-table" | "59-pg-empty-table") {
        let table = TableRef {
            schema: if pg { "public" } else { "zixma" }.into(),
            name: "user_bindings".into(),
        };
        s.objects.push(DbObject {
            table: table.clone(),
            kind: ObjectKind::Table,
        });
        s.open_table(table.clone());
        let result = QueryResult {
            columns: EMPTY_TABLE_COLUMNS.map(str::to_owned).to_vec(),
            sql: format!(
                "SELECT * FROM {} LIMIT 1000 OFFSET 0",
                telecat_core::changes::qualified(s.connection.driver, &table)
            ),
            ..Default::default()
        };
        s.logs.push(log("08:34:39.6140", &result.sql));
        s.current_mut().grid.result = result;
        s.current_mut().info = Some(TableInfo {
            data_size: if pg { "0 bytes" } else { "16 KB" }.into(),
            index_size: if pg { "0 bytes" } else { "0 B" }.into(),
            total_size: if pg { "8192 bytes" } else { "16 KB" }.into(),
            comment: String::new(),
        });
    }
    if matches!(
        name,
        "07-table-data" | "11-two-tables" | "a1-preview" | "a3-shortcuts"
    ) {
        let table = TableRef {
            schema: "zixma".into(),
            name: "sa_banks".into(),
        };
        s.open_table(table);
        s.current_mut().total = 6;
        s.current_mut().info = Some(TableInfo {
            data_size: "16 KB".into(),
            index_size: "48 KB".into(),
            total_size: "64 KB".into(),
            comment: "banks".into(),
        });
        s.current_mut().grid.result = QueryResult {
            columns: [
                "id",
                "bank_code",
                "bank_name_th",
                "bank_name_en",
                "bank_acronym",
                "logo",
                "country_code",
                "is_disabled",
                "created_at",
                "swift_code",
            ]
            .map(str::to_owned)
            .to_vec(),
            rows: (0..6)
                .map(|i| {
                    vec![
                        Some(["1", "2", "3", "4", "5", "7"][i].into()),
                        Some(["002", "004", "006", "014", "025", "777"][i].into()),
                        Some(BANKS[i].into()),
                        Some(
                            [
                                "BANGKOK EDITED",
                                "Kasikornbank Public Company Limited",
                                "Krungthai Bank Public Company Limited",
                                "The Siam Commercial Bank",
                                "BANK OF AYUDHYA",
                                "Test Bank",
                            ][i]
                                .into(),
                        ),
                        Some(["BBL", "KBANK", "KTB", "SCB", "BAY", "TST"][i].into()),
                        None,
                        Some("TH".into()),
                        Some("0".into()),
                        Some("2026-10-17 00:35:38".into()),
                        None,
                    ]
                })
                .collect(),
            ..Default::default()
        };
        if name == "a1-preview" {
            s.current_mut().grid.selected_row = Some(1);
        }
        let (table, key, t1, t2) = if name == "11-two-tables" {
            s.open_table(TableRef {
                schema: "zixma".into(),
                name: "members".into(),
            });
            s.current_mut().total = 2500;
            s.current_mut().grid.result = QueryResult {
                columns: [
                    "MEMBER_ID",
                    "MEMBER_USERNAME",
                    "MEMBER_TYPE",
                    "DELETE",
                    "ACTIVE",
                    "CREATE_DATE",
                ]
                .map(str::to_owned)
                .to_vec(),
                rows: (1..=1000)
                    .map(|i| {
                        vec![
                            Some(i.to_string()),
                            Some(format!("user{i}")),
                            Some(if i % 3 == 0 { "agent" } else { "member" }.into()),
                            Some("0".into()),
                            Some("1".into()),
                            Some("2026-10-17 00:35:38".into()),
                        ]
                    })
                    .collect(),
                ..Default::default()
            };
            s.current_mut().info = Some(TableInfo {
                data_size: "160 KB".into(),
                index_size: "0 B".into(),
                total_size: "160 KB".into(),
                comment: "".into(),
            });
            ("members", "MEMBER_ID", "08:34:44.7790", "08:34:44.7930")
        } else if name.starts_with('a') {
            ("sa_banks", "id", "08:35:07.9280", "08:35:07.9400")
        } else {
            ("sa_banks", "id", "08:34:41.4420", "08:34:41.4430")
        };
        s.logs = vec![
            log(
                "08:34:41.4400",
                &format!(
                    "SELECT column_name FROM information_schema.key_column_usage WHERE table_schema='zixma' AND table_name='{table}' AND constraint_name='PRIMARY' ORDER BY ordinal_position"
                ),
            ),
            log(
                t1,
                &format!("SELECT * FROM `zixma`.`{table}` ORDER BY `{key}` LIMIT 1000 OFFSET 0;"),
            ),
            log(
                t2,
                &format!("SELECT COUNT(*) as count FROM `zixma`.`{table}`;"),
            ),
        ];
        if name == "a3-shortcuts" {
            s.activate(1);
        }
    }
    if name == "d1-narrow-sidebar" {
        for long in [
            "log_call_back_winnerxpay_deposit",
            "log_call_back_winnerxpay_withdraw",
            "log_call_back_worldpay_settlement_reconciliation",
        ] {
            s.objects.push(DbObject {
                table: TableRef {
                    schema: "zixma".into(),
                    name: long.into(),
                },
                kind: ObjectKind::Table,
            });
        }
        s.objects.sort_by(|a, b| a.table.name.cmp(&b.table.name));
    }
    if name == "d2-loading" {
        s.open_table(TableRef {
            schema: "zixma".into(),
            name: "sa_banks".into(),
        });
        let since = std::time::Instant::now() - std::time::Duration::from_secs(1);
        s.current_mut().loads.insert(
            LoadSlot::Part(telecat_app::table_model::Part::Data),
            LoadState::Loading { since },
        );
    }
    if name == "d3-many-tabs" {
        for i in 1..=6 {
            s.open_query(&format!("SQL Query {i}"), "");
        }
        for table in [
            "sa_banks",
            "log_call_back_winnerxpay_deposit_history",
            "members",
            "nopk",
        ] {
            s.open_table(TableRef {
                schema: "zixma".into(),
                name: table.into(),
            });
        }
        s.open_query(
            "A query with a very long title that does not fit in one tab",
            "",
        );
    }
    if name == "c1-lock" {
        let sql = "UPDATE sa_banks SET bank_code = '002' WHERE id = 1";
        s.current_mut().editor = EditorDocument {
            text: sql.into(),
            cursor_utf16: sql.len(),
            selection: None,
            selection_from: 0,
        };
        s.current_mut().set_results(vec![QueryResult {
            sql: sql.into(),
            affected_rows: Some(1),
            duration_ms: 15,
            command: Some("Rows matched: 1  Changed: 0  Warnings: 0".into()),
            ..Default::default()
        }]);
        s.logs = vec![log("08:35:39.5250", ROUTINES), log("08:35:40.0910", sql)];
    }
    if name == "57-running" {
        s.objects.sort_by(|a, b| {
            a.table
                .name
                .to_lowercase()
                .cmp(&b.table.name.to_lowercase())
        });
        s.objects.push(DbObject {
            table: TableRef {
                schema: "zixma".into(),
                name: "untitled_table_337".into(),
            },
            kind: ObjectKind::Table,
        });
        for table in ["sa_banks", "untitled_table_337", "nopk"] {
            s.open_table(TableRef {
                schema: "zixma".into(),
                name: table.into(),
            });
        }
        s.open_running();
        s.current_mut().grid.result = QueryResult {
            columns: [
                "id", "user", "host", "db", "command", "time", "state", "query",
            ]
            .map(str::to_owned)
            .to_vec(),
            rows: vec![
                [
                    "372",
                    "root",
                    "172.17.0.1:64554",
                    "zixma",
                    "Query",
                    "5",
                    "User sleep",
                    "SELECT SLEEP(30) AS s",
                ]
                .map(|s| Some(s.into()))
                .to_vec(),
            ],
            ..Default::default()
        };
        s.logs = vec![
            log("08:37:32.0900", "SELECT COUNT(*) FROM nopk"),
            log("08:37:32.7670", "SELECT SLEEP(30) AS s\nLIMIT 1000"),
        ];
    }
    for tab in &mut s.tabs {
        if let Some(t) = &mut tab.table {
            t.load_data(&TableDataResult {
                result: tab.grid.result.clone(),
                total: tab.total,
                primary_key: match t.table.name.as_str() {
                    "user_bindings" => vec![],
                    "members" => vec!["MEMBER_ID".into()],
                    _ => vec!["id".into()],
                },
                row_ids: (pg && t.table.name == "user_bindings").then(Vec::new),
            });
            if name == "a1-preview" {
                let mut row = t.data.rows[0].cur.clone();
                row[0] = telecat_core::changes::Cell::unset();
                t.data.add(row);
                t.data.add(vec![
                    telecat_core::changes::Cell::unset();
                    t.data.result.columns.len()
                ]);
            }
        }
    }
    if name == "perf-1000" {
        let sql = "-- 1000 × 15 deterministic fixture rows, including Thai and emoji\nSELECT * FROM perf_fixture LIMIT 1000;";
        s.current_mut().editor.text = sql.into();
        let columns = [
            "id",
            "bank_code",
            "bank_name_th",
            "bank_name_en",
            "bank_acronym",
            "country_code",
            "is_disabled",
            "created_at",
            "swift_code",
            "balance",
            "bit_flags",
            "payload",
            "comment",
            "nullable",
            "emoji",
        ];
        let rows = (0..1000)
            .map(|i| {
                vec![
                    Some((i + 1).to_string()),
                    Some(format!("{:03}", 2 + i % 6)),
                    Some(BANKS[i % BANKS.len()].into()),
                    Some("BANGKOK BANK".into()),
                    Some("BBL".into()),
                    Some("TH".into()),
                    Some("0".into()),
                    Some("2026-10-17 00:35:38".into()),
                    None,
                    Some(format!("{}.25", i * 100)),
                    Some("00101".into()),
                    Some(format!(r#"{{"row":{},"active":true}}"#, i + 1)),
                    Some(format!("แถวที่ {} — fixture", i + 1)),
                    None,
                    Some("🐈 🏦".into()),
                ]
            })
            .collect();
        s.current_mut().set_results(vec![QueryResult {
            columns: columns.into_iter().map(str::to_owned).collect(),
            rows,
            duration_ms: 12,
            sql: sql.into(),
            ..Default::default()
        }]);
        s.logs = vec![log("08:34:39.6140", sql)];
    }
    s
}
pub const PREVIEW: &str = "INSERT INTO `zixma`.`sa_banks` (`bank_code`, `bank_name_th`, `bank_name_en`, `bank_acronym`, `logo`, `country_code`, `is_disabled`, `created_at`, `swift_code`) VALUES ('002', 'ธนาคารกรุงเทพ', 'BANGKOK EDITED', 'BBL', NULL, 'TH', '0', '2026-10-17 00:35:38', NULL);\n\nINSERT INTO `zixma`.`sa_banks` () VALUES ();";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scenarios_have_valid_state() {
        for name in SCENARIOS {
            let s = state(name);
            assert!(s.tabs.iter().any(|t| t.id == s.active));
            assert!(s.objects.iter().any(|o| o.table.name == "sa_banks"));
            assert!(
                s.logs
                    .iter()
                    .all(|l| !l.time.is_empty() && !l.sql.is_empty())
            );
        }
        let s = state("06-query-all-rowselected");
        assert_eq!(
            s.current().grid.result.rows[0][0].as_deref(),
            Some("ธนาคารกรุงเทพ")
        );
        assert_eq!(s.current().grid.selected_row, Some(0));
        let s = state("a1-preview");
        assert_eq!(s.current().grid.result.rows.len(), 6);
        assert_eq!(s.current().grid.selected_row, Some(1));
        let s = state("11-two-tables");
        assert_eq!(s.current().grid.result.rows.len(), 1000);
        assert_eq!(s.current().total, 2500);
    }
}
