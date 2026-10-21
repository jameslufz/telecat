//! Commit entry point for the table editor: review → explicit confirmation → guarded write.
use anyhow::{Result, bail};
use std::future::Future;
use telecat_core::{changes::Stmt, types::QueryResult};
use telecat_db::{CommitReview, Session};
pub async fn execute_reviewed<F, Fut>(
    session: &Session,
    statements: &[Stmt],
    confirm: F,
) -> Result<Vec<QueryResult>>
where
    F: FnOnce(CommitReview) -> Fut,
    Fut: Future<Output = bool>,
{
    execute_reviewed_part(session, statements, true, confirm).await
}
/// MySQL DDL implicitly commits; only data and PostgreSQL DDL use transactions.
pub async fn execute_reviewed_part<F, Fut>(
    session: &Session,
    statements: &[Stmt],
    transaction: bool,
    confirm: F,
) -> Result<Vec<QueryResult>>
where
    F: FnOnce(CommitReview) -> Fut,
    Fut: Future<Output = bool>,
{
    execute_reviewed_part_with(session, statements, transaction, &[], confirm).await
}
/// [`execute_reviewed_part`] with the session settings the values were typed under.
pub async fn execute_reviewed_part_with<F, Fut>(
    session: &Session,
    statements: &[Stmt],
    transaction: bool,
    settings: &[(String, String)],
    confirm: F,
) -> Result<Vec<QueryResult>>
where
    F: FnOnce(CommitReview) -> Fut,
    Fut: Future<Output = bool>,
{
    let review = session.review_commit(statements).await?;
    let approved = if review.non_transactional.is_empty() {
        false
    } else {
        if !confirm(review).await {
            bail!("Commit cancelled; nothing was written");
        }
        true
    };
    session
        .execute_after_review_with(statements, transaction, approved, settings)
        .await
}
/// Native confirmation, matching Electron's use of platform warning dialogs.
/// Closing the dialog is cancellation. The workspace awaits this before writing.
pub fn prompt(
    review: &CommitReview,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) -> impl Future<Output = bool> + use<> {
    let response = window.prompt(
        gpui_kit::PromptLevel::Warning,
        "This table cannot undo changes",
        Some(&review.message()),
        &["Cancel", "Continue"],
        cx,
    );
    async move { response.await == Ok(1) }
}
