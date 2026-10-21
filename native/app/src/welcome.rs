use crate::{Services, native_menu, ui::*};
use gpui_kit::{prelude::*, *};
use serde_json::json;
use std::sync::Arc;
use telecat_app::{
    credentials::{NATIVE_SERVICE, resolve_password},
    store::{connection_matches, connection_menu, group_menu, new_connection},
};
use telecat_core::types::*;
#[derive(Clone)]
struct DragConnection {
    id: String,
    name: String,
    group_id: Option<String>,
}
struct DragLabel(String);
impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p(px(8.))
            .rounded(px(8.))
            .bg(rgb(telecat_theme::ACCENT))
            .text_color(rgb(0xffffff))
            .child(self.0.clone())
    }
}
enum Sheet {
    /// Choosing a driver for a new connection, created inside the group when one is given.
    Picker(Driver, Option<String>),
    Group {
        group: ConnectionGroup,
        move_id: Option<String>,
        input: Entity<InputState>,
    },
    Password {
        connection: ConnectionConfig,
        input: Entity<InputState>,
    },
    Error(String),
}
pub struct Welcome {
    services: Services,
    search: Entity<InputState>,
    selected: Option<String>,
    sheet: Option<Sheet>,
    busy: bool,
    smoke: bool,
    dragging: Option<String>,
    subscriptions: Vec<Subscription>,
}
impl Welcome {
    pub fn show_picker(&mut self, cx: &mut Context<Self>) {
        self.sheet = Some(Sheet::Picker(Driver::Postgres, None));
        cx.notify();
    }
    fn keyboard(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            self.sheet = None;
            cx.notify();
            cx.stop_propagation();
        }
        if event.keystroke.key == "enter" {
            self.accept_sheet(window, cx);
        }
    }

    fn accept_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.sheet.as_ref() {
            Some(Sheet::Picker(driver, group)) => {
                let c = new_connection(*driver, group.clone());
                self.sheet = None;
                crate::open_form(self.services.clone(), c, String::new(), "", cx);
                cx.notify();
            }
            Some(Sheet::Password { connection, input }) => {
                let c = connection.clone();
                let pw = input.read(cx).value().to_string();
                self.connect(c, Some(pw), window, cx);
            }
            Some(Sheet::Group {
                group,
                move_id,
                input,
            }) => {
                let mut group = group.clone();
                group.name = input.read(cx).value().trim().to_string();
                let id = move_id.clone();
                if !group.name.is_empty() {
                    match self.services.profile.save_group(group) {
                        Ok(g) => {
                            if let Some(id) = id {
                                let _ = self.services.profile.move_connection(&id, Some(&g.id));
                            }
                            self.sheet = None;
                            self.services.refresh(cx);
                        }
                        Err(e) => self.error(e, cx),
                    }
                    cx.notify();
                }
            }
            Some(Sheet::Error(_)) => {
                self.sheet = None;
                cx.notify();
            }
            _ => {}
        }
    }
    fn watch_sheet_input(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.subscriptions.push(cx.subscribe_in(
            input,
            window,
            |this, _, event: &crate::ui::InputEvent, window, cx| {
                if matches!(event, crate::ui::InputEvent::PressEnter { .. }) {
                    this.accept_sheet(window, cx);
                }
            },
        ));
    }
    pub fn new(
        services: Services,
        scenario: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let notice = (!services.scenario)
            .then(|| telecat_app::recovery::take_notice(&services.profile.root))
            .flatten();
        let search = text_input("", "Search for connection...", false, window, cx);
        if scenario.is_empty() || scenario == "welcome" {
            search.update(cx, |s, cx| s.focus(window, cx));
        }
        let subscriptions = vec![
            cx.subscribe_in(
                &search,
                window,
                |this, _, event: &crate::ui::InputEvent, window, cx| {
                    if matches!(event, crate::ui::InputEvent::PressEnter { .. }) {
                        this.accept_sheet(window, cx);
                    }
                    cx.notify();
                },
            ),
            cx.observe(&services.model, |this, model, cx| {
                if let Some(error) = model.update(cx, |model, _| model.import_error.take()) {
                    this.error(error, cx);
                }
                cx.notify();
            }),
        ];
        Self {
            services,
            search,
            selected: None,
            sheet: if scenario == "02-picker-mysql" {
                Some(Sheet::Picker(Driver::Mysql, None))
            } else {
                notice.map(Sheet::Error)
            },
            busy: false,
            smoke: scenario == "smoke",
            dragging: None,
            subscriptions,
        }
    }
    fn error(&mut self, e: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.sheet = Some(Sheet::Error(e.to_string()));
        cx.notify();
    }
    fn group_sheet(
        &mut self,
        g: Option<ConnectionGroup>,
        move_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = g.unwrap_or(ConnectionGroup {
            id: String::new(),
            name: String::new(),
            collapsed: None,
        });
        let input = text_input(group.name.clone(), "Group name", false, window, cx);
        self.watch_sheet_input(&input, window, cx);
        input.update(cx, |s, cx| s.focus(window, cx));
        self.sheet = Some(Sheet::Group {
            group,
            move_id,
            input,
        });
        cx.notify();
    }
    fn connect(
        &mut self,
        c: ConnectionConfig,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        if c.password_mode == PasswordMode::Ask && password.is_none() {
            let input = text_input("", "Password", true, window, cx);
            self.watch_sheet_input(&input, window, cx);
            input.update(cx, |s, cx| s.focus(window, cx));
            self.sheet = Some(Sheet::Password {
                connection: c,
                input,
            });
            cx.notify();
            return;
        }
        self.selected = Some(c.id.clone());
        self.busy = true;
        self.sheet = None;
        cx.notify();
        let services = self.services.clone();
        let task = services.runtime.spawn({
            let services = services.clone();
            async move {
                if services.scenario {
                    anyhow::bail!("Scenario fixtures do not open database connections");
                }
                let keys = services.profile.keychain.clone();
                let c2 = c.clone();
                let password = services
                    .runtime
                    .spawn_blocking(move || {
                        if password.is_some() || c2.password_mode != PasswordMode::Keychain {
                            Ok(password)
                        } else {
                            keys.get(NATIVE_SERVICE, &c2.id).and_then(|v| {
                                v.map(String::from_utf8).transpose().map_err(Into::into)
                            })
                        }
                    })
                    .await??;
                let pw =
                    resolve_password(&c, password.as_deref(), services.profile.keychain.as_ref())
                        .await?;
                Ok::<_, anyhow::Error>((Arc::new(telecat_db::Session::connect(&c, &pw).await?), c))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(anyhow::Error::from).and_then(|v| v);
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((session, connection)) => {
                        crate::open_workspace(this.services.clone(), session, connection, cx)
                    }
                    Err(e) => this.error(e, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn edit(&mut self, c: ConnectionConfig, cx: &mut Context<Self>) {
        let services = self.services.clone();
        let c2 = c.clone();
        let task = services.runtime.spawn_blocking(move || {
            services
                .profile
                .keychain
                .get(NATIVE_SERVICE, &c2.id)
                .and_then(|p| p.map(String::from_utf8).transpose().map_err(Into::into))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(anyhow::Error::from).and_then(|v| v);
            let _ = this.update(cx, |this, cx| match result {
                Ok(pw) => {
                    crate::open_form(this.services.clone(), c, pw.unwrap_or_default(), "", cx)
                }
                Err(e) => this.error(e, cx),
            });
        })
        .detach();
    }
    fn context(
        &mut self,
        c: ConnectionConfig,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = connection_menu(&c, &self.services.model.read(cx).snapshot);
        native_menu::choose(
            items,
            position,
            window,
            cx,
            move |this, action, window, cx| match action.as_str() {
                "connect" => this.connect(c, None, window, cx),
                "edit" => this.edit(c, cx),
                "move-new" => {
                    match this.services.profile.save_group(ConnectionGroup {
                        id: String::new(),
                        name: "New Group".into(),
                        collapsed: None,
                    }) {
                        Ok(g) => {
                            if let Err(e) =
                                this.services.profile.move_connection(&c.id, Some(&g.id))
                            {
                                this.error(e, cx);
                            } else {
                                this.services.refresh(cx);
                                this.group_sheet(Some(g), None, window, cx);
                            }
                        }
                        Err(e) => this.error(e, cx),
                    }
                }
                "duplicate" | "delete" => {
                    let services = this.services.clone();
                    let duplicate = action == "duplicate";
                    let task = services.runtime.spawn_blocking(move || {
                        if duplicate {
                            services.profile.duplicate_connection(&c.id).map(|_| ())
                        } else {
                            services.profile.delete_connection(&c.id)
                        }
                    });
                    cx.spawn(async move |this, cx| {
                        let result = task.await.map_err(anyhow::Error::from).and_then(|v| v);
                        let _ = this.update(cx, |this, cx| {
                            if let Err(e) = result {
                                this.error(e, cx);
                            }
                            this.services.refresh(cx);
                        });
                    })
                    .detach();
                }
                _ => {
                    if let Some(group) = action.strip_prefix("move:") {
                        let result = this.services.profile.move_connection(
                            &c.id,
                            if group.is_empty() { None } else { Some(group) },
                        );
                        if let Err(e) = result {
                            this.error(e, cx);
                        }
                        this.services.refresh(cx);
                    }
                }
            },
        );
    }
    fn drop_into(&mut self, d: &DragConnection, group: Option<&str>, cx: &mut Context<Self>) {
        if let Err(e) = self.services.profile.drop_connection(&d.id, group) {
            self.error(e, cx);
        }
        self.services.refresh(cx);
    }
    fn connection_row(
        &self,
        c: &ConnectionConfig,
        grouped: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let weak = cx.entity().downgrade();
        let c = c.clone();
        let click = c.clone();
        let context = c.clone();
        let selected = self.selected.as_deref() == Some(&c.id);
        let tag_color = choices()["TAG_COLORS"][&c.tag]
            .as_str()
            .unwrap_or("#8a8a8a")
            .to_string();
        let tag_num =
            u32::from_str_radix(tag_color.trim_start_matches('#'), 16).unwrap_or(0x8a8a8a);
        div()
            .id(SharedString::from(c.id.clone()))
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .tooltip(text_tooltip(if c.name.is_empty() {
                "Untitled".to_owned()
            } else {
                c.name.clone()
            }))
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(if grouped { 8. } else { 32. }))
            .pr(px(8.))
            .py(px(6.))
            .rounded(px(8.))
            .when(selected, |d| d.bg(rgb(telecat_theme::ACCENT)))
            .opacity(if self.dragging.as_deref() == Some(&c.id) {
                0.45
            } else {
                1.
            })
            .child(badge(c.driver, &c.color, 30.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_size(px(15.))
                            .line_height(px(18.))
                            .child(div().min_w_0().truncate().child(if c.name.is_empty() {
                                "Untitled".into()
                            } else {
                                c.name.clone()
                            }))
                            .when(!c.tag.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_size(px(12.))
                                        .line_height(px(14.))
                                        .py(px(1.))
                                        .px(px(7.))
                                        .rounded(px(5.))
                                        .flex_shrink_0()
                                        .bg(rgba((tag_num << 8) | 0x26))
                                        .text_color(rgb(tag_num))
                                        .child(c.tag.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .line_height(px(15.))
                            .text_color(rgb(if selected { 0xdbe6ff } else { 0x8a8a8a }))
                            .truncate()
                            .child(format!(
                                "{}{}{}",
                                c.host,
                                if c.database.is_empty() { "" } else { " : " },
                                c.database
                            )),
                    ),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.selected = Some(click.id.clone());
                if event.click_count() == 2 {
                    this.connect(click.clone(), None, window, cx);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.selected = Some(context.id.clone());
                    this.context(context.clone(), event.position, window, cx);
                    cx.notify();
                }),
            )
            .on_drag(
                DragConnection {
                    id: c.id,
                    name: c.name,
                    group_id: c.group_id,
                },
                move |d, _, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.dragging = Some(d.id.clone());
                        this.selected = Some(d.id.clone());
                        cx.notify();
                    });
                    let view = cx.new(|_| DragLabel(d.name.clone()));
                    let weak = weak.clone();
                    cx.observe_release(&view, move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.dragging = None;
                            cx.notify();
                        });
                    })
                    .detach();
                    view
                },
            )
    }
    fn list(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let snapshot = self.services.model.read(cx).snapshot.clone();
        let query = self.search.read(cx).value().to_string();
        let visible: Vec<_> = snapshot
            .connections
            .iter()
            .filter(|c| connection_matches(c, &query))
            .collect();
        let mut list=div()
            .id("connection-list").flex_1().min_h_0().overflow_y_scroll()
            .pt(px(4.)).px(px(10.)).pb(px(14.))
            .drag_over::<DragConnection>(|s,d,_,_|if d.group_id.is_some(){s.rounded(px(10.)).shadow(vec![BoxShadow{inset:true,color:rgba(0x2f6fed80).into(),offset:point(px(0.),px(0.)),blur_radius:px(0.),spread_radius:px(2.)}])}else{s})
            .on_click(cx.listener(|this,_,_,cx|{this.selected=None;cx.notify();}))
            .on_drop(cx.listener(|this,d:&DragConnection,_,cx|this.drop_into(d,None,cx)))
            .on_mouse_down(MouseButton::Right,cx.listener(|_,event:&MouseDownEvent,window,cx|{
                native_menu::choose(json!([{"id":"new","label":"New Connection…"},{"id":"group","label":"New Group…"}]),event.position,window,cx,|this,id,window,cx|{if id=="new"{this.show_picker(cx);}else{this.group_sheet(None,None,window,cx);}});
            }));
        let group_ids: Vec<_> = snapshot.groups.iter().map(|g| g.id.clone()).collect();
        for group in snapshot.groups {
            let members: Vec<_> = visible
                .iter()
                .filter(|c| c.group_id.as_ref() == Some(&group.id))
                .collect();
            if !query.trim().is_empty() && members.is_empty() {
                continue;
            }
            let expanded = group.collapsed != Some(true) || !query.trim().is_empty();
            let g = group.clone();
            let drop_id = group.id.clone();
            let context = group.clone();
            let group_row = div()
                .id(SharedString::from(format!("group-{}", group.id)))
                .h(px(46.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(icon(
                    if expanded {
                        "ChevronDown.svg"
                    } else {
                        "ChevronRight.svg"
                    },
                    14.,
                    0x8a8a8a,
                ))
                .child(
                    div()
                        .size(px(30.))
                        .rounded_full()
                        .border_2()
                        .border_color(rgb(0xf39a1e))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("Plug.svg", 18., 0xf39a1e)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(group.name),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(
                            snapshot
                                .connections
                                .iter()
                                .filter(|c| c.group_id.as_ref() == Some(&group.id))
                                .count()
                                .to_string(),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let mut g = g.clone();
                    g.collapsed = Some(g.collapsed != Some(true));
                    if let Err(e) = this.services.profile.save_group(g) {
                        this.error(e, cx);
                    }
                    this.services.refresh(cx);
                }));
            let group_row = group_row.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let g = context.clone();
                    native_menu::choose(
                        group_menu(),
                        event.position,
                        window,
                        cx,
                        move |this, id, window, cx| {
                            if id == "new" {
                                this.sheet =
                                    Some(Sheet::Picker(Driver::Postgres, Some(g.id.clone())));
                                cx.notify();
                            } else if id == "rename" {
                                this.group_sheet(Some(g), None, window, cx);
                            } else {
                                if let Err(e) = this.services.profile.delete_group(&g.id) {
                                    this.error(e, cx);
                                }
                                this.services.refresh(cx);
                            }
                        },
                    );
                }),
            );
            list = list.child(
                div()
                    .id(SharedString::from(format!("drop-{}", group.id)))
                    .border_1()
                    .border_color(rgba(0))
                    .rounded(px(10.))
                    .drag_over::<DragConnection>(|s, _, _, _| {
                        s.border_color(rgb(telecat_theme::ACCENT))
                            .bg(rgba(0x2f6fed1f))
                    })
                    .on_drop(cx.listener(move |this, d: &DragConnection, _, cx| {
                        cx.stop_propagation();
                        this.drop_into(d, Some(&drop_id), cx);
                    }))
                    .child(group_row)
                    .when(expanded, |d| {
                        d.child(
                            div()
                                .pl(px(24.))
                                .children(members.iter().map(|c| self.connection_row(c, true, cx))),
                        )
                    }),
            );
        }
        for c in visible.iter().filter(|c| {
            c.group_id.is_none() || !group_ids.iter().any(|g| Some(g) == c.group_id.as_ref())
        }) {
            list = list.child(self.connection_row(c, false, cx));
        }
        if self.dragging.as_ref().is_some_and(|id| {
            snapshot
                .connections
                .iter()
                .any(|c| &c.id == id && c.group_id.is_some())
        }) {
            list = list.child(
                div()
                    .mt(px(8.))
                    .mx(px(8.))
                    .p(px(12.))
                    .border_1()
                    .border_dashed()
                    .border_color(rgba(0xffffff30))
                    .rounded(px(8.))
                    .text_align(TextAlign::Center)
                    .text_color(rgb(telecat_theme::MUTED))
                    .text_size(px(12.5))
                    .child("Drop here to remove from group"),
            );
        }
        if snapshot.connections.is_empty() && group_ids.is_empty() {
            list = list.child(
                div()
                    .mt(px(90.))
                    .text_align(TextAlign::Center)
                    .text_color(rgb(telecat_theme::MUTED))
                    .line_height(px(23.4))
                    .child(div().child("No connections yet."))
                    .child(
                        div()
                            .flex()
                            .justify_center()
                            .child("Click ")
                            .child(div().font_weight(FontWeight::BOLD).child("+"))
                            .child(" to create one."),
                    ),
            );
        }
        list
    }
    fn render_sheet(&self, cx: &mut Context<Self>) -> Option<Div> {
        match self.sheet.as_ref()? {
            Sheet::Picker(driver, group) => {
                let driver = *driver;
                let group = group.clone();
                Some(
                    backdrop().child(
                        sheet(420., "Select a database driver")
                            .child(
                                div().flex().justify_center().gap(px(14.)).children(
                                    [
                                        (Driver::Postgres, "PostgreSQL", "#336791"),
                                        (Driver::Mysql, "MySQL", "#e48e00"),
                                    ]
                                    .into_iter()
                                    .map(|(d, label, col)| {
                                        let group = group.clone();
                                        div()
                                            .id(label)
                                            .w(px(150.))
                                            .h(px(120.))
                                            .border_2()
                                            .border_color(if driver == d {
                                                rgba(0x2f6fedff)
                                            } else {
                                                rgba(0)
                                            })
                                            .bg(if driver == d {
                                                rgba(0x2f6fed22)
                                            } else {
                                                rgba(0xffffff0a)
                                            })
                                            .rounded(px(12.))
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .justify_center()
                                            .gap(px(10.))
                                            .text_size(px(14.))
                                            .child(badge(d, col, 56.))
                                            .child(label)
                                            .on_click(cx.listener(
                                                move |this, event: &ClickEvent, _, cx| {
                                                    if event.click_count() == 2 {
                                                        this.sheet = None;
                                                        crate::open_form(
                                                            this.services.clone(),
                                                            new_connection(d, group.clone()),
                                                            String::new(),
                                                            "",
                                                            cx,
                                                        );
                                                    } else {
                                                        this.sheet =
                                                            Some(Sheet::Picker(d, group.clone()));
                                                    }
                                                    cx.notify();
                                                },
                                            ))
                                    }),
                                ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .mt(px(16.))
                                    .child(
                                        button("picker-cancel", "Cancel", false, false).on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.sheet = None;
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(
                                        button("picker-create", "Create", true, false).on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.sheet = None;
                                                crate::open_form(
                                                    this.services.clone(),
                                                    new_connection(driver, group.clone()),
                                                    String::new(),
                                                    "",
                                                    cx,
                                                );
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            ),
                    ),
                )
            }
            Sheet::Group {
                group,
                move_id,
                input,
            } => {
                let group = group.clone();
                let move_id = move_id.clone();
                let input = input.clone();
                Some(
                    backdrop().child(
                        sheet(
                            340.,
                            if group.id.is_empty() {
                                "New Group"
                            } else {
                                "Rename Group"
                            },
                        )
                        .child(input_box(&input, false, false))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .mt(px(16.))
                                .child(button("group-cancel", "Cancel", false, false).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sheet = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(button("group-save", "Save", true, false).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        let name = input.read(cx).value().trim().to_string();
                                        if name.is_empty() {
                                            return;
                                        }
                                        let mut g = group.clone();
                                        g.name = name;
                                        match this.services.profile.save_group(g) {
                                            Ok(g) => {
                                                if let Some(id) = &move_id {
                                                    if let Err(e) = this
                                                        .services
                                                        .profile
                                                        .move_connection(id, Some(&g.id))
                                                    {
                                                        this.error(e, cx);
                                                        return;
                                                    }
                                                }
                                                this.sheet = None;
                                                this.services.refresh(cx);
                                            }
                                            Err(e) => this.error(e, cx),
                                        }
                                        cx.notify();
                                    }),
                                )),
                        ),
                    ),
                )
            }
            Sheet::Password { connection, input } => {
                let c = connection.clone();
                let input = input.clone();
                Some(
                    backdrop().child(
                        sheet(
                            360.,
                            format!(
                                "Password for {}",
                                if c.name.is_empty() { &c.user } else { &c.name }
                            ),
                        )
                        .child(input_box(&input, false, false))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .mt(px(16.))
                                .child(button("pw-cancel", "Cancel", false, false).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sheet = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(button("pw-ok", "OK", true, false).on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        let pw = input.read(cx).value().to_string();
                                        this.connect(c.clone(), Some(pw), window, cx);
                                    },
                                ))),
                        ),
                    ),
                )
            }
            Sheet::Error(message) => Some(
                backdrop().child(
                    sheet(420., "Connection failed")
                        .child(
                            div()
                                .text_color(rgb(0xffb4ae))
                                .text_size(px(12.5))
                                .child(message.clone()),
                        )
                        .child(div().flex().justify_end().mt(px(16.)).child(
                            button("error-ok", "OK", true, false).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.sheet = None;
                                    cx.notify();
                                },
                            )),
                        )),
                ),
            ),
        }
    }
}
impl Render for Welcome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = &self.subscriptions;
        div()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.keyboard(event, window, cx);
                }
            }))
            .on_key_down(cx.listener(Self::keyboard))
            .relative()
            .size_full()
            .flex()
            .font(ui_font())
            .text_size(px(13.))
            .line_height(px(16.))
            .text_color(rgb(telecat_theme::TEXT))
            .child(
                div()
                    .relative()
                    .w(px(255.))
                    .h_full()
                    .flex_shrink_0()
                    .child(img("welcome-gradient.png").absolute().size_full())
                    .child(
                        div()
                            .absolute()
                            .right_0()
                            .h_full()
                            .w(px(1.))
                            .bg(rgba(0xffffff10)),
                    )
                    .child(
                        div()
                            .relative()
                            .w(px(254.))
                            .h_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .pt(px(44.))
                            .px(px(20.))
                            .pb(px(22.))
                            .child(img("logo.png").size(px(150.)))
                            .child(div().mt(px(4.)).w_full().child(brand()))
                            .child(
                                div()
                                    .mt(px(4.))
                                    .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                    .line_height(px(16.))
                                    .child(concat!("Version ", env!("CARGO_PKG_VERSION"))),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .flex_col()
                                    .gap(px(12.))
                                    .child(pill("Backup Database", "upload.svg", true))
                                    .child(pill("Restore Database", "download.svg", true))
                                    .child(
                                        pill("Create Connection", "plug.svg", false)
                                            .id("create")
                                            .hover(|s| s.bg(rgba(0xffffff1c)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.sheet =
                                                    Some(Sheet::Picker(Driver::Postgres, None));
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(rgba(0x1f1f22e8))
                    .flex()
                    .flex_col()
                    .when(self.smoke, |d| {
                        d.justify_center()
                            .items_center()
                            .gap(px(12.))
                            .child("ธนาคารกรุงเทพ 😺")
                            .child(
                                div()
                                    .font(telecat_theme::mono())
                                    .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                    .child("SELECT 'ธนาคารกรุงเทพ 😺';"),
                            )
                    })
                    .when(!self.smoke, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .pt(px(16.))
                                .px(px(16.))
                                .pb(px(10.))
                                .child(
                                    div()
                                        .id("plus")
                                        .size(px(34.))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(rgba(0xffffff1c))
                                        .bg(rgba(0xffffff0c))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(icon("Plus.svg", 18., 0xe8e8e8))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sheet =
                                                Some(Sheet::Picker(Driver::Postgres, None));
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .h(px(34.))
                                        .rounded(px(17.))
                                        .border_1()
                                        .border_color(rgba(0xffffff1c))
                                        .bg(rgba(0xffffff08))
                                        .px(px(12.))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .child(icon("Search.svg", 16., 0xb8b8b8))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_size(px(14.))
                                                .child(Input::new(&self.search)),
                                        ),
                                ),
                        )
                        .child(self.list(cx))
                    }),
            )
            .children(self.render_sheet(cx))
    }
}
