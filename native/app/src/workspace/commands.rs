use super::*;
#[cfg(test)]
mod tests;
#[derive(Clone, Debug, PartialEq, serde::Deserialize, Action)]
#[action(namespace = workspace, no_json)]
pub struct Command {
    pub name: String,
}
impl Command {
    pub fn new(name: &str) -> Self {
        Self { name: name.into() }
    }
}
/// Every workspace and edit shortcut: (keys, command). The Keyboard Shortcuts sheet lists them
/// all (checked by a test).
pub const BINDINGS: &[(&str, &str)] = &[
    ("cmd-z", "undo"),
    ("cmd-shift-z", "redo"),
    ("cmd-x", "cut"),
    ("cmd-c", "copy"),
    ("cmd-v", "paste"),
    ("alt-cmd-shift-v", "paste-match"),
    ("cmd-a", "select-all"),
    ("cmd-t", "new-query"),
    ("cmd-shift-t", "new-table"),
    ("cmd-s", "commit"),
    ("cmd-shift-s", "save-file"),
    ("cmd-o", "open-file"),
    ("cmd-p", "quick-open"),
    ("cmd-f", "filters"),
    ("cmd-shift-backspace", "discard"),
    ("cmd-e", "open-sql-query"),
    ("cmd-w", "close-tab"),
    ("cmd-r", "refresh"),
    ("alt-cmd-r", "reload-tab"),
    ("ctrl-cmd-[", "show-data"),
    ("ctrl-cmd-]", "show-structure"),
    ("ctrl-cmd-left", "go-back"),
    ("ctrl-cmd-right", "go-forward"),
    ("cmd-shift-r", "reconnect"),
    ("cmd-.", "running-queries"),
    ("cmd-0", "toggle-sidebar"),
    ("cmd-shift-0", "toggle-panel"),
    ("cmd-shift-c", "toggle-console"),
    ("cmd-[", "prev-tab"),
    ("cmd-]", "next-tab"),
    ("cmd-shift-p", "preview"),
    ("cmd-k", "switch-db"),
    ("cmd-enter", "run-current"),
    ("cmd-shift-enter", "run-all"),
    ("cmd-1", "tab-1"),
    ("cmd-2", "tab-2"),
    ("cmd-3", "tab-3"),
    ("cmd-4", "tab-4"),
    ("cmd-5", "tab-5"),
    ("cmd-6", "tab-6"),
    ("cmd-7", "tab-7"),
    ("cmd-8", "tab-8"),
    ("cmd-9", "tab-9"),
    ("cmd-h", "hide"),
    ("alt-cmd-h", "hide-others"),
    ("cmd-m", "minimize"),
    ("ctrl-cmd-f", "fullscreen"),
];
pub fn install(cx: &mut App) {
    cx.bind_keys(
        BINDINGS
            .iter()
            .map(|(key, name)| KeyBinding::new(key, Command::new(name), None)),
    );
    // A focused grid keeps its ⇧⌘C and ⌘↩ (see `telecat_grid::install_keys`).
    telecat_grid::install_keys(cx);
    cx.on_action(|command: &Command, cx| {
        let command = command.clone();
        // Key bindings dispatch this while GPUI is updating the active window, which
        // can't be updated again until that dispatch returns.
        cx.defer(move |cx| {
            if let Some(handle) = cx.active_window() {
                let _ = handle.update(cx, |_, window, cx| {
                    match command.name.as_str() {
                        "about"=>{unsafe {let app:*mut objc2::runtime::AnyObject=objc2::msg_send![objc2::class!(NSApplication), sharedApplication];let _:()=objc2::msg_send![app,orderFrontStandardAboutPanel:std::ptr::null::<objc2::runtime::AnyObject>()];}return;},
                        "show-all"=>{cx.unhide_other_apps();return;},
                        "minimize"=>{window.minimize_window();return;},"zoom"=>{window.zoom_window();return;},"fullscreen"=>{window.toggle_fullscreen();return;},"hide"=>{cx.hide();return;},"hide-others"=>{cx.hide_other_apps();return;},"front"=>{cx.activate(true);return;},_=>{}
                    }
                    let view = host::active_workspace(window,cx);
                    if let Some(view) = view {
                        view.update(cx, |this, cx| this.command(&command.name, window, cx));
                    } else if EDIT.contains(&command.name.as_str()) {
                        components::native_edit(&command.name,window,cx);
                    } else if command.name == "close-tab" {
                        window.remove_window();
                    } else if command.name == "shortcuts" {
                        crate::shortcuts::show_window(false, cx);
                    } else {
                        forward(&command.name, window, cx);
                    }
                });
            }
        });
    });
}
const EDIT: [&str; 8] = [
    "undo",
    "redo",
    "cut",
    "copy",
    "paste",
    "paste-match",
    "select-all",
    "delete",
];
/// A connection-window command chosen while the connections or form window is in front: run it
/// in the frontmost connection window, or say that it needs one.
fn forward(name: &str, window: &mut Window, cx: &mut App) {
    let target = cx
        .window_stack()
        .unwrap_or_else(|| cx.windows())
        .into_iter()
        .find_map(|handle| {
            handle
                .update(cx, |_, window, cx| host::active_workspace(window, cx))
                .ok()
                .flatten()
                .map(|view| (handle, view))
        });
    if let Some((handle, view)) = target {
        let name = name.to_owned();
        let _ = handle.update(cx, |_, window, cx| {
            window.activate_window();
            view.update(cx, |this, cx| this.command(&name, window, cx));
        });
        return;
    }
    let answer = window.prompt(
        PromptLevel::Info,
        &format!("{} needs a connection window", menu_label(name)),
        Some("Connect to a database first: double-click a connection in the list, or create one with ⌘N."),
        &["OK"],
        cx,
    );
    cx.spawn(async move |_| answer.await.ok()).detach();
}
/// The menu wording of a command, for messages.
fn menu_label(name: &str) -> String {
    match name {
        "new-query" => "New Tab",
        "new-table" => "New Table",
        "commit" => "Commit Changes",
        "preview" => "Preview Changes",
        "discard" => "Discard Changes",
        "save-file" => "Save SQL As",
        "open-file" => "Open SQL File",
        "import-dump" => "Import SQL Dump",
        "quick-open" => "Open Anything",
        "open-sql-query" => "Open SQL Query",
        "switch-db" => "Switch Database",
        "running-queries" => "Running Queries",
        "refresh" => "Reload Workspace",
        "reload-tab" => "Reload Current Tab",
        "show-data" => "Show Data",
        "show-structure" => "Show Structure",
        "go-back" => "Go Back",
        "go-forward" => "Go Forward",
        "reconnect" => "Reconnect",
        "toggle-sidebar" => "Toggle Sidebar",
        "toggle-panel" => "Toggle Right Panel",
        "toggle-console" => "Toggle Console Log",
        "prev-tab" => "Previous Tab",
        "next-tab" => "Next Tab",
        "filters" => "Filters",
        "run-current" => "Run Current",
        "run-all" => "Run All",
        s if s.starts_with("tab-") => return format!("Tab {}", &s[4..]),
        _ => "This command",
    }
    .to_owned()
}
/// The system alert sound, for a command that can't run right now.
fn beep() {
    unsafe extern "C" {
        fn NSBeep();
    }
    // SAFETY: AppKit's NSBeep takes no arguments and only plays the alert sound.
    unsafe { NSBeep() }
}
impl Workspace {
    fn command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet.is_some() && EDIT.contains(&name) {
            components::native_edit(name, window, cx);
            return;
        }
        if name == "shortcuts" {
            // Over another sheet (a pending confirmation must not be dropped) it opens in its
            // own window.
            match &self.sheet {
                Some(Sheet::Shortcuts) => {}
                Some(_) => crate::shortcuts::show_window(self.services.scenario, cx),
                None => {
                    self.sheet = Some(Sheet::Shortcuts);
                    cx.notify();
                }
            }
            return;
        }
        // Reconnecting must work whatever is open or hung, a save that never returns included.
        if name == "reconnect" {
            if !matches!(self.sheet, Some(Sheet::ReconnectPassword)) {
                self.reconnect(window, cx);
            }
            return;
        }
        if self.sheet.is_some() || (self.saving && !name.starts_with("toggle-")) {
            // Busy with a dialog or a save: say so instead of doing nothing.
            if !self.services.scenario {
                beep();
            }
            return;
        }
        match name {
            "undo" | "redo" | "cut" | "copy" | "paste" | "paste-match" | "select-all"
            | "delete" => self.edit_menu(name, window, cx),
            s if s.starts_with("tab-") => {
                if let Some(t) = s[4..]
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| self.state.tabs.get(n.saturating_sub(1)))
                {
                    self.state.activate(t.id);
                }
            }
            "commit" => self.save_command(window, cx),
            "save-file" => self.save_sql_file(cx),
            "open-file" => self.open_sql_file(cx),
            "import-dump" => self.import_dump_file(cx),
            "quick-open" => self.open_anything(window, cx),
            "discard" => self.discard_all(window, cx),
            "new-table" => self.new_table(cx),
            "filters" => self.show_filters(window, cx),
            "new-query" => self.new_query("", cx),
            "open-sql-query" => {
                self.reset_component_focus();
                if let Some(id) = self
                    .state
                    .tabs
                    .iter()
                    .find(|t| t.kind == TabKind::Query)
                    .map(|t| t.id)
                {
                    self.state.activate(id);
                } else {
                    self.new_query("", cx);
                }
            }
            "close-tab" => self.request_close_tab(self.state.active, window, cx),
            "refresh" => self.reload_workspace(window, cx),
            "reload-tab" => self.reload_current_tab(window, cx),
            "show-data" => self.show_part(telecat_app::table_model::Part::Data, cx),
            "show-structure" => self.show_part(telecat_app::table_model::Part::Structure, cx),
            "go-back" => self.go(false, window, cx),
            "go-forward" => self.go(true, window, cx),
            "running-queries" => {
                self.state.open_running();
                self.refresh_tab(cx);
            }
            "toggle-sidebar" => self.sidebar = !self.sidebar,
            "toggle-panel" => self.panel = !self.panel,
            "toggle-console" => self.console = !self.console,
            "prev-tab" => self.state.step(-1),
            "next-tab" => self.state.step(1),
            "preview" => {
                self.flush_grid_edits(window, cx);
                self.sheet = Some(Sheet::Preview);
            }
            "switch-db" => self.open_database_picker(window, cx),
            "run-current" => self.run(false, window, cx),
            "run-all" => self.run(true, window, cx),
            _ => {}
        }
        cx.notify();
    }
}
