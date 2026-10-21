//! TableTab/useEditRows state, independent of the grid's rendering and input API.
use anyhow::Result;
use telecat_core::{changes::*, types::*};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    #[default]
    Data,
    Structure,
    Index,
}
#[derive(Debug, Clone, Default)]
pub struct EditBuffer {
    pub revision: u64,
    pub result: QueryResult,
    pub rows: Vec<EditRow>,
    base: Vec<EditRow>,
    past: Vec<Vec<EditRow>>,
    future: Vec<Vec<EditRow>>,
    pub loaded: bool,
}
impl EditBuffer {
    pub fn reset(&mut self, result: QueryResult, ids: Option<&[Vec<String>]>) {
        let rows = to_edit_rows(&result.rows, ids);
        *self = Self {
            revision: self.revision.wrapping_add(1),
            result,
            base: rows.clone(),
            rows,
            loaded: true,
            ..Self::default()
        };
    }
    pub fn dirty(&self) -> bool {
        has_changes(&self.rows)
    }
    pub fn apply(&mut self, edit: impl FnOnce(&mut Vec<EditRow>)) {
        self.revision = self.revision.wrapping_add(1);
        if self.past.len() >= 200 {
            self.past.remove(0);
        }
        self.past.push(self.rows.clone());
        self.future.clear();
        edit(&mut self.rows);
    }
    pub fn set_cell(&mut self, row: usize, column: usize, value: Cell) {
        if self.rows.get(row).is_some_and(|r| column < r.cur.len()) {
            self.apply(|rows| rows[row].cur[column] = value);
        }
    }
    pub fn delete(&mut self, selected: &[usize]) {
        self.apply(|rows| {
            for (i, r) in rows.iter_mut().enumerate() {
                if selected.contains(&i) {
                    r.deleted = true;
                }
            }
            rows.retain(|r| r.orig.is_some() || !r.deleted);
        });
    }
    pub fn add(&mut self, values: Vec<Cell>) {
        self.apply(|rows| {
            rows.push(EditRow {
                key: row_key(),
                orig: None,
                cur: values,
                deleted: false,
                rid: None,
            })
        });
    }
    pub fn undo(&mut self) -> bool {
        let Some(rows) = self.past.pop() else {
            return false;
        };
        self.future.push(std::mem::replace(&mut self.rows, rows));
        self.revision = self.revision.wrapping_add(1);
        true
    }
    pub fn redo(&mut self) -> bool {
        let Some(rows) = self.future.pop() else {
            return false;
        };
        self.past.push(std::mem::replace(&mut self.rows, rows));
        self.revision = self.revision.wrapping_add(1);
        true
    }
    pub fn discard(&mut self) {
        let base = self.base.clone();
        self.apply(|rows| *rows = base);
    }
    pub fn accepted(&mut self) {
        // The saved rows are still the same rows: they keep their identities, so the grid's
        // selection stays on them.
        let keys: Vec<String> = self
            .rows
            .iter()
            .filter(|r| !r.deleted)
            .map(|r| r.key.clone())
            .collect();
        let result = QueryResult {
            rows: self
                .rows
                .iter()
                .filter(|r| !r.deleted)
                .map(|r| r.cur.iter().map(|v| v.value().map(str::to_owned)).collect())
                .collect(),
            ..self.result.clone()
        };
        self.reset(result, None);
        for (row, key) in self.rows.iter_mut().zip(keys) {
            row.key = key;
        }
        self.base = self.rows.clone();
        // Server-assigned defaults and physical row IDs must be fetched before another write.
        self.loaded = false;
    }
}

