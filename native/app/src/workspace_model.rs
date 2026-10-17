//! Shell state and editor/grid integration boundary. No GPUI or component dependencies.
use crate::table_model::{Part, TableEditor};
use telecat_core::changes::{Stmt, table_op_statement};
use telecat_core::{sql, types::*};
#[derive(Debug, Clone)]
pub struct StagedOperation {
    pub object: DbObject,
    pub truncate: bool,
}
#[derive(Debug, Clone)]
pub enum CommitTarget {
    Table(u64, Part),
    Operation(StagedOperation),
}
#[derive(Debug, Clone)]
pub struct CommitUnit {
    pub target: CommitTarget,
    pub statements: Vec<Stmt>,
    pub transaction: bool,
}

#[derive(Debug, Clone, Default)]
pub struct EditorDocument {
    pub text: String,
    /// Electron/CodeMirror positions count UTF-16 code units.
    pub cursor_utf16: usize,
    pub selection: Option<String>,
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
}
impl Tab {
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
            table: None,
        }
    }
    pub fn set_results(&mut self, results: Vec<QueryResult>) {
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
            target: CommitTarget::Operation(op),
        }));
        Ok(units)
    }
    pub fn has_changes(&self) -> bool {
        !self.operations.is_empty()
            || self
                .tabs
                .iter()
                .any(|t| t.table.as_ref().is_some_and(TableEditor::dirty))
    }
    pub fn committed(&mut self, target: &CommitTarget) {
        match target {
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
        let original: Vec<String> = if let Some(selection) = &document.selection {
            sql::split_statements(selection, driver)
                .into_iter()
                .map(|s| s.text)
                .collect()
        } else if all {
            sql::split_statements(&document.text, driver)
                .into_iter()
                .map(|s| s.text)
                .collect()
        } else {
            sql::statement_at(&document.text, document.cursor_utf16, driver)
                .into_iter()
                .map(|s| s.text)
                .collect()
        };
        let dangers = original
            .iter()
            .filter_map(|s| sql::dangerous_reason(s, driver).map(|reason| (s.clone(), reason)))
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
                out.push(QueryResult {
                    sql: sql.clone(),
                    error: Some(e.to_string()),
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
