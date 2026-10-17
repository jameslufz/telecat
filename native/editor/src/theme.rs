//! Editor-specific adapter over the shared Electron theme.
use gpui_kit::*;
use telecat_editor_engine::input::InputEditorStyle;
pub use telecat_theme::{
    ACCENT_2 as SELECTION, BG as BACKGROUND, BLINK_MS, EDITOR_FONT_SIZE as FONT_SIZE,
    EDITOR_FOREGROUND as FOREGROUND, EDITOR_LINE_HEIGHT as LINE_HEIGHT,
    EDITOR_PLACEHOLDER as PLACEHOLDER, mono, ui,
};
pub fn style() -> InputEditorStyle {
    InputEditorStyle {
        foreground: rgb(FOREGROUND).into(),
        muted_foreground: rgb(0x6b6b6b).into(),
        background: rgb(BACKGROUND).into(),
        border: rgb(0x444444).into(),
        selection: rgb(SELECTION).into(),
        caret: rgb(0xffffff).into(),
        editor_active_line: Some(rgba(0xffffff08).into()),
        editor_gutter_background: Some(rgb(BACKGROUND).into()),
        ..Default::default()
    }
}
