//! Shell state and editor/grid integration boundary. No GPUI or component dependencies.
use crate::table_model::{EditBuffer, Part, TableEditor};
use std::time::{Duration, Instant};
use telecat_core::changes::{RowVerify, Stmt, table_op_statement};
use telecat_core::position::{self, Location};
use telecat_core::result_edit::ResultEditPlan;
use telecat_core::{sql, types::*};

/// How long a load runs before its indicator appears, so fast loads never flicker.
pub const LOADING_INDICATOR_DELAY: Duration = Duration::from_millis(150);
/// Which load of a tab: a table part's data/structure/index, or a query's / the running list's
/// results. Matches the tab's grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoadSlot {
    Part(Part),
    Results,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub enum LoadState {
    #[default]
    Idle,
    Loading {
        since: Instant,
    },
    Loaded,
    Failed(String),
    Cancelled,
}
/// What a view shows in place of (or over) its content.
#[derive(Clone, Debug, PartialEq)]
pub enum Placeholder {
    /// Show the content as it is.
    None,
    /// A load just started: change nothing yet (no flicker for fast loads).
    Pending,
    /// "Loading…"; any previous content stays visible but dimmed.
    Loading {
        dimmed: bool,
    },
    /// The load finished with nothing to show ("No rows").
    Empty,
    Failed(String),
    Cancelled,
}
impl LoadState {
    pub fn loading() -> Self {
        Self::Loading {
            since: Instant::now(),
        }
    }
    /// The outcome of a load from its result: a cancelled read is not an error.
    pub fn finished<T>(result: &Result<T, String>) -> Self {
        match result {
            Ok(_) => Self::Loaded,
            Err(e) if e.contains("was cancelled") || e.contains("cancelled by the user") => {
                Self::Cancelled
            }
            Err(e) => Self::Failed(e.clone()),
        }
    }
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }
    pub fn placeholder(&self, has_content: bool, now: Instant) -> Placeholder {
        match self {
            Self::Idle => Placeholder::None,
            Self::Loading { since }
                if now.saturating_duration_since(*since) < LOADING_INDICATOR_DELAY =>
            {
                Placeholder::Pending
            }
            Self::Loading { .. } => Placeholder::Loading {
                dimmed: has_content,
            },
            Self::Loaded if has_content => Placeholder::None,
            Self::Loaded => Placeholder::Empty,
            Self::Failed(e) => Placeholder::Failed(e.clone()),
            Self::Cancelled => Placeholder::Cancelled,
        }
    }
}
#[derive(Debug, Clone)]
pub struct StagedOperation {
    pub object: DbObject,
    pub truncate: bool,
}
#[derive(Debug, Clone)]
pub enum CommitTarget {
    Table(u64, Part),
    /// Edits to the shown result of a query tab.
    Result(u64),
    Operation(StagedOperation),
}
#[derive(Debug, Clone)]
pub struct CommitUnit {
    pub target: CommitTarget,
    pub statements: Vec<Stmt>,
    pub transaction: bool,
    /// Session settings the values were typed under (query results); empty for table edits.
    pub session: Vec<(String, String)>,
    /// See `CommitGroup::verify`.
    pub verify: Vec<RowVerify>,
}
/// What one ⌘S sends to the server in one call.
#[derive(Debug, Clone)]
pub struct CommitBatch {
    pub targets: Vec<CommitTarget>,
    pub statements: Vec<Stmt>,
    pub transaction: bool,
    pub session: Vec<(String, String)>,
    pub verify: Vec<RowVerify>,
}
impl CommitBatch {
    fn of(unit: CommitUnit) -> Self {
        Self {
            targets: vec![unit.target],
            statements: unit.statements,
            transaction: unit.transaction,
            session: unit.session,
            verify: unit.verify,
        }
    }
}
/// Groups a save so it is all-or-nothing wherever the server allows it.
///
/// PostgreSQL runs everything, DDL and TRUNCATE/DROP included, in one transaction. MySQL commits
/// DDL implicitly, so its row edits for every table run first in one transaction, followed by
/// each structure change or table operation on its own. Row edits only depend on their own
/// table, whose structure changes already followed them, so moving them first keeps every
/// dependency in order.
///
/// Edits typed under another session's settings (query results) are saved on their own, with
/// those settings, so they never change how the other edits are read.
pub fn commit_batches(units: Vec<CommitUnit>, driver: Driver) -> Vec<CommitBatch> {
    let (own, units): (Vec<_>, Vec<_>) = units.into_iter().partition(|u| !u.session.is_empty());
    let mut batches = plain_batches(units, driver);
    batches.extend(own.into_iter().map(CommitBatch::of));
    batches
}
fn plain_batches(units: Vec<CommitUnit>, driver: Driver) -> Vec<CommitBatch> {
    if driver == Driver::Postgres {
        let mut all = CommitBatch {
            targets: vec![],
            statements: vec![],
            transaction: true,
            session: vec![],
            verify: vec![],
        };
        for unit in units {
            all.targets.push(unit.target);
            all.statements.extend(unit.statements);
            all.verify.extend(unit.verify);
        }
        return if all.statements.is_empty() {
            vec![]
        } else {
            vec![all]
        };
    }
    let (rows, rest): (Vec<_>, Vec<_>) = units.into_iter().partition(|u| u.transaction);
    let mut out = vec![];
    if !rows.is_empty() {
        let mut first = CommitBatch {
            targets: vec![],
            statements: vec![],
            transaction: true,
            session: vec![],
            verify: vec![],
        };
        for unit in rows {
            first.targets.push(unit.target);
            first.statements.extend(unit.statements);
            first.verify.extend(unit.verify);
        }
        out.push(first);
    }
    out.extend(rest.into_iter().map(CommitBatch::of));
    out
}
/// For a save MySQL can't make atomic, the explanation to confirm before anything runs.
pub fn partial_commit_notice(batches: &[CommitBatch]) -> Option<String> {
    let immediate = batches.iter().filter(|b| !b.transaction).count();
    if batches.len() < 2 || immediate == 0 {
        return None;
    }
    let rows = batches.iter().any(|b| b.transaction);
    Some(format!(
        "MySQL commits structure changes and table operations immediately, so this save can't be undone as a whole.{} Then {immediate} step{} run one at a time. If one fails, the steps before it stay saved and the rest are not run.",
        if rows {
            " Row edits are saved first, together in one transaction."
        } else {
            ""
        },
        if immediate == 1 { "" } else { "s" }
    ))
}

