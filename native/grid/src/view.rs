use crate::model::Modifiers as GridModifiers;
use crate::{model::*, theme::*};
use gpui_kit::base::input;
use gpui_kit::base::input::{InputEditorStyle, InputEvent, Textarea, TextareaState};
use gpui_kit::{prelude::*, *};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

gpui_kit::actions!(telecat_grid, [Copy, Paste, NextCell, PreviousCell]);
struct GridBindings;
impl Global for GridBindings {}
fn init(cx: &mut App) {
    telecat_theme::init(cx);
    if cx.has_global::<GridBindings>() {
        return;
    }
    cx.set_global(GridBindings);
    cx.bind_keys([
        KeyBinding::new("tab", NextCell, Some("TelecatGrid")),
        KeyBinding::new("shift-tab", PreviousCell, Some("TelecatGrid")),
    ]);
}

/// CPU time preparing and painting the grid, not GPU presentation latency.
#[derive(Default, Debug)]
pub struct FrameStats {
    pub frames: u64,
    pub last_cells: usize,
    pub max_cells: usize,
    samples: VecDeque<f64>,
}
impl FrameStats {
    pub fn milliseconds(&self) -> Vec<f64> {
        self.samples.iter().copied().collect()
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    fn record(&mut self, started: Instant, cells: usize) {
        self.frames += 1;
        self.last_cells = cells;
        self.max_cells = self.max_cells.max(cells);
        if self.samples.len() == 1200 {
            self.samples.pop_front();
        }
        self.samples
            .push_back(started.elapsed().as_secs_f64() * 1000.);
    }
}
#[derive(Clone, Copy)]
enum Drag {
    Column {
        col: usize,
        x: f32,
        width: f32,
    },
    Scroll {
        horizontal: bool,
        at: f32,
        offset: f32,
        scale: f32,
    },
    Editor {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverTarget {
    Header(usize),
    Cell(Cell),
}
struct Editor {
    cell: Cell,
    state: Entity<TextareaState>,
    _subscription: Subscription,
}
/// Entity view used by table Data, Structure/Index, and SQL results.
/// Subscribe to GridEvent; edits and sorts are requests and never write/mutate row data.
pub struct DataGrid {
    model: GridModel,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    drag: Option<Drag>,
    editor: Option<Editor>,
    editor_size: (f32, f32),
    hover_scrollbar: Option<bool>,
    hover_target: Option<HoverTarget>,
    tooltip: Option<(SharedString, f32, f32)>,
    tooltip_task: Option<Task<()>>,
    pub frame_stats: FrameStats,
}
impl EventEmitter<GridEvent> for DataGrid {}
impl Focusable for DataGrid {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl DataGrid {
    pub fn new(model: GridModel, cx: &mut Context<Self>) -> Self {
        init(cx);
        Self {
            model,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            drag: None,
            editor: None,
            editor_size: (320., 120.),
            hover_scrollbar: None,
            hover_target: None,
            tooltip: None,
            tooltip_task: None,
            frame_stats: FrameStats::default(),
        }
    }
    pub fn model(&self) -> &GridModel {
        &self.model
    }
    /// Flush the native text engine synchronously before the host takes a commit
    /// snapshot. The host applies these events immediately, without an event-loop race.
    pub fn flush_edit(&mut self, cx: &mut Context<Self>) -> Vec<GridEvent> {
        if let (Some(editor), Some(edit)) = (&self.editor, &mut self.model.editing) {
            edit.value = editor.state.read(cx).value().to_string();
        }
        let events = self.model.finish_edit(true, 0);
        self.editor = None;
        cx.notify();
        events
    }
    pub fn editor_state(&self) -> Option<Entity<TextareaState>> {
        self.editor.as_ref().map(|e| e.state.clone())
    }
    pub fn update_model(&mut self, f: impl FnOnce(&mut GridModel), cx: &mut Context<Self>) {
        f(&mut self.model);
        self.model.reconcile();
        if self.model.editing.is_none() {
            self.editor = None;
        }
        cx.notify();
    }
    pub fn request_edit(
        &mut self,
        cell: Cell,
        nonce: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let events = self.model.request_edit(cell, nonce);
        self.emit(events, cx);
        self.sync_editor(window, cx);
        cx.notify();
    }
    /// Call from the application's Edit menu as well as keyboard shortcuts.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if self.model.editing.is_none() && !self.model.selection.rows.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.model.selected_tsv()));
        }
    }
    pub fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            let events = self.model.paste(&text);
            self.emit(events, cx);
        }
    }
    /// Standard Electron context menu actions. Unknown IDs go back to the caller.
    pub fn activate_menu(&mut self, id: &str, cell: Cell, cx: &mut Context<Self>) {
        let kind = match id {
            "cell" => Some(CopyKind::Cell),
            "row-tsv" => Some(CopyKind::RowTsv),
            "row-json" => Some(CopyKind::RowJson),
            "col" => Some(CopyKind::ColumnName),
            "all-tsv" => Some(CopyKind::AllTsv),
            _ => None,
        };
        if let Some(kind) = kind {
            cx.write_to_clipboard(ClipboardItem::new_string(self.model.copy(kind, cell)));
        } else {
            cx.emit(GridEvent::Menu {
                id: id.into(),
                cell,
            });
        }
    }
    fn emit(&self, events: Vec<GridEvent>, cx: &mut Context<Self>) {
        for event in events {
            cx.emit(event);
        }
    }
    fn sync_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.model.editing.as_ref() else {
            self.editor = None;
            return;
        };
        if self.editor.as_ref().is_some_and(|e| e.cell == edit.cell) {
            return;
        }
        let cell = edit.cell;
        let value = edit.value.clone();
        let multi = value.contains('\n');
        self.editor_size = (320., 120.);
        // Textarea's engine supplies IME, Unicode selection, clipboard and undo. Its unstyled
        // presentation is enclosed by our exact cell-input border/padding/background.
        let state = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx)
                .default_value(value)
                .rows(1)
                .soft_wrap(false);
            input.set_editor_style(InputEditorStyle {
                foreground: rgb(TEXT).into(),
                muted_foreground: rgb(NULL).into(),
                background: rgb(0x111111).into(),
                border: rgb(ACCENT).into(),
                selection: rgba(0x2f6fed66).into(),
                caret: rgb(TEXT).into(),
                ..Default::default()
            });
            if multi {
                let end = input.value().len();
                input.set_selected_range(end..end, cx);
            } else {
                input.select_all(window, cx);
            }
            input.focus(window, cx);
            input
        });
        let subscription =
            cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
                if this.model.editing.as_ref().is_none_or(|e| e.cell != cell) {
                    return;
                }
                match event {
                    InputEvent::Change => {
                        if let Some(edit) = &mut this.model.editing {
                            edit.value = state.read(cx).value().to_string();
                        }
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        // Defer until the click that moved focus has had a chance to start another editor.
                        let weak = cx.entity().downgrade();
                        window.defer(cx, move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                if this.editor.as_ref().is_some_and(|e| {
                                    e.cell == cell
                                        && !e.state.read(cx).focus_handle(cx).is_focused(window)
                                }) {
                                    this.finish_editor(true, 0, false, window, cx);
                                }
                            });
                        });
                    }
                    _ => {}
                }
            });
        self.editor = Some(Editor {
            cell,
            state,
            _subscription: subscription,
        });
    }
    fn finish_editor(
        &mut self,
        commit: bool,
        movement: i8,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let (Some(editor), Some(edit)) = (&self.editor, &mut self.model.editing) {
            edit.value = editor.state.read(cx).value().to_string();
        }
        let events = self.model.finish_edit(commit, movement);
        self.emit(events, cx);
        self.sync_editor(window, cx);
        if focus && self.editor.is_none() {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }
    fn keyboard(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_tooltip(cx);
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        if self.editor.is_some() {
            if key == "enter" && mods.alt {
                let editor = self.editor.as_ref().unwrap().state.clone();
                editor.update(cx, |state, cx| state.replace("\n", window, cx));
                if let Some(edit) = &mut self.model.editing {
                    edit.value = editor.read(cx).value().to_string();
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            let action = match key {
                "enter" => Some((true, 0)),
                "escape" => Some((false, 0)),
                "tab" => Some((true, if mods.shift { -1 } else { 1 })),
                _ => None,
            };
            if let Some((commit, movement)) = action {
                self.finish_editor(commit, movement, true, window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let command = mods.platform || mods.control;
        if command && key.eq_ignore_ascii_case("c") {
            self.copy_selection(cx);
            cx.stop_propagation();
            return;
        }
        if command && key.eq_ignore_ascii_case("v") && self.model.options.hooks.paste {
            self.paste_clipboard(cx);
            cx.stop_propagation();
            return;
        }
        let key = match key {
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "home" => Key::Home,
            "end" => Key::End,
            "tab" => Key::Tab,
            "enter" => Key::Enter,
            "escape" => Key::Escape,
            "backspace" | "delete" if self.model.options.hooks.delete => Key::Delete,
            "space" if self.model.options.hooks.detail => Key::Space,
            "i" if command && self.model.options.hooks.insert => Key::Insert,
            "d" if command && self.model.options.hooks.duplicate => Key::Duplicate,
            _ => return,
        };
        let events = self.model.navigate(key, model_mods(mods));
        self.emit(events, cx);
        self.sync_editor(window, cx);
        cx.stop_propagation();
        cx.notify();
    }
    fn local(&self, pos: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(pos.x - self.bounds.origin.x),
            f32::from(pos.y - self.bounds.origin.y),
        )
    }
    fn hit_column(&self, x: f32) -> Option<usize> {
        if x < self.model.number_width() {
            return None;
        }
        let mut left = self.model.number_width() - self.model.scroll_x;
        for c in self.model.visible_columns() {
            let right = left + self.model.widths[c];
            if x >= left && x < right {
                return Some(c);
            }
            left = right;
        }
        None
    }
    fn resizer(&self, x: f32, y: f32) -> Option<usize> {
        if y >= HEADER_HEIGHT || x < self.model.number_width() {
            return None;
        }
        let mut right = self.model.number_width() - self.model.scroll_x;
        for c in self.model.visible_columns() {
            right += self.model.widths[c];
            if (x - right).abs() <= 3.5 {
                return Some(c);
            }
        }
        None
    }
    fn absolute_cell_rect(&self, cell: Cell) -> Rect {
        let mut rect = self.model.cell_rect(cell);
        rect.x += f32::from(self.bounds.origin.x);
        rect.y += f32::from(self.bounds.origin.y);
        rect
    }
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The root's automatic focus listener runs after this handler. Keep the editor focus
        // assigned by a double click instead of allowing it to jump back to the scroller.
        window.prevent_default();
        self.clear_tooltip(cx);
        let (x, y) = self.local(event.position);
        let l = self.model.layout();
        if event.button == MouseButton::Left {
            if self.editor.is_some() {
                self.finish_editor(true, 0, false, window, cx);
            }
            self.focus.focus(window, cx);
            let horizontal = l.horizontal && y >= l.height;
            let vertical = l.vertical && x >= l.width;
            if horizontal || vertical {
                let bar = scrollbar(&self.model, horizontal);
                let at = if horizontal { x } else { y };
                let start = if horizontal { bar.x } else { bar.y };
                let len = if horizontal { bar.width } else { bar.height };
                let viewport = if horizontal { l.width } else { l.height };
                let extent = if horizontal {
                    l.content_width - l.width
                } else {
                    l.content_height - l.height
                };
                let scale = extent / (viewport - len).max(1.);
                if at < start || at > start + len {
                    let offset = ((at - len / 2.) * scale).clamp(0., extent.max(0.));
                    if horizontal {
                        self.model.scroll_x = offset;
                    } else {
                        self.model.scroll_y = offset;
                    }
                }
                self.drag = Some(Drag::Scroll {
                    horizontal,
                    at,
                    offset: if horizontal {
                        self.model.scroll_x
                    } else {
                        self.model.scroll_y
                    },
                    scale,
                });
                cx.notify();
                return;
            }
            if let Some(col) = self.resizer(x, y) {
                self.drag = Some(Drag::Column {
                    col,
                    x,
                    width: self.model.widths[col],
                });
                cx.notify();
                return;
            }
        }
        if y < HEADER_HEIGHT {
            if event.button == MouseButton::Left {
                if let Some(col) = self.hit_column(x) {
                    let events = self.model.click_header(col);
                    self.emit(events, cx);
                }
            }
            return;
        }
        if x >= l.width || y >= l.height {
            return;
        }
        let row = ((y - HEADER_HEIGHT + self.model.scroll_y) / ROW_HEIGHT)
            .floor()
            .max(0.) as usize;
        if row >= self.model.rows.len() {
            return;
        }
        let col = self.hit_column(x);
        if let Some(col) = col {
            let cell = Cell { row, col };
            let rect = self.absolute_cell_rect(cell);
            if event.button == MouseButton::Right
                || (event.button == MouseButton::Left
                    && (event.modifiers.alt || event.modifiers.control))
            {
                let events = self.model.context_menu(cell, rect);
                self.emit(events, cx);
            } else if event.button == MouseButton::Middle {
                self.model.selected_column = Some(col);
                if self.model.options.hooks.quick_look {
                    cx.emit(GridEvent::QuickLook(cell));
                }
            } else if event.button == MouseButton::Left {
                let local_rect = self.model.cell_rect(cell);
                if self.model.columns[col].chevron
                    && self.model.options.hooks.popup
                    && x >= local_rect.x + local_rect.width - 20.
                {
                    self.model.selection = Selection::single(row);
                    let events = self.model.click(row, Some(col), GridModifiers::default());
                    self.emit(events, cx);
                    cx.emit(GridEvent::CellPopup { cell, bounds: rect });
                } else {
                    let events = self
                        .model
                        .click(row, Some(col), model_mods(event.modifiers));
                    self.emit(events, cx);
                    if event.click_count == 2 {
                        self.model.begin_edit(cell);
                        self.sync_editor(window, cx);
                    }
                }
            }
        } else if event.button == MouseButton::Left {
            let events = self.model.click(row, None, model_mods(event.modifiers));
            self.emit(events, cx);
        }
        cx.notify();
    }
    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (x, y) = self.local(event.position);
        if let Some(drag) = self.drag {
            match drag {
                Drag::Column {
                    col,
                    x: start,
                    width,
                } => {
                    let events = self.model.resize(col, width + x - start);
                    self.emit(events, cx);
                }
                Drag::Scroll {
                    horizontal,
                    at,
                    offset,
                    scale,
                } => {
                    if horizontal {
                        self.model.scroll_x = offset + (x - at) * scale;
                    } else {
                        self.model.scroll_y = offset + (y - at) * scale;
                    }
                    self.model.clamp_scroll();
                }
                Drag::Editor {
                    x: start_x,
                    y: start_y,
                    width,
                    height,
                } => {
                    self.editor_size = (
                        (width + x - start_x).max(100.),
                        (height + y - start_y).max(40.),
                    );
                }
            }
            cx.notify();
        }
        let l = self.model.layout();
        let hovered = if l.horizontal
            && y >= l.height
            && y < self.model.viewport.height
            && x >= 0.
            && x < l.width
        {
            Some(true)
        } else if l.vertical
            && x >= l.width
            && x < self.model.viewport.width
            && y >= 0.
            && y < l.height
        {
            Some(false)
        } else {
            None
        };
        if hovered != self.hover_scrollbar {
            self.hover_scrollbar = hovered;
            cx.notify();
        }
        let target = if self.drag.is_none()
            && self.editor.is_none()
            && x >= 0.
            && y >= 0.
            && x < l.width
            && y < l.height
        {
            self.hit_column(x).and_then(|col| {
                if y < HEADER_HEIGHT {
                    Some(HoverTarget::Header(col))
                } else {
                    let row =
                        ((y - HEADER_HEIGHT + self.model.scroll_y) / ROW_HEIGHT).floor() as usize;
                    let cell = Cell { row, col };
                    self.model
                        .cell_value(cell)
                        .filter(|v| v.encode_utf16().take(41).count() > 40)
                        .map(|_| HoverTarget::Cell(cell))
                }
            })
        } else {
            None
        };
        if target != self.hover_target {
            self.hover_target = target;
            self.tooltip_task = None;
            if self.tooltip.take().is_some() {
                cx.notify();
            }
            if let Some(target) = target {
                let value = match target {
                    HoverTarget::Header(col) => self.model.columns[col].name.as_str(),
                    HoverTarget::Cell(cell) => self.model.cell_value(cell).unwrap_or(""),
                };
                let text: SharedString =
                    String::from_utf16_lossy(&value.encode_utf16().take(2000).collect::<Vec<_>>())
                        .into();
                self.tooltip_task = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(600))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.hover_target == Some(target) {
                            this.tooltip = Some((text, x, y));
                            cx.notify();
                        }
                    });
                }));
            }
        }
    }
    fn clear_tooltip(&mut self, cx: &mut Context<Self>) {
        self.tooltip_task = None;
        self.hover_target = None;
        if self.tooltip.take().is_some() {
            cx.notify();
        }
    }
    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_tooltip(cx);
        let (mut x, mut y) = match event.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(p) => (p.x * ROW_HEIGHT, p.y * ROW_HEIGHT),
        };
        if event.modifiers.shift && x == 0. {
            x = y;
            y = 0.;
        }
        let previous = (self.model.scroll_x, self.model.scroll_y);
        self.model.scroll_x -= x;
        self.model.scroll_y -= y;
        self.model.clamp_scroll();
        cx.stop_propagation();
        if (self.model.scroll_x, self.model.scroll_y) != previous {
            cx.notify();
        }
    }
}
fn model_mods(m: gpui_kit::Modifiers) -> GridModifiers {
    GridModifiers {
        shift: m.shift,
        meta: m.platform,
        control: m.control,
        alt: m.alt,
    }
}

