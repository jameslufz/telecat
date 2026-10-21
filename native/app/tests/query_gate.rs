//! NEW-6: routine statements only ask for confirmation outside Silent mode; real dangers always.
use telecat_app::workspace_model::{EditorDocument, QueryPlan};
use telecat_core::types::{Driver, SafeMode};

fn dangers(sql: &str, mode: SafeMode) -> usize {
    let document = EditorDocument {
        text: sql.into(),
        ..Default::default()
    };
    QueryPlan::new(&document, true, Driver::Mysql, mode, 0)
        .dangers
        .len()
}

#[test]
fn routine_statements_ask_only_outside_silent_mode() {
    for sql in ["CALL report_monthly()", "SET autocommit = 0"] {
        assert_eq!(dangers(sql, SafeMode::Silent), 0, "{sql}");
        for mode in [
            SafeMode::Alert1,
            SafeMode::Alert2,
            SafeMode::Safe1,
            SafeMode::Safe2,
        ] {
            assert_eq!(dangers(sql, mode), 1, "{sql} in {mode:?}");
        }
    }
    for sql in ["DELETE FROM t", "SET foreign_key_checks = 0"] {
        assert_eq!(dangers(sql, SafeMode::Silent), 1, "{sql}");
    }
    assert_eq!(
        dangers(
            "SELECT 1; SET NAMES utf8mb4; CREATE OR REPLACE VIEW v AS SELECT 1",
            SafeMode::Safe2
        ),
        0
    );
}
