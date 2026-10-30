use crate::form_view::FormSecrets;
use crate::{Services, native_menu, ui::*};
use gpui_kit::{prelude::*, *};
use serde_json::json;
use std::sync::Arc;
use telecat_app::{
    connect::{host_key_problem, host_key_prompt, open_session},
    credentials::{Asked, Typed, asked_secrets, connect_error_message, with_keychain_secrets},
    store::{
        connection_matches, connection_menu, delete_connection_prompt, delete_group_prompt,
        enter_target, group_menu, listed_connections, new_connection, step_selection,
    },
};
use telecat_core::{
    types::*,
    url::{connection_url, parse_connection_url},
};
use telecat_db::ssh::{HostKeyKind, HostKeyProblem};
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
    /// The secrets set to "Ask every time", one field each.
    Password {
        connection: ConnectionConfig,
        inputs: Vec<(Asked, Entity<InputState>)>,
    },
    /// Connecting stopped at an SSH host key: trust it (and connect again) or cancel.
    HostKey(Box<PendingHostKey>),
    /// Copy as URL: the URL holds the saved passwords, so it asks first.
    CopyUrl(ConnectionConfig),
    /// Import from URL: a new, unsaved connection form filled from the URL.
    ImportUrl {
        input: Entity<InputState>,
        error: Option<String>,
    },
    /// Asks before deleting a connection (and its Keychain password) or a group.
    ConfirmDelete(DeleteTarget),
    Error(String),
}
/// A connect waiting for the user to decide on the SSH server's host key.
struct PendingHostKey {
    problem: HostKeyProblem,
    connection: ConnectionConfig,
    /// What was typed for the connect, to repeat it once the key is trusted.
    typed: Typed,
}
#[derive(Clone)]
enum DeleteTarget {
    Connection(ConnectionConfig),
    Group(ConnectionGroup),
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
    /// The window itself, focused while a sheet without a field is open so Enter / Esc reach it.
    focus: FocusHandle,
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
        // A held Enter repeats: the repeat must not submit a sheet the first press opened.
        if event.keystroke.key == "enter" && !event.is_held {
            if self.sheet.is_some() {
                self.accept_sheet(window, cx);
            } else {
                self.connect_selected(window, cx);
            }
            cx.stop_propagation();
        }
    }
    /// ↑ / ↓ move the selection through the listed connections, from the search field too.
    fn arrows(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let down = match event.keystroke.key.as_str() {
            "down" => true,
            "up" => false,
            _ => return,
        };
        if self.sheet.is_some() || event.keystroke.modifiers.modified() {
            return;
        }
        let snapshot = self.services.model.read(cx).snapshot.clone();
        let query = self.search.read(cx).value().to_string();
        let listed = listed_connections(&snapshot, &query);
        if let Some(id) = step_selection(&listed, self.selected.as_deref(), down) {
            self.selected = Some(id);
        }
        cx.stop_propagation();
        cx.notify();
    }
    /// Enter: connect to the selected connection, or to the only one the search leaves.
    fn connect_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let snapshot = self.services.model.read(cx).snapshot.clone();
        let query = self.search.read(cx).value().to_string();
        let listed = listed_connections(&snapshot, &query);
        if let Some(c) = enter_target(&listed, self.selected.as_deref()).cloned() {
            self.connect(c, None, window, cx);
        }
    }
    fn confirm_delete(&mut self, target: DeleteTarget, cx: &mut Context<Self>) {
        self.sheet = Some(Sheet::ConfirmDelete(target));
        cx.notify();
    }
    fn delete_confirmed(&mut self, target: DeleteTarget, cx: &mut Context<Self>) {
        self.sheet = None;
        match target {
            DeleteTarget::Group(g) => {
                if let Err(e) = self.services.profile.delete_group(&g.id) {
                    self.error(e, cx);
                }
                self.services.refresh(cx);
            }
            DeleteTarget::Connection(c) => {
                if self.selected.as_deref() == Some(c.id.as_str()) {
                    self.selected = None;
                }
                let services = self.services.clone();
                let task = services
                    .runtime
                    .spawn_blocking(move || services.profile.delete_connection(&c.id));
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
        }
        cx.notify();
    }

    fn accept_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.sheet.as_ref() {
            Some(Sheet::Picker(driver, group)) => {
                let c = new_connection(*driver, group.clone());
                self.sheet = None;
                crate::open_form(self.services.clone(), c, FormSecrets::default(), "", cx);
                cx.notify();
            }
            Some(Sheet::Password { connection, inputs }) => {
                let c = connection.clone();
                let typed = answers(inputs, cx);
                self.connect(c, Some(typed), window, cx);
            }
            Some(Sheet::HostKey(pending)) => {
                // Enter trusts a key seen for the first time; for a changed one it cancels.
                if pending.problem.kind == HostKeyKind::Unknown {
                    self.trust_host_key(window, cx);
                } else {
                    self.sheet = None;
                    cx.notify();
                }
            }
            Some(Sheet::CopyUrl(c)) => {
                let c = c.clone();
                self.copy_url(c, true, cx);
            }
            Some(Sheet::ImportUrl { .. }) => self.import_url(cx),
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
            // Enter never deletes: it closes the confirmation, like Cancel.
            Some(Sheet::Error(_) | Sheet::ConfirmDelete(_)) => {
                self.sheet = None;
                cx.notify();
            }
            None => {}
        }
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
            // Enter is handled once, by the window (`keyboard`), never by the field as well: the
            // keystroke that opens a sheet (Enter on an Ask-password connection) must not also
            // submit it.
            cx.subscribe(&search, |_, _, _: &crate::ui::InputEvent, cx| cx.notify()),
            cx.observe(&services.model, |this, model, cx| {
                if let Some(error) = model.update(cx, |model, _| model.import_error.take()) {
                    this.error(error, cx);
                }
                cx.notify();
            }),
        ];
        Self {
            focus: cx.focus_handle(),
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
        typed: Option<Typed>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let asked = asked_secrets(&c);
        if typed.is_none() && !asked.is_empty() {
            let inputs: Vec<_> = asked
                .into_iter()
                .map(|a| (a, text_input("", a.label(), true, window, cx)))
                .collect();
            inputs[0].1.update(cx, |s, cx| s.focus(window, cx));
            self.sheet = Some(Sheet::Password {
                connection: c,
                inputs,
            });
            cx.notify();
            return;
        }
        let typed = typed.unwrap_or_default();
        self.selected = Some(c.id.clone());
        self.busy = true;
        self.sheet = None;
        cx.notify();
        let services = self.services.clone();
        let task = services.runtime.spawn({
            let services = services.clone();
            let (c, typed) = (c.clone(), typed.clone());
            async move {
                if services.scenario {
                    anyhow::bail!("Scenario fixtures do not open database connections");
                }
                let keys = services.profile.keychain.clone();
                let c2 = c.clone();
                let typed = services
                    .runtime
                    .spawn_blocking(move || with_keychain_secrets(&c2, typed, keys.as_ref()))
                    .await??;
                let session = open_session(
                    &c,
                    &typed,
                    services.profile.keychain.as_ref(),
                    &services.profile.ssh_env,
                )
                .await?;
                Ok::<_, anyhow::Error>(Arc::new(session))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(anyhow::Error::from).and_then(|v| v);
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(session) => crate::open_workspace(this.services.clone(), session, c, cx),
                    Err(e) => match host_key_problem(&e) {
                        Some(problem) => {
                            this.sheet = Some(Sheet::HostKey(Box::new(PendingHostKey {
                                problem,
                                connection: c,
                                typed,
                            })))
                        }
                        None => this.error(connect_error_message(&c, &e), cx),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn trust_host_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Sheet::HostKey(pending)) = self.sheet.take() else {
            return;
        };
        let PendingHostKey {
            problem,
            connection,
            typed,
        } = *pending;
        match telecat_db::ssh::trust_host_key(&self.services.profile.ssh_env, &problem) {
            Ok(()) => self.connect(connection, Some(typed), window, cx),
            Err(e) => self.error(format!("SSH: {e}"), cx),
        }
    }
    /// Copies the connection's URL, with its saved database and SSH passwords when `passwords`.
    fn copy_url(&mut self, c: ConnectionConfig, passwords: bool, cx: &mut Context<Self>) {
        self.sheet = None;
        cx.notify();
        let profile = self.services.profile.clone();
        let id = c.id.clone();
        let task = self.services.runtime.spawn_blocking(move || {
            if passwords {
                profile.saved_secrets(&id)
            } else {
                Ok([None, None, None])
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(anyhow::Error::from).and_then(|v| v);
            let _ = this.update(cx, |this, cx| match result {
                Ok([password, ssh_password, _]) => {
                    let password = password.filter(|_| c.password_mode == PasswordMode::Keychain);
                    let ssh_password = ssh_password
                        .filter(|_| c.ssh.enabled && c.ssh.password_mode == PasswordMode::Keychain);
                    let url = connection_url(&c, password.as_deref(), ssh_password.as_deref());
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                }
                Err(e) => this.error(e, cx),
            });
        })
        .detach();
    }
    fn show_import_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = text_input(
            "",
            "postgresql://user:password@host:5432/database",
            false,
            window,
            cx,
        );
        input.update(cx, |s, cx| s.focus(window, cx));
        self.sheet = Some(Sheet::ImportUrl { input, error: None });
        cx.notify();
    }
    /// Opens a new connection form filled from the URL. Nothing is saved or connected until the
    /// user does so in the form.
    fn import_url(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::ImportUrl { input, error }) = &mut self.sheet else {
            return;
        };
        let text = input.read(cx).value().to_string();
        match parse_connection_url(&text) {
            Ok(u) => {
                self.sheet = None;
                crate::open_form(
                    self.services.clone(),
                    u.config,
                    FormSecrets {
                        password: u.password.unwrap_or_default(),
                        ssh_password: u.ssh_password.unwrap_or_default(),
                        passphrase: String::new(),
                    },
                    "",
                    cx,
                );
            }
            Err(e) => *error = Some(e.to_string()),
        }
        cx.notify();
    }
    fn edit(&mut self, c: ConnectionConfig, cx: &mut Context<Self>) {
        let services = self.services.clone();
        let id = c.id.clone();
        let task = services
            .runtime
            .spawn_blocking(move || services.profile.saved_secrets(&id));
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(anyhow::Error::from).and_then(|v| v);
            let _ = this.update(cx, |this, cx| match result {
                Ok([password, ssh_password, passphrase]) => crate::open_form(
                    this.services.clone(),
                    c,
                    FormSecrets {
                        password: password.unwrap_or_default(),
                        ssh_password: ssh_password.unwrap_or_default(),
                        passphrase: passphrase.unwrap_or_default(),
                    },
                    "",
                    cx,
                ),
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
                "delete" => this.confirm_delete(DeleteTarget::Connection(c), cx),
                "copy-url" => {
                    this.sheet = Some(Sheet::CopyUrl(c));
                    cx.notify();
                }
                "duplicate" => {
                    let services = this.services.clone();
                    let task = services.runtime.spawn_blocking(move || {
                        services.profile.duplicate_connection(&c.id).map(|_| ())
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
                            .text_size(px(13.5))
                            .line_height(px(16.))
                            .child(div().min_w_0().truncate().child(if c.name.is_empty() {
                                "Untitled".into()
                            } else {
                                c.name.clone()
                            }))
                            .when(!c.tag.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_size(px(11.))
                                        .line_height(px(12.5))
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
                            .text_size(px(11.5))
                            .line_height(px(13.5))
                            .text_color(rgb(if selected { 0xdbe6ff } else { 0x8a8a8a }))
                            .truncate()
                            .child(format!(
                                "{}{}{}{}",
                                c.host,
                                if c.database.is_empty() { "" } else { " : " },
                                c.database,
                                if c.ssh.enabled {
                                    format!(" · via SSH {}", c.ssh.host)
                                } else {
                                    String::new()
                                }
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
                native_menu::choose(json!([{"id":"new","label":"New Connection…"},{"id":"group","label":"New Group…"},{"type":"separator"},{"id":"url","label":"Import from URL…"}]),event.position,window,cx,|this,id,window,cx|{match id.as_str(){"new"=>this.show_picker(cx),"url"=>this.show_import_url(window,cx),_=>this.group_sheet(None,None,window,cx)}});
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
                        .text_size(px(14.5))
                        .font_weight(FontWeight::MEDIUM)
                        .child(group.name),
                )
                .child(
                    div()
                        .text_size(px(11.))
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
                            } else if id == "delete" {
                                this.confirm_delete(DeleteTarget::Group(g), cx);
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
                    .text_size(px(11.5))
                    .child("Drop here to remove from group"),
            );
        }
        if snapshot.connections.is_empty() && group_ids.is_empty() {
            list = list.child(
                div()
                    .mt(px(90.))
                    .text_align(TextAlign::Center)
                    .text_color(rgb(telecat_theme::MUTED))
                    .line_height(px(21.))
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
                                            .text_size(px(12.5))
                                            .child(badge(d, col, 56.))
                                            .child(label)
                                            .on_click(cx.listener(
                                                move |this, event: &ClickEvent, _, cx| {
                                                    if event.click_count() == 2 {
                                                        this.sheet = None;
                                                        crate::open_form(
                                                            this.services.clone(),
                                                            new_connection(d, group.clone()),
                                                            FormSecrets::default(),
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
                                        button("picker-url", "Import from URL…", false, false)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.show_import_url(window, cx)
                                            })),
                                    )
                                    .child(div().flex_1())
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
                                                    FormSecrets::default(),
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
            Sheet::Password { connection, inputs } => {
                let c = connection.clone();
                let fields: Vec<_> = inputs.clone();
                Some(
                    backdrop().child(
                        sheet(
                            380.,
                            format!(
                                "Connect to {}",
                                if c.name.is_empty() { &c.user } else { &c.name }
                            ),
                        )
                        .children(fields.iter().map(|(asked, input)| {
                            div()
                                .mb(px(10.))
                                .child(
                                    div()
                                        .mb(px(4.))
                                        .text_size(px(11.5))
                                        .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                        .child(asked.label()),
                                )
                                .child(input_box(input, false, false))
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .mt(px(6.))
                                .child(button("pw-cancel", "Cancel", false, false).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sheet = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(button("pw-ok", "OK", true, false).on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        let typed = answers(&fields, cx);
                                        this.connect(c.clone(), Some(typed), window, cx);
                                    },
                                ))),
                        ),
                    ),
                )
            }
            Sheet::HostKey(pending) => {
                let prompt = host_key_prompt(&pending.problem);
                let mut buttons = div().mt(px(16.)).flex().justify_end().gap(px(8.));
                if let Some(trust) = prompt.trust {
                    buttons = buttons.child(
                        button("host-key-trust", trust, !prompt.warning, false)
                            .when(prompt.warning, |d| d.text_color(rgb(0xff8a80)))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.trust_host_key(window, cx)),
                            ),
                    );
                }
                buttons = buttons.child(
                    button(
                        "host-key-cancel",
                        if prompt.trust.is_some() {
                            "Cancel"
                        } else {
                            "OK"
                        },
                        prompt.warning,
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sheet = None;
                        cx.notify();
                    })),
                );
                Some(backdrop().child(host_key_sheet(&prompt).child(buttons)))
            }
            Sheet::CopyUrl(c) => {
                let (with, without) = (c.clone(), c.clone());
                Some(
                    backdrop().child(
                        sheet(
                            440.,
                            format!(
                                "Copy the URL of {}?",
                                if c.name.is_empty() { "Untitled" } else { &c.name }
                            ),
                        )
                        .child(
                            div()
                                .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .child("The URL includes the saved database and SSH passwords in plain text. Anyone who sees it, or anything that reads your clipboard, can use them."),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .mt(px(16.))
                                .child(button("url-cancel", "Cancel", false, false).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sheet = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(
                                    button("url-copy-plain", "Copy Without Passwords", false, false)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.copy_url(without.clone(), false, cx)
                                        })),
                                )
                                .child(button("url-copy", "Copy with Passwords", true, false).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.copy_url(with.clone(), true, cx)
                                    }),
                                )),
                        ),
                    ),
                )
            }
            Sheet::ImportUrl { input, error } => {
                let input = input.clone();
                Some(
                    backdrop().child(
                        sheet(480., "Import from URL")
                            .child(
                                div()
                                    .mb(px(8.))
                                    .text_size(px(11.5))
                                    .line_height(px(15.))
                                    .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                    .child("mysql://, mariadb://, postgres:// or postgresql:// — add +ssh for driver+ssh://ssh_user@ssh_host/user@host/database. It opens a new connection form; nothing is saved or connected until you choose to."),
                            )
                            .child(input_box(&input, false, true).when(error.is_some(), |d| {
                                d.border_color(rgb(0xff8a80))
                            }))
                            .when_some(error.clone(), |d, e| {
                                d.child(
                                    div()
                                        .id("url-error")
                                        .mt(px(6.))
                                        .text_size(px(11.5))
                                        .text_color(rgb(0xff8a80))
                                        .child(e),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .mt(px(16.))
                                    .child(button("import-url-cancel", "Cancel", false, false).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.sheet = None;
                                            cx.notify();
                                        }),
                                    ))
                                    .child(button("import-url-ok", "Import", true, false).on_click(
                                        cx.listener(|this, _, _, cx| this.import_url(cx)),
                                    )),
                            ),
                    ),
                )
            }
            Sheet::ConfirmDelete(target) => {
                let (title, detail) = match target {
                    DeleteTarget::Connection(c) => delete_connection_prompt(c),
                    DeleteTarget::Group(g) => {
                        let members = self
                            .services
                            .model
                            .read(cx)
                            .snapshot
                            .connections
                            .iter()
                            .filter(|c| c.group_id.as_deref() == Some(g.id.as_str()))
                            .count();
                        delete_group_prompt(g, members)
                    }
                };
                let target = target.clone();
                Some(
                    backdrop().child(
                        sheet(420., title)
                            .child(
                                div()
                                    .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                                    .text_size(px(12.))
                                    .child(detail),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .mt(px(16.))
                                    .child(
                                        button("delete-confirm", "Delete", false, false)
                                            .text_color(rgb(0xff8a80))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.delete_confirmed(target.clone(), cx)
                                            })),
                                    )
                                    .child(
                                        button("delete-cancel", "Cancel", true, false).on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.sheet = None;
                                                cx.notify();
                                            }),
                                        ),
                                    ),
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
                                .text_size(px(11.5))
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
/// The answers typed in the "Ask every time" sheet.
fn answers(inputs: &[(Asked, Entity<InputState>)], cx: &App) -> Typed {
    let mut typed = Typed::default();
    for (asked, input) in inputs {
        let value = Some(input.read(cx).value().to_string());
        match asked {
            Asked::Password => typed.password = value,
            Asked::SshPassword => typed.ssh_password = value,
            Asked::Passphrase => typed.passphrase = value,
        }
    }
    typed
}
impl Welcome {
    /// When the focused field went away with its sheet (the password sheet closed and an error
    /// took its place), keys would reach nothing: focus the open sheet's field, else the window
    /// while a sheet is open, else the search field.
    fn keep_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.focused(cx).is_some() {
            return;
        }
        match &self.sheet {
            Some(Sheet::Password { inputs, .. }) => {
                if let Some((_, input)) = inputs.first() {
                    input.update(cx, |s, cx| s.focus(window, cx))
                }
            }
            Some(Sheet::Group { input, .. } | Sheet::ImportUrl { input, .. }) => {
                input.update(cx, |s, cx| s.focus(window, cx))
            }
            Some(_) => window.focus(&self.focus, cx),
            None => self.search.update(cx, |s, cx| s.focus(window, cx)),
        }
    }
}
impl Render for Welcome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = &self.subscriptions;
        self.keep_focus(window, cx);
        div()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.keyboard(event, window, cx);
                }
                this.arrows(event, cx);
            }))
            .on_key_down(cx.listener(Self::keyboard))
            .relative()
            .size_full()
            .flex()
            .font(ui_font())
            .text_size(px(12.))
            .line_height(px(14.5))
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
                                    .line_height(px(14.5))
                                    .child(concat!("Version ", env!("CARGO_PKG_VERSION"))),
                            )
                            .child(div().flex_1())
                            .child(
                                div().w_full().flex().flex_col().gap(px(12.)).child(
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
                                                .text_size(px(12.5))
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

#[cfg(test)]
mod tests {
    use super::{Sheet, Welcome};
    use crate::Services;
    use gpui::{AnyWindowHandle, TestAppContext};
    use gpui_kit::{AppContext, Entity};
    use std::sync::Arc;
    use telecat_core::types::*;

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Welcome>) {
        let mut ask = empty_connection(Driver::Mysql);
        ask.id = "ask".into();
        ask.name = "Asks".into();
        ask.password_mode = PasswordMode::Ask;
        open_with(cx, vec![ask], true).0
    }
    /// `scenario`: no database is ever opened (connecting ends in an error sheet). Off, opening
    /// a form window works as in the app.
    fn open_with(
        cx: &mut TestAppContext,
        connections: Vec<ConnectionConfig>,
        scenario: bool,
    ) -> ((AnyWindowHandle, Entity<Welcome>), Services) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let root = std::env::temp_dir()
                .join("telecat-ui-profiles")
                .join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&root).unwrap();
            let mut profile = telecat_app::store::Profile::new(
                root.clone(),
                Arc::new(telecat_app::credentials::MemoryKeychain::default()),
            );
            // Never the real ~/.ssh.
            profile.ssh_env = telecat_db::ssh::SshEnv::isolated(&root);
            let profile = Arc::new(profile);
            profile
                .seed(&StoreSnapshot {
                    connections: connections.clone(),
                    groups: vec![],
                })
                .unwrap();
            let services = Services {
                profile,
                runtime: Arc::new(
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .enable_all()
                        .build()
                        .unwrap()
                        .into(),
                ),
                model: cx.new(|_| crate::Model {
                    import_error: None,
                    snapshot: StoreSnapshot {
                        connections,
                        groups: vec![],
                    },
                }),
                scenario,
            };
            let window_services = services.clone();
            let (w, view) = gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| Welcome::new(window_services, "", window, cx))
            })
            .unwrap();
            ((w, view), services)
        })
    }
    /// The connection form windows that are open.
    fn forms(cx: &mut TestAppContext) -> Vec<Entity<crate::form_view::ConnectionForm>> {
        cx.update(|cx| {
            cx.windows()
                .into_iter()
                .filter_map(|handle| {
                    handle
                        .update(cx, |_, window, cx| {
                            let root = window.root::<gpui_kit::base::Root>().flatten()?;
                            root.read(cx)
                                .view()
                                .clone()
                                .downcast::<crate::form_view::ConnectionForm>()
                                .ok()
                        })
                        .ok()
                        .flatten()
                })
                .collect()
        })
    }
    fn wait_idle(cx: &mut TestAppContext, view: &Entity<Welcome>) {
        for _ in 0..200 {
            cx.executor().allow_parking();
            cx.run_until_parked();
            if !view.read_with(cx, |v, _| v.busy) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[gpui::test]
    fn import_from_url_fills_a_new_form_and_never_repeats_a_bad_url(cx: &mut TestAppContext) {
        let ((w, view), services) = open_with(cx, vec![], false);
        cx.update_window(w, |_, window, cx| {
            view.update(cx, |v, cx| v.show_import_url(window, cx))
        })
        .unwrap();
        paint(cx, w);
        cx.simulate_input(w, "mysql://root:hunter2@db:99999/zixma");
        cx.simulate_keystrokes(w, "enter");
        paint(cx, w);
        view.read_with(cx, |v, _| match &v.sheet {
            Some(Sheet::ImportUrl { error: Some(e), .. }) => {
                assert!(e.contains("port") && !e.contains("hunter2"), "{e}")
            }
            _ => panic!("the error is shown in the sheet"),
        });
        assert!(forms(cx).is_empty());
        cx.update_window(w, |_, window, cx| {
            view.update(cx, |v, cx| {
                if let Some(Sheet::ImportUrl { input, .. }) = &v.sheet {
                    input.update(cx, |s, cx| {
                        s.set_value(
                            "postgresql+ssh://ops:s%40sh@bastion:2200/app:p%40ss@10.0.0.5:5433/main?name=Prod&env=production",
                            window,
                            cx,
                        )
                    });
                }
            })
        })
        .unwrap();
        cx.simulate_keystrokes(w, "enter");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.sheet.is_none()));
        let forms = forms(cx);
        assert_eq!(forms.len(), 1, "a form opened");
        forms[0].read_with(cx, |f, cx| {
            let c = f.connection();
            assert_eq!((c.name.as_str(), c.tag.as_str()), ("Prod", "production"));
            assert_eq!(
                (c.host.as_str(), c.port, c.user.as_str()),
                ("10.0.0.5", 5433, "app")
            );
            assert!(c.ssh.enabled);
            assert_eq!((c.ssh.host.as_str(), c.ssh.port), ("bastion", 2200));
            assert!(c.id.is_empty(), "not saved");
            assert_eq!(f.field_value("password", cx), "p@ss");
            assert_eq!(f.field_value("ssh_password", cx), "s@sh");
        });
        assert!(
            services.profile.snapshot().connections.is_empty(),
            "nothing saved"
        );
    }
    #[gpui::test]
    fn copy_as_url_asks_first_and_includes_passwords_only_when_chosen(cx: &mut TestAppContext) {
        let mut c = empty_connection(Driver::Mysql);
        c.id = "c1".into();
        c.name = "Prod".into();
        c.host = "db".into();
        c.user = "root".into();
        c.ssh.enabled = true;
        c.ssh.host = "bastion".into();
        c.ssh.user = "ops".into();
        let ((w, view), services) = open_with(cx, vec![c.clone()], true);
        services
            .profile
            .save_connection_with(
                c.clone(),
                telecat_app::store::Secrets {
                    password: Some("db pw"),
                    ssh_password: Some("ssh/pw"),
                    passphrase: None,
                },
            )
            .unwrap();
        view.update(cx, |v, cx| {
            v.sheet = Some(Sheet::CopyUrl(c.clone()));
            cx.notify();
        });
        paint(cx, w);
        assert!(
            cx.read_from_clipboard().is_none(),
            "nothing copied before confirming"
        );
        // Esc cancels.
        cx.simulate_keystrokes(w, "escape");
        assert!(view.read_with(cx, |v, _| v.sheet.is_none()));
        assert!(cx.read_from_clipboard().is_none());
        view.update(cx, |v, cx| v.copy_url(c.clone(), false, cx));
        wait_idle(cx, &view);
        let url = cx
            .read_from_clipboard()
            .and_then(|i| i.text())
            .unwrap_or_default();
        assert!(
            url.starts_with("mysql+ssh://ops@bastion/root@db:3306/"),
            "{url}"
        );
        view.update(cx, |v, cx| {
            v.sheet = Some(Sheet::CopyUrl(c.clone()));
            cx.notify();
        });
        paint(cx, w);
        cx.simulate_keystrokes(w, "enter");
        let mut url = String::new();
        for _ in 0..100 {
            cx.executor().allow_parking();
            cx.run_until_parked();
            url = cx
                .read_from_clipboard()
                .and_then(|i| i.text())
                .unwrap_or_default();
            if url.contains("db%20pw") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            url.starts_with("mysql+ssh://ops:ssh%2Fpw@bastion/root:db%20pw@db:3306/"),
            "{url}"
        );
    }
    #[gpui::test]
    fn each_secret_set_to_ask_gets_its_own_field(cx: &mut TestAppContext) {
        let mut c = empty_connection(Driver::Postgres);
        c.id = "ssh-ask".into();
        c.name = "Bastion".into();
        c.password_mode = PasswordMode::Ask;
        c.ssh.enabled = true;
        c.ssh.host = "bastion".into();
        c.ssh.password_mode = PasswordMode::Ask;
        let ((w, view), _) = open_with(cx, vec![c.clone()], true);
        cx.update_window(w, |_, window, cx| {
            view.update(cx, |v, cx| v.connect(c.clone(), None, window, cx))
        })
        .unwrap();
        paint(cx, w);
        view.read_with(cx, |v, _| match &v.sheet {
            Some(Sheet::Password { inputs, .. }) => {
                let asked: Vec<_> = inputs.iter().map(|(a, _)| *a).collect();
                assert_eq!(
                    asked,
                    [
                        telecat_app::credentials::Asked::Password,
                        telecat_app::credentials::Asked::SshPassword
                    ]
                );
            }
            _ => panic!("asks for both"),
        });
        cx.simulate_keystrokes(w, "enter");
        wait_idle(cx, &view);
        view.read_with(cx, |v, _| assert!(matches!(v.sheet, Some(Sheet::Error(_)))));
    }
    fn paint(cx: &mut TestAppContext, w: AnyWindowHandle) {
        cx.update_window(w, |_, w, cx| {
            w.draw(cx).clear(cx);
        })
        .unwrap();
    }
    #[gpui::test]
    fn the_enter_that_opens_the_password_sheet_never_submits_it(cx: &mut TestAppContext) {
        let (w, view) = open(cx);
        cx.update_window(w, |_, window, _| window.activate_window())
            .unwrap();
        paint(cx, w);
        // Search focused: ↓ selects the connection, Enter asks for its password.
        cx.simulate_keystrokes(w, "down");
        assert_eq!(
            view.read_with(cx, |v, _| v.selected.clone()).as_deref(),
            Some("ask")
        );
        cx.simulate_keystrokes(w, "enter");
        cx.run_until_parked();
        paint(cx, w);
        view.read_with(cx, |v, _| {
            assert!(
                matches!(v.sheet, Some(Sheet::Password { .. })),
                "the sheet waits for the password"
            );
            assert!(!v.busy, "nothing was submitted");
        });
        // Typing a password and Enter submits it, once.
        cx.simulate_input(w, "pw");
        cx.simulate_keystrokes(w, "enter");
        view.read_with(cx, |v, _| {
            assert!(
                !matches!(v.sheet, Some(Sheet::Password { .. })),
                "submitted"
            );
        });
        // The connect attempt ends (scenario services never open a database).
        for _ in 0..200 {
            cx.executor().allow_parking();
            cx.run_until_parked();
            if !view.read_with(cx, |v, _| v.busy) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        paint(cx, w);
        view.read_with(cx, |v, _| assert!(matches!(v.sheet, Some(Sheet::Error(_)))));
        // The password field went away with its sheet: Enter still reaches the error sheet.
        paint(cx, w);
        cx.simulate_keystrokes(w, "enter");
        cx.run_until_parked();
        paint(cx, w);
        view.read_with(cx, |v, _| {
            assert!(v.sheet.is_none() && !v.busy, "Enter closed it")
        });
        // And Esc closes the next one.
        view.update(cx, |v, cx| v.error("again", cx));
        paint(cx, w);
        cx.simulate_keystrokes(w, "escape");
        view.read_with(cx, |v, _| assert!(v.sheet.is_none(), "Esc closed it"));
    }
}
