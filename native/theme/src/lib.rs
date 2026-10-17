//! Electron styles.css tokens and CSS family resolution, shared by every view.
use gpui_kit::{App, Font, FontFallbacks};
use std::sync::OnceLock;
pub const BG: u32 = 0x1e1e1e;
pub const BG_2: u32 = 0x252525;
pub const BG_3: u32 = 0x2c2c2c;
pub const PANEL: u32 = 0x232323;
pub const CARD: u32 = 0x2a2a2a;
pub const INPUT: u32 = 0x1a1a1a;
pub const INPUT_2: u32 = 0x2f2f2f;
pub const BORDER: u32 = 0x383838;
pub const BORDER_2: u32 = 0x444444;
pub const TEXT: u32 = 0xe8e8e8;
pub const TEXT_SECONDARY: u32 = 0xb8b8b8;
pub const MUTED: u32 = 0x8a8a8a;
pub const ACCENT: u32 = 0x2f6fed;
pub const ACCENT_2: u32 = 0x1d4e9b;
pub const DANGER: u32 = 0xf05a4f;
pub const OK: u32 = 0x3fb950;
pub const ODD: u32 = 0x232323;
pub const HEADER: u32 = 0x262626;
pub const NULL: u32 = 0x666666;
pub const SELECTED: u32 = 0x1d3f73;
pub const MOD: u32 = 0x6e5517;
pub const MOD_CELL: u32 = 0x7a5e14;
pub const DELETED: u32 = 0x5c1717;
pub const NEW: u32 = 0x2f5a2a;
pub const ROW_HEIGHT: f32 = 26.;
pub const HEADER_HEIGHT: f32 = 30.;
pub const SCROLLBAR: f32 = 10.;
pub const EDITOR_FONT_SIZE: f32 = 13.;
pub const EDITOR_LINE_HEIGHT: f32 = 20.8;
pub const EDITOR_FOREGROUND: u32 = 0xe6e6e6;
pub const EDITOR_PLACEHOLDER: &str = "-- Write your SQL here. ⌘↵ runs the current statement.";
pub const BLINK_MS: u64 = 600;
static MONO: OnceLock<&'static str> = OnceLock::new();
/// CoreText silently resolves unknown families to Helvetica. A glyph cascade
/// cannot repair that: resolve the CSS family stack before creating a Font.
/// Chromium's macOS ui-monospace maps to Menlo on the supported macOS version.
pub fn resolve_mono(available: &[String]) -> &'static str {
    ["SF Mono", "Menlo", "Monaco", "Thonburi"]
        .into_iter()
        .find(|name| available.iter().any(|f| f == name))
        .unwrap_or("Menlo")
}
pub fn init(cx: &App) {
    MONO.get_or_init(|| resolve_mono(&cx.text_system().all_font_names()));
}
pub fn mono() -> Font {
    Font {
        family: (*MONO.get().unwrap_or(&"Menlo")).into(),
        fallbacks: Some(FontFallbacks::from_fonts(vec![
            "Menlo".into(),
            "Monaco".into(),
            "Thonburi".into(),
            "Apple Color Emoji".into(),
        ])),
        ..Default::default()
    }
}
pub fn ui() -> Font {
    Font {
        family: ".AppleSystemUIFont".into(),
        fallbacks: Some(FontFallbacks::from_fonts(vec![
            "SF Pro Text".into(),
            "Helvetica Neue".into(),
            "Thonburi".into(),
            "Apple Color Emoji".into(),
        ])),
        ..Default::default()
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn css_family_resolution_skips_missing_sf_mono() {
        let names = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            super::resolve_mono(&names(&["Helvetica", "Menlo", "Thonburi"])),
            "Menlo"
        );
        assert_eq!(
            super::resolve_mono(&names(&["Menlo", "SF Mono"])),
            "SF Mono"
        );
        assert_eq!(super::resolve_mono(&names(&["Monaco"])), "Monaco");
    }
}
