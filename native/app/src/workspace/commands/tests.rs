//! Shortcuts arrive as key bindings, which GPUI dispatches while it is already updating the
//! focused window. These press real keystrokes so the global handlers run in that state.
use crate::workspace::{Workspace, components::tests::setup};
use gpui::{AnyWindowHandle, TestAppContext};
use gpui_kit::{AppContext, Entity};
use telecat_core::changes::Cell;

fn open(cx: &mut TestAppContext, scenario: &str) -> (AnyWindowHandle, Entity<Workspace>) {
    let (window, workspace) = setup(cx, scenario);
    let services = workspace.read_with(cx, |w, _| w.services.clone());
    cx.update(|cx| crate::install_menus(services, cx));
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    paint(cx, window);
    (window, workspace)
}
fn paint(cx: &mut TestAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
}
fn stage_edit(cx: &mut TestAppContext, workspace: &Entity<Workspace>) {
    workspace.update(cx, |w, _| {
        w.state.current_mut().table.as_mut().unwrap().data.set_cell(
            0,
            1,
            Cell::Text("staged".into()),
        )
    });
    assert!(workspace.read_with(cx, |w, _| w.state.has_changes()));
}

#[gpui::test]
fn layout_and_tab_shortcuts_reach_the_workspace(cx: &mut TestAppContext) {
    let (window, workspace) = open(cx, "04-ws-mysql");
    let tabs = workspace.read_with(cx, |w, _| w.state.tabs.len());
    cx.simulate_keystrokes(window, "cmd-0 cmd-shift-0 cmd-shift-c cmd-t");
    workspace.read_with(cx, |w, _| {
        assert!(!w.sidebar, "cmd-0 hides the sidebar");
        assert!(!w.panel, "cmd-shift-0 hides the right panel");
        assert!(!w.console, "cmd-shift-c hides the console");
        assert_eq!(w.state.tabs.len(), tabs + 1, "cmd-t opens a query tab");
    });
    cx.simulate_keystrokes(window, "cmd-0");
    assert!(workspace.read_with(cx, |w, _| w.sidebar));
}

#[gpui::test]
fn cmd_f_opens_the_table_filters(cx: &mut TestAppContext) {
    let (window, workspace) = open(cx, "07-table-data");
    cx.simulate_keystrokes(window, "cmd-f");
    assert!(workspace.read_with(cx, |w, _| {
        w.state.current().table.as_ref().unwrap().show_filters
    }));
}

#[gpui::test]
fn cmd_z_and_cmd_shift_z_undo_and_redo_a_staged_cell(cx: &mut TestAppContext) {
    let (window, workspace) = open(cx, "07-table-data");
    stage_edit(cx, &workspace);
    cx.simulate_keystrokes(window, "cmd-z");
    assert!(!workspace.read_with(cx, |w, _| w.state.has_changes()));
    cx.simulate_keystrokes(window, "cmd-shift-z");
    assert!(workspace.read_with(cx, |w, _| w.state.has_changes()));
}

#[gpui::test]
fn cmd_q_with_staged_changes_asks_before_quitting(cx: &mut TestAppContext) {
    let (window, workspace) = open(cx, "07-table-data");
    stage_edit(cx, &workspace);
    cx.simulate_keystrokes(window, "cmd-q");
    assert!(
        cx.has_pending_prompt(),
        "staged edits must not be discarded silently"
    );
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(workspace.read_with(cx, |w, _| w.state.has_changes()));
}

#[gpui::test]
fn cmd_shift_w_with_staged_changes_asks_before_closing(cx: &mut TestAppContext) {
    let (window, workspace) = open(cx, "07-table-data");
    stage_edit(cx, &workspace);
    cx.simulate_keystrokes(window, "cmd-shift-w");
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(cx.windows().contains(&window));
}

#[gpui::test]
fn menu_bar_commands_reach_the_workspace(cx: &mut TestAppContext) {
    let (_, workspace) = open(cx, "04-ws-mysql");
    cx.update(|cx| cx.dispatch_action(&super::Command::new("toggle-sidebar")));
    cx.run_until_parked();
    assert!(!workspace.read_with(cx, |w, _| w.sidebar));
}