#[derive(Debug, Clone, Default)]
pub struct EditorDocument {
    pub text: String,
    /// Electron/CodeMirror positions count UTF-16 code units.
    pub cursor_utf16: usize,
    pub selection: Option<String>,
    /// Where the selection starts (UTF-16), so errors in a selected run map into the document.
    pub selection_from: usize,
}
#[derive(Debug, Clone, Default)]
pub struct GridDocument {
    pub revision: u64,
    pub result: QueryResult,
    pub selected_row: Option<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKind {
    Query,
    Table(TableRef),
    Running,
}
#[derive(Debug, Clone)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub kind: TabKind,
    pub editor: EditorDocument,
    pub grid: GridDocument,
    pub results: Vec<QueryResult>,
    pub shown: usize,
    pub message: bool,
    pub total: u64,
    pub offset: u64,
    pub limit: u64,
    pub info: Option<TableInfo>,
    pub table: Option<TableEditor>,
    pub loads: std::collections::BTreeMap<LoadSlot, LoadState>,
    /// The shown query result's edits, once its columns were mapped onto tables.
    pub result_edit: Option<ResultEditor>,
    /// For each of `results`, where its error points in the SQL that ran.
    pub error_locations: Vec<Option<Location>>,
    /// The editor text the results were produced from.
    pub run_text: String,
    /// Notices about this tab's server session (it was reset by a reconnect), shown after the
    /// results in Messages.
    pub notes: Vec<String>,
}
/// Staged edits to a query result: rows, undo and discard work as for a table's Data grid.
#[derive(Debug, Clone)]
pub struct ResultEditor {
    pub plan: ResultEditPlan,
    pub data: EditBuffer,
    /// The grid revision the plan was made for.
    pub revision: u64,
    /// Made on a session that was since replaced by a reconnect: checked against the new one
    /// before its staged edits are saved.
    pub recheck: bool,
}
/// Why a result read before a reconnect can't be edited any more.
pub const RESULT_BEFORE_RECONNECT: &str =
    "re-run the query after reconnecting: these results were read on the previous connection";
