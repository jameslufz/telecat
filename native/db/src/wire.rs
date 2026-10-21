use crate::tls::pg_tls;
use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use futures_util::{TryStreamExt, pin_mut};
use mysql_async::{
    ClientIdentity, Conn, OptsBuilder, Params, SslOpts, Value as MyValue, prelude::Queryable,
};
use serde_json::Value;
use std::{
    error::Error,
    time::{Duration, Instant},
};
use telecat_core::{changes::js_string, sql::split_statements, types::*};
use tokio_postgres::{
    Client, Config,
    types::{Format, FromSql, IsNull, ToSql, Type},
};
pub enum Wire {
    Mysql(Conn),
    Postgres {
        client: Client,
        task: tokio::task::JoinHandle<()>,
    },
}
#[derive(Debug)]
struct PgParam(Option<String>);
impl ToSql for PgParam {
    fn to_sql(
        &self,
        _: &Type,
        out: &mut BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn Error + Sync + Send>> {
        match &self.0 {
            Some(s) => {
                out.extend_from_slice(s.as_bytes());
                Ok(IsNull::No)
            }
            None => Ok(IsNull::Yes),
        }
    }
    fn accepts(_: &Type) -> bool {
        true
    }
    fn encode_format(&self, _: &Type) -> Format {
        Format::Text
    }
    tokio_postgres::types::to_sql_checked!();
}
#[derive(Debug)]
struct PgText(String);
impl<'a> FromSql<'a> for PgText {
    fn from_sql(
        t: &Type,
        raw: &'a [u8],
    ) -> std::result::Result<Self, Box<dyn Error + Send + Sync>> {
        let s = std::str::from_utf8(raw)?;
        Ok(Self(if t.oid() == 16 {
            if s == "t" { "true" } else { "false" }.into()
        } else {
            s.into()
        }))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}
/// Optional harness fence. Production configurations are unrestricted.
pub fn validate_endpoint(c: &ConnectionConfig) -> Result<()> {
    if std::env::var_os("TELECAT_TEST_DB_ONLY").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Ok(());
    }
    validate_test_endpoint(c)
}
pub fn validate_test_endpoint(c: &ConnectionConfig) -> Result<()> {
    let port = if c.driver == Driver::Mysql {
        3307
    } else {
        5434
    };
    if c.host != "127.0.0.1" || (c.port != port && !test_proxy_for(c.port, port)) {
        bail!("Test safety: only 127.0.0.1:{port} is permitted");
    }
    Ok(())
}
/// A test-only proxy in front of the test server (see `test_proxy`); never in a release build.
fn test_proxy_for(port: u16, upstream: u16) -> bool {
    #[cfg(feature = "test-support")]
    return crate::test_proxy::forwards(port, upstream);
    #[cfg(not(feature = "test-support"))]
    {
        let _ = (port, upstream);
        false
    }
}
fn fatal(e: &anyhow::Error) -> bool {
    if let Some(e) = e.downcast_ref::<tokio_postgres::Error>() {
        if e.code()
            .is_some_and(|c| matches!(c.code(), "28P01" | "3D000"))
        {
            return true;
        }
    }
    if let Some(mysql_async::Error::Server(e)) = e.downcast_ref::<mysql_async::Error>() {
        if matches!(e.code, 1045 | 1049) {
            return true;
        }
    }
    e.chain().any(|e| {
        e.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::NotFound
            )
        })
    }) || e.to_string().to_lowercase().contains("timeout")
}
/// How long opening one connection may take: a blackholed VPN route otherwise waits for the
/// operating system's TCP timeout, minutes.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPALIVE_IDLE: Duration = Duration::from_secs(15);
impl Wire {
    pub async fn connect(c: &ConnectionConfig, password: &str) -> Result<Self> {
        validate_endpoint(c)?;
        let first = !matches!(c.ssl_mode, SslMode::Disabled | SslMode::Allow);
        match Self::open(c, password, first).await {
            Ok(w) => Ok(w),
            Err(e) if matches!(c.ssl_mode, SslMode::Preferred | SslMode::Allow) && !fatal(&e) => {
                Self::open(c, password, !first).await
            }
            Err(e) => Err(e),
        }
    }
    async fn open(c: &ConnectionConfig, password: &str, tls: bool) -> Result<Self> {
        match c.driver {
            Driver::Mysql => {
                let ssl = if tls {
                    let mut s = SslOpts::default()
                        .with_danger_accept_invalid_certs(!matches!(
                            c.ssl_mode,
                            SslMode::VerifyCa | SslMode::VerifyFull
                        ))
                        .with_danger_skip_domain_validation(c.ssl_mode != SslMode::VerifyFull);
                    if let Some(ca) = c.ssl_ca.as_ref().filter(|s| !s.is_empty()) {
                        s = s
                            .with_disable_built_in_roots(true)
                            .with_root_certs(vec![std::path::PathBuf::from(ca).into()]);
                    }
                    if let (Some(cert), Some(key)) = (&c.ssl_cert, &c.ssl_key) {
                        s = s.with_client_identity(Some(ClientIdentity::new(
                            std::path::PathBuf::from(cert).into(),
                            std::path::PathBuf::from(key).into(),
                        )));
                    }
                    Some(s)
                } else {
                    None
                };
                let opts = OptsBuilder::default()
                    .ip_or_hostname(if c.host.is_empty() || c.host.starts_with('/') {
                        "127.0.0.1"
                    } else {
                        &c.host
                    })
                    .socket(c.host.starts_with('/').then_some(c.host.clone()))
                    .tcp_port(c.port)
                    .user(Some(&c.user))
                    .pass(Some(password))
                    .db_name(if c.database.is_empty() {
                        None
                    } else {
                        Some(&c.database)
                    })
                    .ssl_opts(ssl)
                    .prefer_socket(false)
                    .client_found_rows(true)
                    // Notices a dead idle link (a VPN that went away) without any query.
                    .tcp_keepalive(Some(KEEPALIVE_IDLE.as_millis() as u32))
                    .init(vec!["SET NAMES utf8mb4"]);
                let conn = tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts))
                    .await
                    .context("Connection timeout")??;
                Ok(Self::Mysql(conn))
            }
            Driver::Postgres => {
                let mut cfg = Config::new();
                cfg.host(&c.host)
                    .port(c.port)
                    .user(&c.user)
                    .password(password)
                    .dbname(if c.database.is_empty() {
                        "postgres"
                    } else {
                        &c.database
                    })
                    .application_name("Telecat")
                    .connect_timeout(CONNECT_TIMEOUT)
                    .keepalives(true)
                    .keepalives_idle(KEEPALIVE_IDLE)
                    .keepalives_interval(Duration::from_secs(5))
                    .keepalives_retries(3);
                cfg.ssl_mode(if tls {
                    tokio_postgres::config::SslMode::Require
                } else {
                    tokio_postgres::config::SslMode::Disable
                });
                if tls && c.negotiation == Negotiation::Direct {
                    cfg.ssl_negotiation(tokio_postgres::config::SslNegotiation::Direct);
                }
                let mut tls_config = c.clone();
                if !tls {
                    tls_config.ssl_ca = None;
                    tls_config.ssl_cert = None;
                    tls_config.ssl_key = None;
                }
                let connector = tokio_postgres_rustls::MakeRustlsConnect::new(pg_tls(&tls_config)?);
                // connect_timeout covers the socket only; TLS and authentication can hang too.
                let (client, connection) =
                    tokio::time::timeout(CONNECT_TIMEOUT, cfg.connect(connector))
                        .await
                        .context("Connection timeout")??;
                let task = tokio::spawn(async move {
                    // Client requests receive connection failures through their Result.
                    // Server error text may contain SQL/values: never duplicate it to stderr.
                    let _ = connection.await;
                });
                Ok(Self::Postgres { client, task })
            }
        }
    }
    pub async fn run(&mut self, sql: &str, params: &[Value]) -> Result<(QueryResult, u64)> {
        self.run_inner(sql, params).await.map_err(server_error)
    }
    async fn run_inner(&mut self, sql: &str, params: &[Value]) -> Result<(QueryResult, u64)> {
        let d = match self {
            Self::Mysql(_) => Driver::Mysql,
            Self::Postgres { .. } => Driver::Postgres,
        };
        if split_statements(sql, d).len() > 1 {
            bail!("Only one SQL statement is allowed per call");
        }
        let t = Instant::now();
        match self {
            Self::Mysql(conn) => {
                let p = Params::Positional(
                    params
                        .iter()
                        .map(|v| match v {
                            Value::Null => MyValue::NULL,
                            Value::Bool(b) => MyValue::Int(i64::from(*b)),
                            Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    MyValue::Int(i)
                                } else if let Some(u) = n.as_u64() {
                                    MyValue::UInt(u)
                                } else {
                                    MyValue::Double(n.as_f64().unwrap_or(0.0))
                                }
                            }
                            _ => MyValue::Bytes(js_string(v).into_bytes()),
                        })
                        .collect(),
                );
                // Text protocol preserves date precision. Bound executions use the binary protocol, never SQL interpolation.
                let (fields, rows, affected, info) = if params.is_empty() {
                    let mut q = conn.query_iter(sql).await?;
                    let fields = q.columns_ref().to_vec();
                    let rows = q.collect::<mysql_async::Row>().await?;
                    let affected = q.affected_rows();
                    let info = q.info().into_owned();
                    q.drop_result().await?;
                    (fields, rows, affected, info)
                } else {
                    let mut q = conn.exec_iter(sql, p).await?;
                    let fields = q.columns_ref().to_vec();
                    let rows = q.collect::<mysql_async::Row>().await?;
                    let affected = q.affected_rows();
                    let info = q.info().into_owned();
                    q.drop_result().await?;
                    (fields, rows, affected, info)
                };
                let mut result = QueryResult {
                    sql: sql.into(),
                    duration_ms: (t.elapsed().as_secs_f64() * 1000.0).round() as u64,
                    ..Default::default()
                };
                if fields.is_empty() {
                    result.affected_rows = Some(affected);
                    result.command = (!info.is_empty()).then_some(info);
                } else {
                    result.columns = fields.iter().map(|f| f.name_str().into_owned()).collect();
                    result.origins = Some(
                        fields
                            .iter()
                            .map(|f| {
                                let (table, column) = (f.org_table_str(), f.org_name_str());
                                (!table.is_empty() && !column.is_empty()).then(|| {
                                    ColumnOrigin::Mysql {
                                        schema: f.schema_str().into_owned(),
                                        alias: f.table_str().into_owned(),
                                        table: table.into_owned(),
                                        column: column.into_owned(),
                                    }
                                })
                            })
                            .collect(),
                    );
                    result.numeric = Some(
                        fields
                            .iter()
                            .map(|f| {
                                matches!(
                                    f.column_type() as u8,
                                    0 | 1 | 2 | 3 | 4 | 5 | 8 | 9 | 13 | 246
                                )
                            })
                            .collect(),
                    );
                    result.binary = Some(
                        fields
                            .iter()
                            .map(|f| {
                                f.character_set() == 63
                                    && matches!(
                                        f.column_type() as u8,
                                        15 | 16 | 249 | 250 | 251 | 252 | 253 | 254 | 255
                                    )
                            })
                            .collect(),
                    );
                    result.rows = rows
                        .into_iter()
                        .map(|r| {
                            r.unwrap()
                                .into_iter()
                                .enumerate()
                                .map(|(i, v)| mysql_cell(v, &fields[i]))
                                .collect()
                        })
                        .collect();
                }
                Ok((result, affected))
            }
            Self::Postgres { client, .. } => {
                let stmt = client.prepare(sql).await?; // server Parse rejects hidden second statements as well.
                let columns = stmt.columns();
                let mut result = QueryResult {
                    sql: sql.into(),
                    columns: columns.iter().map(|f| f.name().into()).collect(),
                    ..Default::default()
                };
                if !columns.is_empty() {
                    result.origins = Some(
                        columns
                            .iter()
                            .map(|c| match (c.table_oid(), c.column_id()) {
                                (Some(table_oid), Some(attnum)) if table_oid != 0 && attnum > 0 => {
                                    Some(ColumnOrigin::Postgres { table_oid, attnum })
                                }
                                _ => None,
                            })
                            .collect(),
                    );
                    result.numeric = Some(
                        columns
                            .iter()
                            .map(|f| {
                                matches!(f.type_().oid(), 20 | 21 | 23 | 26 | 700 | 701 | 1700)
                            })
                            .collect(),
                    );
                    result.binary = Some(columns.iter().map(|f| f.type_().oid() == 17).collect());
                }
                let params = params
                    .iter()
                    .map(|v| {
                        PgParam(if v.is_null() {
                            None
                        } else {
                            Some(js_string(v))
                        })
                    })
                    .collect::<Vec<_>>();
                let bound: Vec<&(dyn ToSql + Sync)> =
                    params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
                let stream = client.query_raw_text(&stmt, bound).await?;
                pin_mut!(stream);
                while let Some(row) = stream.try_next().await? {
                    let mut values = vec![];
                    for i in 0..columns.len() {
                        values.push(row.try_get::<_, Option<PgText>>(i)?.map(|s| s.0));
                    }
                    result.rows.push(values);
                }
                let affected = stream.rows_affected().unwrap_or(0);
                result.command = stream
                    .command_tag()
                    .and_then(|s| s.split_whitespace().next())
                    .map(str::to_owned);
                if columns.is_empty() {
                    result.affected_rows = Some(affected);
                }
                result.duration_ms = (t.elapsed().as_secs_f64() * 1000.0).round() as u64;
                Ok((result, affected))
            }
        }
    }
    /// Runs one statement of a script as written: MySQL's text protocol, PostgreSQL's simple
    /// query protocol. No splitting or parameters: the caller has already split the script.
    pub async fn execute_raw(&mut self, sql: &str) -> Result<()> {
        let r: Result<()> = match self {
            Self::Mysql(conn) => conn.query_drop(sql).await.map_err(Into::into),
            // The extended protocol: the server itself refuses more than one statement.
            Self::Postgres { client, .. } => client
                .execute(sql, &[])
                .await
                .map(|_| ())
                .map_err(Into::into),
        };
        r.map_err(server_error)
    }
    /// PostgreSQL `COPY … FROM STDIN`: the sink the data rows are written to.
    pub async fn copy_in(&mut self, sql: &str) -> Result<tokio_postgres::CopyInSink<bytes::Bytes>> {
        match self {
            Self::Postgres { client, .. } => client
                .copy_in(sql)
                .await
                .map_err(|e| server_error(e.into())),
            Self::Mysql(_) => bail!("COPY is PostgreSQL only"),
        }
    }
    /// The cheapest round trip: COM_PING, or an empty simple query.
    pub async fn ping(&mut self) -> Result<()> {
        let r: Result<()> = match self {
            Self::Mysql(conn) => conn.ping().await.map_err(Into::into),
            Self::Postgres { client, .. } => client
                .simple_query("")
                .await
                .map(|_| ())
                .map_err(Into::into),
        };
        r.map_err(server_error)
    }
    /// PostgreSQL knows without a round trip when its connection task has ended.
    pub fn is_closed(&self) -> bool {
        match self {
            Self::Mysql(_) => false,
            Self::Postgres { client, .. } => client.is_closed(),
        }
    }
    pub async fn close(self) {
        match self {
            Self::Mysql(c) => {
                let _ = c.disconnect().await;
            }
            Self::Postgres { client, task } => {
                drop(client);
                let _ = task.await;
            }
        }
    }
}
pub fn buffer_cell(bytes: &[u8]) -> String {
    if telecat_core::changes::binary_shown_as_text(bytes) {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        format!(
            "0x{}",
            bytes.iter().map(|b| format!("{b:02X}")).collect::<String>()
        )
    }
}
fn mysql_cell(v: MyValue, f: &mysql_async::Column) -> Option<String> {
    Some(match v {
        MyValue::NULL => return None,
        MyValue::Bytes(b) => {
            if f.column_type() as u8 == 245 {
                return crate::values::json_cell(&b);
            } else if f.column_type() as u8 == 255 {
                return crate::values::geometry_cell(&b);
            } else if matches!(f.column_type() as u8, 4 | 5) {
                crate::values::float_cell(&String::from_utf8_lossy(&b))
            } else if f.character_set() == 63
                && matches!(
                    f.column_type() as u8,
                    15 | 16 | 249 | 250 | 251 | 252 | 253 | 254 | 255
                )
            {
                buffer_cell(&b)
            } else {
                String::from_utf8_lossy(&b).into_owned()
            }
        }
        MyValue::Int(v) => v.to_string(),
        MyValue::UInt(v) => v.to_string(),
        // mysql2 uses the text protocol: MySQL prints FLOAT with FLT_DIG (6)
        // significant digits, even when our bound execution receives binary f32.
        MyValue::Float(v) => crate::values::float_cell(&format!("{v:.5e}")),
        MyValue::Double(v) => js_string(&serde_json::json!(v)),
        MyValue::Date(y, m, d, h, mi, s, us) => {
            let date = format!("{y:04}-{m:02}-{d:02}");
            if f.column_type() as u8 == 10 {
                date
            } else {
                format!("{date} {h:02}:{mi:02}:{s:02}{}", fraction(us, f.decimals()))
            }
        }
        MyValue::Time(neg, days, h, m, s, us) => format!(
            "{}{:02}:{m:02}:{s:02}{}",
            if neg { "-" } else { "" },
            days * 24 + h as u32,
            fraction(us, f.decimals())
        ),
    })
}
fn fraction(us: u32, decimals: u8) -> String {
    if decimals == 0 {
        String::new()
    } else {
        format!(".{}", &format!("{us:06}")[..usize::from(decimals.min(6))])
    }
}
pub fn format_bytes(v: f64) -> String {
    let mut x = v;
    let mut i = 0;
    let u = ["B", "KB", "MB", "GB", "TB"];
    if x == 0.0 || x.is_nan() {
        return "0 B".into();
    }
    while x >= 1024.0 && i < 4 {
        x /= 1024.0;
        i += 1;
    }
    if i != 0 {
        x = (x * 10.0).round() / 10.0;
    }
    format!("{} {}", telecat_core::changes::js_number(x), u[i])
}

