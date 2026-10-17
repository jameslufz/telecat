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
    if c.host != "127.0.0.1" || c.port != port {
        bail!("Test safety: only 127.0.0.1:{port} is permitted");
    }
    Ok(())
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
                    .init(vec!["SET NAMES utf8mb4"]);
                let conn = tokio::time::timeout(Duration::from_secs(10), Conn::new(opts))
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
                    .connect_timeout(Duration::from_secs(10));
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
                let (client, connection) = cfg.connect(connector).await?;
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
        self.run_inner(sql, params)
            .await
            .map_err(|e| anyhow::anyhow!(err_message(&e)))
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
    let s = String::from_utf8_lossy(bytes);
    if s.chars()
        .any(|c| matches!(c,'\u{0}'..='\u{8}'|'\u{e}'..='\u{1f}'|'�'))
    {
        format!(
            "0x{}",
            bytes.iter().map(|b| format!("{b:02X}")).collect::<String>()
        )
    } else {
        s.into_owned()
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
