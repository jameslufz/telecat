use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    Mysql,
    Postgres,
}
pub type Dialect = Driver;
macro_rules! string_enum { ($name:ident { $($variant:ident => $value:literal),* $(,)? }) => {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum $name { $(#[serde(rename=$value)] $variant),* }
}; }
string_enum!(SafeMode { Silent=>"silent", Alert1=>"alert1", Alert2=>"alert2", Safe1=>"safe1", Safe2=>"safe2" });
string_enum!(PasswordMode { Keychain=>"keychain", Ask=>"ask", Command=>"command", None=>"none" });
string_enum!(SslMode { Preferred=>"PREFERRED", Disabled=>"DISABLED", Required=>"REQUIRED", Allow=>"ALLOW", VerifyCa=>"VERIFY-CA", VerifyFull=>"VERIFY-FULL" });
string_enum!(Negotiation { Postgres=>"POSTGRES", Direct=>"DIRECT" });
string_enum!(ObjectKind { Table=>"table", View=>"view", Function=>"function", Procedure=>"procedure" });
string_enum!(Combine { And=>"AND", Or=>"OR" });
impl Default for Combine {
    fn default() -> Self {
        Self::And
    }
}
impl Combine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::And => "AND",
            Self::Or => "OR",
        }
    }
}
string_enum!(Direction { Asc=>"ASC", Desc=>"DESC" });
impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}
string_enum!(FilterOp { Eq=>"=", Ne=>"<>", Lt=>"<", Gt=>">", Le=>"<=", Ge=>">=", In=>"in", NotIn=>"not in", IsNull=>"is null", IsNotNull=>"is not null", Between=>"between", NotBetween=>"not between", Like=>"like", Contains=>"contains", NotContains=>"not contains", ContainsCs=>"contains cs", NotContainsCs=>"not contains cs", Prefix=>"has prefix", Suffix=>"has suffix" });
impl FilterOp {
    pub fn as_str(self) -> String {
        serde_json::to_value(self).unwrap().as_str().unwrap().into()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub driver: Driver,
    pub name: String,
    pub color: String,
    pub tag: String,
    pub safe_mode: SafeMode,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password_mode: PasswordMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_command: Option<String>,
    pub database: String,
    pub ssl_mode: SslMode,
    pub negotiation: Negotiation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssl_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssl_cert: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssl_ca: Option<String>,
    pub group_id: Option<String>,
}
pub fn default_port(d: Driver) -> u16 {
    if d == Driver::Mysql { 3306 } else { 5432 }
}
pub fn empty_connection(d: Driver) -> ConnectionConfig {
    ConnectionConfig {
        id: String::new(),
        driver: d,
        name: String::new(),
        color: "#3a7d44".into(),
        tag: String::new(),
        safe_mode: SafeMode::Silent,
        host: "127.0.0.1".into(),
        port: default_port(d),
        user: String::new(),
        password_mode: PasswordMode::Keychain,
        password_command: None,
        database: String::new(),
        ssl_mode: SslMode::Preferred,
        negotiation: Negotiation::Postgres,
        ssl_key: None,
        ssl_cert: None,
        ssl_ca: None,
        group_id: None,
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionGroup {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collapsed: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreSnapshot {
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
    #[serde(default)]
    pub groups: Vec<ConnectionGroup>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRef {
    pub schema: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DbObject {
    #[serde(flatten)]
    pub table: TableRef,
    pub kind: ObjectKind,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numeric: Option<Vec<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<Vec<bool>>,
    pub rows: Vec<Vec<Option<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub duration_ms: u64,
    pub sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Where each column's values come from, as the server's protocol reported it. Only read
    /// results carry it; it decides whether a query result can be edited.
    #[serde(skip)]
    pub origins: Option<Vec<Option<ColumnOrigin>>>,
    /// PostgreSQL: the 1-based character in the statement the error points at.
    #[serde(skip)]
    pub error_position: Option<usize>,
    /// PostgreSQL: where the error happened (CONTEXT) and, inside a function body, the internal
    /// query position and text.
    #[serde(skip)]
    pub error_context: Option<String>,
    #[serde(skip)]
    pub error_internal: Option<(usize, String)>,
}
/// The source of a result column according to the wire protocol's column metadata, never the
/// SQL text. A computed column has no origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ColumnOrigin {
    /// MySQL column definition: database, the table's alias in the query, the base table and the
    /// base column.
    Mysql {
        schema: String,
        alias: String,
        table: String,
        column: String,
    },
    /// PostgreSQL RowDescription: the table's OID and the column's number in it.
    Postgres { table_oid: u32, attnum: i16 },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub connection: ConnectionConfig,
    pub server_version: String,
    pub tls: Option<String>,
    pub user: String,
    pub database: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub time: String,
    pub sql: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub enabled: bool,
    pub column: String,
    pub op: FilterOp,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value2: Option<String>,
}
pub const ANY_COLUMN: &str = "";
pub const RAW_SQL: &str = "__raw__";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderBy {
    pub column: String,
    pub dir: Direction,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDataRequest {
    pub table: TableRef,
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub combine: Combine,
    pub order_by: Option<OrderBy>,
    pub limit: u64,
    pub offset: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDataResult {
    #[serde(flatten)]
    pub result: QueryResult,
    pub total: u64,
    pub primary_key: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_ids: Option<Vec<Vec<String>>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableInfo {
    pub data_size: String,
    pub index_size: String,
    pub total_size: String,
    pub comment: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextMenuItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submenu: Option<Vec<ContextMenuItem>>,
}
/// UI choice lists copied from types.ts by the fixture generator, with exact labels.
pub fn choices() -> serde_json::Value {
    serde_json::from_str(include_str!("choices.json")).expect("checked in choices")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    pub sql: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SqlDraft {
    pub title: String,
    pub sql: String,
}
pub type SavedQueries = std::collections::BTreeMap<String, Vec<SavedQuery>>;
pub type Drafts = std::collections::BTreeMap<String, Vec<SqlDraft>>;