impl Render for DataGrid {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prepaint = cx.entity().downgrade();
        let paint = prepaint.clone();
        let mut root = div()
            .id("telecat-grid")
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(BG))
            .track_focus(&self.focus)
            .key_context("TelecatGrid")
            .capture_key_down(cx.listener(Self::keyboard))
            .on_action(cx.listener(|this, _: &NextCell, window, cx| {
                let events = this.model.navigate(Key::Tab, GridModifiers::default());
                this.emit(events, cx);
                this.sync_editor(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PreviousCell, window, cx| {
                let events = this.model.navigate(
                    Key::Tab,
                    GridModifiers {
                        shift: true,
                        ..Default::default()
                    },
                );
                this.emit(events, cx);
                this.sync_editor(window, cx);
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &input::Enter, window, cx| {
                if this.editor.is_some() {
                    this.finish_editor(true, 0, true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &input::Escape, window, cx| {
                if this.editor.is_some() {
                    this.finish_editor(false, 0, true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &input::IndentInline, window, cx| {
                if this.editor.is_some() {
                    this.finish_editor(true, 1, true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &input::OutdentInline, window, cx| {
                if this.editor.is_some() {
                    this.finish_editor(true, -1, true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &input::Paste, window, cx| {
                // HTML <input> strips CR/LF on paste; the expanded <textarea> preserves them.
                if let Some(edit) = this.model.editing.as_ref() {
                    let multi = edit.value.contains('\n');
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        if let Some(editor) = &this.editor {
                            editor.state.update(cx, |state, cx| {
                                state.replace(
                                    if multi {
                                        text.replace("\r\n", "\n").replace('\r', "\n")
                                    } else {
                                        text.replace(['\r', '\n'], "")
                                    },
                                    window,
                                    cx,
                                )
                            });
                            if let Some(edit) = &mut this.model.editing {
                                edit.value = editor.state.read(cx).value().to_string();
                            }
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &input::Copy, _, cx| {
                if this.editor.is_none() {
                    this.copy_selection(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &input::Paste, _, cx| {
                if this.editor.is_none() {
                    this.paste_clipboard(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, window, cx| {
                if this.editor.is_some() {
                    window.defer(cx, |window, cx| {
                        window.dispatch_action(Box::new(input::Copy), cx)
                    });
                } else {
                    this.copy_selection(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Paste, window, cx| {
                if this.editor.is_some() {
                    window.defer(cx, |window, cx| {
                        window.dispatch_action(Box::new(input::Paste), cx)
                    });
                } else {
                    this.paste_clipboard(cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        prepaint
                            .update(cx, |this, _| {
                                let started = Instant::now();
                                this.bounds = bounds;
                                this.model.viewport = Rect {
                                    x: 0.,
                                    y: 0.,
                                    width: f32::from(bounds.size.width),
                                    height: f32::from(bounds.size.height),
                                };
                                this.model.clamp_scroll();
                                let mut frame = Frame::new(started, bounds);
                                frame.prepare(&this.model, this.hover_scrollbar, window);
                                let mut left = this.model.number_width() - this.model.scroll_x;
                                for c in this.model.visible_columns() {
                                    left += this.model.widths[c];
                                    if left >= this.model.number_width()
                                        && left < this.model.layout().width
                                    {
                                        let hit = window.insert_hitbox(
                                            rect(bounds, left - 3., 0., 7., HEADER_HEIGHT),
                                            HitboxBehavior::Normal,
                                        );
                                        frame.resizers.push(hit);
                                    }
                                }
                                frame
                            })
                            .ok()
                    },
                    move |_, frame, window, cx| {
                        if let Some(frame) = frame {
                            for hit in &frame.resizers {
                                window.set_cursor_style(CursorStyle::ResizeLeftRight, hit);
                            }
                            frame.paint(window, cx);
                            let _ = paint.update(cx, |this, _| {
                                this.frame_stats.record(frame.started, frame.cells)
                            });
                        }
                        // Window listeners keep an active drag alive after leaving the grid bounds.
                        let weak = paint.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble {
                                let _ =
                                    weak.update(cx, |this, cx| this.mouse_move(event, window, cx));
                            }
                        });
                        let weak = paint.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                let _ = weak.update(cx, |this, cx| {
                                    if this.drag.take().is_some() {
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    },
                )
                .size_full(),
            );
        if let (Some(editor), Some(edit)) = (&self.editor, &self.model.editing) {
            let r = self.model.cell_rect(edit.cell);
            let multi = edit.value.contains('\n');
            let w = if multi {
                self.editor_size.0.max(r.width)
            } else {
                r.width
            };
            let h = if multi {
                self.editor_size.1
            } else {
                ROW_HEIGHT
            };
            let state = editor.state.clone();
            let mut overlay = div()
                .id("cell-editor")
                .absolute()
                .left(px(r.x))
                .top(px(r.y))
                .w(px(w))
                .h(px(h))
                .border_2()
                .border_color(rgb(ACCENT))
                .bg(rgb(0x111111))
                .px(px(if multi { 6. } else { 5. }))
                .py(px(if multi { 4. } else { 0. }))
                .font(mono_font())
                .text_size(px(13.))
                .line_height(px(if multi { 18.2 } else { 22. }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(Textarea::new(&state));
            if multi {
                overlay = overlay.child(
                    div()
                        .absolute()
                        .right_0()
                        .bottom_0()
                        .size(px(10.))
                        .cursor(CursorStyle::ResizeUpLeftDownRight)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                let (x, y) = this.local(event.position);
                                this.drag = Some(Drag::Editor {
                                    x,
                                    y,
                                    width: w,
                                    height: h,
                                });
                                cx.stop_propagation();
                            }),
                        ),
                );
            }
            root = root.child(overlay);
        }
        if let Some((text, x, y)) = &self.tooltip {
            root = root.child(
                div()
                    .absolute()
                    .left(px(
                        (*x + 10.).min((self.model.viewport.width - 360.).max(0.))
                    ))
                    .top(px(
                        (*y + 18.).min((self.model.viewport.height - 80.).max(0.))
                    ))
                    .max_w(px(350.))
                    .px(px(7.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(rgb(0x555555))
                    .bg(rgb(0x2a2a2a))
                    .font(ui_font())
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(rgb(TEXT))
                    .child(text.clone()),
            );
        }
        root
    }
}

fn rect(bounds: Bounds<Pixels>, x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(
        bounds.origin + point(px(x), px(y)),
        size(px(w.max(0.)), px(h.max(0.))),
    )
}
fn scrollbar(m: &GridModel, horizontal: bool) -> Rect {
    let l = m.layout();
    let (viewport, content, offset) = if horizontal {
        (l.width, l.content_width, m.scroll_x)
    } else {
        (l.height, l.content_height, m.scroll_y)
    };
    let len = (viewport * viewport / content.max(1.))
        .round()
        .max(26.)
        .min(viewport);
    let at = (offset / (content - viewport).max(1.) * (viewport - len)).round();
    if horizontal {
        Rect {
            x: at,
            y: l.height,
            width: len,
            height: SCROLLBAR,
        }
    } else {
        Rect {
            x: l.width,
            y: at,
            width: SCROLLBAR,
            height: len,
        }
    }
}
struct TextPaint {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    align: TextAlign,
}
enum Primitive {
    Quad(PaintQuad, Bounds<Pixels>),
    Text(TextPaint),
}
struct Frame {
    started: Instant,
    bounds: Bounds<Pixels>,
    items: Vec<Primitive>,
    cells: usize,
    resizers: Vec<Hitbox>,
}
impl Frame {
    fn new(started: Instant, bounds: Bounds<Pixels>) -> Self {
        Self {
            started,
            bounds,
            items: Vec::new(),
            cells: 0,
            resizers: vec![],
        }
    }
    fn quad(&mut self, r: Rect, color: Rgba, clip: Bounds<Pixels>) {
        self.items.push(Primitive::Quad(
            fill(rect(self.bounds, r.x, r.y, r.width, r.height), color),
            clip,
        ));
    }
    fn text(
        &mut self,
        text: &str,
        r: Rect,
        font: Font,
        font_size: f32,
        color: u32,
        align: TextAlign,
        clip: Bounds<Pixels>,
        deleted: bool,
        window: &Window,
    ) {
        if text.is_empty() || r.width <= 0. {
            return;
        }
        // white-space:pre clips later lines at the cell edge. Limit work to Electron's 500 UTF-16 units.
        let first = text.split(['\n', '\r']).next().unwrap_or("");
        let utf16: Vec<u16> = first.encode_utf16().take(501).collect();
        let display = if utf16.len() > 500 {
            format!("{}…", String::from_utf16_lossy(&utf16[..500]))
        } else {
            first.to_owned()
        };
        let shape = |s: String| {
            let run = TextRun {
                len: s.len(),
                font: font.clone(),
                color: rgb(color).into(),
                ..Default::default()
            };
            window
                .text_system()
                .shape_line(s.into(), px(font_size), &[run], None)
        };
        let mut line = shape(display.clone());
        if line.width() > px(r.width) {
            let ellipsis = shape("…".into());
            let budget = (px(r.width) - ellipsis.width()).max(px(0.));
            let mut at = line.index_for_x(budget).unwrap_or(display.len());
            while at > 0 && !display.is_char_boundary(at) {
                at -= 1;
            }
            line = shape(format!("{}…", &display[..at]));
        }
        let bounds = rect(self.bounds, r.x, r.y, r.width, r.height);
        if deleted {
            // CSS text-decoration-color:#ffffff60 (not an entire-cell strike).
            let width = f32::from(line.width()).min(r.width);
            let x = r.x
                + match align {
                    TextAlign::Right => r.width - width,
                    TextAlign::Center => (r.width - width) / 2.,
                    _ => 0.,
                };
            // Insert after text below so the translucent strike is composited on top.
            self.items.push(Primitive::Text(TextPaint {
                line,
                bounds,
                clip,
                align,
            }));
            self.quad(
                Rect {
                    x,
                    y: r.y + r.height / 2.,
                    width,
                    height: 1.,
                },
                rgba(0xffffff60),
                clip,
            );
        } else {
            self.items.push(Primitive::Text(TextPaint {
                line,
                bounds,
                clip,
                align,
            }));
        }
    }
    fn prepare(&mut self, m: &GridModel, hover: Option<bool>, window: &mut Window) {
        let l = m.layout();
        let num = m.number_width();
        let whole = self.bounds;
        let body = rect(
            self.bounds,
            0.,
            HEADER_HEIGHT,
            l.width,
            (l.height - HEADER_HEIGHT).max(0.),
        );
        let data_clip = rect(
            self.bounds,
            num,
            HEADER_HEIGHT,
            (l.width - num).max(0.),
            (l.height - HEADER_HEIGHT).max(0.),
        );
        let mut columns = vec![];
        let mut left = num - m.scroll_x;
        for c in m.visible_columns() {
            let width = m.widths[c];
            if left + width > num && left < l.width {
                columns.push((c, left, width));
            }
            left += width;
        }
        let count = m.rows.len() + filler_count(m.rows.len(), l.height);
        for row in virtual_range(count, m.scroll_y, l.height - HEADER_HEIGHT, OVERSCAN) {
            let y = HEADER_HEIGHT + row as f32 * ROW_HEIGHT - m.scroll_y;
            if y + ROW_HEIGHT <= HEADER_HEIGHT || y >= l.height {
                continue;
            }
            let actual = row < m.rows.len();
            let selected = actual && m.selection.rows.contains(&row);
            let state = m.edits.rows.get(&row).copied().unwrap_or_default();
            let bg = match state {
                RowState::Deleted => DELETED,
                RowState::New => NEW,
                _ if selected => SELECTED,
                _ if row % 2 == 1 => ODD,
                _ => BG,
            };
            self.quad(
                Rect {
                    x: 0.,
                    y,
                    width: l.width,
                    height: ROW_HEIGHT,
                },
                rgb(bg),
                body,
            );
            for &(col, x, width) in &columns {
                let cell = Cell { row, col };
                let r = Rect {
                    x,
                    y,
                    width,
                    height: ROW_HEIGHT,
                };
                if actual && m.edits.modified.contains(&cell) {
                    self.quad(r, rgb(MOD_CELL), data_clip);
                }
                self.quad(
                    Rect {
                        x: x + width - 1.,
                        y,
                        width: 1.,
                        height: ROW_HEIGHT,
                    },
                    rgba(0xffffff08),
                    data_clip,
                );
                if actual {
                    let null = m.cell_value(cell).is_none() || m.is_default(cell);
                    let empty = m.cell_value(cell) == Some("") && m.options.show_empty;
                    let text = if empty { "EMPTY" } else { m.cell_text(cell) };
                    let chev = m.columns[col].chevron;
                    let padding = if chev { 20. } else { 7. };
                    let text_r = Rect {
                        x: x + 7.,
                        y: y + m.options.row_text_offset,
                        width: width - 8. - padding,
                        height: ROW_HEIGHT,
                    };
                    self.text(
                        text,
                        text_r,
                        mono_font(),
                        13.,
                        if null || empty { NULL } else { TEXT },
                        if m.columns[col].numeric && (!null || m.is_default(cell)) {
                            TextAlign::Right
                        } else {
                            TextAlign::Left
                        },
                        data_clip.intersect(&rect(self.bounds, x, y, width - 1., ROW_HEIGHT)),
                        state == RowState::Deleted,
                        window,
                    );
                    if chev && m.options.hooks.popup {
                        self.text(
                            "⌃",
                            Rect {
                                x: x + width - 14.,
                                y: y + 1.,
                                width: 8.,
                                height: 14.,
                            },
                            mono_font(),
                            8.,
                            MUTED,
                            TextAlign::Left,
                            data_clip,
                            false,
                            window,
                        );
                        self.text(
                            "⌄",
                            Rect {
                                x: x + width - 14.,
                                y: y + 8.,
                                width: 8.,
                                height: 14.,
                            },
                            mono_font(),
                            8.,
                            MUTED,
                            TextAlign::Left,
                            data_clip,
                            false,
                            window,
                        );
                    }
                    if selected && m.selected_column == Some(col) {
                        self.items.push(Primitive::Quad(
                            outline(
                                rect(self.bounds, x, y, width, ROW_HEIGHT),
                                rgb(ACCENT),
                                BorderStyle::Solid,
                            )
                            .border_widths(px(2.)),
                            data_clip,
                        ));
                    }
                    self.cells += 1;
                }
            }
            if num > 0. {
                self.quad(
                    Rect {
                        x: 0.,
                        y,
                        width: num,
                        height: ROW_HEIGHT,
                    },
                    rgb(bg),
                    body,
                );
                self.quad(
                    Rect {
                        x: num - 1.,
                        y,
                        width: 1.,
                        height: ROW_HEIGHT,
                    },
                    rgba(0xffffff08),
                    body,
                );
                if actual {
                    self.text(
                        &m.options.row_offset.saturating_add(row + 1).to_string(),
                        Rect {
                            x: 7.,
                            y,
                            width: num - 15.,
                            height: ROW_HEIGHT,
                        },
                        mono_font(),
                        13.,
                        MUTED,
                        TextAlign::Center,
                        body,
                        state == RowState::Deleted,
                        window,
                    );
                }
            }
        }
        let header = rect(self.bounds, 0., 0., l.width, HEADER_HEIGHT);
        let header_data = rect(self.bounds, num, 0., (l.width - num).max(0.), HEADER_HEIGHT);
        self.quad(
            Rect {
                x: 0.,
                y: 0.,
                width: l.width,
                height: HEADER_HEIGHT,
            },
            rgb(HEADER),
            header,
        );
        let mut font = ui_font();
        font.weight = FontWeight::MEDIUM;
        for &(col, x, width) in &columns {
            let sorted = m.sort.as_ref().filter(|s| s.column == m.columns[col].name);
            let indicator = if sorted.is_some() { 13. } else { 0. };
            let normalized = m.columns[col]
                .name
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let name = &normalized;
            let natural = window
                .text_system()
                .shape_line(
                    name.clone().into(),
                    px(13.5),
                    &[TextRun {
                        len: name.len(),
                        font: font.clone(),
                        color: rgb(TEXT).into(),
                        ..Default::default()
                    }],
                    None,
                )
                .width();
            let label_width = f32::from(natural).min((width - 17. - indicator).max(0.));
            let label_left = x + (width - 1. - label_width - indicator) / 2.;
            self.text(
                name,
                Rect {
                    x: label_left,
                    y: 0.,
                    width: label_width,
                    height: HEADER_HEIGHT - 1.,
                },
                font.clone(),
                13.5,
                TEXT,
                TextAlign::Center,
                header_data.intersect(&rect(self.bounds, x, 0., width - 1., HEADER_HEIGHT)),
                false,
                window,
            );
            if let Some(s) = sorted {
                self.text(
                    if s.direction == Direction::Asc {
                        "▲"
                    } else {
                        "▼"
                    },
                    Rect {
                        x: label_left + label_width + 4.,
                        y: 0.,
                        width: 9.,
                        height: HEADER_HEIGHT - 1.,
                    },
                    ui_font(),
                    9.,
                    TEXT_SECONDARY,
                    TextAlign::Center,
                    header_data,
                    false,
                    window,
                );
            }
            self.quad(
                Rect {
                    x: x + width - 1.,
                    y: 0.,
                    width: 1.,
                    height: HEADER_HEIGHT,
                },
                rgba(0xffffff10),
                header_data,
            );
        }
        if num > 0. {
            self.quad(
                Rect {
                    x: 0.,
                    y: 0.,
                    width: num,
                    height: HEADER_HEIGHT,
                },
                rgb(HEADER),
                header,
            );
            self.quad(
                Rect {
                    x: num - 1.,
                    y: 0.,
                    width: 1.,
                    height: HEADER_HEIGHT,
                },
                rgba(0xffffff10),
                header,
            );
        }
        self.quad(
            Rect {
                x: 0.,
                y: HEADER_HEIGHT - 1.,
                width: l.width,
                height: 1.,
            },
            rgb(0x111111),
            header,
        );
        if m.rows.is_empty() && !m.options.empty_text.is_empty() {
            let text = &m.options.empty_text;
            let line = window.text_system().shape_line(
                text.clone().into(),
                px(13.),
                &[TextRun {
                    len: text.len(),
                    font: ui_font(),
                    color: rgb(TEXT_SECONDARY).into(),
                    ..Default::default()
                }],
                None,
            );
            let text_width = f32::from(line.width());
            let w = text_width + 30.; // CSS: 14px padding + 1px border on each side.
            let x = (l.width - w) / 2.;
            let r = rect(self.bounds, x, 90., w, 30.);
            self.items.push(Primitive::Quad(
                fill(r, rgba(0x2a2a2aee))
                    .corner_radii(px(8.))
                    .border_widths(px(1.))
                    .border_color(rgb(0x3a3a3a)),
                whole,
            ));
            // This pill sizes itself to its text; it must never use cell elision.
            // Reuse the measured line: (text_width + 30) - 30 can round down in f32
            // and make the cell helper replace a fitting label with an ellipsis.
            self.items.push(Primitive::Text(TextPaint {
                line,
                bounds: rect(self.bounds, x + 15., 97., text_width, 16.),
                clip: whole,
                align: TextAlign::Left,
            }));
        }
        for horizontal in [false, true] {
            if (horizontal && !l.horizontal) || (!horizontal && !l.vertical) {
                continue;
            }
            let r = scrollbar(m, horizontal);
            let b = rect(self.bounds, r.x + 2., r.y + 2., r.width - 4., r.height - 4.);
            self.items.push(Primitive::Quad(
                fill(
                    b,
                    rgba(if hover == Some(horizontal) {
                        0xffffff50
                    } else {
                        0xffffff30
                    }),
                )
                .corner_radii(px(3.)),
                whole,
            ));
        }
    }
    fn paint(&self, window: &mut Window, cx: &mut App) {
        for item in &self.items {
            match item {
                Primitive::Quad(q, clip) => window
                    .with_content_mask(Some(ContentMask { bounds: *clip }), |window| {
                        window.paint_quad(q.clone())
                    }),
                Primitive::Text(t) => {
                    window.with_content_mask(Some(ContentMask { bounds: t.clip }), |window| {
                        let _ = t.line.paint(
                            t.bounds.origin,
                            t.bounds.size.height,
                            t.align,
                            Some(t.bounds.size.width),
                            window,
                            cx,
                        );
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, Primitive};
    use crate::{Column, GridModel, Rect};
    use gpui::{Bounds, TestAppContext, point, px, size};
    use std::time::Instant;

    #[cfg(target_os = "macos")]
    #[test]
    fn empty_pill_native_text_is_not_elided_at_1x_or_2x() {
        let mut cx = TestAppContext::build_with_text_system(
            gpui::TestDispatcher::new(0),
            None,
            std::sync::Arc::new(gpui_apple::AppleTextSystem::new()),
        );
        let window = cx.add_empty_window();
        for scale in [1., 2.] {
            window.simulate_scale_factor_change(scale);
            for label in ["No rows", "Loading…"] {
                window.update(|window, cx| {
                    telecat_theme::init(cx);
                    assert_eq!(window.scale_factor(), scale);
                    let mut model = GridModel::new(vec![Column::new("account_id")], vec![]);
                    model.options.empty_text = label.into();
                    model.viewport = Rect {
                        width: 848.,
                        height: 511.,
                        ..Default::default()
                    };
                    let bounds = Bounds::new(point(px(270.), px(92.)), size(px(848.), px(511.)));
                    let mut frame = Frame::new(Instant::now(), bounds);
                    frame.prepare(&model, None, window);
                    let pills: Vec<_> = frame
                        .items
                        .iter()
                        .enumerate()
                        .filter_map(|(i, item)| match item {
                            Primitive::Quad(q, _)
                                if q.background == gpui::rgba(0x2a2a2aee).into() =>
                            {
                                Some((i, q))
                            }
                            _ => None,
                        })
                        .collect();
                    assert_eq!(pills.len(), 1);
                    let (index, pill) = pills[0];
                    let Primitive::Text(text) = &frame.items[index + 1] else {
                        panic!("pill must be followed by its text primitive");
                    };
                    let measured = window.text_system().shape_line(
                        label.into(),
                        px(13.),
                        &[gpui::TextRun {
                            len: label.len(),
                            font: telecat_theme::ui(),
                            color: gpui::rgb(telecat_theme::TEXT_SECONDARY).into(),
                            ..Default::default()
                        }],
                        None,
                    );
                    assert_eq!(
                        text.line.text.as_ref(),
                        label,
                        "{scale}x, measured={:?}, text width={:?}, pill width={:?}",
                        measured.width(),
                        text.bounds.size.width,
                        pill.bounds.size.width
                    );
                    assert_eq!(text.line.width(), measured.width());
                    assert_eq!(text.bounds.size.width, text.line.width());
                    let near = |a: gpui::Pixels, b: gpui::Pixels| {
                        assert!((a - b).as_f32().abs() < 0.0001, "{a:?} != {b:?}")
                    };
                    // Electron padding: horizontal 14 + border 1; vertical 6 + border 1.
                    near(text.bounds.left() - pill.bounds.left(), px(15.));
                    near(pill.bounds.right() - text.bounds.right(), px(15.));
                    near(text.bounds.top() - pill.bounds.top(), px(7.));
                    near(pill.bounds.bottom() - text.bounds.bottom(), px(7.));
                });
            }
        }
    }

    #[gpui::test]
    fn empty_message_is_one_text_primitive_pinned_to_viewport(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            telecat_theme::init(cx);
            let mut model = GridModel::new(
                [
                    "org_fingerprint",
                    "user_fingerprint",
                    "account_id",
                    "created_at",
                ]
                .map(Column::new)
                .to_vec(),
                vec![],
            );
            model.options.empty_text = "No rows".into();
            for (x, y, width) in [(0., 0., 848.), (260., 92., 848.), (350., 180., 400.)] {
                let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(511.)));
                model.viewport = Rect {
                    width,
                    height: 511.,
                    ..Default::default()
                };
                for scroll in [0., 150.] {
                    model.scroll_x = scroll;
                    model.scroll_y = scroll;
                    model.clamp_scroll();
                    let mut frame = Frame::new(Instant::now(), bounds);
                    frame.prepare(&model, None, window);
                    let messages: Vec<_> = frame
                        .items
                        .iter()
                        .filter_map(|item| match item {
                            Primitive::Text(text) if text.line.text == "No rows" => Some(text),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(messages.len(), 1);
                    let text = messages[0];
                    assert_eq!(text.bounds.origin.y, px(y + 97.));
                    assert!(
                        (f32::from(text.bounds.center().x) - (x + model.layout().width / 2.)).abs()
                            < 0.01
                    );
                    assert_eq!(text.clip, bounds);
                }
            }
        });
    }
}
