use crate::ui::InputEvent;
use crate::{Services, native_menu, ui::*};
use gpui_kit::{prelude::*, *};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use telecat_app::{
    credentials::resolve_password,
    form::{self, FormStatus},
};
use telecat_core::types::*;
pub struct ConnectionForm {
    services: Services,
    c: ConnectionConfig,
    fields: BTreeMap<&'static str, Entity<InputState>>,
    status: FormStatus,
    subscriptions: Vec<Subscription>,
    static_focus: bool,
    static_test_hover: bool,
    color_editor: Option<Entity<InputState>>,
    password: String,
}
impl ConnectionForm {
    pub fn new(
        services: Services,
        c: ConnectionConfig,
        password: String,
        scenario: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut fields = BTreeMap::new();
        let mut subscriptions = vec![];
        for (key, value, placeholder) in [
            ("name", c.name.clone(), "Connection name"),
            ("host", c.host.clone(), "127.0.0.1"),
            (
                "port",
                c.port.to_string(),
                if c.driver == Driver::Mysql {
                    "3306"
                } else {
                    "5432"
                },
            ),
            ("user", c.user.clone(), "user name"),
            (
                "password",
                if c.password_mode == PasswordMode::Command {
                    c.password_command.clone().unwrap_or_default()
                } else {
                    password.clone()
                },
                form::password_placeholder(c.password_mode),
            ),
            ("database", c.database.clone(), "database name"),
        ] {
            let input = text_input(
                value,
                placeholder,
                key == "password" && c.password_mode != PasswordMode::Command,
                window,
                cx,
            );
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit("connect", window, cx),
                    InputEvent::Change if key == "port" => {
                        let raw = input.read(cx).value().to_string();
                        let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
                        if raw != digits {
                            input.update(cx, |s, cx| s.set_value(digits, window, cx));
                        }
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.static_focus = false;
                        cx.notify();
                    }
                    _ => cx.notify(),
                },
            ));
            fields.insert(key, input);
        }
        let status = match scenario {
            "03-form-mysql" => FormStatus::Ok("Connection is OK — MySQL 8.0.46 · TLSv1.3".into()),
            "50-form-testing" => FormStatus::Busy("Testing…".into()),
            _ => FormStatus::Idle,
        };
        for (key, input) in &fields {
            if status.busy() || (*key == "password" && c.password_mode == PasswordMode::None) {
                input.update(cx, |s, cx| s.set_disabled(true, cx));
            }
        }
        if scenario.is_empty() {
            fields["name"].update(cx, |s, cx| s.focus(window, cx));
        }
        Self {
            services,
            c,
            fields,
            status,
            subscriptions,
            static_focus: scenario == "30-form-pg",
            static_test_hover: scenario == "03-form-mysql",
            color_editor: None,
            password,
        }
    }
    fn value(&self, key: &str, cx: &App) -> String {
        self.fields[key].read(cx).value().to_string()
    }
    fn config(&self, cx: &App) -> anyhow::Result<ConnectionConfig> {
        let mut c = self.c.clone();
        c.name = self.value("name", cx);
        c.host = self.value("host", cx);
        c.user = self.value("user", cx);
        c.database = self.value("database", cx);
        if c.password_mode == PasswordMode::Command {
            c.password_command = Some(self.value("password", cx));
        }
        form::validate(&c, &self.value("port", cx))
    }
    fn status(&mut self, status: FormStatus, cx: &mut Context<Self>) {
        let busy = status.busy();
        self.status = status;
        for (key, input) in &self.fields {
            let disabled =
                busy || (*key == "password" && self.c.password_mode == PasswordMode::None);
            input.update(cx, |s, cx| s.set_disabled(disabled, cx));
        }
        cx.notify();
    }
    fn submit(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.busy() {
            return;
        }
        let config = match self.config(cx) {
            Ok(c) => c,
            Err(e) => {
                self.status(FormStatus::Error(e.to_string()), cx);
                return;
            }
        };
        let password = self.value("password", cx);
        let services = self.services.clone();
        self.status(
            FormStatus::Busy(
                if action == "test" {
                    "Testing…"
                } else if action == "save" {
                    "Saving…"
                } else {
                    "Connecting…"
                }
                .into(),
            ),
            cx,
        );
        let task = services.runtime.spawn({
            let services = services.clone();
            async move {
                let config = if action != "test" {
                    let profile = services.profile.clone();
                    let pw = password.clone();
                    services
                        .runtime
                        .spawn_blocking(move || {
                            let keep = config_password_keychain(&config);
                            profile.save_connection(
                                config,
                                if keep { Some(pw.as_str()) } else { None },
                            )
                        })
                        .await??
                } else {
                    config
                };
                if action == "save" {
                    return Ok::<_, anyhow::Error>((config, None));
                }
                // A scenario never performs networking, including the deliberately unreachable reference host.
                if services.scenario {
                    anyhow::bail!("Scenario fixtures do not open database connections");
                }
                let pw =
                    resolve_password(&config, Some(&password), services.profile.keychain.as_ref())
                        .await?;
                let session = Arc::new(telecat_db::Session::connect(&config, &pw).await?);
                if action == "test" {
                    session.close().await;
                }
                Ok((config, Some(session)))
            }
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(anyhow::Error::from).and_then(|v| v);
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.services.refresh(cx);
                    match result {
                        Ok((c, session)) => {
                            this.c = c;
                            if action == "test" {
                                let db = session.unwrap();
                                this.status(
                                    FormStatus::Ok(format!(
                                        "Connection is OK — {} · {}",
                                        db.server_version,
                                        db.tls.as_deref().unwrap_or("no TLS")
                                    )),
                                    cx,
                                );
                            } else {
                                if let Some(db) = session {
                                    crate::open_workspace(
                                        this.services.clone(),
                                        db,
                                        this.c.clone(),
                                        cx,
                                    );
                                }
                                window.remove_window();
                            }
                        }
                        Err(e) => this.status(FormStatus::Error(e.to_string()), cx),
                    }
                });
            });
        })
        .detach();
    }
    fn field(&self, key: &'static str, window: &Window, cx: &App) -> Div {
        let disabled =
            self.status.busy() || (key == "password" && self.c.password_mode == PasswordMode::None);
        let focus = (key == "name" && self.static_focus)
            || self.fields[key]
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
        let d = input_box(&self.fields[key], disabled, focus);
        if key == "name" && self.static_focus {
            d.relative().child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(5.))
                    .h(px(18.))
                    .w(px(1.))
                    .bg(rgb(telecat_theme::TEXT)),
            )
        } else {
            d
        }
    }
    fn select(
        &mut self,
        key: &'static str,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let v = Value::String(id.clone());
        match key {
            "tag" => self.c.tag = id,
            "safe" => {
                if let Ok(v) = serde_json::from_value(v) {
                    self.c.safe_mode = v;
                }
            }
            "ssl" => {
                if let Ok(v) = serde_json::from_value(v) {
                    self.c.ssl_mode = v;
                }
            }
            "neg" => {
                if let Ok(v) = serde_json::from_value(v) {
                    self.c.negotiation = v;
                }
            }
            "password" => {
                if let Ok(mode) = serde_json::from_value(v) {
                    if self.c.password_mode == PasswordMode::Command {
                        self.c.password_command = Some(self.value("password", cx));
                    } else if self.c.password_mode != PasswordMode::None {
                        self.password = self.value("password", cx);
                    }
                    self.c.password_mode = mode;
                    let value = match mode {
                        PasswordMode::None => String::new(),
                        PasswordMode::Command => {
                            self.c.password_command.clone().unwrap_or_default()
                        }
                        _ => self.password.clone(),
                    };
                    self.fields["password"].update(cx, |s, cx| {
                        s.set_masked(mode != PasswordMode::Command, window, cx);
                        s.set_placeholder(form::password_placeholder(mode), window, cx);
                        s.set_value(value, window, cx);
                        s.set_disabled(mode == PasswordMode::None, cx);
                    });
                }
            }
            _ => {}
        }
        cx.notify();
    }
    fn popup(
        &self,
        key: &'static str,
        label: String,
        items: Value,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let disabled = self.status.busy();
        let bounds = std::rc::Rc::new(std::cell::Cell::new(Bounds::<Pixels>::default()));
        let paint_bounds = bounds.clone();
        let key_bounds = bounds.clone();
        let key_items = items.clone();
        let mut d = div()
            .id(key)
            .flex_1()
            .min_w_0()
            .h(px(30.))
            .px(px(10.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .border_1()
            .border_color(rgb(0x3c3c3c))
            .rounded(px(7.))
            .bg(rgb(telecat_theme::INPUT_2))
            .text_size(px(14.));
        if key == "safe" {
            let safe = choices()["SAFE_MODES"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["value"] == json!(self.c.safe_mode))
                .cloned()
                .unwrap();
            if self.c.safe_mode == SafeMode::Silent {
                d = d.child(icon("safe-unlock.svg", 16., 0xe8e8e8));
            } else {
                let col = color(safe["color"].as_str().unwrap());
                d = d.child(
                    div()
                        .size(px(12.))
                        .rounded_full()
                        .flex_shrink_0()
                        .when(safe["icon"] == "dot", |d| d.bg(col))
                        .when(safe["icon"] == "ring", |d| d.border_1().border_color(col)),
                );
            }
        }
        d.relative()
            .child(
                canvas(|_, _, _| (), move |b, _, _, _| paint_bounds.set(b))
                    .absolute()
                    .inset_0()
                    .size_full(),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(icon("UpDown.svg", 14., 0xe8e8e8).opacity(0.8))
            .when(!disabled, |d| {
                d.tab_index(0)
                    .hover(|s| s.bg(rgb(telecat_theme::BORDER)))
                    .focus_visible(|s| s.border_color(rgb(telecat_theme::ACCENT)))
                    .on_key_down(cx.listener(move |_, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "down" {
                            cx.stop_propagation();
                            native_menu::choose(
                                key_items.clone(),
                                key_bounds.get().origin + point(px(4.), px(3.)),
                                window,
                                cx,
                                move |this, id, window, cx| this.select(key, id, window, cx),
                            );
                        }
                    }))
                    .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
                        let items = items.clone();
                        let measured = bounds.get();
                        let position = if measured.size.width > px(0.) {
                            measured.origin + point(px(4.), px(3.))
                        } else {
                            event.position()
                        };
                        native_menu::choose(
                            items,
                            position,
                            window,
                            cx,
                            move |this, id, window, cx| this.select(key, id, window, cx),
                        );
                    }))
            })
    }
    fn pick(&mut self, key: usize, cx: &mut Context<Self>) {
        if self.status.busy() || self.c.ssl_mode == SslMode::Disabled {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                [
                    "Select SSL key",
                    "Select SSL certificate",
                    "Select CA certificate",
                ][key]
                    .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.first() {
                    let path = path.to_string_lossy().into_owned();
                    let _ = this.update(cx, |this, cx| {
                        match key {
                            0 => this.c.ssl_key = Some(path),
                            1 => this.c.ssl_cert = Some(path),
                            _ => this.c.ssl_ca = Some(path),
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }
}
fn config_password_keychain(c: &ConnectionConfig) -> bool {
    c.password_mode == PasswordMode::Keychain
}
fn row(label: &'static str) -> Div {
    div()
        .w_full()
        .min_h(px(32.))
        .mb(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .w(px(84.))
                .flex_shrink_0()
                .text_align(TextAlign::Right)
                .text_size(px(14.))
                .child(label),
        )
}
fn card() -> Div {
    div()
        .bg(rgb(telecat_theme::CARD))
        .rounded(px(12.))
        .pt(px(12.))
        .px(px(12.))
        .pb(px(6.))
        .mb(px(12.))
}
fn small(label: &'static str) -> Div {
    div()
        .ml(px(6.))
        .flex_shrink_0()
        .text_size(px(14.))
        .child(label)
}
fn menu(values: &Value, current: Value) -> Value {
    let mut items = vec![];
    for (i, v) in values.as_array().unwrap().iter().enumerate() {
        if i == 1 && values[0] == "" {
            items.push(json!({"type":"separator"}));
        }
        items.push(if v.is_object(){json!({"id":v["value"],"label":v["label"],"checked":v["value"]==current,"icon":v["icon"],"color":v["color"]})}else{json!({"id":v,"label":if v==""{json!("None")}else{v.clone()},"checked":*v==current})});
    }
    Value::Array(items)
}
impl Render for ConnectionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = &self.subscriptions;
        let choices = choices();
        let busy = self.status.busy();
        let safe = choices["SAFE_MODES"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["value"] == json!(self.c.safe_mode))
            .unwrap()["label"]
            .as_str()
            .unwrap()
            .to_string();
        let pw = choices["PASSWORD_MODES"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["value"] == json!(self.c.password_mode))
            .unwrap()["label"]
            .as_str()
            .unwrap()
            .to_string();
        let color_row = row("Status Color")
            .child(
                div()
                    .id("custom-color")
                    .w(px(54.))
                    .h(px(26.))
                    .flex_shrink_0()
                    .rounded(px(13.))
                    .border_2()
                    .border_color(rgba(0xffffff40))
                    .bg(color(&self.c.color))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.status.busy() {
                            this.color_editor = Some(text_input(
                                this.c.color.clone(),
                                "#rrggbb",
                                false,
                                window,
                                cx,
                            ));
                            cx.notify();
                        }
                    })),
            )
            .children(
                choices["STATUS_COLORS"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        let col = v.as_str().unwrap().to_string();
                        let chosen = self.c.color == col;
                        div()
                            .id(SharedString::from(col.clone()))
                            .size(px(24.))
                            .flex_shrink_0()
                            .rounded(px(6.))
                            .border_2()
                            .border_color(if chosen { rgba(0xffffffff) } else { rgba(0) })
                            .bg(color(&col))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.status.busy() {
                                    this.c.color = col.clone();
                                    cx.notify();
                                }
                            }))
                    }),
            )
            .child(small("Tag"))
            .child(self.popup(
                "tag",
                self.c.tag.clone(),
                menu(&choices["TAGS"], json!(self.c.tag)),
                cx,
            ));
        let mut ssl = row("SSL mode").child(self.popup(
            "ssl",
            json!(self.c.ssl_mode).as_str().unwrap().into(),
            menu(&choices["SSL_MODES"], json!(self.c.ssl_mode)),
            cx,
        ));
        if self.c.driver == Driver::Postgres {
            ssl = ssl.child(small("Negotiation")).child(
                self.popup(
                    "neg",
                    json!(self.c.negotiation).as_str().unwrap().into(),
                    menu(&choices["NEGOTIATIONS"], json!(self.c.negotiation)),
                    cx,
                )
                .flex_none()
                .w(px(150.)),
            );
        }
        let mut keys = row("SSL keys");
        for (i, path) in [&self.c.ssl_key, &self.c.ssl_cert, &self.c.ssl_ca]
            .iter()
            .enumerate()
        {
            let label = path
                .as_deref()
                .and_then(|s| std::path::Path::new(s).file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| ["Key...", "Cert...", "CA Cert..."][i].into());
            keys = keys.child(
                button(
                    ("key", i),
                    label,
                    false,
                    busy || self.c.ssl_mode == SslMode::Disabled,
                )
                .flex_1()
                .min_w_0()
                .px(px(8.))
                .text_size(px(13.))
                .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                .on_click(cx.listener(move |this, _, _, cx| this.pick(i, cx))),
            );
        }
        keys = keys.child(
            button("clear-keys", "", false, busy)
                .w(px(30.))
                .h(px(26.))
                .px_0()
                .child(icon("Minus.svg", 14., 0xb8b8b8))
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.status.busy() {
                        this.c.ssl_key = None;
                        this.c.ssl_cert = None;
                        this.c.ssl_ca = None;
                        cx.notify();
                    }
                })),
        );
        let (status, col) = match &self.status {
            FormStatus::Idle => ("", 0xb8b8b8),
            FormStatus::Busy(s) => (s.as_str(), 0xb8b8b8),
            FormStatus::Ok(s) => (s.as_str(), 0x3fb950),
            FormStatus::Error(s) => (s.as_str(), 0xff8a80),
        };
        let mut root = div()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.color_editor = None;
                    cx.notify();
                }
            }))
            .relative()
            .size_full()
            .bg(rgb(telecat_theme::BG))
            .font(ui_font())
            .text_color(rgb(telecat_theme::TEXT))
            .text_size(px(13.))
            .line_height(px(16.))
            .px(px(18.))
            .pb(px(16.))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(44.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .child(if self.c.driver == Driver::Mysql {
                        "MySQL Connection"
                    } else {
                        "PostgreSQL Connection"
                    }),
            )
            .child(
                div()
                    .opacity(if busy { 0.6 } else { 1. })
                    .child(
                        card()
                            .child(row("Name").child(self.field("name", window, cx)))
                            .child(color_row)
                            .child(row("Safe Mode").child(self.popup(
                                "safe",
                                safe,
                                menu(&choices["SAFE_MODES"], json!(self.c.safe_mode)),
                                cx,
                            ))),
                    )
                    .child(
                        card()
                            .child(
                                row("Host/Socket")
                                    .child(self.field("host", window, cx))
                                    .child(small("Port"))
                                    .child(self.field("port", window, cx).flex_none().w(px(96.))),
                            )
                            .child(row("User").child(self.field("user", window, cx)))
                            .child(
                                row("Password")
                                    .child(self.field("password", window, cx))
                                    .child(
                                        self.popup(
                                            "password",
                                            pw,
                                            menu(
                                                &choices["PASSWORD_MODES"],
                                                json!(self.c.password_mode),
                                            ),
                                            cx,
                                        )
                                        .flex_none()
                                        .w(px(170.)),
                                    ),
                            )
                            .child(row("Database").child(self.field("database", window, cx)))
                            .child(ssl)
                            .child(keys),
                    ),
            )
            .child(
                div()
                    .id("form-status")
                    .overflow_y_scroll()
                    .min_h(px(20.))
                    .max_h(px(52.))
                    .px(px(4.))
                    .pb(px(8.))
                    .text_size(px(12.5))
                    .text_color(rgb(col))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(busy, |d| d.child(icon("spinner.svg", 12., 0xb8b8b8)))
                    .child(status.to_owned()),
            )
            .child(div().flex_1())
            .child(
                div().flex().gap(px(10.)).justify_end().children(
                    [("save", "Save"), ("test", "Test"), ("connect", "Connect")]
                        .into_iter()
                        .map(|(id, label)| {
                            button(id, label, id == "connect", busy)
                                .when(id == "test" && self.static_test_hover, |d| {
                                    d.bg(rgb(telecat_theme::BORDER))
                                })
                                .min_w(px(100.))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.submit(id, window, cx)
                                }))
                        }),
                ),
            );
        if let Some(input) = self.color_editor.clone() {
            root = root.child(
                backdrop().child(
                    sheet(340., "Custom color")
                        .child(input_box(&input, false, false))
                        .child(
                            div()
                                .mt(px(16.))
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(button("color-cancel", "Cancel", false, false).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.color_editor = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(button("color-save", "Save", true, false).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        let value = input.read(cx).value().to_string();
                                        if regex::Regex::new("^#[0-9a-fA-F]{6}$")
                                            .unwrap()
                                            .is_match(&value)
                                        {
                                            this.c.color = value;
                                            this.color_editor = None;
                                            cx.notify();
                                        }
                                    }),
                                )),
                        ),
                ),
            );
        }
        root
    }
}