/// Electron's errMessage, including symbolic MySQL codes and PostgreSQL SQLSTATE.
/// A server's error with where it points in the statement, when the server said.
#[derive(Debug)]
pub struct ServerError {
    pub message: String,
    /// PostgreSQL ErrorResponse 'P': 1-based character position in the statement.
    pub position: Option<usize>,
    /// PostgreSQL 'p'/'q': a position inside an internally generated query (a function body).
    pub internal: Option<(usize, String)>,
    /// PostgreSQL 'W': where it happened ("COPY t, line 3", "PL/pgSQL function … line 4").
    pub context: Option<String>,
    /// The SQLSTATE code.
    pub code: Option<String>,
    /// The connection itself failed (see [`is_connection_lost`]), not the statement.
    pub lost: bool,
}
impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ServerError {}
pub(crate) fn server_error(e: anyhow::Error) -> anyhow::Error {
    let pg = e
        .downcast_ref::<tokio_postgres::Error>()
        .and_then(|e| e.as_db_error());
    let context = pg.and_then(|d| d.where_()).map(str::to_owned);
    let code = pg.map(|d| d.code().code().to_owned()).or_else(|| {
        e.downcast_ref::<tokio_postgres::Error>()
            .and_then(|e| e.code())
            .map(|c| c.code().to_owned())
    });
    let db = pg.and_then(|d| d.position().cloned());
    let (position, internal) = match db {
        Some(tokio_postgres::error::ErrorPosition::Original(p)) => (Some(p as usize), None),
        Some(tokio_postgres::error::ErrorPosition::Internal { position, query }) => {
            (None, Some((position as usize, query)))
        }
        None => (None, None),
    };
    let lost = lost_cause(&e);
    anyhow::Error::new(ServerError {
        lost,
        message: err_message(&e),
        position,
        internal,
        context,
        code,
    })
}
pub fn err_message(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if let Some(pg) = cause.downcast_ref::<tokio_postgres::Error>() {
            if let Some(db) = pg.as_db_error() {
                return format!("{}: {}", db.code().code(), db.message());
            }
        }
        if let Some(mysql_async::Error::Server(db)) = cause.downcast_ref::<mysql_async::Error>() {
            static CODES: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
                serde_json::from_str(include_str!("mysql-errors.json"))
                    .expect("generated error names")
            });
            let code = CODES[db.code.to_string()]
                .as_str()
                .unwrap_or("UNKNOWN_CODE_PLEASE_REPORT");
            return if db.message.contains(code) {
                db.message.clone()
            } else {
                format!("{code}: {}", db.message)
            };
        }
    }
    e.to_string()
}

