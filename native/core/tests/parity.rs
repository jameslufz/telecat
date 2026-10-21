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
/// Telecat's Safe Mode classification deliberately departs from Electron's, but only towards
/// caution: a reason Electron gives is kept word for word, Telecat may flag what Electron
/// didn't, and may call a statement not read-only that Electron called read-only. Everything
/// else (splitting, stripping, LIMIT, …) must match exactly.
#[test]
fn sql_parity() {
    let (mut flagged, mut not_read_only) = (vec![], 0);
    for (i, c) in fixture("sql").iter().enumerate() {
        let s = c["sql"].as_str().unwrap();
        let d = de(&c["d"]);
        let mut expected = c["out"].clone();
        let got = json!({"split":split_statements(s,d),"strip":strip_comments(s,d),"keyword":first_keyword(s,d),"limit":apply_limit(s,Some(100),d),"uglify":uglify(s,d),"at":c["positions"].as_array().unwrap().iter().map(|p|statement_at(s,p.as_u64().unwrap() as usize,d)).collect::<Vec<_>>()});
        let danger = expected.as_object_mut().unwrap().remove("danger").unwrap();
        let read_only = expected
            .as_object_mut()
            .unwrap()
            .remove("readOnly")
            .unwrap();
        assert_eq!(got, expected, "sql case {i}, {d:?}: {s:?}");
        let ours = dangerous_reason(s, d);
        if danger.is_null() {
            if let Some(r) = ours {
                flagged.push(format!("{d:?} {s:?} -> {r}"));
            }
        } else {
            assert_eq!(
                json!(ours),
                danger,
                "Electron's reason must be kept: case {i}, {d:?}: {s:?}"
            );
        }
        let ro = is_read_only(s, d);
        if read_only == json!(false) {
            assert!(
                !ro,
                "never less strict than Electron: case {i}, {d:?}: {s:?}"
            );
        } else if !ro {
            not_read_only += 1;
        }
    }
    println!(
        "{} statements newly flagged, {not_read_only} no longer read-only. Sample:\n{}",
        flagged.len(),
        flagged
            .iter()
            .filter(|f| !f.contains("statements:"))
            .take(60)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
/// Where Telecat's generated writes deliberately differ from Electron's, they must be strictly
/// narrower: the same statements, each WHERE extended with more conditions (and the COUNT
/// check that goes with it), or a refusal where Electron would have guessed.
fn narrower_writes(ours: &Value, electron: &Value) -> Result<(), String> {
    if ours == electron {
        return Ok(());
    }
    if let (Some(err), Some(theirs)) = (ours["error"].as_str(), electron["ok"].as_array()) {
        // Refusing a binary key whose display text can't be told apart from hex.
        let sql = theirs
            .iter()
            .map(|s| s["sql"].as_str().unwrap_or(""))
            .collect::<String>();
        return if err.contains(BINARY_AMBIGUOUS) && sql.contains("UNHEX") {
            Ok(())
        } else {
            Err(format!("refused: {err}"))
        };
    }
    let (Some(ours), Some(theirs)) = (ours["ok"].as_array(), electron["ok"].as_array()) else {
        return Err("outcome kind changed".into());
    };
    if ours.len() != theirs.len() {
        return Err("statement count changed".into());
    }
    for (a, b) in ours.iter().zip(theirs) {
        if a == b {
            continue;
        }
        let core = |s: &Value| {
            s["sql"]
                .as_str()
                .unwrap_or("")
                .trim_end_matches(';')
                .trim_end_matches(" LIMIT 1")
                .to_owned()
        };
        let (sa, sb) = (core(a), core(b));
        if a["expect"] != b["expect"] || !sa.starts_with(&format!("{sb} AND ")) {
            return Err(format!("not a narrower WHERE:\n  ours {sa}\n  theirs {sb}"));
        }
        let pa = a["params"].as_array().cloned().unwrap_or_default();
        let pb = b["params"].as_array().cloned().unwrap_or_default();
        if !pa.starts_with(&pb) {
            return Err("existing parameters changed".into());
        }
        let check = &a["check"];
        let unnumbered = |s: &str| {
            regex::Regex::new(r"\$\d+")
                .unwrap()
                .replace_all(s, "$$")
                .into_owned()
        };
        let predicate = unnumbered(&sa[sa.find(" WHERE ").unwrap_or(0)..]);
        if !(check == &b["check"]
            || (b["check"].is_null()
                && check["sql"]
                    .as_str()
                    .is_some_and(|c| unnumbered(c).ends_with(&predicate))))
        {
            return Err(format!(
                "COUNT check doesn't use the same predicate: {check}"
            ));
        }
    }
    Ok(())
}
#[test]
fn data_parity() {
    let mut narrower = 0;
    for (i, c) in fixture("data").iter().enumerate() {
        let rows: Vec<EditRow> = de(&c["rows"]);
        let ours = outcome(data_statements(
            de(&c["d"]),
            &de(&c["t"]),
            &de(&c["m"]),
            &rows,
        ));
        if ours != c["out"] {
            narrower += 1;
        }
        if let Err(e) = narrower_writes(&ours, &c["out"]) {
            panic!("data {i}: {e}\n{c}");
        }
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
    println!("{narrower} data cases deliberately narrower than Electron");
}
/// The only deliberate structure difference: a MySQL ALTER that changes a column type carries
/// a check that no stored value would change.
fn with_lossy_check_removed(ours: &Value, electron: &Value) -> Value {
    let mut ours = ours.clone();
    if let (Some(list), Some(theirs)) = (
        ours.get_mut("ok").and_then(Value::as_array_mut),
        electron["ok"].as_array(),
    ) {
        for (a, b) in list.iter_mut().zip(theirs) {
            if b.get("check").is_none()
                && a["sql"]
                    .as_str()
                    .is_some_and(|s| s.contains("CHANGE COLUMN"))
                && a["check"]["expect"] == json!(0)
            {
                a.as_object_mut().unwrap().remove("check");
            }
        }
    }
    ours
}
#[test]
fn structure_parity() {
    for (i, c) in fixture("structure").iter().enumerate() {
        let ours = outcome(structure_statements(de(&c["d"]), &de(&c["c"])));
        assert_eq!(
            with_lossy_check_removed(&ours, &c["out"]),
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
        // Electron also replaced the placeholder-looking text inside the quoted literal
        // (BUG-012); the literal must stay as written and parameters fill only real placeholders.
        let st: Stmt = de(&c["st"]);
        let p = |i: usize| match &st.params[i] {
            Value::Null => "NULL".to_owned(),
            v => literal(d, &js_string(v)),
        };
        let expected = if d == Driver::Mysql {
            format!("SELECT {}, '?', {}, {}", p(0), p(1), p(2))
        } else {
            format!("SELECT {}, '$1', {}, {}", p(1), p(0), p(2))
        };
        assert_eq!(
            st.sql,
            if d == Driver::Mysql {
                "SELECT ?, '?', ?, ?"
            } else {
                "SELECT $2, '$1', $1, $3"
            }
        );
        assert_eq!(inline_params(d, &st), expected);
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
