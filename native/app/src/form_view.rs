use crate::ui::InputEvent;
use crate::{Services, native_menu, ui::*};
use gpui_kit::{prelude::*, *};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use telecat_app::{
    connect::{host_key_problem, host_key_prompt, open_session},
    credentials::{Typed, connect_error_message},
    form::{self, FormStatus},
    store::Secrets,
};
use telecat_core::types::*;
use telecat_db::ssh::{HostKeyKind, HostKeyProblem};
use telecat_editor::{EditorOptions, SqlEditor};

/// The form's window: its height fits the form (with its SSH section when that is on); the cards
/// scroll when the screen is shorter.
pub const WIDTH: f32 = 520.;
pub const HEIGHT: f32 = 740.;
const SSH_HEIGHT: f32 = 170.;
pub fn height(c: &ConnectionConfig) -> f32 {
    HEIGHT + if c.ssh.enabled { SSH_HEIGHT } else { 0. }
}

/// The secrets a form starts with: saved ones when editing, a URL's when importing.
#[derive(Clone, Default)]
pub struct FormSecrets {
    pub password: String,
    pub ssh_password: String,
    pub passphrase: String,
}

pub struct ConnectionForm {
    services: Services,
    c: ConnectionConfig,
    fields: BTreeMap<&'static str, Entity<InputState>>,
    status: FormStatus,
    subscriptions: Vec<Subscription>,
    static_focus: bool,
    static_test_hover: bool,
    color_editor: Option<Entity<InputState>>,
    /// Why the typed custom colour can't be used.
    color_error: Option<String>,
    /// Field values kept while a password mode that doesn't use them is chosen.
    password: String,
    ssh_password: String,
    /// The startup SQL being edited.
    startup: Option<Entity<SqlEditor>>,
    /// A host key to decide on, and the action to repeat once it is trusted.
    host_key: Option<(HostKeyProblem, &'static str)>,
    scroll: ScrollHandle,
}
impl ConnectionForm {
    pub fn new(
        services: Services,
        c: ConnectionConfig,
        secrets: FormSecrets,
        scenario: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut fields = BTreeMap::new();
        let mut subscriptions = vec![];
        let command_or = |mode: PasswordMode, command: &Option<String>, value: &str| {
            if mode == PasswordMode::Command {
                command.clone().unwrap_or_default()
            } else {
                value.to_owned()
            }
        };
        for (key, value, placeholder, masked) in [
            ("name", c.name.clone(), "Connection name", false),
            ("host", c.host.clone(), "127.0.0.1", false),
            (
                "port",
                c.port.to_string(),
                if c.driver == Driver::Mysql {
                    "3306"
                } else {
                    "5432"
                },
                false,
            ),
            ("user", c.user.clone(), "user name", false),
            (
                "password",
                command_or(c.password_mode, &c.password_command, &secrets.password),
                form::password_placeholder(c.password_mode),
                c.password_mode != PasswordMode::Command,
            ),
            ("database", c.database.clone(), "database name", false),
            (
                "ssh_host",
                c.ssh.host.clone(),
                "server, or a Host from ~/.ssh/config",
                false,
            ),
            (
                "ssh_port",
                if c.ssh.port == 0 {
                    String::new()
                } else {
                    c.ssh.port.to_string()
                },
                "22",
                false,
            ),
            (
                "ssh_user",
                c.ssh.user.clone(),
                "from ~/.ssh/config, or your user name",
                false,
            ),
            (
                "ssh_password",
                command_or(
                    c.ssh.password_mode,
                    &c.ssh.password_command,
                    &secrets.ssh_password,
                ),
                form::ssh_password_placeholder(c.ssh.password_mode),
                c.ssh.password_mode != PasswordMode::Command,
            ),
            (
                "passphrase",
                secrets.passphrase.clone(),
                form::passphrase_placeholder(c.ssh.passphrase_mode),
                true,
            ),
        ] {
            let input = text_input(value, placeholder, masked, window, cx);
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit("connect", window, cx),
                    InputEvent::Change if key == "port" || key == "ssh_port" => {
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
        if scenario.is_empty() {
            fields["name"].update(cx, |s, cx| s.focus(window, cx));
        }
        let mut form = Self {
            services,
            c,
            fields,
            status,
            subscriptions,
            static_focus: scenario == "30-form-pg",
            static_test_hover: scenario == "03-form-mysql",
            color_editor: None,
            color_error: None,
            password: secrets.password,
            ssh_password: secrets.ssh_password,
            startup: None,
            host_key: None,
            scroll: ScrollHandle::new(),
        };
        form.sync_disabled(cx);
        form
    }
    #[cfg(test)]
    pub(crate) fn connection(&self) -> &ConnectionConfig {
        &self.c
    }
    #[cfg(test)]
    pub(crate) fn field_value(&self, key: &str, cx: &App) -> String {
        self.value(key, cx)
    }
    fn value(&self, key: &str, cx: &App) -> String {
        self.fields[key].read(cx).value().to_string()
    }
    fn disabled(&self, key: &str) -> bool {
        self.status.busy()
            || match key {
                "password" => self.c.password_mode == PasswordMode::None,
                "ssh_password" => self.c.ssh.password_mode == PasswordMode::None,
                "passphrase" => !self.c.ssh.use_key,
                _ => false,
            }
    }
    fn sync_disabled(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<&'static str> = self.fields.keys().copied().collect();
        for key in keys {
            let disabled = self.disabled(key);
            self.fields[key].update(cx, |s, cx| s.set_disabled(disabled, cx));
        }
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
        c.ssh.host = self.value("ssh_host", cx);
        c.ssh.user = self.value("ssh_user", cx);
        if c.ssh.password_mode == PasswordMode::Command {
            c.ssh.password_command = Some(self.value("ssh_password", cx));
        }
        let c = form::validate(&c, &self.value("port", cx))?;
        form::validate_ssh(&c, &self.value("ssh_port", cx))
    }
    /// What the fields hold for the secrets, for Test and Connect (as "Ask every time" answers
    /// too) and for saving to the Keychain.
    fn typed(&self, cx: &App) -> Typed {
        let typed = |mode: PasswordMode, key: &str| {
            matches!(mode, PasswordMode::Keychain | PasswordMode::Ask).then(|| self.value(key, cx))
        };
        Typed {
            password: (self.c.password_mode != PasswordMode::Command)
                .then(|| self.value("password", cx)),
            ssh_password: typed(self.c.ssh.password_mode, "ssh_password"),
            passphrase: self.c.ssh.use_key.then(|| self.value("passphrase", cx)),
        }
    }
    fn status(&mut self, status: FormStatus, cx: &mut Context<Self>) {
        self.status = status;
        self.sync_disabled(cx);
        cx.notify();
    }
    fn submit(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.busy() || self.startup.is_some() || self.host_key.is_some() {
            return;
        }
        let config = match self.config(cx) {
            Ok(c) => c,
            Err(e) => {
                self.status(FormStatus::Error(e.to_string()), cx);
                return;
            }
        };
        let typed = self.typed(cx);
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
                    let typed = typed.clone();
                    let unsaved = config.clone();
                    let saved = services
                        .runtime
                        .spawn_blocking(move || {
                            profile.save_connection_with(
                                config,
                                Secrets {
                                    password: typed.password.as_deref(),
                                    ssh_password: typed.ssh_password.as_deref(),
                                    passphrase: typed.passphrase.as_deref(),
                                },
                            )
                        })
                        .await;
                    match saved {
                        Ok(Ok(c)) => c,
                        Ok(Err(e)) => return Err((unsaved, e)),
                        Err(e) => return Err((unsaved, anyhow::Error::from(e))),
                    }
                } else {
                    config
                };
                if action == "save" {
                    return Ok::<_, (ConnectionConfig, anyhow::Error)>((config, None));
                }
                // A scenario never performs networking, including the deliberately unreachable reference host.
                if services.scenario {
                    return Err((
                        config,
                        anyhow::anyhow!("Scenario fixtures do not open database connections"),
                    ));
                }
                let session = open_session(
                    &config,
                    &typed,
                    services.profile.keychain.as_ref(),
                    &services.profile.ssh_env,
                )
                .await;
                match session {
                    Ok(session) => {
                        let session = Arc::new(session);
                        if action == "test" {
                            session.close().await;
                        }
                        Ok((config, Some(session)))
                    }
                    Err(e) => Err((config, e)),
                }
            }
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err((c, e))) => Err((Some(c), e)),
                Err(e) => Err((None, anyhow::Error::from(e))),
            };
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
                                        "Connection is OK — {} · {}{}",
                                        db.server_version,
                                        db.tls.as_deref().unwrap_or("no TLS"),
                                        if this.c.ssh.enabled {
                                            " · over SSH"
                                        } else {
                                            ""
                                        }
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
                        Err((c, e)) => {
                            // A connection saved before the failure keeps its new id.
                            if let Some(c) = c {
                                this.c.id = c.id;
                            }
                            if let Some(problem) = host_key_problem(&e) {
                                this.host_key = Some((problem, action));
                                this.status(FormStatus::Idle, cx);
                            } else {
                                let message = connect_error_message(&this.c, &e);
                                this.status(FormStatus::Error(message), cx);
                            }
                        }
                    }
                });
            });
        })
        .detach();
    }
    fn trust_host_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((problem, action)) = self.host_key.take() else {
            return;
        };
        match telecat_db::ssh::trust_host_key(&self.services.profile.ssh_env, &problem) {
            Ok(()) => self.submit(action, window, cx),
            Err(e) => self.status(FormStatus::Error(format!("SSH: {e}")), cx),
        }
    }
    fn cancel_host_key(&mut self, cx: &mut Context<Self>) {
        if let Some((problem, _)) = self.host_key.take() {
            let message = format!("SSH: {}", problem.summary());
            self.status(FormStatus::Error(message), cx);
        }
    }
    fn field(&self, key: &'static str, window: &Window, cx: &App) -> Div {
        let focus = (key == "name" && self.static_focus)
            || self.fields[key]
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
        let d = input_box(&self.fields[key], self.disabled(key), focus);
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
            "password" | "ssh_password" => {
                if let Ok(mode) = serde_json::from_value(v) {
                    self.password_mode(key, mode, window, cx);
                }
            }
            "passphrase" => {
                if let Ok(mode) = serde_json::from_value::<PassphraseMode>(v) {
                    self.c.ssh.passphrase_mode = mode;
                    self.fields["passphrase"].update(cx, |s, cx| {
                        s.set_placeholder(form::passphrase_placeholder(mode), window, cx);
                    });
                }
            }
            _ => {}
        }
        cx.notify();
    }
    /// Switches a password field's mode: the field shows the command for Command Line and the
    /// password otherwise, each kept while the other is shown.
    fn password_mode(
        &mut self,
        key: &'static str,
        mode: PasswordMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let typed = self.value(key, cx);
        let ssh = key == "ssh_password";
        let (current, command, stash) = if ssh {
            (
                &mut self.c.ssh.password_mode,
                &mut self.c.ssh.password_command,
                &mut self.ssh_password,
            )
        } else {
            (
                &mut self.c.password_mode,
                &mut self.c.password_command,
                &mut self.password,
            )
        };
        match *current {
            PasswordMode::Command => *command = Some(typed),
            PasswordMode::None => {}
            _ => *stash = typed,
        }
        *current = mode;
        let value = match mode {
            PasswordMode::None => String::new(),
            PasswordMode::Command => command.clone().unwrap_or_default(),
            _ => stash.clone(),
        };
        let placeholder = if ssh {
            form::ssh_password_placeholder(mode)
        } else {
            form::password_placeholder(mode)
        };
        self.fields[key].update(cx, |s, cx| {
            s.set_masked(mode != PasswordMode::Command, window, cx);
            s.set_placeholder(placeholder, window, cx);
            s.set_value(value, window, cx);
        });
        self.sync_disabled(cx);
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
            .text_size(px(12.5));
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
    /// Picks a file: an SSL key (0), certificate (1), CA certificate (2) or the SSH key (3).
    fn pick(&mut self, key: usize, cx: &mut Context<Self>) {
        if self.status.busy() || (key < 3 && self.c.ssl_mode == SslMode::Disabled) {
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
                    "Select SSH private key",
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
                            2 => this.c.ssl_ca = Some(path),
                            _ => {
                                this.c.ssh.key_path = Some(path);
                                this.c.ssh.use_key = true;
                                this.sync_disabled(cx);
                            }
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }
    fn toggle(&mut self, what: &'static str, cx: &mut Context<Self>) {
        if self.status.busy() {
            return;
        }
        match what {
            "ssh" => {
                self.c.ssh.enabled = !self.c.ssh.enabled;
                if self.c.ssh.enabled {
                    // Show the section that just opened.
                    self.scroll.scroll_to_bottom();
                }
            }
            "use_key" => self.c.ssh.use_key = !self.c.ssh.use_key,
            "channel_binding" => self.c.disable_channel_binding = !self.c.disable_channel_binding,
            "system_schemas" => self.c.load_system_schemas = !self.c.load_system_schemas,
            _ => {}
        }
        self.sync_disabled(cx);
        cx.notify();
    }
    fn edit_startup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.busy() {
            return;
        }
        let driver = self.c.driver;
        let text = self.c.startup_sql.clone();
        let editor = cx.new(|cx| {
            SqlEditor::new(
                EditorOptions {
                    driver,
                    text,
                    placeholder: if driver == Driver::Mysql {
                        "SET time_zone = '+07:00';".into()
                    } else {
                        "SET search_path TO app, public;".into()
                    },
                    ..EditorOptions::default()
                },
                window,
                cx,
            )
        });
        editor.update(cx, |e, cx| e.focus(window, cx));
        self.startup = Some(editor);
        cx.notify();
    }
    fn save_startup(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.startup else {
            return;
        };
        let text = editor.read(cx).text(cx).to_string();
        if telecat_core::startup::startup_statements(&text, self.c.driver).is_ok() {
            self.c.startup_sql = text;
            self.startup = None;
        }
        cx.notify();
    }
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
                .text_size(px(12.5))
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
        .text_size(px(12.5))
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
fn label_of(list: &Value, value: Value) -> String {
    list.as_array()
        .unwrap()
        .iter()
        .find(|v| v["value"] == value)
        .and_then(|v| v["label"].as_str())
        .unwrap_or_default()
        .to_string()
}
fn passphrase_modes() -> Value {
    json!([
        {"value": "keychain", "label": "Store in keychain"},
        {"value": "ask", "label": "Ask every time"}
    ])
}
impl ConnectionForm {
    fn file_button(
        &self,
        id: (&'static str, usize),
        path: &Option<String>,
        empty: &'static str,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = path
            .as_deref()
            .and_then(|s| std::path::Path::new(s).file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| empty.into());
        let which = id.1;
        button(id, label, false, disabled)
            .flex_1()
            .min_w_0()
            .px(px(8.))
            .text_size(px(12.))
            .text_color(rgb(telecat_theme::TEXT_SECONDARY))
            .when_some(path.clone(), |d, p| d.tooltip(text_tooltip(p)))
            .on_click(cx.listener(move |this, _, _, cx| this.pick(which, cx)))
    }
    fn ssl_rows(&self, choices: &Value, cx: &mut Context<Self>) -> (Div, Div) {
        let busy = self.status.busy();
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
        let ssl_off = busy || self.c.ssl_mode == SslMode::Disabled;
        for (i, (path, empty)) in [
            (&self.c.ssl_key, "Key..."),
            (&self.c.ssl_cert, "Cert..."),
            (&self.c.ssl_ca, "CA Cert..."),
        ]
        .into_iter()
        .enumerate()
        {
            keys = keys.child(self.file_button(("key", i), path, empty, ssl_off, cx));
        }
        keys = keys.child(
            button("clear-keys", "", false, busy)
                .w(px(30.))
                .h(px(26.))
                .px_0()
                .child(icon("Minus.svg", 14., 0xb8b8b8))
                .tooltip(text_tooltip("Clear SSL keys"))
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.status.busy() {
                        this.c.ssl_key = None;
                        this.c.ssl_cert = None;
                        this.c.ssl_ca = None;
                        cx.notify();
                    }
                })),
        );
        (ssl, keys)
    }
    fn options_rows(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let busy = self.status.busy();
        let mut rows = vec![];
        if self.c.driver == Driver::Postgres {
            rows.push(
                row("Options")
                    .gap(px(18.))
                    .child(
                        checkbox(
                            "channel-binding",
                            "Disable channel binding",
                            self.c.disable_channel_binding,
                            busy,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle("channel_binding", cx))),
                    )
                    .child(
                        checkbox(
                            "system-schemas",
                            "Load system schemas",
                            self.c.load_system_schemas,
                            busy,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle("system_schemas", cx))),
                    ),
            );
        }
        rows.push(
            row("Startup SQL")
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                        .child(form::startup_summary(&self.c)),
                )
                .child(
                    button("startup-edit", "Edit…", false, busy)
                        .min_w(px(80.))
                        .tooltip(text_tooltip(
                            "SQL run on every new database connection, right after it opens",
                        ))
                        .on_click(cx.listener(|this, _, window, cx| this.edit_startup(window, cx))),
                ),
        );
        rows
    }
    fn ssh_card(&self, choices: &Value, window: &Window, cx: &mut Context<Self>) -> Div {
        let busy = self.status.busy();
        let s = &self.c.ssh;
        let mut card = card().child(
            row("").child(
                checkbox("over-ssh", "Over SSH", s.enabled, busy)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle("ssh", cx))),
            ),
        );
        if !s.enabled {
            return card;
        }
        let ssh_password_label = label_of(&choices["PASSWORD_MODES"], json!(s.password_mode));
        card = card
            .child(
                row("Server")
                    .child(self.field("ssh_host", window, cx))
                    .child(small("Port"))
                    .child(self.field("ssh_port", window, cx).flex_none().w(px(96.))),
            )
            .child(row("User").child(self.field("ssh_user", window, cx)))
            .child(
                row("Password")
                    .child(self.field("ssh_password", window, cx))
                    .child(
                        self.popup(
                            "ssh_password",
                            ssh_password_label,
                            menu(&choices["PASSWORD_MODES"], json!(s.password_mode)),
                            cx,
                        )
                        .flex_none()
                        .w(px(170.)),
                    ),
            )
            .child(
                row("Private key")
                    .child(
                        checkbox("use-key", "Use SSH key", s.use_key, busy)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle("use_key", cx))),
                    )
                    .child(self.file_button(
                        ("ssh-key", 3),
                        &s.key_path,
                        "Import a private key…",
                        busy,
                        cx,
                    ))
                    .child(
                        button("clear-ssh-key", "", false, busy || s.key_path.is_none())
                            .w(px(30.))
                            .h(px(26.))
                            .px_0()
                            .child(icon("Minus.svg", 14., 0xb8b8b8))
                            .tooltip(text_tooltip("Clear SSH private key"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.status.busy() {
                                    this.c.ssh.key_path = None;
                                    this.c.ssh.use_key = false;
                                    this.sync_disabled(cx);
                                    cx.notify();
                                }
                            })),
                    ),
            );
        if s.use_key {
            card = card.child(
                row("Passphrase")
                    .child(self.field("passphrase", window, cx))
                    .child(
                        self.popup(
                            "passphrase",
                            label_of(&passphrase_modes(), json!(s.passphrase_mode)),
                            menu(&passphrase_modes(), json!(s.passphrase_mode)),
                            cx,
                        )
                        .flex_none()
                        .w(px(170.)),
                    ),
            );
        }
        card.child(
            div()
                .ml(px(92.))
                .mb(px(8.))
                .text_size(px(11.))
                .line_height(px(14.))
                .text_color(rgb(telecat_theme::MUTED))
                .child(
                    "Host and Port above are the database as the SSH server sees it. With no password or key, ~/.ssh/config and ssh-agent are used.",
                ),
        )
    }
    fn startup_sheet(&self, editor: Entity<SqlEditor>, cx: &mut Context<Self>) -> Div {
        let text = editor.read(cx).text(cx).to_string();
        let problem = telecat_core::startup::startup_statements(&text, self.c.driver)
            .err()
            .map(|e| e.to_string());
        let ok = problem.is_none();
        backdrop().child(
            sheet(480., "Startup SQL")
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                        .mb(px(10.))
                        .child("Runs on every new database connection right after it opens, reconnects included. Only session settings are allowed: SET, RESET or a read-only SELECT. An error stops the connection."),
                )
                .child(
                    div()
                        .h(px(160.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(rgb(if ok { 0x383838 } else { 0xff8a80 }))
                        .bg(rgb(telecat_theme::INPUT))
                        .overflow_hidden()
                        .child(editor),
                )
                .when_some(problem, |d, e| {
                    d.child(
                        div()
                            .id("startup-error")
                            .mt(px(6.))
                            .text_size(px(11.5))
                            .text_color(rgb(0xff8a80))
                            .child(e),
                    )
                })
                .child(
                    div()
                        .mt(px(16.))
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(button("startup-cancel", "Cancel", false, false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.startup = None;
                                cx.notify();
                            }),
                        ))
                        .child(
                            button("startup-save", "Done", true, !ok).on_click(
                                cx.listener(|this, _, _, cx| this.save_startup(cx)),
                            ),
                        ),
                ),
        )
    }
    fn host_key_sheet(&self, problem: &HostKeyProblem, cx: &mut Context<Self>) -> Div {
        let prompt = host_key_prompt(problem);
        let mut buttons = div().mt(px(16.)).flex().justify_end().gap(px(8.));
        if let Some(trust) = prompt.trust {
            let changed = problem.kind == HostKeyKind::Changed;
            buttons = buttons.child(
                button("host-key-trust", trust, !changed, false)
                    .when(changed, |d| d.text_color(rgb(0xff8a80)))
                    .on_click(cx.listener(|this, _, window, cx| this.trust_host_key(window, cx))),
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
            .on_click(cx.listener(|this, _, _, cx| this.cancel_host_key(cx))),
        );
        // Cancel is the default for a changed key: listed last, it is the primary button.
        backdrop().child(host_key_sheet(&prompt).child(buttons))
    }
}
impl Render for ConnectionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = &self.subscriptions;
        let choices = choices();
        let busy = self.status.busy();
        let safe = label_of(&choices["SAFE_MODES"], json!(self.c.safe_mode));
        let pw = label_of(&choices["PASSWORD_MODES"], json!(self.c.password_mode));
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
                            this.color_error = None;
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
        let (ssl, keys) = self.ssl_rows(&choices, cx);
        let options = self.options_rows(cx);
        let ssh = self.ssh_card(&choices, window, cx);
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
                    this.startup = None;
                    this.cancel_host_key(cx);
                    cx.notify();
                }
            }))
            .relative()
            .size_full()
            .bg(rgb(telecat_theme::BG))
            .font(ui_font())
            .text_color(rgb(telecat_theme::TEXT))
            .text_size(px(12.))
            .line_height(px(14.5))
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
                    .text_size(px(13.5))
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
                    .id("form-cards")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
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
                            .child(keys)
                            .children(options),
                    )
                    .child(ssh),
            )
            .child(
                div()
                    .id("form-status")
                    .flex_shrink_0()
                    .overflow_y_scroll()
                    .min_h(px(20.))
                    .max_h(px(52.))
                    .px(px(4.))
                    .pt(px(4.))
                    .pb(px(8.))
                    .text_size(px(11.5))
                    .text_color(rgb(col))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(busy, |d| d.child(icon("spinner.svg", 12., 0xb8b8b8)))
                    .child(status.to_owned()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .gap(px(10.))
                    .justify_end()
                    .children(
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
        if let Some(editor) = self.startup.clone() {
            root = root.child(self.startup_sheet(editor, cx));
        }
        if let Some((problem, _)) = self.host_key.clone() {
            root = root.child(self.host_key_sheet(&problem, cx));
        }
        if let Some(input) = self.color_editor.clone() {
            root = root.child(
                backdrop().child(
                    sheet(340., "Custom color")
                        .child(
                            input_box(&input, false, false).when(self.color_error.is_some(), |d| {
                                d.border_color(rgb(0xff8a80))
                            }),
                        )
                        .when_some(self.color_error.clone(), |d, e| {
                            d.child(
                                div()
                                    .id("color-error")
                                    .mt(px(6.))
                                    .text_size(px(11.5))
                                    .text_color(rgb(0xff8a80))
                                    .child(e),
                            )
                        })
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
                                        match telecat_app::form::parse_hex_color(&value) {
                                            Ok(color) => {
                                                this.c.color = color;
                                                this.color_editor = None;
                                                this.color_error = None;
                                            }
                                            Err(e) => this.color_error = Some(e.to_string()),
                                        }
                                        cx.notify();
                                    }),
                                )),
                        ),
                ),
            );
        }
        root
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it brings gpui's `test` macro in place of the built-in one.
    use super::{ConnectionForm, FormSecrets, FormStatus, Services, form};
    use gpui::{AnyWindowHandle, TestAppContext};
    use gpui_kit::{AppContext, Entity};
    use std::sync::Arc;
    use telecat_app::credentials::{Keychain, MemoryKeychain, NATIVE_SERVICE};
    use telecat_core::types::*;

    fn open(
        cx: &mut TestAppContext,
        c: ConnectionConfig,
    ) -> (
        AnyWindowHandle,
        Entity<ConnectionForm>,
        Services,
        Arc<MemoryKeychain>,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let root = std::env::temp_dir()
                .join("telecat-ui-profiles")
                .join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&root).unwrap();
            let keychain = Arc::new(MemoryKeychain::default());
            let mut profile = telecat_app::store::Profile::new(root.clone(), keychain.clone());
            profile.ssh_env = telecat_db::ssh::SshEnv::isolated(&root);
            let services = Services {
                profile: Arc::new(profile),
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
                        connections: vec![],
                        groups: vec![],
                    },
                }),
                // Saving works; connecting never opens a database.
                scenario: true,
            };
            let form_services = services.clone();
            let (w, form) = gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| {
                    ConnectionForm::new(form_services, c, FormSecrets::default(), "", window, cx)
                })
            })
            .unwrap();
            (w, form, services, keychain)
        })
    }
    fn paint(cx: &mut TestAppContext, w: AnyWindowHandle) {
        cx.update_window(w, |_, w, cx| {
            w.draw(cx).clear(cx);
        })
        .unwrap();
    }
    fn set(
        cx: &mut TestAppContext,
        w: AnyWindowHandle,
        f: &Entity<ConnectionForm>,
        key: &str,
        value: &str,
    ) {
        cx.update_window(w, |_, window, cx| {
            let input = f.read(cx).fields[key].clone();
            input.update(cx, |s, cx| s.set_value(value.to_owned(), window, cx));
        })
        .unwrap();
    }
    fn secret(k: &MemoryKeychain, account: &str) -> Option<String> {
        k.get(NATIVE_SERVICE, account)
            .unwrap()
            .map(|v| String::from_utf8(v).unwrap())
    }

    #[gpui::test]
    fn over_ssh_reveals_its_section_and_saves_its_settings_and_secrets(cx: &mut TestAppContext) {
        ssh_section(cx);
    }
    fn ssh_section(cx: &mut TestAppContext) {
        let (w, f, services, keychain) = open(cx, empty_connection(Driver::Postgres));
        paint(cx, w);
        set(cx, w, &f, "name", "Prod");
        set(cx, w, &f, "host", "10.0.0.5");
        // Over SSH without a server is refused before anything is saved.
        f.update(cx, |f, cx| f.toggle("ssh", cx));
        paint(cx, w);
        cx.update_window(w, |_, window, cx| {
            f.update(cx, |f, cx| f.submit("save", window, cx))
        })
        .unwrap();
        f.read_with(cx, |f, _| {
            assert!(matches!(&f.status, FormStatus::Error(e) if e.contains("SSH server")))
        });
        set(cx, w, &f, "ssh_host", "bastion");
        cx.update_window(w, |_, window, cx| {
            let input = f.read(cx).fields["ssh_port"].clone();
            input.update(cx, |s, cx| s.focus(window, cx));
        })
        .unwrap();
        cx.simulate_input(w, "22x00");
        set(cx, w, &f, "ssh_user", "ops");
        set(cx, w, &f, "ssh_password", "ssh s3cret");
        cx.run_until_parked();
        assert_eq!(
            f.read_with(cx, |f, cx| f.value("ssh_port", cx)),
            "2200",
            "digits only"
        );
        // Command Line shows the command and keeps the password for later.
        cx.update_window(w, |_, window, cx| {
            f.update(cx, |f, cx| {
                f.select("ssh_password", "command".into(), window, cx)
            })
        })
        .unwrap();
        assert_eq!(f.read_with(cx, |f, cx| f.value("ssh_password", cx)), "");
        cx.update_window(w, |_, window, cx| {
            f.update(cx, |f, cx| {
                f.select("ssh_password", "keychain".into(), window, cx)
            })
        })
        .unwrap();
        assert_eq!(
            f.read_with(cx, |f, cx| f.value("ssh_password", cx)),
            "ssh s3cret"
        );
        // A key with a passphrase; the passphrase field only works with "Use SSH key" on.
        assert!(f.read_with(cx, |f, _| f.disabled("passphrase")));
        f.update(cx, |f, cx| {
            f.c.ssh.key_path = Some("/keys/id_ed25519".into());
            f.toggle("use_key", cx);
        });
        assert!(!f.read_with(cx, |f, _| f.disabled("passphrase")));
        set(cx, w, &f, "passphrase", "k3y-s3cret");
        f.update(cx, |f, cx| f.toggle("channel_binding", cx));
        paint(cx, w);
        cx.update_window(w, |_, window, cx| {
            f.update(cx, |f, cx| f.submit("save", window, cx))
        })
        .unwrap();
        // Saved, the form closes.
        for _ in 0..300 {
            cx.executor().allow_parking();
            cx.run_until_parked();
            if !services.profile.snapshot().connections.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let saved = services.profile.snapshot().connections;
        assert_eq!(saved.len(), 1);
        let c = &saved[0];
        assert!(c.ssh.enabled && c.ssh.use_key && c.disable_channel_binding);
        assert_eq!(
            (c.ssh.host.as_str(), c.ssh.port, c.ssh.user.as_str()),
            ("bastion", 2200, "ops")
        );
        assert_eq!(c.ssh.key_path.as_deref(), Some("/keys/id_ed25519"));
        let id = c.id.clone();
        assert_eq!(
            secret(
                &keychain,
                &telecat_app::credentials::ssh_password_account(&id)
            )
            .as_deref(),
            Some("ssh s3cret")
        );
        assert_eq!(
            secret(
                &keychain,
                &telecat_app::credentials::ssh_passphrase_account(&id)
            )
            .as_deref(),
            Some("k3y-s3cret")
        );
        let json = std::fs::read_to_string(services.profile.root.join("connections.json")).unwrap();
        for secret in ["ssh s3cret", "k3y-s3cret"] {
            assert!(!json.contains(secret), "no secret in the profile file");
        }
    }

    #[gpui::test]
    fn startup_sql_is_edited_in_a_sheet_that_refuses_writes(cx: &mut TestAppContext) {
        startup_sheet(cx);
    }
    fn startup_sheet(cx: &mut TestAppContext) {
        let (w, f, _, _) = open(cx, empty_connection(Driver::Mysql));
        paint(cx, w);
        cx.update_window(w, |_, window, cx| {
            f.update(cx, |f, cx| f.edit_startup(window, cx))
        })
        .unwrap();
        paint(cx, w);
        let editor = f
            .read_with(cx, |f, _| f.startup.clone())
            .expect("the sheet is open");
        cx.update_window(w, |_, window, cx| {
            editor.update(cx, |e, cx| e.set_text("DELETE FROM orders", window, cx))
        })
        .unwrap();
        paint(cx, w);
        f.update(cx, |f, cx| f.save_startup(cx));
        assert!(
            f.read_with(cx, |f, _| f.startup.is_some()),
            "a write can't be kept"
        );
        cx.update_window(w, |_, window, cx| {
            editor.update(cx, |e, cx| {
                e.set_text("SET time_zone = '+07:00';\nSET NAMES utf8mb4;", window, cx)
            })
        })
        .unwrap();
        f.update(cx, |f, cx| f.save_startup(cx));
        f.read_with(cx, |f, _| {
            assert!(f.startup.is_none());
            assert_eq!(
                f.c.startup_sql,
                "SET time_zone = '+07:00';\nSET NAMES utf8mb4;"
            );
            assert_eq!(form::startup_summary(&f.c), "2 statements");
        });
        // Escape closes the sheet without keeping its text.
        cx.update_window(w, |_, window, cx| {
            f.update(cx, |f, cx| f.edit_startup(window, cx))
        })
        .unwrap();
        paint(cx, w);
        cx.simulate_keystrokes(w, "escape");
        assert!(f.read_with(cx, |f, _| f.startup.is_none()));
    }
}
