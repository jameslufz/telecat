//! Function and procedure tabs: the definition in a SQL editor, applied with ⌘S.
use super::*;

impl Workspace {
    pub(super) fn routine_content(&self, cx: &mut Context<Self>) -> Div {
        let tab = self.state.current();
        let TabKind::Routine(object) = &tab.kind else {
            return div();
        };
        let loaded = tab.routine.as_ref().is_some_and(|r| r.original.is_some());
        let dirty = tab.routine_dirty();
        let mut content = div().flex_1().min_h_0().relative().flex().flex_col();
        if loaded {
            content = content.child(self.editor_view());
        } else {
            let placeholder = tab
                .load(LoadSlot::Results)
                .placeholder(false, std::time::Instant::now());
            content = content
                .child(div().flex_1())
                .children(super::panels::load_state_view(
                    &placeholder,
                    "routine-state",
                ))
                .when(matches!(placeholder, Placeholder::Failed(_)), |d| {
                    d.child(
                        div()
                            .absolute()
                            .bottom(px(40.))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                small_button("routine-retry", "Try Again")
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh_tab(cx))),
                            ),
                    )
                });
        }
        content = content.children(self.hint_view());
        let what = object.kind.label();
        let bottom =
            subbar()
                .child(icon("Function.svg", 15., 0x8a8a8a))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(match &object.signature {
                            Some(args) => format!("{what} {}({args})", object.table.name),
                            None => format!("{what} {}", object.table.name),
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_center()
                        .text_color(rgb(0xe0b84a))
                        .when(dirty, |d| {
                            d.child("unsaved changes — ⌘S to save, ⇧⌘P to preview")
                        }),
                )
                .when(dirty, |d| {
                    d.child(small_button("routine-revert", "Revert").on_click(
                        cx.listener(|this, _, window, cx| this.revert_routine(window, cx)),
                    ))
                })
                .child(
                    small_button("routine-reload", "")
                        .gap(px(6.))
                        .child(icon("Refresh.svg", 13., 0xe8e8e8))
                        .child("Reload")
                        .when(dirty, |d| {
                            d.opacity(0.4)
                                .tooltip(text_tooltip("Save or revert your changes first"))
                        })
                        .when(!dirty, |d| {
                            d.on_click(cx.listener(|this, _, _, cx| this.refresh_tab(cx)))
                        }),
                );
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(content)
            .child(bottom)
    }
    /// Puts the definition back the way it was loaded.
    fn revert_routine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(original) = self
            .state
            .current()
            .routine
            .as_ref()
            .and_then(|r| r.original.clone())
        else {
            return;
        };
        self.replace_editor_text(original, window, cx);
    }
}
