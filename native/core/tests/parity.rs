use serde_json::{Value, json};
#[test]
fn js_number_formatting() {
    for c in fixture("numbers") {
        assert_eq!(
            json!(telecat_core::changes::js_string(&c["input"])),
            c["out"]
        );
    }
}
#[test]
fn documented_safety_differences() {
    for c in fixture("safety-differences") {
        let err = telecat_core::changes::data_statements(
            de(&c["d"]),
            &de(&c["t"]),
            &de(&c["m"]),
            &de::<Vec<telecat_core::changes::EditRow>>(&c["rows"]),
        )
        .unwrap_err();
        assert_eq!(json!(err.to_string()), c["rustError"], "{}", c["name"]);
    }
}
use telecat_core::{changes::*, filters::*, sql::*, types::*};
fn fixture(s: &str) -> Vec<Value> {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/{s}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}
fn de<T: serde::de::DeserializeOwned>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap()
}
fn outcome<T: serde::Serialize>(r: anyhow::Result<T>) -> Value {
    match r {
        Ok(v) => json!({"ok":v}),
        Err(e) => json!({"error":e.to_string()}),
    }
}
#[test]
fn sql_parity() {
    for (i, c) in fixture("sql").iter().enumerate() {
        let s = c["sql"].as_str().unwrap();
        let d = de(&c["d"]);
        let got = json!({"split":split_statements(s,d),"strip":strip_comments(s,d),"keyword":first_keyword(s,d),"readOnly":is_read_only(s,d),"danger":dangerous_reason(s,d),"limit":apply_limit(s,Some(100),d),"uglify":uglify(s,d),"at":c["positions"].as_array().unwrap().iter().map(|p|statement_at(s,p.as_u64().unwrap() as usize,d)).collect::<Vec<_>>()});
        assert_eq!(got, c["out"], "sql case {i}, {d:?}: {s:?}");
    }
}
#[test]
fn data_parity() {
    for (i, c) in fixture("data").iter().enumerate() {
        let rows: Vec<EditRow> = de(&c["rows"]);
        assert_eq!(
            outcome(data_statements(
                de(&c["d"]),
                &de(&c["t"]),
                &de(&c["m"]),
                &rows
            )),
            c["out"],
            "data {i}"
        );
        assert_eq!(json!(is_modified(&rows[0])), c["modified"]);
        assert_eq!(json!(is_new_row(&rows[0])), c["new"]);
        assert_eq!(json!(has_changes(&rows)), c["has"]);
        assert_eq!(
            json!(
                (0..4)
                    .map(|j| cell_modified(&rows[0], j))
                    .collect::<Vec<_>>()
            ),
            c["cells"]
        );
    }
}
#[test]
fn structure_parity() {
    for (i, c) in fixture("structure").iter().enumerate() {
        assert_eq!(
            outcome(structure_statements(de(&c["d"]), &de(&c["c"]))),
            c["out"],
            "structure {i}: {c}"
        );
    }
}
#[test]
fn indexes_parity() {
    for (i, c) in fixture("indexes").iter().enumerate() {
        assert_eq!(
            outcome(index_statements(
                de(&c["d"]),
                &de(&c["t"]),
                &de::<Vec<String>>(&c["columns"]),
                &de::<Vec<EditRow>>(&c["rows"])
            )),
            c["out"],
            "indexes {i}"
        );
    }
}
#[test]
fn filters_parity() {
    for (i, c) in fixture("filters").iter().enumerate() {
        assert_eq!(
            json!(build_where(
                &de::<Vec<Filter>>(&c["filters"]),
                &de::<Vec<String>>(&c["columns"]),
                de(&c["d"]),
                de(&c["combine"])
            )),
            c["out"],
            "filters {i}"
        );
    }
}
#[test]
fn misc_parity() {
    for c in fixture("misc") {
        let d = de(&c["d"]);
        let s = c["s"].as_str().unwrap();
        assert_eq!(json!(quote_ident(d, s)), c["quote"]);
        assert_eq!(json!(literal(d, s)), c["literal"]);
        assert_eq!(json!(inline_params(d, &de(&c["st"]))), c["inline"]);
    }
}
#[test]
fn types_parity() {
    for c in fixture("types") {
        let config: ConnectionConfig = de(&c);
        assert_eq!(json!(config), c);
        assert_eq!(json!(empty_connection(config.driver)), c);
    }
}
#[test]
fn defaults_and_binary_edges() {
    for (i, c) in fixture("defaults").iter().enumerate() {
        assert_eq!(
            outcome(structure_statements(de(&c["d"]), &de(&c["c"]))),
            c["out"],
            "default {i}: {c}"
        );
    }
    for (i, c) in fixture("binary").iter().enumerate() {
        assert_eq!(
            outcome(data_statements(
                de(&c["d"]),
                &de(&c["t"]),
                &de(&c["m"]),
                &de::<Vec<EditRow>>(&c["rows"])
            )),
            c["out"],
            "binary {i}: {c}"
        );
    }
}
