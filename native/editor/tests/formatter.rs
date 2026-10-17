use telecat_editor::{Driver, format::beautify};
#[test]
fn real_sql_formatter_parity() {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/beautify.json")).unwrap();
    let cases = data["fixtures"].as_array().unwrap();
    let mut matched = 0;
    let mut diffs = vec![];
    for case in cases {
        let Some(expected) = case["output"].as_str() else {
            continue;
        };
        let driver = if case["dialect"] == "mysql" {
            Driver::Mysql
        } else {
            Driver::Postgres
        };
        let actual = beautify(case["input"].as_str().unwrap(), driver);
        if actual == expected {
            matched += 1;
        } else {
            diffs.push(serde_json::json!({"id":case["id"],"input":case["input"],"expected":expected,"actual":actual}));
        }
    }
    println!(
        "BEAUTIFY {matched}/{} = {:.2}%",
        cases.len(),
        matched as f64 * 100. / cases.len() as f64
    );
    if std::env::var_os("TELECAT_UPDATE_DIFFS").is_some() {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/beautify-differences.json"
            ),
            serde_json::to_string_pretty(&diffs).unwrap() + "\n",
        )
        .unwrap();
    } else {
        let recorded: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/beautify-differences.json")).unwrap();
        assert_eq!(
            serde_json::json!(diffs),
            recorded,
            "formatter parity changed; regenerate/review the diff ledger"
        );
    }
    assert!(
        matched * 100 >= cases.len() * 90,
        "common-query parity regressed"
    );
}