#[derive(Debug, Clone)]
pub struct TableEditor {
    pub table: TableRef,
    pub part: Part,
    pub name: String,
    pub is_new: bool,
    pub primary: String,
    pub primary_key: Vec<String>,
    pub data: EditBuffer,
    pub structure: EditBuffer,
    pub index: EditBuffer,
    pub filters: Vec<Filter>,
    pub applied: Vec<Filter>,
    pub combine: Combine,
    pub show_filters: bool,
    pub order_by: Option<OrderBy>,
    pub hidden: std::collections::BTreeSet<String>,
    pub column_filter: Option<String>,
    pub show_ddl: bool,
    pub ddl: Option<String>,
}
pub fn new_filter() -> Filter {
    Filter {
        enabled: true,
        column: String::new(),
        op: FilterOp::Contains,
        value: String::new(),
        value2: None,
    }
}
pub fn structure_columns(d: Driver) -> Vec<String> {
    [
        "#",
        "column_name",
        "data_type",
        "character_set",
        "collation",
        "is_nullable",
        "column_default",
        "extra",
        "foreign_key",
        "comment",
    ]
    .into_iter()
    .filter(|s| d == Driver::Mysql || *s != "character_set")
    .map(str::to_owned)
    .collect()
}
pub fn index_columns(d: Driver) -> Vec<String> {
    [
        "index_name",
        "index_algorithm",
        "is_unique",
        "column_name",
        "condition",
        "constraint_type",
    ]
    .into_iter()
    .take(if d == Driver::Mysql { 4 } else { 6 })
    .map(str::to_owned)
    .collect()
}
impl TableEditor {
    pub fn new(table: TableRef, driver: Driver, is_new: bool) -> Self {
        let mut this = Self {
            name: table.name.clone(),
            table,
            part: if is_new { Part::Structure } else { Part::Data },
            is_new,
            primary: "id".into(),
            primary_key: vec![],
            data: EditBuffer::default(),
            structure: EditBuffer::default(),
            index: EditBuffer::default(),
            filters: vec![new_filter()],
            applied: vec![],
            combine: Combine::And,
            show_filters: false,
            order_by: None,
            hidden: Default::default(),
            column_filter: None,
            show_ddl: false,
            ddl: None,
        };
        this.structure.result.columns = structure_columns(driver);
        this.index.result.columns = index_columns(driver);
        if is_new {
            this.structure.loaded = true;
            this.index.loaded = true;
            let row = this
                .structure
                .result
                .columns
                .iter()
                .map(|c| match c.as_str() {
                    "column_name" => Cell::Text("id".into()),
                    "data_type" => Cell::Text(
                        if driver == Driver::Mysql {
                            "int"
                        } else {
                            "integer"
                        }
                        .into(),
                    ),
                    "is_nullable" => Cell::Text("NO".into()),
                    "extra" => Cell::Text(
                        if driver == Driver::Mysql {
                            "auto_increment"
                        } else {
                            "identity"
                        }
                        .into(),
                    ),
                    _ => Cell::Null,
                })
                .collect();
            this.structure.add(row);
        }
        this
    }
    pub fn buffer(&self) -> &EditBuffer {
        match self.part {
            Part::Data => &self.data,
            Part::Structure => &self.structure,
            Part::Index => &self.index,
        }
    }
    pub fn buffer_mut(&mut self) -> &mut EditBuffer {
        match self.part {
            Part::Data => &mut self.data,
            Part::Structure => &mut self.structure,
            Part::Index => &mut self.index,
        }
    }
    pub fn dirty(&self) -> bool {
        self.is_new
            || self.name != self.table.name
            || self.data.dirty()
            || self.structure.dirty()
            || self.index.dirty()
    }
    pub fn load_data(&mut self, data: &TableDataResult) {
        self.primary_key = data.primary_key.clone();
        self.data
            .reset(data.result.clone(), data.row_ids.as_deref());
    }
    pub fn add_row(&mut self, d: Driver) {
        if !self.buffer().loaded {
            return;
        }
        let values = self
            .buffer()
            .result
            .columns
            .iter()
            .map(|col| match self.part {
                Part::Data => Cell::unset(),
                Part::Structure => match col.as_str() {
                    "column_name" => {
                        Cell::Text(format!("column_{}", self.structure.rows.len() + 1))
                    }
                    "data_type" => Cell::Text(
                        if d == Driver::Mysql {
                            "varchar(255)"
                        } else {
                            "text"
                        }
                        .into(),
                    ),
                    "is_nullable" => Cell::Text("YES".into()),
                    _ => Cell::Null,
                },
                Part::Index => match col.as_str() {
                    "index_name" => Cell::Text("NEW_INDX".into()),
                    "index_algorithm" => Cell::Text("DEFAULT".into()),
                    "is_unique" => Cell::Text("FALSE".into()),
                    _ => Cell::Null,
                },
            })
            .collect();
        self.buffer_mut().add(values);
    }
    pub fn apply_filters(&mut self, combine: Combine, only: Option<usize>) {
        self.applied = match only {
            Some(i) => self
                .filters
                .get(i)
                .cloned()
                .map(|mut f| {
                    f.enabled = true;
                    f
                })
                .into_iter()
                .collect(),
            None => self.filters.iter().filter(|f| f.enabled).cloned().collect(),
        };
        self.combine = if only.is_some() {
            Combine::And
        } else {
            combine
        };
    }
    pub fn groups(&self, d: Driver, tab_id: u64) -> Result<Vec<CommitGroup>> {
        let mut groups = vec![];
        if self.data.dirty() {
            anyhow::ensure!(self.data.loaded, "Reload table data before editing again");
            let meta = TableMeta {
                columns: self.data.result.columns.clone(),
                primary_key: self.primary_key.clone(),
                binary: self.data.result.binary.clone(),
            };
            groups.push(CommitGroup {
                tab_id,
                part: Part::Data,
                transaction: true,
                statements: data_statements(d, &self.table, &meta, &self.data.rows)?,
                verify: if d == Driver::Mysql {
                    mysql_verify_checks(&self.table, &meta, &self.data.rows)
                } else {
                    vec![]
                },
            });
        }
        if self.is_new || self.name != self.table.name || self.structure.dirty() {
            groups.push(CommitGroup {
                tab_id,
                part: Part::Structure,
                transaction: d == Driver::Postgres,
                statements: structure_statements(
                    d,
                    &StructureChange {
                        table: self.table.clone(),
                        new_name: self.name.clone(),
                        is_new: self.is_new,
                        primary_key: if self.is_new {
                            self.primary
                                .split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                                .collect()
                        } else {
                            vec![]
                        },
                        columns: self.structure.result.columns.clone(),
                        rows: self.structure.rows.clone(),
                    },
                )?,
                verify: vec![],
            });
        }
        if self.index.dirty() {
            groups.push(CommitGroup {
                tab_id,
                part: Part::Index,
                transaction: d == Driver::Postgres,
                statements: index_statements(
                    d,
                    &TableRef {
                        schema: self.table.schema.clone(),
                        name: self.name.clone(),
                    },
                    &self.index.result.columns,
                    &self.index.rows,
                )?,
                verify: vec![],
            });
        }
        groups.retain(|g| !g.statements.is_empty());
        Ok(groups)
    }
    pub fn committed(&mut self, part: Part) {
        match part {
            Part::Data => self.data.accepted(),
            Part::Structure => {
                self.table.name = self.name.clone();
                self.is_new = false;
                self.structure.accepted();
                self.ddl = None;
            }
            Part::Index => self.index.accepted(),
        }
    }
    pub fn discard(&mut self) {
        self.data.discard();
        self.structure.discard();
        self.index.discard();
        self.name = self.table.name.clone();
    }
}
#[derive(Debug, Clone)]
pub struct CommitGroup {
    pub tab_id: u64,
    pub part: Part,
    pub transaction: bool,
    pub statements: Vec<Stmt>,
    /// How to tell afterwards whether each row edit was saved, if the connection drops
    /// during the save (MySQL row edits only; see `mysql_verify_checks`).
    pub verify: Vec<RowVerify>,
}
