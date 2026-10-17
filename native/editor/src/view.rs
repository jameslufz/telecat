use crate::{
    Driver, Schema,
    completion::{CompletionIndex, CompletionResult},
    search::SearchQuery,
    syntax::{self},
    theme, utf16,
};
use gpui_kit::{prelude::*, *};
use std::{ops::Range, rc::Rc, sync::Arc};
use telecat_editor_engine::input::{
    Editor, EditorState, Input, InputEvent, InputState, RangeDecoration, RangeDecorationCollection,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}
impl Selection {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionInfo {
    pub lines: usize,
    pub selected: usize,
    pub chars: usize,
}
#[derive(Clone, Debug)]
pub enum EditorEvent {
    Change(SharedString),
    SelectionChange(SelectionInfo),
    RunCurrent,
    RunAll,
    Beautify,
    Uglify,
    PreviousTab,
    NextTab,
    SwitchConnection,
    Focus,
    Blur,
}
#[derive(Clone)]
pub struct EditorOptions {
    pub driver: Driver,
    pub text: String,
    pub schema: Schema,
    pub placeholder: String,
}
impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            driver: Driver::Mysql,
            text: String::new(),
            schema: Schema::new(),
            placeholder: theme::PLACEHOLDER.into(),
        }
    }
}
/// A view containing the text engine and application-owned popup/search presentation.
/// Formatting shortcuts emit events; the shell invokes `beautify` / `uglify` when ready.
pub struct SqlEditor {
    state: Entity<EditorState>,
    driver: Driver,
    value: SharedString,
    focused: bool,
    selections: Vec<(usize, usize)>,
    decorations: RangeDecorationCollection,
    index: CompletionIndex,
    popup: Option<CompletionResult>,
    completion_selected: usize,
    completion_task: Task<()>,
    completion_generation: u64,
    suppress_completion: bool,
    search_open: bool,
    find: Entity<InputState>,
    replace: Entity<InputState>,
    search: SearchQuery,
    search_matches: Vec<Range<usize>>,
    search_error: Option<String>,
    subscriptions: Vec<Subscription>,
    latency: Option<crate::latency::LatencyProbe>,
}
impl EventEmitter<EditorEvent> for SqlEditor {}
impl Focusable for SqlEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx).clone()
    }
}
impl SqlEditor {
    pub fn new(options: EditorOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::init(cx);
        let driver = options.driver;
        let state = cx.new(|cx| {
            let mut s = EditorState::new(window, cx)
                .language(if driver == Driver::Mysql {
                    "mysql"
                } else {
                    "postgresql"
                })
                .searchable(false)
                .folding(false)
                .indent_guides(false)
                .soft_wrap(false)
                .scroll_beyond_last_line(None)
                .cursor_surrounding_lines(Some(0))
                .placeholder(options.placeholder)
                .default_value(options.text.clone());
            s.set_editor_style(theme::style());
            s.set_editor_paddings(Edges {
                top: px(4.),
                bottom: px(4.),
                left: px(0.),
                right: px(2.),
            });
            s.set_highlighter_factory(
                Rc::new(move |_| Some(Box::new(syntax::Syntax::new(driver)))),
                cx,
            );
            s
        });
        let make_input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                let mut style = theme::style();
                style.background = rgb(0x1a1a1a).into();
                style.muted_foreground = rgb(0x666666).into();
                s.set_editor_style(style);
                s
            })
        };
        let find = make_input("Find", window, cx);
        let replace = make_input("Replace", window, cx);
        let decorations = state.update(cx, |s, cx| {
            s.create_range_decorations_collection(vec![], cx)
        });
        let value = state.read(cx).value();
        let selections = state.read(cx).selection_offsets();
        let mut result = Self {
            state: state.clone(),
            driver,
            value,
            focused: false,
            selections,
            decorations,
            index: CompletionIndex::new(driver, &options.schema),
            popup: None,
            completion_selected: 0,
            completion_task: Task::ready(()),
            completion_generation: 0,
            suppress_completion: false,
            search_open: false,
            find: find.clone(),
            replace: replace.clone(),
            search: SearchQuery::default(),
            search_matches: vec![],
            search_error: None,
            subscriptions: vec![],
            latency: None,
        };
        result.subscriptions.push(
            cx.subscribe_in(&state, window, |this, _, event, window, cx| match event {
                InputEvent::Change => this.changed(window, cx),
                InputEvent::Focus => {
                    this.focused = true;
                    this.decorate(cx);
                    cx.emit(EditorEvent::Focus);
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.focused = false;
                    this.decorate(cx);
                    this.dismiss_completion(cx);
                    cx.emit(EditorEvent::Blur);
                }
                _ => {}
            }),
        );
        result
            .subscriptions
            .push(cx.observe_in(&state, window, |this, _, _, cx| {
                let selections = this.state.read(cx).selection_offsets();
                if selections != this.selections {
                    this.selections = selections;
                    this.dismiss_completion(cx);
                    this.decorate(cx);
                    cx.emit(EditorEvent::SelectionChange(this.selection_info(cx)));
                    cx.notify();
                }
            }));
        result
            .subscriptions
            .push(cx.subscribe_in(&find, window, |this, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search.query = this.find.read(cx).value().to_string();
                    this.decorate(cx);
                    cx.notify();
                }
            }));
        result
            .subscriptions
            .push(cx.subscribe_in(&replace, window, |this, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search.replacement = this.replace.read(cx).value().to_string();
                }
            }));
        result
    }
    pub fn text(&self, cx: &App) -> SharedString {
        self.state.read(cx).value()
    }
    pub fn get_value(&self, cx: &App) -> SharedString {
        self.text(cx)
    }
    pub fn set_text(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.suppress_completion = true;
        self.state
            .update(cx, |s, cx| s.replace_all(text, window, cx));
    }
    pub fn set_value(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_text(text, window, cx)
    }
    pub fn cursor(&self, cx: &App) -> usize {
        utf16::from_byte(&self.text(cx), self.state.read(cx).cursor())
    }
    pub fn selections(&self, cx: &App) -> Vec<Selection> {
        let text = self.text(cx);
        self.state
            .read(cx)
            .selection_offsets()
            .into_iter()
            .map(|(a, h)| Selection {
                anchor: utf16::from_byte(&text, a),
                head: utf16::from_byte(&text, h),
            })
            .collect()
    }
    pub fn selection(&self, cx: &App) -> Range<usize> {
        let s = self.state.read(cx);
        let text = s.value();
        let r = s.selected_range();
        utf16::from_byte(&text, r.start)..utf16::from_byte(&text, r.end)
    }
    pub fn selected_text(&self, cx: &App) -> Option<String> {
        let s = self.state.read(cx);
        (!s.selected_range().is_empty()).then(|| s.selected_text().to_string())
    }
    pub fn set_selections(&mut self, selections: &[Selection], cx: &mut Context<Self>) {
        let text = self.text(cx);
        let offsets: Vec<_> = selections
            .iter()
            .map(|s| {
                (
                    utf16::to_byte(&text, s.anchor),
                    utf16::to_byte(&text, s.head),
                )
            })
            .collect();
        self.state
            .update(cx, |s, cx| s.set_selection_offsets(&offsets, cx));
    }
    pub fn selection_info(&self, cx: &App) -> SelectionInfo {
        let text = self.text(cx);
        let range = self.state.read(cx).selected_range();
        let selected = &text[range.clone()];
        SelectionInfo {
            lines: if range.is_empty() {
                text.lines().count() + usize::from(text.is_empty() || text.ends_with('\n'))
            } else {
                selected.split('\n').count()
            },
            selected: selected.encode_utf16().count(),
            chars: text.encode_utf16().count(),
        }
    }
    pub fn replace_range(
        &mut self,
        range: Range<usize>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = utf16::range_to_byte(&self.text(cx), range);
        self.suppress_completion = true;
        self.state.update(cx, |s, cx| {
            s.edit_batch(&[(range, text.to_string())], window, cx)
        });
    }
    /// Like Electron's shell insert: replace the main selection, then focus it.
    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = self.selection(cx);
        let end = range.start + text.encode_utf16().count();
        self.replace_range(range, text, window, cx);
        self.set_selections(
            &[Selection {
                anchor: end,
                head: end,
            }],
            cx,
        );
        self.focus(window, cx);
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.focus(window, cx));
    }
    /// Native Edit-menu routing uses this engine's action types. The app's form
    /// inputs use a separate engine, so their actions must not be sent here.
    pub fn menu_action(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        use telecat_editor_engine::input::{Copy, Cut, Delete, Paste, SelectAll};
        match name {
            "undo" => self.undo(window, cx),
            "redo" => self.redo(window, cx),
            "copy" => window.dispatch_action(Box::new(Copy), cx),
            "cut" => window.dispatch_action(Box::new(Cut), cx),
            "paste" | "paste-match" => window.dispatch_action(Box::new(Paste), cx),
            "select-all" => window.dispatch_action(Box::new(SelectAll), cx),
            "delete" => window.dispatch_action(Box::new(Delete), cx),
            _ => {}
        }
    }
    pub fn set_driver(
        &mut self,
        driver: Driver,
        schema: &Schema,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.driver = driver;
        self.index = CompletionIndex::new(driver, schema);
        self.state.update(cx, |s, cx| {
            s.set_highlighter(
                if driver == Driver::Mysql {
                    "mysql"
                } else {
                    "postgresql"
                },
                cx,
            );
            s.set_highlighter_factory(
                Rc::new(move |_| Some(Box::new(syntax::Syntax::new(driver)))),
                cx,
            );
        });
        self.dismiss_completion(cx);
        self.decorate(cx);
    }
    pub fn set_schema(&mut self, schema: &Schema, cx: &mut Context<Self>) {
        self.index = CompletionIndex::new(self.driver, schema);
        self.dismiss_completion(cx);
    }
    pub fn current_statement_range(&self, cx: &App) -> Option<Range<usize>> {
        telecat_core::sql::statement_at(&self.text(cx), self.cursor(cx), self.driver)
            .map(|s| s.from..s.to)
    }
    pub fn beautify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selected_text(cx);
        let text = selection
            .clone()
            .unwrap_or_else(|| self.text(cx).to_string());
        let result = crate::format::beautify(&text, self.driver);
        if selection.is_some() {
            self.insert(&result, window, cx);
        } else {
            self.set_text(result, window, cx);
        }
    }
    pub fn uglify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selected_text(cx);
        let text = selection
            .clone()
            .unwrap_or_else(|| self.text(cx).to_string());
        let result = crate::format::uglify(&text, self.driver);
        if selection.is_some() {
            self.insert(&result, window, cx);
        } else {
            self.set_text(result, window, cx);
        }
    }
    pub fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suppress_completion = true;
        self.state.update(cx, |s, cx| s.undo_edit(window, cx));
    }
    pub fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suppress_completion = true;
        self.state.update(cx, |s, cx| s.redo_edit(window, cx));
    }
    /// Engine access for IME integration and deterministic interaction tests; its offsets are bytes.
    pub fn set_latency_probe(
        &mut self,
        probe: crate::latency::LatencyProbe,
        cx: &mut Context<Self>,
    ) {
        self.latency = Some(probe);
        cx.notify();
    }
    pub fn input_state(&self) -> &Entity<EditorState> {
        &self.state
    }
    pub fn freeze_caret(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.freeze_caret(visible, cx));
    }
    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(probe) = &self.latency {
            probe.arm();
        }
        self.value = self.state.read(cx).value();
        self.selections = self.state.read(cx).selection_offsets();
        self.decorate(cx);
        cx.emit(EditorEvent::Change(self.value.clone()));
        cx.emit(EditorEvent::SelectionChange(self.selection_info(cx)));
        if std::mem::take(&mut self.suppress_completion) {
            self.dismiss_completion(cx);
        } else {
            self.request_completion(false, window, cx);
        }
        cx.notify();
    }
    fn dismiss_completion(&mut self, cx: &mut Context<Self>) {
        self.completion_generation = self.completion_generation.wrapping_add(1);
        self.completion_task = Task::ready(());
        if self.popup.take().is_some() {
            cx.notify();
        }
    }
    /// Completion runs off the input thread and results carry a document/cursor generation.
    pub fn request_completion(
        &mut self,
        explicit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_completion(cx);
        let generation = self.completion_generation;
        let index = self.index.clone();
        let text: Arc<str> = Arc::from(self.state.read(cx).value().as_ref());
        let cursor = self.state.read(cx).cursor();
        let driver = self.driver;
        if self.selections.len() != 1 || !self.state.read(cx).selected_range().is_empty() {
            return;
        }
        let task = cx.background_executor().spawn(async move {
            if CompletionIndex::allowed(&text, cursor, driver) {
                Some(index.query(&text, cursor, explicit))
            } else {
                None
            }
        });
        self.completion_task = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.completion_generation {
                    return;
                }
                this.popup = result.filter(|r| !r.items.is_empty());
                this.completion_selected = 0;
                cx.notify();
            });
        });
    }
    pub fn completion_count(&self) -> usize {
        self.popup.as_ref().map_or(0, |p| p.items.len())
    }
    pub fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let Some(item) = popup.items.get(self.completion_selected) else {
            return;
        };
        let range = popup.replace;
        let end = range.start + item.label.len();
        self.suppress_completion = true;
        self.state.update(cx, |s, cx| {
            s.edit_batch(&[(range, item.label.to_string())], window, cx);
            s.set_selection_offsets(&[(end, end)], cx);
        });
        self.focus(window, cx);
    }
    pub fn select_next_occurrence(&mut self, all: bool, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let mut offsets = self.state.read(cx).selection_offsets();
        let range = self.state.read(cx).selected_range();
        if range.is_empty() {
            let cursor = self.state.read(cx).cursor();
            let word =
                |c: char| c.is_alphanumeric() || c == '_' || ('\u{0e31}'..='\u{0e4e}').contains(&c);
            let start = text[..cursor]
                .char_indices()
                .rev()
                .take_while(|(_, c)| word(*c))
                .last()
                .map_or(cursor, |(i, _)| i);
            let end = cursor
                + text[cursor..]
                    .chars()
                    .take_while(|c| word(*c))
                    .map(char::len_utf8)
                    .sum::<usize>();
            if start == end {
                return;
            }
            offsets = vec![(start, end)];
        }
        let &(a, h) = offsets.first().unwrap();
        let needle = &text[a.min(h)..a.max(h)];
        if needle.is_empty() {
            return;
        }
        if all {
            offsets = text
                .match_indices(needle)
                .map(|(i, _)| (i, i + needle.len()))
                .collect();
        } else if !range.is_empty() {
            let end = range.end;
            let next = text[end..]
                .match_indices(needle)
                .map(|(i, _)| i + end)
                .chain(text[..end].match_indices(needle).map(|(i, _)| i))
                .find(|&i| !offsets.iter().any(|&(a, h)| a.min(h) <= i && i < a.max(h)));
            if let Some(i) = next {
                offsets.insert(0, (i, i + needle.len()));
            }
        }
        self.state
            .update(cx, |s, cx| s.set_selection_offsets(&offsets, cx));
    }
    pub fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        // CodeMirror aligns a selected block's comments to its minimum indent;
        // blank lines are skipped unless they are the sole selected line.
        let mut rows = std::collections::BTreeMap::new();
        let mut selections = self.state.read(cx).selection_offsets();
        selections.sort_by_key(|&(a, h)| a.min(h));
        for (a, h) in selections {
            let start = a.min(h);
            let end = a.max(h);
            let first = text[..start].rfind('\n').map_or(0, |i| i + 1);
            let mut starts = vec![first];
            starts.extend(
                text[first..end]
                    .match_indices('\n')
                    .map(|(i, _)| first + i + 1)
                    .filter(|&i| i < end),
            );
            let single = starts.len() == 1;
            let block: Vec<_> = starts
                .into_iter()
                .filter(|i| !rows.contains_key(i))
                .map(|start| {
                    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
                    let indent = text[start..end]
                        .chars()
                        .take_while(|c| c.is_whitespace())
                        .map(char::len_utf8)
                        .sum::<usize>();
                    (start, end, indent)
                })
                .collect();
            let min = block
                .iter()
                .filter(|(s, e, i)| s + i < *e)
                .map(|(_, _, i)| *i)
                .min();
            for (start, end, indent) in block {
                rows.insert(start, (end, indent, min.unwrap_or(indent), single));
            }
        }
        let comment = rows.iter().any(|(&start, &(end, indent, _, single))| {
            (single || start + indent < end) && !text[start + indent..end].starts_with("--")
        });
        let edits: Vec<_> = rows
            .iter()
            .filter_map(|(&start, &(end, indent, min, single))| {
                if comment {
                    if single || start + indent < end {
                        let at = start + if start + indent == end { indent } else { min };
                        Some((at..at, "-- ".into()))
                    } else {
                        None
                    }
                } else if text[start + indent..end].starts_with("--") {
                    let at = start + indent;
                    let n = 2 + usize::from(text[at + 2..end].starts_with(' '));
                    Some((at..at + n, String::new()))
                } else {
                    None
                }
            })
            .collect();
        self.suppress_completion = true;
        self.state
            .update(cx, |s, cx| s.edit_batch(&edits, window, cx));
    }
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_completion(cx);
        self.search_open = true;
        if let Some(selected) = self.selected_text(cx) {
            self.search.query = selected.clone();
            self.find
                .update(cx, |s, cx| s.set_value(selected, window, cx));
        }
        self.find.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.decorate(cx);
        cx.notify();
    }
    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.decorate(cx);
        self.focus(window, cx);
        cx.notify();
    }
    pub fn set_search(&mut self, query: SearchQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.find
            .update(cx, |s, cx| s.set_value(query.query.clone(), window, cx));
        self.replace.update(cx, |s, cx| {
            s.set_value(query.replacement.clone(), window, cx)
        });
        self.search = query;
        self.decorate(cx);
        cx.notify();
    }
    /// Current search matches, in UTF-16 units.
    pub fn search_matches(&self, cx: &App) -> Vec<Range<usize>> {
        let text = self.text(cx);
        self.search_matches
            .iter()
            .map(|r| utf16::from_byte(&text, r.start)..utf16::from_byte(&text, r.end))
            .collect()
    }
    pub fn find_next(&mut self, backward: bool, cx: &mut Context<Self>) {
        let range = self.state.read(cx).selected_range();
        let next = if backward {
            self.search_matches
                .iter()
                .rev()
                .find(|r| r.start < range.start)
                .or(self.search_matches.last())
        } else {
            self.search_matches
                .iter()
                .find(|r| r.start >= range.end && **r != range)
                .or(self.search_matches.first())
        }
        .cloned();
        if let Some(r) = next {
            self.state.update(cx, |s, cx| s.set_selected_range(r, cx));
        }
    }
    pub fn select_search_matches(&mut self, cx: &mut Context<Self>) {
        let selections: Vec<_> = self
            .search_matches
            .iter()
            .map(|r| (r.start, r.end))
            .collect();
        self.state
            .update(cx, |s, cx| s.set_selection_offsets(&selections, cx));
    }
    pub fn replace_matches(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(mut edits) = self.search.edits(&self.text(cx)) else {
            return;
        };
        if !all {
            let range = self.state.read(cx).selected_range();
            edits.retain(|(r, _)| *r == range);
            edits.truncate(1);
        }
        self.suppress_completion = true;
        self.state
            .update(cx, |s, cx| s.edit_batch(&edits, window, cx));
        if !all {
            self.decorate(cx);
            self.find_next(false, cx);
        }
    }
    fn decorate(&mut self, cx: &mut Context<Self>) {
        let text = self.state.read(cx).value();
        let range = self.state.read(cx).selected_range();
        let cursor = self.state.read(cx).cursor();
        let mut marks = vec![];
        if self.search_open {
            match self.search.matches(&text) {
                Ok(matches) => {
                    self.search_matches = matches;
                    self.search_error = None;
                }
                Err(e) => {
                    self.search_matches.clear();
                    self.search_error = Some(e.to_string());
                }
            }
            for r in &self.search_matches {
                marks
                    .push(RangeDecoration::new(r.clone()).with_color(
                        rgba(if *r == range { 0xff00ff8a } else { 0x00ffff8a }).into(),
                    ));
            }
        } else {
            self.search_matches.clear();
        }
        if !range.is_empty() && self.selections.len() == 1 {
            let needle = &text[range.clone()];
            if !needle.contains('\n') && (1..=200).contains(&needle.encode_utf16().count()) {
                let matches: Vec<_> = text.match_indices(needle).take(102).collect();
                if matches.len() <= 101 {
                    for (i, _) in matches {
                        if i != range.start {
                            marks.push(
                                RangeDecoration::new(i..i + needle.len())
                                    .with_color(rgba(0xffffff18).into()),
                            );
                        }
                    }
                }
            }
        } else if range.is_empty() && self.focused {
            for (range, matched) in syntax::brackets(&text, cursor, self.driver) {
                marks.push(
                    RangeDecoration::new(range)
                        .with_color(rgba(if matched { 0x328c8252 } else { 0xbb555544 }).into()),
                );
            }
        }
        self.decorations.set(marks, cx);
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = k.modifiers;
        let key = k.key.as_str();
        let editing = self.state.read(cx).focus_handle(cx).is_focused(window);
        let mut handled = true;
        if m.platform && key == "f" {
            self.open_find(window, cx);
        } else if m.platform && key == "g" && m.control && editing {
            self.select_next_occurrence(true, cx);
        } else if m.platform && key == "g" {
            self.find_next(m.shift, cx);
        } else if self.search_open && key == "escape" {
            self.close_find(window, cx);
        } else if self.search_open && !editing && key == "enter" {
            if self.replace.read(cx).focus_handle(cx).is_focused(window) {
                self.replace_matches(m.platform, window, cx);
            } else {
                self.find_next(m.shift, cx);
            }
        } else if !editing {
            handled = false;
        } else if m.platform && key == "enter" {
            cx.emit(if m.shift {
                EditorEvent::RunAll
            } else {
                EditorEvent::RunCurrent
            });
        } else if m.platform && key == "i" {
            cx.emit(if m.shift {
                EditorEvent::Uglify
            } else {
                EditorEvent::Beautify
            });
        } else if m.platform && key == "[" {
            cx.emit(EditorEvent::PreviousTab);
        } else if m.platform && key == "]" {
            cx.emit(EditorEvent::NextTab);
        } else if m.platform && m.shift && key == "k" {
            cx.emit(EditorEvent::SwitchConnection);
        } else if m.platform && key == "/" {
            self.toggle_comment(window, cx);
        } else if m.platform && key == "d" {
            self.select_next_occurrence(false, cx);
        } else if m.control && key == "space" {
            self.request_completion(true, window, cx);
        } else if let Some(popup) = &self.popup {
            match key {
                "escape" => self.dismiss_completion(cx),
                "enter" | "tab" => self.accept_completion(window, cx),
                "up" => {
                    self.completion_selected = self.completion_selected.saturating_sub(1);
                    cx.notify();
                }
                "down" => {
                    self.completion_selected =
                        (self.completion_selected + 1).min(popup.items.len() - 1);
                    cx.notify();
                }
                _ => handled = false,
            }
        } else {
            handled = false;
        }
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    }
    fn search_panel(&self, cx: &Context<Self>) -> Div {
        let field = |id: &'static str, state: &Entity<InputState>| {
            div()
                .id(id)
                .w(px(160.))
                .h(px(24.))
                .px(px(4.))
                .border_1()
                .border_color(rgb(0x555555))
                .bg(rgb(0x1a1a1a))
                .child(Input::new(state))
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(6.))
                .py(px(2.))
                .border_1()
                .border_color(rgb(0x555555))
                .rounded(px(3.))
                .bg(rgb(0x363636))
                .cursor_pointer()
                .child(label)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(6.))
            .bg(rgb(0x2d2d2d))
            .text_color(rgb(0xeeeeee))
            .text_size(px(12.))
            .line_height(px(18.))
            .font_family(theme::ui().family)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(field("find", &self.find))
                    .child(button("previous", "previous").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.find_next(true, cx)),
                    ))
                    .child(button("next", "next").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.find_next(false, cx)),
                    ))
                    .child(button("select-matches", "all").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.select_search_matches(cx)),
                    ))
                    .child(
                        button(
                            "case",
                            if self.search.case_sensitive {
                                "[x] match case"
                            } else {
                                "[ ] match case"
                            },
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.search.case_sensitive = !this.search.case_sensitive;
                                this.decorate(cx);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        button(
                            "regex",
                            if self.search.regex {
                                "[x] regexp"
                            } else {
                                "[ ] regexp"
                            },
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.search.regex = !this.search.regex;
                                this.decorate(cx);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        button(
                            "word",
                            if self.search.whole_word {
                                "[x] by word"
                            } else {
                                "[ ] by word"
                            },
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.search.whole_word = !this.search.whole_word;
                                this.decorate(cx);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(button("close-search", "x").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, w, cx| this.close_find(w, cx)),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(field("replace", &self.replace))
                    .child(button("replace-one", "replace").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, w, cx| this.replace_matches(false, w, cx)),
                    ))
                    .child(button("replace-all", "replace all").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, w, cx| this.replace_matches(true, w, cx)),
                    ))
                    .child(self.search_error.as_ref().map_or_else(
                        || format!("{} matches", self.search_matches.len()),
                        |_| "Invalid regular expression".into(),
                    )),
            )
    }
    fn completion_popup(&self, window: &Window, cx: &Context<Self>) -> Option<AnyElement> {
        let popup = self.popup.as_ref()?;
        let state = self.state.read(cx);
        let (caret, _) = state.cursor_layout()?;
        let input = state.input_bounds();
        let content_width = popup
            .items
            .iter()
            .map(|i| i.label.chars().count())
            .max()
            .unwrap_or(0) as f32
            * 7.8
            + 30.;
        let width = content_width
            .clamp(250., 700.)
            .min(f32::from(input.size.width).max(100.));
        let x = (f32::from(caret.left() - input.left()) - 3.)
            .clamp(0., (f32::from(input.size.width) - width).max(0.));
        let rows = popup.items.len().min(7);
        let height = rows as f32 * 18. + 2.;
        let below = f32::from(caret.bottom() - input.top());
        let y = if below + height <= f32::from(input.size.height) {
            below
        } else {
            (f32::from(caret.top() - input.top()) - height).max(0.)
        };
        let first = self.completion_selected.saturating_sub(6);
        let items = popup
            .items
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(i, item)| {
                div()
                    .id(("completion", i))
                    .h(px(18.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .bg(if i == self.completion_selected {
                        rgb(0x2f6fed)
                    } else {
                        rgb(0x2b2b2b)
                    })
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            window.prevent_default();
                            this.completion_selected = i;
                            this.accept_completion(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(16.))
                            .mr(px(6.))
                            .rounded(px(3.))
                            .bg(rgb(item.kind.color()))
                            .text_size(px(10.))
                            .line_height(px(16.))
                            .font_family(theme::ui().family)
                            .font_weight(FontWeight::BOLD)
                            .text_center()
                            .text_color(rgb(0xffffff))
                            .child(item.kind.badge()),
                    )
                    .child(item.label.to_string())
            });
        let _ = window;
        Some(
            deferred(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y + 4.))
                    .w(px(width))
                    .border_1()
                    .border_color(rgb(0x444444))
                    .rounded(px(6.))
                    .overflow_hidden()
                    .bg(rgb(0x2b2b2b))
                    .font_family(theme::mono().family)
                    .text_size(px(13.))
                    .line_height(px(15.6))
                    .text_color(rgb(0xffffff))
                    .children(items),
            )
            .with_priority(50)
            .into_any_element(),
        )
    }
}
impl Render for SqlEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = theme::mono();
        let mut root = div()
            .id("telecat-sql-editor")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BACKGROUND))
            .text_color(rgb(theme::FOREGROUND))
            .text_size(px(theme::FONT_SIZE))
            .line_height(px(theme::LINE_HEIGHT))
            .font_family(mono.family)
            .capture_key_down(cx.listener(Self::key))
            .on_action(cx.listener(|this, action: &crate::Shortcut, w, cx| {
                this.key(
                    &KeyDownEvent {
                        keystroke: Keystroke::parse(&action.key).unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    },
                    w,
                    cx,
                );
            }))
            .capture_action(
                cx.listener(|this, _: &telecat_editor_engine::input::Enter, w, cx| {
                    if this.popup.is_some()
                        || this.search_open && !this.state.read(cx).focus_handle(cx).is_focused(w)
                    {
                        this.key(
                            &KeyDownEvent {
                                keystroke: Keystroke::parse("enter").unwrap(),
                                is_held: false,
                                prefer_character_input: false,
                            },
                            w,
                            cx,
                        );
                    }
                }),
            )
            .capture_action(cx.listener(
                |this, _: &telecat_editor_engine::input::IndentInline, w, cx| {
                    if this.popup.is_some() {
                        this.accept_completion(w, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(
                cx.listener(|this, _: &telecat_editor_engine::input::MoveUp, _, cx| {
                    if this.popup.is_some() {
                        this.completion_selected = this.completion_selected.saturating_sub(1);
                        cx.notify();
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(
                |this, _: &telecat_editor_engine::input::MoveDown, _, cx| {
                    if let Some(popup) = &this.popup {
                        this.completion_selected =
                            (this.completion_selected + 1).min(popup.items.len() - 1);
                        cx.notify();
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(cx.listener(
                |this, _: &telecat_editor_engine::input::Escape, w, cx| {
                    if this.popup.is_some() {
                        this.dismiss_completion(cx);
                        cx.stop_propagation();
                    } else if this.search_open {
                        this.close_find(w, cx);
                        cx.stop_propagation();
                    }
                },
            ));
        root.style().text.font_fallbacks = mono.fallbacks;
        root.when_some(self.latency.clone(), |d, probe| {
            d.child(
                deferred(
                    canvas(|_, _, _| (), move |_, _, _, _| probe.painted())
                        .absolute()
                        .size_full(),
                )
                .with_priority(100),
            )
        })
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(Editor::new(&self.state))
                .children(self.completion_popup(window, cx)),
        )
        .when(self.search_open, |d| d.child(self.search_panel(cx)))
    }
}
