//! Loading states: "Loading…" only after a short delay, previous content dimmed rather than
//! blanked, "No rows" only after a completed empty load, errors and cancellation distinct.
use std::time::{Duration, Instant};
use telecat_app::workspace_model::{LOADING_INDICATOR_DELAY, LoadState, Placeholder};

#[test]
fn fast_loads_change_nothing() {
    let now = Instant::now();
    let state = LoadState::Loading { since: now };
    // Within the delay: content stays as it is; an empty view stays blank (no "No rows").
    assert_eq!(state.placeholder(true, now), Placeholder::Pending);
    assert_eq!(
        state.placeholder(false, now + Duration::from_millis(100)),
        Placeholder::Pending
    );
}

#[test]
fn slow_loads_show_loading_and_keep_previous_content_dimmed() {
    let since = Instant::now();
    let later = since + LOADING_INDICATOR_DELAY + Duration::from_millis(1);
    let state = LoadState::Loading { since };
    assert_eq!(
        state.placeholder(true, later),
        Placeholder::Loading { dimmed: true }
    );
    assert_eq!(
        state.placeholder(false, later),
        Placeholder::Loading { dimmed: false }
    );
}

#[test]
fn no_rows_only_after_a_completed_empty_load() {
    let now = Instant::now();
    assert_eq!(
        LoadState::Loaded.placeholder(false, now),
        Placeholder::Empty
    );
    assert_eq!(LoadState::Loaded.placeholder(true, now), Placeholder::None);
    assert_eq!(LoadState::Idle.placeholder(false, now), Placeholder::None);
    assert_eq!(
        LoadState::loading().placeholder(false, now),
        Placeholder::Pending
    );
}

#[test]
fn errors_and_cancellation_are_distinct() {
    let now = Instant::now();
    assert_eq!(
        LoadState::finished::<()>(&Err("ER_NO_SUCH_TABLE: gone".into())).placeholder(true, now),
        Placeholder::Failed("ER_NO_SUCH_TABLE: gone".into())
    );
    assert_eq!(
        LoadState::finished::<()>(&Err("Loading the table was cancelled.".into())),
        LoadState::Cancelled
    );
    assert_eq!(
        LoadState::Cancelled.placeholder(true, now),
        Placeholder::Cancelled
    );
    assert_eq!(LoadState::finished(&Ok::<_, String>(1)), LoadState::Loaded);
}
