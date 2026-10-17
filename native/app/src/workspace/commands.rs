use super::*;
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
pub fn install(cx: &mut App) {
    cx.bind_keys(
        [
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
        ]
        .into_iter()
        .map(|(key, name)| KeyBinding::new(key, Command::new(name), None)),
    );
    cx.on_action(|command: &Command, cx| {
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
                } else if ["undo","redo","cut","copy","paste","paste-match","select-all","delete"].contains(&command.name.as_str()) {
                    components::native_edit(&command.name,window,cx);
                } else if command.name == "close-tab" {
                    window.remove_window();
                }
            });
        }
    });
}
impl Workspace {
    fn command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet.is_some()
            && [
                "undo",
                "redo",
                "cut",
                "copy",
                "paste",
                "paste-match",
                "select-all",
                "delete",
            ]
            .contains(&name)
        {
            components::native_edit(name, window, cx);
            return;
        }
        if self.sheet.is_some() || (self.saving && !name.starts_with("toggle-")) {
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
            "close-window" => self.request_close_window(window, cx),
            "refresh" => self.reload_workspace(window, cx),
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
            "shortcuts" => self.sheet = Some(Sheet::Shortcuts),
            "run-current" => self.run(false, cx),
            "run-all" => self.run(true, cx),
            _ => {}
        }
        cx.notify();
    }
}
