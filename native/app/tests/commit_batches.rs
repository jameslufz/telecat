//! BUG-009: one ⌘S is all-or-nothing wherever the server allows it.
use telecat_app::{
    table_model::Part,
    workspace_model::{
        CommitBatch, CommitTarget, CommitUnit, StagedOperation, commit_batches,
        partial_commit_notice,
    },
};
use telecat_core::{changes::Stmt, types::*};

fn unit(target: CommitTarget, sql: &str, transaction: bool) -> CommitUnit {
    CommitUnit {
        target,
        statements: vec![Stmt::new(sql)],
        transaction,
        session: vec![],
        verify: vec![],
    }
}
fn op() -> CommitTarget {
    CommitTarget::Operation(StagedOperation {
        object: DbObject {
            table: TableRef {
                schema: "s".into(),
                name: "old".into(),
            },
            kind: ObjectKind::Table,
        },
        truncate: false,
    })
}
fn sql(b: &CommitBatch) -> Vec<&str> {
    b.statements.iter().map(|s| s.sql.as_str()).collect()
}
/// What collect_changes produces: new tables first, then per tab data, structure, index, then
/// staged table operations.
fn save(d: Driver) -> Vec<CommitUnit> {
    let pg = d == Driver::Postgres;
    vec![
        unit(
            CommitTarget::Table(1, Part::Structure),
            "CREATE TABLE n",
            pg,
        ),
        unit(CommitTarget::Table(2, Part::Data), "UPDATE a", true),
        unit(CommitTarget::Table(2, Part::Structure), "ALTER TABLE a", pg),
        unit(CommitTarget::Table(3, Part::Data), "DELETE b", true),
        unit(op(), "DROP TABLE old", false),
    ]
}

#[test]
fn postgres_runs_the_whole_save_in_one_transaction() {
    let batches = commit_batches(save(Driver::Postgres), Driver::Postgres);
    assert_eq!(batches.len(), 1);
    assert!(batches[0].transaction);
    assert_eq!(batches[0].targets.len(), 5);
    assert_eq!(
        sql(&batches[0]),
        [
            "CREATE TABLE n",
            "UPDATE a",
            "ALTER TABLE a",
            "DELETE b",
            "DROP TABLE old"
        ]
    );
    assert_eq!(partial_commit_notice(&batches), None);
}

#[test]
fn mysql_runs_every_row_edit_in_one_transaction_before_implicit_commits() {
    let batches = commit_batches(save(Driver::Mysql), Driver::Mysql);
    let shape: Vec<_> = batches.iter().map(|b| (b.transaction, sql(b))).collect();
    assert_eq!(
        shape,
        [
            (true, vec!["UPDATE a", "DELETE b"]),
            (false, vec!["CREATE TABLE n"]),
            (false, vec!["ALTER TABLE a"]),
            (false, vec!["DROP TABLE old"]),
        ]
    );
    assert_eq!(batches[0].targets.len(), 2);
    let notice = partial_commit_notice(&batches).expect("must be explained before running");
    assert!(
        notice.contains("Row edits are saved first") && notice.contains("3 steps"),
        "{notice}"
    );
}

#[test]
fn mysql_saves_that_are_already_atomic_need_no_notice() {
    let rows = vec![
        unit(CommitTarget::Table(2, Part::Data), "UPDATE a", true),
        unit(CommitTarget::Table(3, Part::Data), "DELETE b", true),
    ];
    let batches = commit_batches(rows, Driver::Mysql);
    assert_eq!(batches.len(), 1);
    assert_eq!(partial_commit_notice(&batches), None);
    let one_alter = vec![unit(
        CommitTarget::Table(2, Part::Structure),
        "ALTER TABLE a",
        false,
    )];
    assert_eq!(
        partial_commit_notice(&commit_batches(one_alter, Driver::Mysql)),
        None
    );
    assert!(commit_batches(vec![], Driver::Postgres).is_empty());
}
/// Query-result edits typed under the editor session's settings are saved in their own batch,
/// with those settings, so they never change how other edits are read.
#[test]
fn result_edits_with_session_settings_get_their_own_batch() {
    for driver in [Driver::Mysql, Driver::Postgres] {
        let mut result = unit(CommitTarget::Result(9), "UPDATE r", true);
        result.session = vec![("time_zone".into(), "+07:00".into())];
        let batches = commit_batches(
            vec![
                unit(CommitTarget::Table(2, Part::Data), "UPDATE a", true),
                result,
            ],
            driver,
        );
        assert_eq!(batches.len(), 2, "{driver:?}");
        assert!(batches[0].session.is_empty());
        assert_eq!(batches[1].targets.len(), 1);
        assert_eq!(
            batches[1].session,
            [("time_zone".to_owned(), "+07:00".to_owned())]
        );
        assert!(batches[1].transaction);
    }
}