/// Whether `e` means the connection to the server is gone or unusable: the network or VPN
/// dropped, the server restarted or ended the session, a timeout, TLS or name resolution. A
/// statement the server refused is not.
pub fn is_connection_lost(e: &anyhow::Error) -> bool {
    lost_cause(e)
}
/// Whether `e` is the server refusing the credentials or the database: retrying with the same
/// password can't help and may lock the account.
pub fn is_auth_failure(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        if let Some(s) = cause.downcast_ref::<ServerError>() {
            return s
                .code
                .as_deref()
                .is_some_and(|c| matches!(c, "28P01" | "28000" | "3D000"))
                || [
                    "ER_ACCESS_DENIED_ERROR",
                    "ER_BAD_DB_ERROR",
                    "ER_DBACCESS_DENIED_ERROR",
                ]
                .iter()
                .any(|n| s.message.starts_with(n));
        }
        if let Some(pg) = cause.downcast_ref::<tokio_postgres::Error>() {
            return pg
                .code()
                .is_some_and(|c| matches!(c.code(), "28P01" | "28000" | "3D000"));
        }
        if let Some(mysql_async::Error::Server(s)) = cause.downcast_ref::<mysql_async::Error>() {
            return matches!(s.code, 1044 | 1045 | 1049 | 1698);
        }
        false
    })
}
fn lost_cause(e: &anyhow::Error) -> bool {
    use std::io::ErrorKind::*;
    for cause in e.chain() {
        if let Some(s) = cause.downcast_ref::<ServerError>() {
            return s.lost;
        }
        if cause.is::<tokio::time::error::Elapsed>() || cause.is::<crate::CommitInterrupted>() {
            return true;
        }
        if let Some(pg) = cause.downcast_ref::<tokio_postgres::Error>() {
            if pg.is_closed() {
                return true;
            }
            if let Some(code) = pg.code() {
                let code = code.code();
                return code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03");
            }
            // tokio_postgres doesn't expose its error kinds; these are the transport ones.
            let text = pg.to_string();
            if [
                "error communicating with the server",
                "error performing TLS handshake",
                "timeout waiting for server",
                "error connecting to server",
            ]
            .iter()
            .any(|k| text.starts_with(k))
            {
                return true;
            }
        }
        if let Some(my) = cause.downcast_ref::<mysql_async::Error>() {
            match my {
                mysql_async::Error::Io(_) => return true,
                mysql_async::Error::Driver(mysql_async::DriverError::ConnectionClosed) => {
                    return true;
                }
                // Server shutdown, connection killed, idle session closed by the server.
                mysql_async::Error::Server(s) => {
                    return matches!(s.code, 1053 | 1927 | 2006 | 2013 | 4031);
                }
                _ => {}
            }
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                ConnectionReset
                    | ConnectionAborted
                    | ConnectionRefused
                    | BrokenPipe
                    | UnexpectedEof
                    | TimedOut
                    | NotConnected
                    | HostUnreachable
                    | NetworkUnreachable
                    | NetworkDown
                    | AddrNotAvailable
            ) {
                return true;
            }
        }
    }
    let text = format!("{e:#}").to_lowercase();
    [
        "server has gone away",
        "lost connection",
        "connection closed",
        "connection is closed",
        "connection reset",
        "connection refused",
        "connection timeout",
        "broken pipe",
        "timed out",
        "no route to host",
        "network is unreachable",
        "host is unreachable",
        "network is down",
        "failed to lookup address",
        "nodename nor servname",
        "name or service not known",
        "unexpected eof",
        "error performing tls handshake",
    ]
    .iter()
    .any(|k| text.contains(k))
}
