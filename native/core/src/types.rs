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
string_enum!(PassphraseMode { Keychain=>"keychain", Ask=>"ask" });
string_enum!(ObjectKind { Table=>"table", View=>"view", MaterializedView=>"materialized view", Function=>"function", Procedure=>"procedure" });
impl ObjectKind {
    /// Tables and views of any kind: objects with rows that open in a table tab.
    pub fn is_relation(self) -> bool {
        matches!(self, Self::Table | Self::View | Self::MaterializedView)
    }
    pub fn is_view(self) -> bool {
        matches!(self, Self::View | Self::MaterializedView)
    }
    pub fn is_routine(self) -> bool {
        matches!(self, Self::Function | Self::Procedure)
    }
    /// The word used in SQL and in messages: "table", "view", "materialized view"…
    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
            Self::Function => "function",
            Self::Procedure => "procedure",
        }
    }
}
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
    /// Older profiles have none of the fields below; each defaults to off.
    #[serde(default, skip_serializing_if = "SshConfig::is_unset")]
    pub ssh: SshConfig,
    /// SQL run on every new database connection, right after it opens (see `startup`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub startup_sql: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub disable_channel_binding: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub load_system_schemas: bool,
}
fn is_false(b: &bool) -> bool {
    !*b
}
/// Reaching the database through an SSH server. The database host and port are then as the SSH
/// server sees them. Secrets are never stored here: they live in the Keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SshConfig {
    pub enabled: bool,
    /// A host name, an address, or a `Host` alias from ~/.ssh/config.
    pub host: String,
    /// 0: from ~/.ssh/config, else 22.
    pub port: u16,
    /// Empty: from ~/.ssh/config, else the local user name.
    pub user: String,
    pub password_mode: PasswordMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_command: Option<String>,
    pub use_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    pub passphrase_mode: PassphraseMode,
}
impl Default for SshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 0,
            user: String::new(),
            password_mode: PasswordMode::Keychain,
            password_command: None,
            use_key: false,
            key_path: None,
            passphrase_mode: PassphraseMode::Keychain,
        }
    }
}
impl SshConfig {
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }
    /// The private key file chosen in the form, when "Use SSH key" is on.
    pub fn key_file(&self) -> Option<&str> {
        self.key_path
            .as_deref()
            .filter(|p| self.use_key && !p.is_empty())
    }
}
/// Why `host` can't be an SSH server name, if it can't. known_hosts files read `* ? , !` as
/// patterns, `[ ]` and `|` as syntax and whitespace as a field break, so a name with any of
/// them could stand for other hosts once recorded; none of them occurs in a real host name or
/// address. (IPv6 addresses are written without brackets here.)
pub fn ssh_host_problem(host: &str) -> Option<&'static str> {
    if host.is_empty() {
        Some("is empty")
    } else if host.len() > 255 {
        Some("is too long")
    } else if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("can't contain spaces or control characters")
    } else if host.contains(['*', '?', ',', '!', '[', ']', '|', '#', '@', '/']) {
        Some("can't contain * ? , ! [ ] | # @ or /")
    } else {
        None
    }
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
        ssh: SshConfig::default(),
        startup_sql: String::new(),
        disable_channel_binding: false,
        load_system_schemas: false,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbObject {
    #[serde(flatten)]
    pub table: TableRef,
    pub kind: ObjectKind,
    /// PostgreSQL routines: the identity arguments (`pg_get_function_identity_arguments`), which
    /// tell overloads apart and are needed to drop one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}
impl DbObject {
    pub fn new(table: TableRef, kind: ObjectKind) -> Self {
        Self {
            table,
            kind,
            signature: None,
        }
    }
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
    /// MySQL JSON columns: (column, each row's value as the server's JSON text). The rows hold
    /// the grid's display form, which doesn't load back as the same JSON; exports use these.
    #[serde(skip)]
    pub json_text: Option<Vec<(usize, Vec<Option<String>>)>>,
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
    /// The tab was closed: the SQL waits to be reopened by the next new SQL Query tab.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
}
pub type SavedQueries = std::collections::BTreeMap<String, Vec<SavedQuery>>;
pub type Drafts = std::collections::BTreeMap<String, Vec<SqlDraft>>;
