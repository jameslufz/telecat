use super::*;
impl Workspace {
    pub(super) fn sheet_view(&self, s: &Sheet, cx: &mut Context<Self>) -> Div {
        let close = || {
            small_button("sheet-close", "Close").on_click(cx.listener(|this, _, _, cx| {
                this.cancel_sheet(cx);
                cx.notify();
            }))
        };
        let content = match s {
            Sheet::QuickLook { title, value } => {
                let pretty = if value.trim_start().starts_with(['[', '{']) {
                    serde_json::from_str::<serde_json::Value>(value)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                        .unwrap_or_else(|| value.clone())
                } else {
                    value.clone()
                };
                let copy = value.clone();
                sheet(640., title.clone())
                    .child(
                        div()
                            .relative()
                            .child(
                                div()
                                    .id("quick-look")
                                    .max_h(px(self.viewport_height * 0.6))
                                    .overflow_y_scroll()
                                    .track_scroll(&self.detail_scroll)
                                    .font(mono())
                                    .text_size(px(12.5))
                                    .line_height(px(15.))
                                    .p(px(10.))
                                    .border_1()
                                    .border_color(rgb(telecat_theme::BORDER))
                                    .rounded(px(8.))
                                    .bg(rgb(telecat_theme::INPUT))
                                    .child(pretty),
                            )
                            .child(super::layout::scrollbar(&self.detail_scroll, cx)),
                    )
                    .child(
                        div()
                            .mt(px(16.))
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(small_button("copy-quicklook", "Copy").on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                },
                            )))
                            .child(
                                button("close-quicklook", "Close", true, false)
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_sheet(cx))),
                            ),
                    )
            }
            Sheet::Shortcuts => {
                let groups: serde_json::Value =
                    serde_json::from_str(include_str!("shortcuts.json")).unwrap();
                let groups = groups.as_array().unwrap();
                let columns = [&groups[..2], &groups[2..]].into_iter().map(|groups| {
                    let blocks = groups.iter().map(|g| {
                        let rows = g["keys"].as_array().unwrap().iter().map(|row| {
                            div()
                                .flex()
                                .gap(px(10.))
                                .py(px(2.))
                                .text_size(px(12.5))
                                // Chromium's normal 12.5px system-font line box is 15px.
                                .line_height(px(15.))
                                .child(
                                    div()
                                        .w(px(118.))
                                        .flex_shrink_0()
                                        .child(row[0].as_str().unwrap().to_owned()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                        .child(row[1].as_str().unwrap().to_owned()),
                                )
                        });
                        div()
                            .mb(px(14.))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .mb(px(6.))
                                    .child(g["title"].as_str().unwrap().to_owned()),
                            )
                            .children(rows)
                    });
                    div().flex_1().min_w_0().children(blocks)
                });
                sheet(720., "Keyboard Shortcuts")
                    .line_height(px(17.))
                    .child(
                        div()
                            .flex()
                            .line_height(px(15.5))
                            .gap(px(28.))
                            .children(columns),
                    )
                    .child(div().mt(px(16.)).flex().justify_end().child(
                        button("done", "Done", true, false).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.sheet = None;
                                cx.notify();
                            },
                        )),
                    ))
            }
            Sheet::Preview => {
                let preview = self.preview_text().unwrap_or_else(|e| e.to_string());
                let has = !preview.is_empty();
                sheet(680., "Preview Changes")
                    .child(if has {
                        div()
                            .p(px(10.))
                            .border_1()
                            .border_color(rgb(telecat_theme::BORDER))
                            .rounded(px(8.))
                            .bg(rgb(telecat_theme::INPUT))
                            .font(mono())
                            .text_size(px(12.))
                            .line_height(px(14.4))
                            .child(preview)
                    } else {
                        div()
                            .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                            .child("There are no staged changes.")
                    })
                    .child(
                        div()
                            .mt(px(16.))
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(close())
                            .when(has, |d| {
                                d.child(button("discard", "Discard…", false, false).on_click(
                                    cx.listener(|this, _, window, cx| this.discard_all(window, cx)),
                                ))
                                .child(
                                    button("commit", "Commit ⌘S", true, false).on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.request_commit(window, cx)
                                        }),
                                    ),
                                )
                            }),
                    )
            }
            Sheet::Gate {
                sql,
                password,
                danger,
                attempts,
            } => {
                let unlock = matches!(self.pending, Some(PendingAction::Unlock(_)));
                let title = if danger.is_some() {
                    "This query can change a whole table"
                } else if unlock {
                    "Enter the database password to change Safe Mode"
                } else if *password {
                    "Enter the database password to execute"
                } else {
                    "Do you want to execute this query?"
                };
                let safe = match self.state.connection.safe_mode {
                    SafeMode::Safe1 => "Safe Mode 1",
                    SafeMode::Safe2 => "Safe Mode 2",
                    SafeMode::Alert1 => "Alert Mode 1",
                    SafeMode::Alert2 => "Alert Mode 2",
                    SafeMode::Silent => "Silent Mode",
                };
                let detail = format!(
                    "{} is in {safe}.{}",
                    self.state.connection.name,
                    if *attempts > 0 {
                        "\nWrong password, try again."
                    } else {
                        ""
                    }
                );
                let mut body = sheet(
                    if *password {
                        360.
                    } else if danger.is_some() {
                        600.
                    } else {
                        520.
                    },
                    title,
                );
                if danger.is_some() {
                    let items = if let Some(PendingAction::Query { plan, .. }) = &self.pending {
                        plan.dangers.clone()
                    } else {
                        vec![(sql.clone(), danger.clone().unwrap())]
                    };
                    for (sql, reason) in items {
                        body = body.child(
                            div()
                                .mb(px(12.))
                                .child(
                                    div()
                                        .text_color(rgb(0xff8a80))
                                        .mb(px(6.))
                                        .child(format!("⚠ {reason}")),
                                )
                                .child(
                                    div()
                                        .p(px(10.))
                                        .rounded(px(8.))
                                        .border_1()
                                        .border_color(rgb(telecat_theme::BORDER))
                                        .bg(rgb(telecat_theme::INPUT))
                                        .font(mono())
                                        .text_size(px(12.))
                                        .line_height(px(15.))
                                        .child(sql),
                                ),
                        );
                    }
                    body=body.child(div().text_color(rgb(telecat_theme::TEXT_SECONDARY)).child("Telecat always asks before running these, whatever the Safe Mode. Check the statement before you continue."));
                    return backdrop().child(
                        body.child(
                            div()
                                .mt(px(16.))
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    button("gate-run-anyway", "Run Anyway", false, false)
                                        .text_color(rgb(0xff8a80))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.approve_gate(window, cx)
                                        })),
                                )
                                .child(
                                    button("gate-cancel", "Cancel", true, false).on_click(
                                        cx.listener(|this, _, _, cx| this.cancel_sheet(cx)),
                                    ),
                                ),
                        ),
                    );
                }
                if *password {
                    body = body
                        .child(
                            div()
                                .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                .mb(px(10.))
                                .child(format!(
                                    "{detail}\n\n{}",
                                    sql.chars().take(600).collect::<String>()
                                )),
                        )
                        .child(input_box(&self.gate_password, false, true));
                } else {
                    body = body
                        .child(
                            div()
                                .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                .mb(px(10.))
                                .child(danger.as_ref().map(|d| format!("⚠ {d}")).unwrap_or(detail)),
                        )
                        .child(
                            div()
                                .p(px(10.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(telecat_theme::BORDER))
                                .bg(rgb(telecat_theme::INPUT))
                                .font(mono())
                                .text_size(px(12.))
                                .line_height(px(15.))
                                .child(sql.clone()),
                        );
                }
                body.child(
                    div()
                        .mt(px(16.))
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            button("gate-cancel", "Cancel", false, false)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_sheet(cx))),
                        )
                        .child(
                            button(
                                "gate-execute",
                                if *password { "OK" } else { "Execute" },
                                true,
                                false,
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.approve_gate(window, cx)),
                            ),
                        ),
                )
            }
            Sheet::SwitchDatabase => self.picker_view(true, cx),
            Sheet::QuickOpen => self.picker_view(false, cx),
            Sheet::SaveQuery { .. } => sheet(360., "Save Query")
                .child(input_box(&self.sheet_input, false, true))
                .child(
                    div()
                        .mt(px(16.))
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            button("cancel-save-query", "Cancel", false, false).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.sheet = None;
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            button("save-named-query", "Save", true, false)
                                .on_click(cx.listener(|this, _, _, cx| this.save_named_query(cx))),
                        ),
                ),
            Sheet::Import(stage) => self.import_sheet(stage, cx),
            Sheet::ReconnectPassword => self.reconnect_password_sheet(cx),
            Sheet::CommitUnknown => self.commit_unknown_sheet(cx),
            Sheet::Error(message) => sheet(560., "Telecat")
                .child(div().text_color(rgb(0xff8a80)).child(message.clone()))
                .child(div().mt(px(16.)).flex().justify_end().child(close())),
        };
        backdrop().child(content)
    }
}
