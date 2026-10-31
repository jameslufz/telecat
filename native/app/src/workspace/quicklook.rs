//! Quick Look (⌘↩, middle-click, or the cell menu): a cell's value as JSON (a tree, or raw text
//! with PRETTY / MINIFY), text, an image or a hex dump. Edits are staged like any cell edit, so
//! ⌘S saves them and ⌘Z undoes them; "Open in Window" shows the same in its own window.
use super::components::GridKey;
use super::*;
use gpui_kit::base::input::InputEditorStyle;
use std::collections::BTreeSet;
use telecat_core::changes::Cell as Value;
use telecat_core::columns::{NOT_JSON, ValueKind, json_minify, json_pretty};
use telecat_core::preview::{ImageKind, Preview, detect, hex_dump};

/// Larger values are shown, not edited: a text field over megabytes would make every keystroke
/// slow.
const EDIT_LIMIT: usize = 1024 * 1024;
/// How much of a binary value the hex dump lists.
const HEX_BYTES: usize = 64 * 1024;
/// How many JSON nodes the tree draws.
const TREE_NODES: usize = 3000;

/// The cell a Quick Look edits: its tab, its row's identity and its data column.
#[derive(Clone, Debug)]
pub(super) struct Target {
    tab: u64,
    row: String,
    col: usize,
}
pub(super) enum QuickLookEvent {
    Close,
    OpenInWindow,
}
impl EventEmitter<QuickLookEvent> for QuickLook {}
pub(super) struct QuickLook {
    workspace: WeakEntity<Workspace>,
    target: Option<Target>,
    title: String,
    kind: ValueKind,
    binary: bool,
    /// The value as opened; `None` for NULL.
    original: Option<String>,
    /// The editor's text as opened: Apply with this text changes nothing.
    initial: String,
    preview: Preview,
    /// JSON shown as raw text rather than as a tree.
    raw: bool,
    editor: Entity<TextareaState>,
    collapsed: BTreeSet<String>,
    image: Option<Arc<Image>>,
    error: Option<String>,
    in_window: bool,
    focus: FocusHandle,
    _subscription: Subscription,
}
impl Focusable for QuickLook {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
pub(super) struct Opened {
    pub title: String,
    pub value: Option<String>,
    pub kind: ValueKind,
    pub binary: bool,
    pub editable: bool,
}
impl QuickLook {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        workspace: WeakEntity<Workspace>,
        target: Option<Target>,
        opened: Opened,
        driver: Driver,
        in_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value = opened.value.clone().unwrap_or_default();
        let preview = detect(driver, &value, opened.kind, opened.binary);
        let editable = opened.editable
            && value.len() <= EDIT_LIMIT
            && matches!(preview, Preview::Json | Preview::Text);
        let shown = if value.len() > EDIT_LIMIT {
            super::inspector::preview(&value).into_owned()
        } else if preview == Preview::Json {
            json_pretty(&value).unwrap_or_else(|_| value.clone())
        } else {
            value.clone()
        };
        let initial = shown.clone();
        let editor = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx)
                .soft_wrap(true)
                .placeholder(if opened.value.is_none() {
                    "NULL"
                } else {
                    "EMPTY"
                })
                .default_value(shown);
            s.set_editor_style(InputEditorStyle {
                foreground: rgb(telecat_theme::TEXT).into(),
                muted_foreground: rgb(0x666666).into(),
                background: rgb(telecat_theme::INPUT).into(),
                border: rgb(telecat_theme::BORDER).into(),
                selection: rgba(0x2f6fed66).into(),
                caret: rgb(telecat_theme::TEXT).into(),
                ..Default::default()
            });
            s.set_readonly(!editable, cx);
            s
        });
        let subscription = cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                cx.notify();
            }
        });
        let image = match &preview {
            Preview::Image(kind, bytes) => Some(Arc::new(Image::from_bytes(
                match kind {
                    ImageKind::Png => ImageFormat::Png,
                    ImageKind::Jpeg => ImageFormat::Jpeg,
                    ImageKind::Gif => ImageFormat::Gif,
                    ImageKind::Webp => ImageFormat::Webp,
                },
                bytes.clone(),
            ))),
            _ => None,
        };
        Self {
            workspace,
            target: editable.then_some(target).flatten(),
            title: opened.title,
            kind: opened.kind,
            binary: opened.binary,
            original: opened.value,
            initial,
            raw: false,
            preview,
            editor,
            collapsed: BTreeSet::new(),
            image,
            error: None,
            in_window,
            focus: cx.focus_handle(),
            _subscription: subscription,
        }
    }
    fn text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }
    fn reshape(&mut self, pretty: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        match if pretty {
            json_pretty(&text)
        } else {
            json_minify(&text)
        } {
            Ok(shaped) => self
                .editor
                .update(cx, |s, cx| s.set_value(shaped, window, cx)),
            Err(message) => self.error = Some(message.into()),
        }
        cx.notify();
    }
    /// Stages the edited value into its cell (found again by the row's identity), or says why
    /// it can't be.
    fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let text = self.text(cx);
        if text == self.initial {
            cx.emit(QuickLookEvent::Close);
            return;
        }
        if (self.kind == ValueKind::Json || self.preview == Preview::Json)
            && serde_json::from_str::<serde_json::Value>(&text).is_err()
        {
            self.error = Some(NOT_JSON.into());
            cx.notify();
            return;
        }
        let outcome = self
            .workspace
            .update(cx, |ws, cx| ws.stage_quick_look(&target, text, cx))
            .unwrap_or_else(|_| Err("The connection window was closed.".into()));
        match outcome {
            Ok(()) => cx.emit(QuickLookEvent::Close),
            Err(reason) => {
                self.error = Some(reason);
                cx.notify();
            }
        }
    }
    fn json_tree(&self, cx: &mut Context<Self>) -> Div {
        let text = self.text(cx);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return div().text_color(rgb(0xff6b6b)).child(NOT_JSON);
        };
        let mut rows: Vec<AnyElement> = vec![];
        self.tree_rows(&value, "ROOT", String::new(), 0, &mut rows, cx);
        div().flex().flex_col().children(rows)
    }
    fn tree_rows(
        &self,
        value: &serde_json::Value,
        key: &str,
        path: String,
        depth: usize,
        out: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        if out.len() >= TREE_NODES {
            return;
        }
        use serde_json::Value as J;
        let (badge, summary) = match value {
            J::Object(m) => ("{}", format!("{} keys", m.len())),
            J::Array(a) => ("[]", format!("{} items", a.len())),
            J::String(s) => ("Aa", format!("\"{s}\"")),
            J::Number(n) => ("#", n.to_string()),
            J::Bool(b) => ("☐", b.to_string()),
            J::Null => ("∅", "null".into()),
        };
        let container = matches!(value, J::Object(_) | J::Array(_));
        let open = container && !self.collapsed.contains(&path);
        let toggle_path = path.clone();
        out.push(
            div()
                .id(SharedString::from(format!("json-node-{path}")))
                .flex()
                .items_center()
                .gap(px(6.))
                .pl(px(depth as f32 * 16.))
                .h(px(20.))
                .child(
                    div()
                        .w(px(10.))
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(if !container {
                            ""
                        } else if open {
                            "▾"
                        } else {
                            "▸"
                        }),
                )
                .child(
                    div()
                        .px(px(4.))
                        .rounded(px(3.))
                        .bg(rgb(0x333333))
                        .text_size(px(10.))
                        .text_color(rgb(telecat_theme::TEXT_SECONDARY))
                        .child(badge),
                )
                .child(div().font_weight(FontWeight::MEDIUM).child(key.to_owned()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(if container {
                            summary
                        } else {
                            format!(": {summary}")
                        }),
                )
                .when(container, |d| {
                    d.cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.collapsed.remove(&toggle_path) {
                                this.collapsed.insert(toggle_path.clone());
                            }
                            cx.notify();
                        }))
                })
                .into_any_element(),
        );
        if !open {
            return;
        }
        match value {
            J::Object(m) => {
                for (k, v) in m {
                    self.tree_rows(v, k, format!("{path}/{k}"), depth + 1, out, cx);
                }
            }
            J::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    self.tree_rows(v, &i.to_string(), format!("{path}/{i}"), depth + 1, out, cx);
                }
            }
            _ => {}
        }
    }
}
impl Render for QuickLook {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.target.is_some();
        let json = self.preview == Preview::Json;
        let height = if self.in_window {
            (window.viewport_size().height.as_f32() - 110.).max(160.)
        } else {
            420.
        };
        let body: AnyElement = match &self.preview {
            Preview::Image(kind, bytes) => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .size_full()
                .when_some(self.image.clone(), |d, image| {
                    d.child(
                        img(image)
                            .max_w_full()
                            .max_h(px(height - 30.))
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(telecat_theme::MUTED))
                        .child(format!("{kind:?} image · {}", byte_size(bytes.len()))),
                )
                .into_any_element(),
            Preview::Binary(bytes) => div()
                .id("hex")
                .size_full()
                .overflow_y_scroll()
                .font(mono())
                .text_size(px(11.5))
                .line_height(px(15.))
                .child(format!(
                    "{} · read-only\n\n{}",
                    byte_size(bytes.len()),
                    hex_dump(bytes, HEX_BYTES)
                ))
                .into_any_element(),
            _ if json && !self.raw => div()
                .id("json-tree")
                .size_full()
                .overflow_y_scroll()
                .font(mono())
                .text_size(px(12.))
                .child(self.json_tree(cx))
                .into_any_element(),
            _ => div()
                .id("quicklook-text")
                .size_full()
                .overflow_y_scroll()
                .font(mono())
                .text_size(px(12.))
                .line_height(px(17.))
                .child(Textarea::new(&self.editor))
                .into_any_element(),
        };
        let segment = |id: &'static str, label: &'static str, on: bool, raw: bool| {
            div()
                .id(id)
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .when(on, |d| d.bg(rgb(telecat_theme::ACCENT)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.raw = raw;
                    cx.notify();
                }))
        };
        div()
            .id("quick-look")
            .track_focus(&self.focus)
            .key_context("QuickLook")
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" && this.in_window {
                    cx.emit(QuickLookEvent::Close);
                }
            }))
            .flex()
            .flex_col()
            .gap(px(10.))
            .when(self.in_window, |d| {
                d.size_full().p(px(14.)).bg(rgb(telecat_theme::CARD))
            })
            .text_color(rgb(telecat_theme::TEXT))
            .text_size(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .when(json, |d| {
                        d.child(
                            div()
                                .flex()
                                .p(px(2.))
                                .rounded(px(7.))
                                .bg(rgba(0xffffff10))
                                .child(segment("ql-tree", "Tree", !self.raw, false))
                                .child(segment("ql-raw", "Raw", self.raw, true)),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(height))
                    .p(px(8.))
                    .border_1()
                    .border_color(rgb(telecat_theme::BORDER))
                    .rounded(px(8.))
                    .bg(rgb(telecat_theme::INPUT))
                    .child(body),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_color(rgb(0xff6b6b)).child(e))
            })
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .when(json && self.raw && editable, |d| {
                        d.child(button("ql-pretty", "PRETTY", false, false).on_click(
                            cx.listener(|this, _, window, cx| this.reshape(true, window, cx)),
                        ))
                        .child(
                            button("ql-minify", "MINIFY", false, false).on_click(
                                cx.listener(|this, _, window, cx| this.reshape(false, window, cx)),
                            ),
                        )
                    })
                    .when(!self.in_window, |d| {
                        d.child(
                            button("ql-window", "Open in Window", false, false).on_click(
                                cx.listener(|_, _, _, cx| cx.emit(QuickLookEvent::OpenInWindow)),
                            ),
                        )
                    })
                    .child(
                        button("ql-copy", "Copy", false, false).on_click(cx.listener(
                            |this, _, _, cx| {
                                let text = this.original.clone().unwrap_or_default();
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            },
                        )),
                    )
                    .child(div().flex_1())
                    .when(!editable, |d| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(telecat_theme::MUTED))
                                .child("Read-only"),
                        )
                    })
                    .child(
                        button(
                            "ql-close",
                            if editable { "Cancel" } else { "Close" },
                            false,
                            false,
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(QuickLookEvent::Close))),
                    )
                    .when(editable, |d| {
                        d.child(
                            button("ql-apply", "Apply", true, false)
                                .tooltip(text_tooltip("Stage the change; ⌘S saves it"))
                                .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                        )
                    }),
            )
    }
}
fn byte_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} bytes")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.)
    } else {
        format!("{:.1} MB", n as f64 / 1024. / 1024.)
    }
}
impl Workspace {
    /// Opens Quick Look on row `row` (a staged row, or a result row) and data column `col` of
    /// grid `k`.
    pub(super) fn open_quick_look(
        &mut self,
        k: GridKey,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.state.tabs.iter().find(|t| t.id == k.0) else {
            return;
        };
        let (opened, target) = match (&tab.table, &tab.result_edit) {
            (Some(t), _) if t.part == telecat_app::table_model::Part::Data => {
                let Some(r) = t.data.rows.get(row) else {
                    return;
                };
                let name = t.data.result.columns.get(col).cloned().unwrap_or_default();
                let detail = t.columns.as_ref().and_then(|c| c.get(&name));
                let json = detail.is_some_and(|d| d.kind == ValueKind::Json);
                let value = if json {
                    super::data::json_source(tab, row, col)
                } else {
                    r.cur.get(col).and_then(|v| v.value().map(str::to_owned))
                };
                (
                    Opened {
                        title: name,
                        value,
                        kind: detail.map_or(ValueKind::Other, |d| d.kind),
                        binary: t
                            .data
                            .result
                            .binary
                            .as_ref()
                            .and_then(|b| b.get(col))
                            .copied()
                            .unwrap_or(false),
                        editable: !r.deleted && t.data_column_editable(col) && t.data.loaded,
                    },
                    Some(Target {
                        tab: tab.id,
                        row: r.key.clone(),
                        col,
                    }),
                )
            }
            (Some(t), _) => {
                let b = t.buffer();
                (
                    Opened {
                        title: b.result.columns.get(col).cloned().unwrap_or_default(),
                        value: b
                            .rows
                            .get(row)
                            .and_then(|r| r.cur.get(col))
                            .and_then(|v| v.value().map(str::to_owned)),
                        kind: ValueKind::Other,
                        binary: false,
                        editable: false,
                    },
                    None,
                )
            }
            (None, Some(re)) => {
                let Some(r) = re.data.rows.get(row) else {
                    return;
                };
                (
                    Opened {
                        title: re.data.result.columns.get(col).cloned().unwrap_or_default(),
                        value: super::data::json_source(tab, row, col),
                        kind: ValueKind::Other,
                        binary: re
                            .data
                            .result
                            .binary
                            .as_ref()
                            .and_then(|b| b.get(col))
                            .copied()
                            .unwrap_or(false),
                        editable: re.plan.cell_reason(&re.data.rows, row, col).is_none(),
                    },
                    Some(Target {
                        tab: tab.id,
                        row: r.key.clone(),
                        col,
                    }),
                )
            }
            (None, None) => (
                Opened {
                    title: tab
                        .grid
                        .result
                        .columns
                        .get(col)
                        .cloned()
                        .unwrap_or_default(),
                    value: tab
                        .grid
                        .result
                        .rows
                        .get(row)
                        .and_then(|r| r.get(col))
                        .cloned()
                        .flatten(),
                    kind: ValueKind::Other,
                    binary: tab
                        .grid
                        .result
                        .binary
                        .as_ref()
                        .and_then(|b| b.get(col))
                        .copied()
                        .unwrap_or(false),
                    editable: false,
                },
                None,
            ),
        };
        let driver = self.state.connection.driver;
        let weak = cx.entity().downgrade();
        let view =
            cx.new(|cx| QuickLook::new(weak, target.clone(), opened, driver, false, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, view, event: &QuickLookEvent, window, cx| match event {
                QuickLookEvent::Close => {
                    if matches!(&this.sheet, Some(Sheet::QuickLook { view: v, .. }) if v == view) {
                        this.cancel_sheet(cx);
                    }
                }
                QuickLookEvent::OpenInWindow => {
                    let (driver, view) = (this.state.connection.driver, view.clone());
                    this.cancel_sheet(cx);
                    open_window(cx.entity().downgrade(), view, driver, window, cx);
                }
            },
        );
        self.sheet = Some(Sheet::QuickLook {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }
    /// Stages a value edited in Quick Look: the grid's own staging path, by the row's identity.
    pub(super) fn stage_quick_look(
        &mut self,
        target: &Target,
        text: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.saving {
            return Err("A save is in progress; try again when it has finished.".into());
        }
        let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == target.tab) else {
            return Err("Its tab was closed.".into());
        };
        let rows = match (&tab.table, &tab.result_edit) {
            (Some(t), _) => &t.data.rows,
            (None, Some(r)) => &r.data.rows,
            _ => return Err("These results are read-only.".into()),
        };
        let Some(row) = rows.iter().position(|r| r.key == target.row) else {
            return Err(
                "The row is no longer shown (the table was reloaded); nothing was staged.".into(),
            );
        };
        super::data::stage_cells(tab, vec![(row, target.col, Value::Text(text))])?;
        cx.notify();
        Ok(())
    }
}
/// "Open in Window": the same value in a resizable window of its own.
fn open_window(
    workspace: WeakEntity<Workspace>,
    from: Entity<QuickLook>,
    driver: Driver,
    window: &mut Window,
    cx: &mut App,
) {
    let (target, opened) = {
        let q = from.read(cx);
        (
            q.target.clone(),
            Opened {
                title: q.title.clone(),
                value: q.original.clone(),
                kind: q.kind,
                binary: q.binary,
                editable: q.target.is_some(),
            },
        )
    };
    let edited = from.read(cx).text(cx);
    let title = format!("Quick Look — {}", opened.title);
    let size = size(px(720.), px(560.));
    let bounds = Bounds::centered(None, size, cx);
    let _ = window;
    let result = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            is_resizable: true,
            window_min_size: Some(gpui_kit::size(px(360.), px(260.))),
            ..Default::default()
        },
        cx,
        move |window, cx| {
            let view = cx.new(|cx| {
                let q = QuickLook::new(workspace, target, opened, driver, true, window, cx);
                q.editor.update(cx, |s, cx| s.set_value(edited, window, cx));
                q
            });
            let handle = window.window_handle();
            cx.subscribe(&view, move |_, event: &QuickLookEvent, cx| {
                if matches!(event, QuickLookEvent::Close) {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }
            })
            .detach();
            view.update(cx, |q, cx| q.focus.focus(window, cx));
            view
        },
    );
    if let Err(e) = result {
        log::warn!("Open Quick Look window: {e:#}");
    }
}
