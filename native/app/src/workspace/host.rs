//! One native window with independent, mounted connection workspaces.
use super::*;
pub struct WorkspaceHost {
    services: Services,
    panes: Vec<Entity<Workspace>>,
    active: usize,
    _subscriptions: Vec<Subscription>,
}
impl WorkspaceHost {
    fn new(
        services: Services,
        session: Arc<telecat_db::Session>,
        connection: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut host = Self {
            services,
            panes: vec![],
            active: 0,
            _subscriptions: vec![],
        };
        host.add(session, connection, window, cx);
        host
    }
    fn add(
        &mut self,
        session: Arc<telecat_db::Session>,
        connection: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.entity().downgrade();
        let services = self.services.clone();
        let view = cx.new(|cx| {
            let mut w = Workspace::new(
                services,
                Some(session),
                connection,
                String::new(),
                window,
                cx,
            );
            w.host = Some(parent);
            w
        });
        self._subscriptions
            .push(cx.observe(&view, |_, _, cx| cx.notify()));
        self.panes.push(view);
        self.activate(self.panes.len() - 1, window, cx);
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| {
                this.flush(window, cx);
                let (busy, dirty) = this.close_state(cx);
                if busy {
                    false
                } else if dirty {
                    this.close_window(window, cx);
                    false
                } else {
                    true
                }
            })
            .unwrap_or(true)
        });
    }
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.panes.len() {
            return;
        }
        self.active = index;
        let rail = self.panes.len() > 1;
        for (i, pane) in self.panes.iter().enumerate() {
            pane.update(cx, |p, cx| {
                p.rail_visible = rail;
                p.host_active = i == index;
                p.prepare_running_timer(cx);
                if i == index {
                    window.focus(&p.focus, cx);
                    p.reset_component_focus();
                }
                cx.notify();
            });
        }
        cx.notify();
    }
    pub fn active(&self) -> Option<Entity<Workspace>> {
        self.panes.get(self.active).cloned()
    }
    pub fn close_state(&self, cx: &App) -> (bool, bool) {
        self.panes.iter().fold((false, false), |(busy, dirty), p| {
            let (b, d) = p.read(cx).close_state();
            (busy || b, dirty || d)
        })
    }
    pub fn flush(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for p in &self.panes {
            p.update(cx, |p, cx| p.flush_grid_edits(window, cx));
        }
    }
    pub fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush(window, cx);
        let (busy, dirty) = self.close_state(cx);
        if busy {
            return;
        }
        if !dirty {
            window.remove_window();
            return;
        }
        let prompt=window.prompt(PromptLevel::Warning,"Discard unsaved changes?",Some("Closing the window discards staged changes in every connection. SQL drafts are kept."),&["Discard","Cancel"],cx);
        // Lock all panes while the decision is pending.
        for p in &self.panes {
            p.update(cx, |p, cx| {
                p.saving = true;
                cx.notify();
            });
        }
        cx.spawn_in(window, async move |this, cx| {
            let yes = prompt.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                for p in &this.panes {
                    p.update(cx, |p, cx| {
                        p.saving = false;
                        cx.notify();
                    });
                }
            });
            if yes {
                let _ = cx.update(|window, _| window.remove_window());
            }
        })
        .detach();
    }
    pub fn close_connection(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.panes.iter().position(|p| p.entity_id() == id) else {
            return;
        };
        self.panes[index].update(cx, |p, cx| p.flush_grid_edits(window, cx));
        let (busy, dirty) = self.panes[index].read(cx).close_state();
        if busy {
            return;
        }
        if !dirty {
            self.remove(id, window, cx);
            return;
        }
        let name = self.panes[index].read(cx).state.connection.name.clone();
        let prompt = window.prompt(
            PromptLevel::Warning,
            &format!("Close {name}?"),
            Some("It has unsaved changes that will be lost."),
            &["Close", "Cancel"],
            cx,
        );
        self.panes[index].update(cx, |p, cx| {
            p.saving = true;
            cx.notify();
        });
        cx.spawn_in(window, async move |this, cx| {
            let yes = prompt.await == Ok(0);
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(p) = this.panes.iter().find(|p| p.entity_id() == id) {
                    p.update(cx, |p, cx| {
                        p.saving = false;
                        cx.notify();
                    });
                }
                if yes {
                    this.remove(id, window, cx);
                }
            });
        })
        .detach();
    }
    fn remove(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active().map(|p| p.entity_id());
        self.panes.retain(|p| p.entity_id() != id);
        if self.panes.is_empty() {
            window.remove_window();
            return;
        }
        let index = active
            .and_then(|id| self.panes.iter().position(|p| p.entity_id() == id))
            .unwrap_or(self.active.min(self.panes.len() - 1));
        self.activate(index, window, cx);
    }
    fn menu(
        &mut self,
        id: EntityId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        native_menu::choose(
            json!([{"id":"close","label":"Close Connection"},{"id":"others","label":"Close Other Connections","enabled":self.panes.len()>1},{"type":"separator"},{"id":"new","label":"Open Another Connection…"}]),
            position,
            window,
            cx,
            move |this, action, window, cx| match action.as_str() {
                "close" => this.close_connection(id, window, cx),
                "new" => crate::show_welcome(this.services.clone(), false, cx),
                "others" => {
                    this.flush(window, cx);
                    // One concrete confirmation covers exactly the connections being closed.
                    let other: Vec<_> = this
                        .panes
                        .iter()
                        .filter(|p| p.entity_id() != id)
                        .map(|p| p.entity_id())
                        .collect();
                    let busy = this
                        .panes
                        .iter()
                        .filter(|p| other.contains(&p.entity_id()))
                        .any(|p| p.read(cx).saving);
                    if busy {
                        return;
                    }
                    let dirty = this
                        .panes
                        .iter()
                        .filter(|p| other.contains(&p.entity_id()))
                        .any(|p| p.read(cx).state.has_changes());
                    if !dirty {
                        this.panes.retain(|p| p.entity_id() == id);
                        this.activate(0, window, cx);
                        return;
                    }
                    let prompt = window.prompt(
                        PromptLevel::Warning,
                        "Close other connections?",
                        Some("Their unsaved changes will be lost."),
                        &["Close", "Cancel"],
                        cx,
                    );
                    for p in this.panes.iter().filter(|p| other.contains(&p.entity_id())) {
                        p.update(cx, |p, cx| {
                            p.saving = true;
                            cx.notify();
                        });
                    }
                    cx.spawn_in(window, async move |this, cx| {
                        let yes = prompt.await == Ok(0);
                        let _ = this.update_in(cx, |this, window, cx| {
                            for p in this.panes.iter().filter(|p| other.contains(&p.entity_id())) {
                                p.update(cx, |p, cx| {
                                    p.saving = false;
                                    cx.notify();
                                });
                            }
                            if yes {
                                this.panes.retain(|p| !other.contains(&p.entity_id()));
                            }
                            if let Some(i) = this.panes.iter().position(|p| p.entity_id() == id) {
                                this.activate(i, window, cx);
                            }
                        });
                    })
                    .detach();
                }
                _ => {}
            },
        );
    }
}
impl Render for WorkspaceHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .id("workspace-host")
            .size_full()
            .flex()
            .font(ui_font())
            .text_color(rgb(telecat_theme::TEXT))
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.modifiers.control
                    && !e.keystroke.modifiers.platform
                    && !e.keystroke.modifiers.alt
                {
                    if let Ok(n) = e.keystroke.key.parse::<usize>() {
                        if (1..=9).contains(&n) {
                            this.activate(n - 1, window, cx);
                            cx.stop_propagation();
                        }
                    }
                }
            }));
        if self.panes.len() > 1 {
            let mut rail = div()
                .w(px(84.))
                .h_full()
                .flex_shrink_0()
                .bg(rgb(telecat_theme::INPUT))
                .border_r_1()
                .border_color(rgb(0x111111))
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(54.))
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(rgb(telecat_theme::CARD)),
                );
            for (i, pane) in self.panes.iter().enumerate() {
                let p = pane.read(cx);
                let label = if p.state.database.is_empty() {
                    p.state.connection.name.clone()
                } else {
                    p.state.database.clone()
                };
                let on = i == self.active;
                let id = pane.entity_id();
                let color =
                    u32::from_str_radix(p.state.connection.color.trim_start_matches('#'), 16)
                        .unwrap_or(0xaaaaaa);
                rail = rail.child(
                    div()
                        .id(("connection", i))
                        .h(px(84.))
                        .flex_shrink_0()
                        .relative()
                        .border_b_1()
                        .border_color(rgb(telecat_theme::CARD))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .when(on, |d| d.bg(rgb(0x262626)))
                        .child(icon(
                            "Database.svg",
                            26.,
                            if on { color } else { telecat_theme::MUTED },
                        ))
                        .child(div().text_size(px(12.)).child(label))
                        .child(
                            div()
                                .id(("close-connection", i))
                                .absolute()
                                .top(px(4.))
                                .right(px(4.))
                                .child(icon("X.svg", 10., telecat_theme::MUTED))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_connection(id, window, cx);
                                })),
                        )
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.activate(i, window, cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                this.menu(id, e.position, window, cx)
                            }),
                        ),
                );
            }
            rail = rail.child(
                div()
                    .id("rail-new")
                    .mx(px(26.))
                    .mt(px(10.))
                    .size(px(30.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgba(0xffffff1c))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("Plus.svg", 16., telecat_theme::TEXT_SECONDARY))
                    .on_click(cx.listener(|this, _, _, cx| {
                        crate::show_welcome(this.services.clone(), false, cx)
                    })),
            );
            root = root.child(rail);
        }
        root.child(div().flex_1().min_w_0().h_full().children(self.active()))
    }
}
pub fn open(
    services: Services,
    session: Arc<telecat_db::Session>,
    connection: ConnectionConfig,
    cx: &mut App,
) {
    for handle in cx.windows() {
        let added = handle
            .update(cx, |_, window, cx| {
                let Some(host) = root_host(window, cx) else {
                    return false;
                };
                host.update(cx, |host, cx| {
                    host.add(session.clone(), connection.clone(), window, cx)
                });
                window.activate_window();
                true
            })
            .unwrap_or(false);
        if added {
            return;
        }
    }
    let mut options = crate::options(cx, 1440., 816., false);
    options.is_resizable = true;
    options.is_minimizable = true;
    if let Err(e) = gpui_kit::open_window(options, cx, move |window, cx| {
        crate::print_window(window, "workspace", 1440., 816., services.scenario, cx);
        cx.new(|cx| WorkspaceHost::new(services, session, connection, window, cx))
    }) {
        eprintln!("Open workspace: {e}");
    }
}
pub fn root_host(window: &Window, cx: &App) -> Option<Entity<WorkspaceHost>> {
    window
        .root::<gpui_kit::base::Root>()
        .flatten()
        .and_then(|root| {
            root.read(cx)
                .view()
                .clone()
                .downcast::<WorkspaceHost>()
                .ok()
        })
}
pub fn active_workspace(window: &Window, cx: &App) -> Option<Entity<Workspace>> {
    root_host(window, cx)
        .and_then(|h| h.read(cx).active())
        .or_else(|| {
            window
                .root::<gpui_kit::base::Root>()
                .flatten()
                .and_then(|root| root.read(cx).view().clone().downcast::<Workspace>().ok())
        })
}
pub fn close_state(window: &Window, cx: &App) -> (bool, bool) {
    if let Some(h) = root_host(window, cx) {
        h.read(cx).close_state(cx)
    } else {
        active_workspace(window, cx)
            .map(|w| w.read(cx).close_state())
            .unwrap_or((false, false))
    }
}
#[cfg(test)]
mod tests {
    use super::WorkspaceHost;
    use crate::workspace::{Workspace, components, fixtures};
    use gpui::{AppContext, Entity, TestAppContext, WindowOptions};
    #[gpui::test]
    fn rail_keeps_independent_edits_and_confirms_close(cx: &mut TestAppContext) {
        let (old, source) = components::tests::setup(cx, "07-table-data");
        let services = source.read_with(cx, |s, _| s.services.clone());
        cx.update_window(old.into(), |_, window, _| window.remove_window())
            .unwrap();
        let (window, host) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    let parent = cx.entity().downgrade();
                    let panes: Vec<Entity<Workspace>> = ["07-table-data", "22-pg-table"]
                        .into_iter()
                        .map(|scenario| {
                            cx.new(|cx| {
                                let mut p = Workspace::new(
                                    services.clone(),
                                    None,
                                    fixtures::state(scenario).connection,
                                    scenario.into(),
                                    window,
                                    cx,
                                );
                                p.host = Some(parent.clone());
                                p
                            })
                        })
                        .collect();
                    let mut host = WorkspaceHost {
                        services,
                        panes,
                        active: 0,
                        _subscriptions: vec![],
                    };
                    host.activate(0, window, cx);
                    host
                })
            })
            .unwrap()
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
        let pane = host.read_with(cx, |h, _| h.active().unwrap());
        let id = pane.entity_id();
        pane.update(cx, |p, _| {
            p.state.current_mut().table.as_mut().unwrap().data.set_cell(
                0,
                1,
                telecat_core::changes::Cell::Text("staged".into()),
            )
        });
        cx.update_window(window.into(), |_, window, cx| {
            host.update(cx, |h, cx| {
                h.activate(1, window, cx);
                assert!(!h.active().unwrap().read(cx).state.has_changes());
                h.activate(0, window, cx);
                assert!(h.active().unwrap().read(cx).state.has_changes());
                h.close_connection(id, window, cx);
            })
        })
        .unwrap();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(host.read_with(cx, |h, _| h.panes.len()), 2);
        cx.update_window(window.into(), |_, window, cx| {
            host.update(cx, |h, cx| h.close_connection(id, window, cx))
        })
        .unwrap();
        cx.simulate_prompt_answer("Close");
        cx.run_until_parked();
        assert_eq!(host.read_with(cx, |h, _| h.panes.len()), 1);
        assert!(!host.read_with(cx, |h, cx| h.active().unwrap().read(cx).rail_visible));
    }
}
