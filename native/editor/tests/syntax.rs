use telecat_editor::{
    Driver,
    syntax::{Kind, lex},
    utf16,
};
#[test]
fn codemirror_syntax_colors() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/syntax.json")).unwrap();
    let mut diffs = vec![];
    for case in cases.as_array().unwrap() {
        let text = case["input"].as_str().unwrap();
        let driver = if case["dialect"] == "mysql" {
            Driver::Mysql
        } else {
            Driver::Postgres
        };
        let actual = lex(text, driver);
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                continue;
            }
            let offset = utf16::from_byte(text, i);
            let expected = case["ranges"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| {
                    r["from"].as_u64().unwrap() as usize <= offset
                        && offset < (r["to"].as_u64().unwrap() as usize)
                })
                .map_or("Plain", |r| r["kind"].as_str().unwrap());
            let actual = actual
                .iter()
                .find(|t| t.range.contains(&i))
                .map_or(Kind::Name, |t| t.kind);
            if format!("{actual:?}") != expected {
                diffs.push(format!(
                    "{} byte={i} {c:?} expected={expected} actual={actual:?}",
                    case["id"]
                ));
            }
        }
    }
    assert!(
        diffs.is_empty(),
        "{} syntax differences:\n{}",
        diffs.len(),
        diffs.into_iter().take(60).collect::<Vec<_>>().join("\n")
    );
}
