use serde_json::Value;
#[test]
fn buffer_display_matches_electron() {
    let v: Vec<Value> =
        serde_json::from_str(include_str!("../../core/tests/fixtures/cells.json")).unwrap();
    for c in v {
        let bytes: Vec<u8> = serde_json::from_value(c["bytes"].clone()).unwrap();
        assert_eq!(serde_json::json!(telecat_db::buffer_cell(&bytes)), c["out"]);
    }
}
#[test]
fn byte_sizes_match_electron() {
    let v: Vec<Value> =
        serde_json::from_str(include_str!("../../core/tests/fixtures/bytes.json")).unwrap();
    for c in v {
        let n = c["n"]
            .as_f64()
            .or_else(|| c["n"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0.0);
        assert_eq!(serde_json::json!(telecat_db::format_bytes(n)), c["out"]);
    }
}
