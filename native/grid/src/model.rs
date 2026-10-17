use crate::RowStore;
use crate::theme::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub numeric: bool,
    pub editable: bool,
    pub hidden: bool,
    pub chevron: bool,
}
impl Column {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            numeric: false,
            editable: false,
            hidden: false,
            chevron: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowState {
    #[default]
    Clean,
    Modified,
    Deleted,
    New,
}
/// Sparse edit metadata. `defaults` distinguishes JS undefined (DEFAULT) from SQL NULL.
/// It does not duplicate the caller's row strings.
#[derive(Clone, Debug, Default)]
pub struct Edits {
    pub rows: BTreeMap<usize, RowState>,
    pub modified: BTreeSet<Cell>,
    pub defaults: BTreeSet<Cell>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub rows: BTreeSet<usize>,
    pub anchor: Option<usize>,
}
impl Selection {
    pub fn single(row: usize) -> Self {
        Self {
            rows: [row].into(),
            anchor: Some(row),
        }
    }
    pub fn select(&mut self, row: usize, shift: bool, meta: bool) {
        if let Some(anchor) = self.anchor.filter(|_| shift) {
            self.rows = (anchor.min(row)..=anchor.max(row)).collect();
        } else if meta {
            if !self.rows.remove(&row) {
                self.rows.insert(row);
            }
            self.anchor = Some(row);
        } else if self.rows.len() != 1 || !self.rows.contains(&row) {
            *self = Self::single(row);
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: String,
    pub direction: Direction,
}
#[derive(Clone, Debug, Default)]
pub struct Hooks {
    pub sort: bool,
    pub edit: bool,
    pub delete: bool,
    pub insert: bool,
    pub duplicate: bool,
    pub paste: bool,
    pub detail: bool,
    pub quick_look: bool,
    pub popup: bool,
    /// Parent will handle Escape (e.g. discard staged edits), preserving selection.
    pub escape: bool,
}
#[derive(Clone, Debug)]
pub struct Options {
    pub row_offset: usize,
    pub show_row_numbers: bool,
    pub show_empty: bool,
    /// Baseline adjustment for the host's metadata form cells, in points.
    pub row_text_offset: f32,
    pub empty_text: String,
    pub hooks: Hooks,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            row_offset: 0,
            show_row_numbers: true,
            show_empty: false,
            row_text_offset: 0.,
            empty_text: String::new(),
            hooks: Hooks::default(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub enum GridEvent {
    Selection {
        selection: Selection,
        column: Option<usize>,
    },
    Edit {
        cell: Cell,
        value: String,
    },
    Sort(Option<Sort>),
    Resize {
        column: usize,
        width: f32,
    },
    Paste(Vec<Vec<String>>),
    ContextMenu {
        cell: Cell,
        bounds: Rect,
    },
    Menu {
        id: String,
        cell: Cell,
    },
    QuickLook(Cell),
    CellPopup {
        cell: Cell,
        bounds: Rect,
    },
    ToggleDetail,
    InsertRow,
    DuplicateRows(Vec<usize>),
    DeleteRows(Vec<usize>),
    Escape,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    Cell,
    RowTsv,
    RowJson,
    ColumnName,
    AllTsv,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editing {
    pub cell: Cell,
    pub value: String,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub meta: bool,
    pub control: bool,
    pub alt: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Enter,
    Escape,
    Delete,
    Space,
    Insert,
    Duplicate,
}

/// Owned row model; UI updates never clone this table. Mutate via DataGrid::update_model.
#[derive(Debug)]
pub struct GridModel {
    pub columns: Vec<Column>,
    pub rows: RowStore,
    pub edits: Edits,
    pub options: Options,
    pub selection: Selection,
    pub selected_column: Option<usize>,
    pub sort: Option<Sort>,
    pub editing: Option<Editing>,
    pub widths: Vec<f32>,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub viewport: Rect,
    last_edit_nonce: Option<u64>,
}
impl GridModel {
    pub fn new(columns: Vec<Column>, rows: Vec<Vec<Option<String>>>) -> Self {
        let widths = default_widths(&columns, &rows);
        Self {
            columns,
            rows: rows.into(),
            widths,
            edits: Edits::default(),
            options: Options::default(),
            selection: Selection::default(),
            selected_column: None,
            sort: None,
            editing: None,
            scroll_x: 0.,
            scroll_y: 0.,
            viewport: Rect::default(),
            last_edit_nonce: None,
        }
    }
    /// Stream pages without first allocating a 100k-row Vec of Strings.
    pub fn from_rows(
        columns: Vec<Column>,
        rows: impl IntoIterator<Item = Vec<Option<String>>>,
    ) -> Self {
        let rows: RowStore = rows.into_iter().collect();
        let widths = arena_widths(&columns, &rows);
        let mut model = Self::new(columns, vec![]);
        model.rows = rows;
        model.widths = widths;
        model
    }
    /// Match Electron: sample again only when the NUL-joined column names change.
    pub fn replace_data(&mut self, columns: Vec<Column>, rows: impl Into<RowStore>) {
        let rows = rows.into();
        let names = |cols: &[Column]| {
            cols.iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join("\0")
        };
        if names(&columns) != names(&self.columns) {
            self.widths = arena_widths(&columns, &rows);
        }
        self.columns = columns;
        self.rows = rows;
        self.reconcile();
    }
    pub fn reconcile(&mut self) {
        self.selection.rows.retain(|&r| r < self.rows.len());
        if self.selection.anchor.is_some_and(|r| r >= self.rows.len()) {
            self.selection.anchor = None;
        }
        if self
            .selected_column
            .is_some_and(|c| c >= self.columns.len())
        {
            self.selected_column = None;
        }
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.cell.row >= self.rows.len() || e.cell.col >= self.columns.len())
        {
            self.editing = None;
        }
        if self.widths.len() != self.columns.len() {
            self.widths = arena_widths(&self.columns, &self.rows);
        }
        self.clamp_scroll();
    }
    pub fn visible_columns(&self) -> impl Iterator<Item = usize> + '_ {
        self.columns
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.hidden)
            .map(|(i, _)| i)
    }
    pub fn number_width(&self) -> f32 {
        if self.options.show_row_numbers {
            (self
                .options
                .row_offset
                .saturating_add(self.rows.len())
                .to_string()
                .len() as f32
                * 9.
                + 22.)
                .max(44.)
        } else {
            0.
        }
    }
    pub fn total_width(&self) -> f32 {
        self.number_width() + self.visible_columns().map(|i| self.widths[i]).sum::<f32>()
    }
    /// Resolve overflow gutters from real content only. Filler stripes are painted
    /// through the viewport's bottom edge; their rounded-up last row cannot scroll.
    pub fn layout(&self) -> Layout {
        let real_height = if self.rows.is_empty() {
            0. // A clipped sticky header alone is not vertically scrollable.
        } else {
            HEADER_HEIGHT + self.rows.len() as f32 * ROW_HEIGHT
        };
        let total_width = self.total_width();
        let (mut vertical, mut horizontal) = (false, false);
        // With fixed content dimensions each gutter can only turn on, so this
        // settles even when a genuine scrollbar forces overflow on the other axis.
        for _ in 0..3 {
            let w = (self.viewport.width - if vertical { SCROLLBAR } else { 0. }).max(0.);
            let h = (self.viewport.height - if horizontal { SCROLLBAR } else { 0. }).max(0.);
            horizontal = total_width > w;
            vertical = real_height > h;
        }
        let width = (self.viewport.width - if vertical { SCROLLBAR } else { 0. }).max(0.);
        let height = (self.viewport.height - if horizontal { SCROLLBAR } else { 0. }).max(0.);
        Layout {
            width,
            height,
            content_height: real_height.max(height),
            content_width: total_width.max(width),
            horizontal,
            vertical,
        }
    }
    pub fn clamp_scroll(&mut self) {
        let l = self.layout();
        self.scroll_x = finite(self.scroll_x).clamp(0., (l.content_width - l.width).max(0.));
        self.scroll_y = finite(self.scroll_y).clamp(0., (l.content_height - l.height).max(0.));
    }
    pub fn scroll_to_row(&mut self, row: usize) {
        let h = (self.layout().height - HEADER_HEIGHT).max(ROW_HEIGHT);
        let top = row as f32 * ROW_HEIGHT;
        if top < self.scroll_y {
            self.scroll_y = top;
        }
        if top + ROW_HEIGHT > self.scroll_y + h {
            self.scroll_y = top + ROW_HEIGHT - h;
        }
        self.clamp_scroll();
    }
    pub fn scroll_to_cell(&mut self, cell: Cell) {
        self.scroll_to_row(cell.row);
        let x = self.column_left(cell.col);
        let num = self.number_width();
        let right = x + self.widths.get(cell.col).copied().unwrap_or(0.);
        if x < self.scroll_x + num {
            self.scroll_x = x - num;
        }
        if right > self.scroll_x + self.layout().width {
            self.scroll_x = right - self.layout().width;
        }
        self.clamp_scroll();
    }
    pub fn column_left(&self, col: usize) -> f32 {
        self.number_width()
            + self
                .visible_columns()
                .take_while(|&i| i != col)
                .map(|i| self.widths[i])
                .sum::<f32>()
    }
    pub fn cell_rect(&self, cell: Cell) -> Rect {
        Rect {
            x: self.column_left(cell.col) - self.scroll_x,
            y: HEADER_HEIGHT + cell.row as f32 * ROW_HEIGHT - self.scroll_y,
            width: self.widths.get(cell.col).copied().unwrap_or(0.),
            height: ROW_HEIGHT,
        }
    }
    pub fn cell_value(&self, cell: Cell) -> Option<&str> {
        if self.edits.defaults.contains(&cell) {
            return None;
        }
        self.rows.cell(cell.row, cell.col)
    }
    pub fn is_default(&self, cell: Cell) -> bool {
        self.edits.defaults.contains(&cell)
            || self.rows.get(cell.row).is_some_and(|r| cell.col >= r.len())
    }
    pub fn cell_text(&self, cell: Cell) -> &str {
        if self.is_default(cell) {
            "DEFAULT"
        } else {
            self.cell_value(cell).unwrap_or("NULL")
        }
    }
    pub fn can_edit(&self, col: usize) -> bool {
        self.options.hooks.edit && self.columns.get(col).is_some_and(|c| c.editable)
    }
    fn selection_event(&self) -> GridEvent {
        GridEvent::Selection {
            selection: self.selection.clone(),
            column: self.selected_column,
        }
    }
    pub fn click(&mut self, row: usize, col: Option<usize>, m: Modifiers) -> Vec<GridEvent> {
        if row >= self.rows.len() {
            return vec![];
        }
        self.selection.select(row, m.shift, m.meta);
        // Row-number clicks preserve selCol, just as the DOM row handler does.
        if let Some(c) = col {
            self.selected_column = Some(c);
        }
        vec![self.selection_event()]
    }
    pub fn context_menu(&mut self, cell: Cell, bounds: Rect) -> Vec<GridEvent> {
        if cell.row >= self.rows.len() || cell.col >= self.columns.len() {
            return vec![];
        }
        if !self.selection.rows.contains(&cell.row) {
            self.selection = Selection::single(cell.row);
        }
        self.selected_column = Some(cell.col);
        vec![
            self.selection_event(),
            GridEvent::ContextMenu { cell, bounds },
        ]
    }
    pub fn click_header(&mut self, col: usize) -> Vec<GridEvent> {
        if !self.options.hooks.sort || col >= self.columns.len() {
            return vec![];
        }
        let name = &self.columns[col].name;
        let next = match &self.sort {
            Some(s) if &s.column == name && s.direction == Direction::Desc => None,
            Some(s) if &s.column == name => Some(Sort {
                column: name.clone(),
                direction: Direction::Desc,
            }),
            _ => Some(Sort {
                column: name.clone(),
                direction: Direction::Asc,
            }),
        };
        // Caller controls sort; a guarded/rejected request must not change the indicator.
        vec![GridEvent::Sort(next)]
    }
    pub fn resize(&mut self, col: usize, width: f32) -> Vec<GridEvent> {
        let Some(column) = self.columns.get(col) else {
            return vec![];
        };
        let width = finite(width).max(40.);
        for (i, c) in self.columns.iter().enumerate() {
            if c.name == column.name {
                self.widths[i] = width;
            }
        }
        let event = GridEvent::Resize { column: col, width };
        self.clamp_scroll();
        vec![event]
    }
    pub fn begin_edit(&mut self, cell: Cell) -> bool {
        if !self.can_edit(cell.col) || cell.row >= self.rows.len() {
            return false;
        }
        self.open_editor(cell);
        true
    }
    fn open_editor(&mut self, cell: Cell) {
        let value = self.cell_value(cell).unwrap_or("").to_owned();
        self.editing = Some(Editing { cell, value });
    }
    /// Explicit editRequest bypasses editable like Electron's effect; nonce deduplicates rerenders.
    pub fn request_edit(&mut self, cell: Cell, nonce: u64) -> Vec<GridEvent> {
        if self.last_edit_nonce == Some(nonce) {
            return vec![];
        }
        self.last_edit_nonce = Some(nonce);
        if cell.row >= self.rows.len() || cell.col >= self.columns.len() {
            return vec![];
        }
        self.selection = Selection::single(cell.row);
        self.selected_column = Some(cell.col);
        self.scroll_to_cell(cell);
        self.open_editor(cell);
        vec![self.selection_event()]
    }
    pub fn finish_edit(&mut self, commit: bool, movement: i8) -> Vec<GridEvent> {
        let Some(e) = self.editing.take() else {
            return vec![];
        };
        let mut events = vec![];
        if commit && e.value != self.cell_value(e.cell).unwrap_or("") {
            events.push(GridEvent::Edit {
                cell: e.cell,
                value: e.value,
            });
        }
        if movement != 0 {
            let visible: Vec<_> = self.visible_columns().collect();
            if let Some(at) = visible.iter().position(|&i| i == e.cell.col) {
                let next = if movement > 0 {
                    visible[at + 1..]
                        .iter()
                        .copied()
                        .find(|&c| self.can_edit(c))
                } else {
                    visible[..at]
                        .iter()
                        .rev()
                        .copied()
                        .find(|&c| self.can_edit(c))
                };
                if let Some(col) = next {
                    let cell = Cell {
                        row: e.cell.row,
                        col,
                    };
                    self.selected_column = Some(col);
                    self.open_editor(cell);
                    self.scroll_to_cell(cell);
                    events.push(self.selection_event());
                }
            }
        }
        events
    }
    pub fn navigate(&mut self, key: Key, m: Modifiers) -> Vec<GridEvent> {
        if self.editing.is_some() {
            return match key {
                Key::Escape => self.finish_edit(false, 0),
                Key::Enter if !m.alt => self.finish_edit(true, 0),
                Key::Tab => self.finish_edit(true, if m.shift { -1 } else { 1 }),
                _ => vec![],
            };
        }
        let hooks = &self.options.hooks;
        let selected: Vec<_> = self.selection.rows.iter().copied().collect();
        match key {
            Key::Space if !m.meta && !m.control && hooks.detail => {
                return vec![GridEvent::ToggleDetail];
            }
            Key::Insert if hooks.insert && !m.shift && !m.alt => return vec![GridEvent::InsertRow],
            Key::Duplicate if hooks.duplicate && !selected.is_empty() && !m.shift && !m.alt => {
                return vec![GridEvent::DuplicateRows(selected)];
            }
            Key::Delete if hooks.delete && !selected.is_empty() => {
                return vec![GridEvent::DeleteRows(selected)];
            }
            Key::Escape => {
                if hooks.escape {
                    return vec![GridEvent::Escape];
                }
                self.selection = Selection::default();
                self.selected_column = None;
                return vec![self.selection_event()];
            }
            _ => {}
        }
        let Some(cur) = self.selection.anchor.filter(|&r| r < self.rows.len()) else {
            return vec![];
        };
        let last = self.rows.len() - 1;
        let page = ((self.layout().height - HEADER_HEIGHT) / ROW_HEIGHT)
            .floor()
            .max(1.) as usize;
        let target = match key {
            Key::Up => Some(cur.saturating_sub(1)),
            Key::Down => Some((cur + 1).min(last)),
            Key::PageUp => Some(cur.saturating_sub(page)),
            Key::PageDown => Some(cur.saturating_add(page).min(last)),
            Key::Home => Some(0),
            Key::End => Some(last),
            _ => None,
        };
        if let Some(row) = target {
            if m.shift {
                self.selection.select(row, true, false);
            } else {
                self.selection = Selection::single(row);
            }
            self.scroll_to_row(row);
            return vec![self.selection_event()];
        }
        match key {
            Key::Left => {
                if let Some(c) = self.selected_column {
                    self.selected_column = Some(c.saturating_sub(1));
                }
            }
            Key::Right => {
                if let Some(c) = self.selected_column {
                    self.selected_column = Some((c + 1).min(self.columns.len().saturating_sub(1)));
                }
            }
            Key::Enter => {
                if let Some(col) = self.selected_column {
                    self.begin_edit(Cell { row: cur, col });
                }
                return vec![];
            }
            Key::Tab => {
                let cols: Vec<_> = self.visible_columns().collect();
                if cols.is_empty() {
                    return vec![];
                }
                let at = self
                    .selected_column
                    .and_then(|c| cols.iter().position(|&v| v == c));
                let (row, i) = match (at, m.shift) {
                    (None, false) => (cur, 0),
                    (None, true) => (cur, cols.len() - 1),
                    (Some(0), true) if cur > 0 => (cur - 1, cols.len() - 1),
                    (Some(i), false) if i + 1 == cols.len() && cur < last => (cur + 1, 0),
                    (Some(i), true) => (cur, i.saturating_sub(1)),
                    (Some(i), false) => (cur, (i + 1).min(cols.len() - 1)),
                };
                self.selected_column = Some(cols[i]);
                self.selection = Selection::single(row);
                self.scroll_to_cell(Cell { row, col: cols[i] });
            }
            _ => return vec![],
        }
        vec![self.selection_event()]
    }
    fn row_tsv(&self, row: usize) -> String {
        (0..self.rows.get(row).map_or(0, |r| r.len()))
            .map(|col| self.cell_text(Cell { row, col }))
            .collect::<Vec<_>>()
            .join("\t")
    }
    pub fn selected_tsv(&self) -> String {
        if self.selection.rows.len() == 1 {
            if let (Some(&row), Some(col)) = (self.selection.rows.first(), self.selected_column) {
                return self.cell_text(Cell { row, col }).to_owned();
            }
        }
        self.selection
            .rows
            .iter()
            .filter(|&&r| r < self.rows.len())
            .map(|&r| self.row_tsv(r))
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn copy(&self, kind: CopyKind, cell: Cell) -> String {
        if cell.row >= self.rows.len() || cell.col >= self.columns.len() {
            return String::new();
        }
        match kind {
            CopyKind::Cell => self.cell_text(cell).to_owned(),
            CopyKind::RowTsv => self.row_tsv(cell.row),
            CopyKind::ColumnName => self.columns[cell.col].name.clone(),
            CopyKind::AllTsv => std::iter::once(
                self.columns
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join("\t"),
            )
            .chain((0..self.rows.len()).map(|r| self.row_tsv(r)))
            .collect::<Vec<_>>()
            .join("\n"),
            CopyKind::RowJson => {
                let mut obj = serde_json::Map::new();
                for (col, c) in self.columns.iter().enumerate() {
                    obj.insert(
                        c.name.clone(),
                        self.cell_value(Cell { row: cell.row, col })
                            .map_or(serde_json::Value::Null, |v| {
                                serde_json::Value::String(v.into())
                            }),
                    );
                }
                // Object.fromEntries/JSON.stringify enumerate integer-index keys first.
                let mut indexed: Vec<_> = obj
                    .keys()
                    .filter_map(|k| {
                        k.parse::<u32>()
                            .ok()
                            .filter(|n| *n != u32::MAX && n.to_string() == *k)
                            .map(|n| (n, k.clone()))
                    })
                    .collect();
                indexed.sort_by_key(|v| v.0);
                let mut ordered = serde_json::Map::new();
                for (_, k) in indexed {
                    ordered.insert(k.clone(), obj.shift_remove(&k).unwrap());
                }
                ordered.extend(obj);
                serde_json::to_string_pretty(&ordered).unwrap_or_default()
            }
        }
    }
    pub fn paste(&self, text: &str) -> Vec<GridEvent> {
        if !self.options.hooks.paste || self.editing.is_some() || text.is_empty() {
            vec![]
        } else {
            vec![GridEvent::Paste(parse_tsv(text))]
        }
    }
    /// Owned allocations; excludes allocator overhead, toolkit and process/GPU memory.
    pub fn storage_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.rows.storage_bytes()
            + self.columns.capacity() * std::mem::size_of::<Column>()
            + self
                .columns
                .iter()
                .map(|c| c.name.capacity())
                .sum::<usize>()
            + self.widths.capacity() * std::mem::size_of::<f32>()
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub content_width: f32,
    pub content_height: f32,
    pub horizontal: bool,
    pub vertical: bool,
}
fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0. }
}
pub fn default_widths(columns: &[Column], rows: &[Vec<Option<String>>]) -> Vec<f32> {
    let widths: BTreeMap<_, _> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut max = c.name.encode_utf16().count() + 2;
            for row in rows.iter().take(60) {
                let len = row
                    .get(i)
                    .and_then(Option::as_ref)
                    .map_or(7, |v| v.encode_utf16().take(60).count());
                max = max.max(len);
            }
            (
                c.name.as_str(),
                (max as f32 * 7.6 + 22. + if c.chevron { 16. } else { 0. })
                    .round()
                    .clamp(60., 420.),
            )
        })
        .collect();
    columns.iter().map(|c| widths[c.name.as_str()]).collect()
}
pub fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let text = text.replace('\r', "");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    text.split('\n')
        .map(|l| l.split('\t').map(str::to_owned).collect())
        .collect()
}
pub fn filler_count(rows: usize, height: f32) -> usize {
    (((height - HEADER_HEIGHT).max(0.) / ROW_HEIGHT).ceil() as usize).saturating_sub(rows)
}
pub fn virtual_range(rows: usize, scroll: f32, height: f32, overscan: usize) -> Range<usize> {
    let first = (finite(scroll).max(0.) / ROW_HEIGHT).floor() as usize;
    let end = ((finite(scroll).max(0.) + finite(height).max(0.)) / ROW_HEIGHT).ceil() as usize;
    first.saturating_sub(overscan).min(rows)..end.saturating_add(overscan).min(rows)
}

/// Append these after the caller's extra menu items (and a separator when extras exist).
/// A None entry is Electron's separator. Pass chosen IDs to DataGrid::activate_menu.
pub const COPY_MENU: &[Option<(&str, &str)>] = &[
    Some(("cell", "Copy")),
    Some(("row-tsv", "Copy Row")),
    Some(("row-json", "Copy Row as JSON")),
    Some(("col", "Copy Column Name")),
    None,
    Some(("all-tsv", "Copy All Rows (TSV)")),
];

fn arena_widths(columns: &[Column], rows: &RowStore) -> Vec<f32> {
    default_widths(
        columns,
        &rows
            .iter()
            .take(60)
            .map(|r| r.to_owned())
            .collect::<Vec<_>>(),
    )
}