impl ResultEditor {
    pub fn new(plan: ResultEditPlan, result: QueryResult, revision: u64) -> Self {
        let mut data = EditBuffer::default();
        data.reset(result, None);
        Self {
            plan,
            data,
            revision,
            recheck: false,
        }
    }
    /// Stages `value` into cell (`row`, `col`) and every displayed copy of it, as one undo step.
    pub fn stage(
        &mut self,
        row: usize,
        col: usize,
        value: telecat_core::changes::Cell,
    ) -> Result<(), String> {
        if let Some(r) = self.plan.cell_reason(&self.data.rows, row, col) {
            return Err(r);
        }
        let plan = &self.plan;
        let mut result = Ok(());
        self.data
            .apply(|rows| result = plan.stage(rows, row, col, value));
        result
    }
    pub fn delete(&mut self, rows: &[usize]) -> Result<(), String> {
        self.plan.delete.clone()?;
        let plan = &self.plan;
        let mut result = Ok(());
        self.data.apply(|all| result = plan.delete(all, rows));
        result
    }
}
impl Tab {
    /// The load shown by this tab's current grid.
    pub fn slot(&self) -> LoadSlot {
        self.table
            .as_ref()
            .map_or(LoadSlot::Results, |t| LoadSlot::Part(t.part))
    }
    pub fn load(&self, slot: LoadSlot) -> &LoadState {
        static IDLE: LoadState = LoadState::Idle;
        self.loads.get(&slot).unwrap_or(&IDLE)
    }
    fn new(id: u64, title: String, kind: TabKind) -> Self {
        Self {
            id,
            title,
            kind,
            editor: EditorDocument::default(),
            grid: GridDocument::default(),
            results: vec![],
            shown: 0,
            message: false,
            total: 0,
            offset: 0,
            limit: 1000,
            info: None,
            loads: Default::default(),
            table: None,
            result_edit: None,
            error_locations: vec![],
            run_text: String::new(),
            notes: vec![],
        }
    }
    /// The Messages panel's text: each statement and its outcome. Errors say where they are,
    /// with the line they point into and a caret.
    pub fn messages_text(&self) -> String {
        let results = self.results.iter().enumerate().map(|(i, r)| {
            let outcome = if let Some(e) = &r.error {
                match self.error_locations.get(i).copied().flatten() {
                    Some(at) => format!(
                        "Query {} ERROR: Error at line {}, column {}: {e}\n{}",
                        i + 1,
                        at.line,
                        at.column,
                        position::caret(&self.run_text, at)
                    ),
                    None => {
                        // Located only inside a function body: say so plainly.
                        let body_line = r.error_context.as_deref().and_then(|c| {
                            c.split("line ")
                                .nth(1)?
                                .split_whitespace()
                                .next()
                                .map(str::to_owned)
                        });
                        match body_line {
                            Some(l) => format!(
                                "Query {} ERROR (line {l} of the function body): {e}",
                                i + 1
                            ),
                            None => format!("Query {} ERROR: {e}", i + 1),
                        }
                    }
                }
            } else if !r.columns.is_empty() {
                format!("OK, {} rows returned ({} ms)", r.rows.len(), r.duration_ms)
            } else {
                format!(
                    "OK, {} row{} affected ({} ms){}",
                    r.affected_rows.unwrap_or(0),
                    if r.affected_rows == Some(1) { "" } else { "s" },
                    r.duration_ms,
                    r.command
                        .as_ref()
                        .map(|c| format!(" — {c}"))
                        .unwrap_or_default()
                )
            };
            if r.sql.is_empty() {
                outcome
            } else {
                format!("{}\n{outcome}", r.sql)
            }
        });
        results
            .chain(self.notes.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    /// Results of running `plan` on `text`, with each error mapped to its line and column.
    pub fn set_run_results(&mut self, results: Vec<QueryResult>, plan: &QueryPlan, text: &str) {
        let locations = results
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let error = r.error.as_ref()?;
                let start = *plan.starts.get(i)?;
                let statement = plan.statements.get(i)?;
                match (r.error_position, &r.error_context) {
                    (Some(p), _) => Some(position::postgres(text, start, p)),
                    // Inside a PL/pgSQL body: the CONTEXT line within the body.
                    (None, Some(context)) => position::plpgsql(
                        text,
                        start,
                        statement,
                        context,
                        r.error_internal.as_ref().map(|(p, q)| (*p, q.as_str())),
                    ),
                    (None, None) => position::mysql(text, start, statement, error),
                }
            })
            .collect();
        self.set_results(results);
        self.error_locations = locations;
        self.run_text = text.to_owned();
    }
    /// Attaches the edit plan made for the result shown at grid revision `revision`. A plan for
    /// a result that is no longer shown is ignored.
    pub fn attach_result_plan(&mut self, revision: u64, plan: ResultEditPlan) {
        if self.grid.revision == revision && self.kind == TabKind::Query {
            self.result_edit = Some(ResultEditor::new(plan, self.grid.result.clone(), revision));
        }
    }
    pub fn result_dirty(&self) -> bool {
        self.result_edit.as_ref().is_some_and(|r| r.data.dirty())
    }
    pub fn set_results(&mut self, results: Vec<QueryResult>) {
        self.result_edit = None;
        self.error_locations.clear();
        let data: Vec<_> = results
            .iter()
            .filter(|r| !r.columns.is_empty() && r.error.is_none())
            .collect();
        self.shown = data.len().saturating_sub(1);
        self.grid = GridDocument {
            revision: self.grid.revision.wrapping_add(1),
            result: data.last().map(|r| (*r).clone()).unwrap_or_default(),
            selected_row: None,
        };
        self.message = data.is_empty() || results.iter().any(|r| r.error.is_some());
        self.results = results;
    }
    pub fn select_result(&mut self, index: usize) {
        if self.result_dirty() {
            return;
        }
        self.result_edit = None;
        if let Some(r) = self
            .results
            .iter()
            .filter(|r| !r.columns.is_empty() && r.error.is_none())
            .nth(index)
        {
            self.shown = index;
            self.grid = GridDocument {
                revision: self.grid.revision.wrapping_add(1),
                result: r.clone(),
                selected_row: None,
            };
        }
    }
}
#[derive(Debug, Clone)]
pub struct WorkspaceState {
    pub connection: ConnectionConfig,
    pub server_version: String,
    pub tls: Option<String>,
    pub user: String,
    pub database: String,
    pub objects: Vec<DbObject>,
    pub objects_load: LoadState,
    pub tabs: Vec<Tab>,
    pub active: u64,
    pub logs: Vec<LogEntry>,
    pub operations: Vec<StagedOperation>,
    next_id: u64,
    navigation: Vec<u64>,
    nav_pos: usize,
}
impl WorkspaceState {
    pub fn new(connection: ConnectionConfig) -> Self {
        Self {
            user: connection.user.clone(),
            database: connection.database.clone(),
            connection,
            server_version: String::new(),
            tls: None,
            objects: vec![],
            objects_load: LoadState::Idle,
            tabs: vec![Tab::new(1, "SQL Query".into(), TabKind::Query)],
            active: 1,
            logs: vec![],
            operations: vec![],
            next_id: 2,
            navigation: vec![1],
            nav_pos: 0,
        }
    }
    pub fn current(&self) -> &Tab {
        self.tabs
            .iter()
            .find(|t| t.id == self.active)
            .expect("workspace always has an active tab")
    }
    pub fn collect_changes(&self) -> anyhow::Result<Vec<CommitUnit>> {
        let mut tables: Vec<_> = self
            .tabs
            .iter()
            .filter_map(|t| t.table.as_ref().map(|e| (t.id, e)))
            .collect();
        tables.sort_by_key(|(_, e)| !e.is_new);
        let mut units = vec![];
        for (id, table) in tables {
            for group in table.groups(self.connection.driver, id)? {
                units.push(CommitUnit {
                    target: CommitTarget::Table(group.tab_id, group.part),
                    statements: group.statements,
                    transaction: group.transaction,
                    session: vec![],
                    verify: group.verify,
                });
            }
        }
        for t in &self.tabs {
            if let Some(r) = t.result_edit.as_ref().filter(|r| r.data.dirty()) {
                units.push(CommitUnit {
                    target: CommitTarget::Result(t.id),
                    statements: r.plan.statements(&r.data.rows)?,
                    transaction: true,
                    session: r.plan.session.clone(),
                    verify: vec![],
                });
            }
        }
        let mut ops = self.operations.clone();
        ops.sort_by_key(|o| !o.truncate);
        units.extend(ops.into_iter().map(|op| CommitUnit {
            statements: vec![table_op_statement(
                self.connection.driver,
                &op.object.table,
                op.truncate,
                op.object.kind == ObjectKind::View,
            )],
            transaction: false,
            session: vec![],
            verify: vec![],
            target: CommitTarget::Operation(op),
        }));
        Ok(units)
    }
    pub fn has_changes(&self) -> bool {
        !self.operations.is_empty()
            || self
                .tabs
                .iter()
                .any(|t| t.table.as_ref().is_some_and(TableEditor::dirty) || t.result_dirty())
    }
    pub fn committed(&mut self, target: &CommitTarget) {
        match target {
            CommitTarget::Result(id) => {
                if let Some(r) = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.id == *id)
                    .and_then(|t| t.result_edit.as_mut())
                {
                    // The saved values are shown in place; rows keep their identities.
                    r.data.accepted();
                    r.data.loaded = true;
                }
            }
            CommitTarget::Table(id, part) => {
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == *id) {
                    if let Some(e) = &mut t.table {
                        e.committed(*part);
                        t.kind = TabKind::Table(e.table.clone());
                        t.title = display_name(&e.table, self.connection.driver);
                    }
                }
            }
            CommitTarget::Operation(op) => {
                self.operations
                    .retain(|o| o.object.table != op.object.table);
                if !op.truncate {
                    self.objects.retain(|o| o.table != op.object.table);
                    if let Some(id) = self
                        .tabs
                        .iter()
                        .find(|t| t.kind == TabKind::Table(op.object.table.clone()))
                        .map(|t| t.id)
                    {
                        if self.tabs.len() == 1 {
                            self.open_query("SQL Query", "");
                        }
                        self.close_tab(id);
                    }
                }
            }
        }
    }
    pub fn current_mut(&mut self) -> &mut Tab {
        self.tabs
            .iter_mut()
            .find(|t| t.id == self.active)
            .expect("workspace always has an active tab")
    }
    pub fn activate(&mut self, id: u64) {
        if self.active == id || !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        self.active = id;
        self.navigation.truncate(self.nav_pos + 1);
        self.navigation.push(id);
        self.nav_pos = self.navigation.len() - 1;
    }
    pub fn can_back(&self) -> bool {
        self.nav_pos > 0
    }
    pub fn can_forward(&self) -> bool {
        self.nav_pos + 1 < self.navigation.len()
    }
    pub fn navigate(&mut self, forward: bool) {
        if forward && self.can_forward() {
            self.nav_pos += 1;
        }
        if !forward && self.can_back() {
            self.nav_pos -= 1;
        }
        self.active = self.navigation[self.nav_pos];
    }
    pub fn step(&mut self, delta: isize) {
        let index = self
            .tabs
            .iter()
            .position(|t| t.id == self.active)
            .unwrap_or(0);
        let next = (index as isize + delta).rem_euclid(self.tabs.len() as isize) as usize;
        self.activate(self.tabs[next].id);
    }
    fn open(&mut self, title: String, kind: TabKind) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab::new(id, title, kind));
        self.activate(id);
        id
    }
    pub fn open_query(&mut self, title: &str, sql: &str) -> u64 {
        let id = self.open(title.into(), TabKind::Query);
        self.current_mut().editor.text = sql.into();
        id
    }
    pub fn open_table(&mut self, table: TableRef) -> u64 {
        if let Some(id) = self
            .tabs
            .iter()
            .find(|t| t.kind == TabKind::Table(table.clone()))
            .map(|t| t.id)
        {
            self.activate(id);
            return id;
        }
        let editor = TableEditor::new(table.clone(), self.connection.driver, false);
        let id = self.open(
            display_name(&table, self.connection.driver),
            TabKind::Table(table),
        );
        self.current_mut().table = Some(editor);
        id
    }
    pub fn open_running(&mut self) -> u64 {
        if let Some(id) = self
            .tabs
            .iter()
            .find(|t| t.kind == TabKind::Running)
            .map(|t| t.id)
        {
            self.activate(id);
            return id;
        }
        self.open("Running Queries".into(), TabKind::Running)
    }
    /// false means the host should close this connection. Never creates a zero-tab state.
    pub fn close_tab(&mut self, id: u64) -> bool {
        if self.tabs.len() == 1 {
            return false;
        }
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return true;
        };
        self.tabs.remove(index);
        self.navigation.retain(|x| *x != id);
        if self.active == id {
            self.active = self.tabs[index.min(self.tabs.len() - 1)].id;
        }
        self.nav_pos = self
            .navigation
            .iter()
            .rposition(|x| *x == self.active)
            .unwrap_or(0);
        true
    }
    pub fn drafts(&self) -> Vec<SqlDraft> {
        self.tabs
            .iter()
            .filter(|t| t.kind == TabKind::Query)
            .map(|t| SqlDraft {
                title: t.title.clone(),
                sql: t.editor.text.clone(),
            })
            .collect()
    }
    pub fn restore_drafts(&mut self, drafts: &[SqlDraft]) {
        if drafts.is_empty() {
            return;
        }
        self.tabs.clear();
        self.navigation.clear();
        self.active = 0;
        for d in drafts {
            self.open_query(&d.title, &d.sql);
        }
        self.active = self.tabs[0].id;
        self.navigation = vec![self.active];
        self.nav_pos = 0;
    }
    pub fn status_text(&self) -> String {
        format!(
            "{} | {} : {} : {} : {} : {}",
            if self.connection.tag.is_empty() {
                "NONE".into()
            } else {
                self.connection.tag.to_uppercase()
            },
            self.server_version,
            self.tls.as_deref().unwrap_or("no TLS"),
            self.user,
            self.database,
            if self.current().kind == TabKind::Query {
                "SQL Query"
            } else {
                &self.current().title
            }
        )
    }
}
pub fn display_name(table: &TableRef, driver: Driver) -> String {
    if driver == Driver::Postgres && table.schema != "public" && !table.schema.is_empty() {
        format!("{}.{}", table.schema, table.name)
    } else {
        table.name.clone()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    None,
    Alert,
    Password,
}
#[derive(Debug, Clone)]
pub struct QueryPlan {
    pub original: Vec<String>,
    /// Where each statement of `original` starts in the editor's text (UTF-16).
    pub starts: Vec<usize>,
    pub statements: Vec<String>,
    pub dangers: Vec<(String, String)>,
    pub gate: Gate,
}
impl QueryPlan {
    pub fn new(
        document: &EditorDocument,
        all: bool,
        driver: Driver,
        mode: SafeMode,
        limit: u64,
    ) -> Self {
        let found: Vec<(String, usize)> = if let Some(selection) = &document.selection {
            sql::split_statements(selection, driver)
                .into_iter()
                .map(|s| (s.text, document.selection_from + s.from))
                .collect()
        } else if all {
            sql::split_statements(&document.text, driver)
                .into_iter()
                .map(|s| (s.text, s.from))
                .collect()
        } else {
            sql::statement_at(&document.text, document.cursor_utf16, driver)
                .into_iter()
                .map(|s| (s.text, s.from))
                .collect()
        };
        let (original, starts): (Vec<String>, Vec<usize>) = found.into_iter().unzip();
        let dangers = original
            .iter()
            .filter_map(|s| {
                sql::dangerous_reason(s, driver)
                    .or_else(|| {
                        (mode != SafeMode::Silent)
                            .then(|| sql::caution_reason(s, driver))
                            .flatten()
                    })
                    .map(|reason| (s.clone(), reason))
            })
            .collect();
        let gate = if mode == SafeMode::Silent
            || original.is_empty()
            || (matches!(mode, SafeMode::Alert2 | SafeMode::Safe2)
                && original.iter().all(|s| sql::is_read_only(s, driver)))
        {
            Gate::None
        } else if matches!(mode, SafeMode::Alert1 | SafeMode::Alert2) {
            Gate::Alert
        } else {
            Gate::Password
        };
        let statements = original
            .iter()
            .map(|s| {
                sql::apply_limit(
                    s,
                    (limit > 0).then_some(limit.min(i64::MAX as u64) as i64),
                    driver,
                )
            })
            .collect();
        Self {
            original,
            starts,
            statements,
            dangers,
            gate,
        }
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryApproval {
    pub danger: bool,
    pub safe_mode: bool,
}
/// Ordered execution stops at the first error or cancellation. A killed MySQL
/// SLEEP can return success; the cancellation flag still prevents the next write.
/// How a statement cut off by a lost connection is reported. It is never run again.
pub const INTERRUPTED: &str = "Interrupted: the connection was lost while this ran, so whether it took effect is unknown. It was not run again; check before you run it yourself.";
pub async fn run_plan(
    session: &telecat_db::Session,
    plan: &QueryPlan,
    approval: QueryApproval,
    cancelled: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<Vec<QueryResult>> {
    anyhow::ensure!(
        plan.dangers.is_empty() || approval.danger,
        "Dangerous statements require confirmation"
    );
    anyhow::ensure!(
        plan.gate == Gate::None || approval.safe_mode,
        "Safe Mode confirmation is required"
    );
    let mut out = vec![];
    for sql in &plan.statements {
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        match session.query_checked(sql, approval.danger).await {
            Ok(r) => out.push(r),
            Err(e) => {
                let server = e.downcast_ref::<telecat_db::ServerError>();
                out.push(QueryResult {
                    sql: sql.clone(),
                    error: Some(if telecat_db::is_connection_lost(&e) {
                        format!("{INTERRUPTED} ({e})")
                    } else {
                        e.to_string()
                    }),
                    error_position: server.and_then(|s| s.position),
                    error_context: server.and_then(|s| s.context.clone()),
                    error_internal: server.and_then(|s| s.internal.clone()),
                    ..Default::default()
                });
                break;
            }
        }
    }
    if cancelled.load(std::sync::atomic::Ordering::SeqCst) && !out.iter().any(|r| r.error.is_some())
    {
        out.push(QueryResult {
            error: Some("Cancelled by user".into()),
            ..Default::default()
        });
    }
    Ok(out)
}
