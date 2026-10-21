use gpui_kit::{prelude::*, *};
use std::sync::Arc;
pub use telecat_theme::ui as ui_font;
pub fn brand() -> impl IntoElement {
    canvas(
        |_, window, _| {
            let mut font = ui_font();
            font.weight = FontWeight::BOLD;
            let mut line = window.text_system().shape_line(
                "Telecat".into(),
                px(34.),
                &[TextRun {
                    len: 7,
                    font,
                    color: rgb(telecat_theme::TEXT).into(),
                    ..Default::default()
                }],
                None,
            );
            // CSS letter-spacing: -0.5px. Preserve CoreText kerning and move the shaped glyphs.
            let layout: &mut Arc<LineLayout> = &mut line;
            let mut runs = layout.runs.clone();
            for run in &mut runs {
                for glyph in &mut run.glyphs {
                    glyph.position.x -= px(glyph.index as f32 * 0.5);
                }
            }
            *layout = Arc::new(LineLayout {
                font_size: layout.font_size,
                width: layout.width - px(3.5),
                ascent: layout.ascent,
                descent: layout.descent,
                runs,
                len: layout.len,
            });
            line
        },
        |bounds, line, window, cx| {
            let origin = point(
                bounds.origin.x + (bounds.size.width - line.width()) / 2.,
                bounds.origin.y,
            );
            if let Err(e) = line.paint(origin, px(40.), TextAlign::Left, None, window, cx) {
                eprintln!("brand paint: {e}");
            }
        },
    )
    .w_full()
    .h(px(40.))
}
pub fn pill(label: &'static str, icon: &'static str, disabled: bool) -> Div {
    div()
        .w_full()
        .h(px(40.))
        .flex_shrink_0()
        .rounded(px(20.))
        .border_1()
        .border_color(rgba(0xffffff1a))
        .bg(rgba(0xffffff10))
        .px(px(16.))
        .flex()
        .flex_row()
        .justify_start()
        .text_align(TextAlign::Left)
        .items_center()
        .gap(px(12.))
        .text_size(px(15.))
        .line_height(px(18.))
        .text_color(rgb(0xdddddd))
        .opacity(if disabled { 0.5 } else { 1.0 })
        .child(
            svg()
                .path(icon)
                .size(px(20.))
                .text_color(rgb(0xdddddd))
                .flex_shrink_0(),
        )
        .child(label)
}

pub use gpui_kit::base::input::{Input, InputEditorStyle, InputEvent, InputState};
pub fn icon(path: &'static str, n: f32, color: u32) -> Svg {
    svg()
        .path(path)
        .size(px(n))
        .text_color(rgb(color))
        .flex_shrink_0()
}
pub fn color(hex: &str) -> Rgba {
    rgb(u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x6e6e73))
}
pub fn text_input(
    value: impl Into<SharedString>,
    placeholder: impl Into<SharedString>,
    masked: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let value = value.into();
    let placeholder = placeholder.into();
    cx.new(|cx| {
        let mut s = InputState::new(window, cx)
            .default_value(value)
            .placeholder(placeholder)
            .masked(masked);
        s.set_editor_style(InputEditorStyle {
            foreground: rgb(telecat_theme::TEXT).into(),
            muted_foreground: rgb(0x6c6c6c).into(),
            background: rgb(telecat_theme::INPUT).into(),
            border: rgb(telecat_theme::BORDER).into(),
            selection: rgba(0x2f6fed66).into(),
            caret: rgb(telecat_theme::TEXT).into(),
            ..Default::default()
        });
        s
    })
}
pub fn input_box(state: &Entity<InputState>, disabled: bool, focus: bool) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h(px(30.))
        .flex()
        .items_center()
        .px(px(10.))
        .rounded(px(7.))
        .border_1()
        .border_color(rgb(if focus { 0x2f6fed } else { 0x383838 }))
        .bg(rgb(telecat_theme::INPUT))
        .text_size(px(14.))
        .line_height(px(17.))
        .opacity(if disabled { 0.45 } else { 1. })
        .when(focus, |d| {
            d.shadow(vec![BoxShadow {
                inset: false,
                color: rgba(0x2f6fed40).into(),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
            }])
        })
        .child(Input::new(state))
}
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
    disabled: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(30.))
        .px(px(14.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .border_1()
        .border_color(rgb(if primary { 0x2f6fed } else { 0x3c3c3c }))
        .bg(rgb(if primary { 0x2f6fed } else { 0x2f2f2f }))
        .text_size(px(14.))
        .line_height(px(17.))
        .text_color(rgb(if primary { 0xffffff } else { 0xe8e8e8 }))
        .opacity(if disabled { 0.4 } else { 1. })
        .when(!disabled, |d| {
            d.tab_index(0)
                .focus_visible(|s| s.border_color(rgb(telecat_theme::ACCENT)))
                .hover(move |s| s.bg(rgb(if primary { 0x3a7af9 } else { 0x383838 })))
        })
        .child(label.into())
}
pub fn badge(driver: telecat_core::types::Driver, hex: &str, n: f32) -> Div {
    div()
        .size(px(n))
        .flex_shrink_0()
        .rounded_full()
        .bg(color(hex))
        .text_color(rgb(0xffffff))
        .text_size(px(n * 0.42))
        .font_weight(FontWeight::MEDIUM)
        .flex()
        .items_center()
        .justify_center()
        .child(if driver == telecat_core::types::Driver::Mysql {
            "Ms"
        } else {
            "Pg"
        })
}
pub fn sheet(width: f32, title: impl Into<SharedString>) -> Div {
    div()
        .w(px(width))
        .bg(rgb(telecat_theme::CARD))
        .border_1()
        .border_color(rgb(0x484848))
        .rounded(px(12.))
        .pt(px(18.))
        .px(px(20.))
        .pb(px(16.))
        .shadow(vec![BoxShadow {
            inset: false,
            color: rgba(0x000000aa).into(),
            offset: point(px(0.), px(20.)),
            // CSS blur-radius is twice the Gaussian sigma used by GPUI.
            blur_radius: px(30.),
            spread_radius: px(0.),
        }])
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .mb(px(12.))
                .child(title.into()),
        )
}
pub fn backdrop() -> Div {
    div()
        .occlude()
        .absolute()
        .inset_0()
        .bg(rgba(0x00000070))
        .flex()
        .justify_center()
        .items_start()
        .pt(px(60.))
}
/// The full text of something shown truncated, as a hover tooltip.
pub struct TextTooltip(SharedString);
impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(520.))
            .px(px(8.))
            .py(px(5.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(0x3a3a3a))
            .bg(rgb(0x2b2b2b))
            .text_color(rgb(0xe8e8e8))
            .text_size(px(12.))
            .line_height(px(16.))
            .child(self.0.clone())
    }
}
pub fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| TextTooltip(text.clone())).into()
}
