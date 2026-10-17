//! Telecat's standalone SQL editor. Call `init` after `gpui_kit::init`, then
//! create an `Entity<SqlEditor>`. Every shell-facing offset is UTF-16.
pub mod completion;
pub mod format;
pub mod latency;
pub mod search;
pub mod syntax;
pub mod theme;
pub mod utf16;
mod view;
pub use completion::Schema;
use gpui_kit::*;
use std::rc::Rc;
pub use telecat_core::types::Driver;
pub use view::{EditorEvent, EditorOptions, Selection, SelectionInfo, SqlEditor};
#[derive(Default)]
struct Installed;
impl Global for Installed {}
pub fn init(cx: &mut App) {
    telecat_theme::init(cx);
    if cx.has_global::<Installed>() {
        return;
    }
    cx.set_global(Installed);
    telecat_editor_engine::init(cx);
    for key in [
        "cmd-enter",
        "shift-cmd-enter",
        "cmd-i",
        "shift-cmd-i",
        "cmd-[",
        "cmd-]",
        "shift-cmd-k",
        "cmd-/",
        "cmd-d",
        "ctrl-cmd-g",
        "cmd-g",
        "shift-cmd-g",
        "cmd-f",
        "alt-cmd-f",
        "ctrl-space",
    ] {
        cx.bind_keys([KeyBinding::new(
            key,
            Shortcut { key: key.into() },
            Some("TelecatInput"),
        )]);
    }
    telecat_editor_engine::input::set_language_provider(Rc::new(syntax::Languages), cx);
}

#[derive(Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace=telecat_editor, no_json)]
pub(crate) struct Shortcut {
    key: String,
}
