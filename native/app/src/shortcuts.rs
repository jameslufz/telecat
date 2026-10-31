//! Help ▸ Keyboard Shortcuts: the list in `workspace/shortcuts.json`, shown as a sheet in a
//! connection window and in its own window everywhere else.
use crate::ui::*;
use gpui_kit::{prelude::*, *};

const SHORTCUTS: &str = include_str!("workspace/shortcuts.json");

/// (group title, [(keys, action)]) in display order.
pub fn groups() -> Vec<(String, Vec<(String, String)>)> {
    let groups: serde_json::Value = serde_json::from_str(SHORTCUTS).expect("checked-in shortcuts");
    groups
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            (
                g["title"].as_str().unwrap_or_default().to_owned(),
                g["keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|row| {
                        (
                            row[0].as_str().unwrap_or_default().to_owned(),
                            row[1].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}
/// The groups in two columns: the first two groups left, the rest right.
pub fn body() -> Div {
    let groups = groups();
    let split = 2.min(groups.len());
    let column = |groups: &[(String, Vec<(String, String)>)]| {
        div()
            .flex_1()
            .min_w_0()
            .children(groups.iter().map(|(title, keys)| {
                div()
                    .mb(px(14.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .mb(px(6.))
                            .child(title.clone()),
                    )
                    .children(keys.iter().map(|(keys, action)| {
                        div()
                            .flex()
                            .gap(px(10.))
                            .py(px(2.))
                            .text_size(px(11.5))
                            .line_height(px(13.5))
                            .child(div().w(px(118.)).flex_shrink_0().child(keys.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                    .child(action.clone()),
                            )
                    }))
            }))
    };
    div()
        .flex()
        .line_height(px(14.))
        .gap(px(28.))
        .child(column(&groups[..split]))
        .child(column(&groups[split..]))
}
struct ShortcutsWindow {
    scroll: ScrollHandle,
}
impl Render for ShortcutsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .font(ui_font())
            .text_size(px(12.))
            .text_color(rgb(telecat_theme::TEXT))
            .bg(rgb(telecat_theme::CARD))
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, window, _| {
                if e.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(
                div()
                    .pt(px(34.))
                    .px(px(20.))
                    .pb(px(10.))
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Keyboard Shortcuts"),
            )
            .child(
                div()
                    .id("shortcuts-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px(px(20.))
                    .child(body()),
            )
            .child(
                div().p(px(16.)).flex().justify_end().child(
                    button("shortcuts-done", "Done", true, false)
                        .on_click(|_, window, _| window.remove_window()),
                ),
            )
    }
}
/// Opens the Keyboard Shortcuts window, or brings it to the front when it is open.
pub fn show_window(scenario: bool, cx: &mut App) {
    for handle in cx.windows() {
        let found = handle
            .update(cx, |_, window, cx| {
                window
                    .root::<gpui_kit::base::Root>()
                    .flatten()?
                    .read(cx)
                    .view()
                    .clone()
                    .downcast::<ShortcutsWindow>()
                    .ok()?;
                window.activate_window();
                Some(())
            })
            .ok()
            .flatten();
        if found.is_some() {
            return;
        }
    }
    let result = gpui_kit::open_window(
        crate::options(cx, 760., 620., false),
        cx,
        move |window, cx| {
            // Unit tests have no native window to announce (a scenario run would exit on that).
            if !cfg!(test) {
                crate::print_window(window, "shortcuts", 760., 620., scenario, cx);
            }
            cx.new(|_| ShortcutsWindow {
                scroll: ScrollHandle::new(),
            })
        },
    );
    if let Err(e) = result {
        eprintln!("Open shortcuts: {e}");
    }
}
#[cfg(test)]
mod tests {
    /// gpui's `cmd-shift-t` as the sheet writes it: `⇧⌘T`.
    fn mac(keys: &str) -> String {
        let parts: Vec<&str> = keys.split('-').collect();
        let (mods, key) = parts.split_at(parts.len() - 1);
        let mut out = String::new();
        for (m, glyph) in [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("cmd", "⌘")] {
            if mods.contains(&m) {
                out += glyph;
            }
        }
        out += &match key[0] {
            "enter" => "↵".to_owned(),
            "left" => "←".to_owned(),
            "right" => "→".to_owned(),
            "up" => "↑".to_owned(),
            "down" => "↓".to_owned(),
            "backspace" => "⌫".to_owned(),
            "space" => "Space".to_owned(),
            k => k.to_uppercase(),
        };
        out
    }
    /// Every key binding the app installs is listed, so the sheet can't drift from the code.
    #[test]
    fn every_binding_is_listed() {
        let listed: std::collections::BTreeSet<String> = super::groups()
            .into_iter()
            .flat_map(|(_, keys)| keys.into_iter().map(|k| k.0))
            .flat_map(|cell| {
                cell.split("  /  ")
                    .flat_map(|k| k.split(" … "))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        // The global actions bound in `install_menus`.
        let app = ["cmd-q", "cmd-n", "cmd-shift-k", "cmd-shift-w"];
        let all = crate::workspace::commands::BINDINGS
            .iter()
            .map(|b| b.0)
            .chain(telecat_editor::SHORTCUTS.iter().copied())
            .chain(app);
        for keys in all {
            let mut shown = mac(keys);
            // ⌘1 … ⌘9 is one row.
            if let Some(d) = shown.strip_prefix('⌘').and_then(|k| k.parse::<u8>().ok())
                && (2..=8).contains(&d)
            {
                shown = "⌘1".into();
            }
            // The editor's ⌘[ / ⌘] and ⇧⌘K are the same commands as the workspace's.
            assert!(
                listed.contains(&shown),
                "{keys} ({shown}) is missing from shortcuts.json"
            );
        }
        assert_eq!(mac("alt-cmd-shift-v"), "⌥⇧⌘V");
    }
}
