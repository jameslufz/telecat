//! Connection URLs in TablePlus's format:
//! `driver://user:password@host:port/database?option=value` and, over SSH,
//! `driver+ssh://ssh_user:ssh_password@ssh_host:ssh_port/user:password@host:port/database?…`.
//!
//! These URLs carry passwords, so no error message ever repeats any part of the input.
use crate::types::*;
use anyhow::{Result, bail};

/// A parsed URL: the connection settings and the passwords it carried, for a new unsaved form.
#[derive(Debug, Clone, PartialEq)]
pub struct UrlImport {
    pub config: ConnectionConfig,
    pub password: Option<String>,
    pub ssh_password: Option<String>,
}

/// `tLSMode` numbers. TablePlus's own numbering isn't documented; this follows the order of its
/// SSL mode menu (unverified), and names are accepted too.
const TLS_MODES: [SslMode; 6] = [
    SslMode::Preferred,
    SslMode::Disabled,
    SslMode::Required,
    SslMode::Allow,
    SslMode::VerifyCa,
    SslMode::VerifyFull,
];

pub fn parse_connection_url(input: &str) -> Result<UrlImport> {
    let input = input.trim();
    let Some((scheme, rest)) = input.split_once("://") else {
        bail!(
            "This isn't a connection URL. It should start with mysql://, mariadb://, postgres:// or postgresql://."
        );
    };
    let scheme = scheme.to_ascii_lowercase();
    let (base, over_ssh) = match scheme.strip_suffix("+ssh") {
        Some(base) => (base, true),
        None => (scheme.as_str(), false),
    };
    let driver = match base {
        "mysql" | "mariadb" => Driver::Mysql,
        "postgres" | "postgresql" => Driver::Postgres,
        _ => bail!(
            "Unsupported URL scheme. Telecat opens mysql://, mariadb://, postgres:// and postgresql:// URLs (with +ssh for SSH)."
        ),
    };
    let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
    let (main, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut c = empty_connection(driver);
    let mut ssh_password = None;
    let database_part = if over_ssh {
        let Some((ssh_authority, database_part)) = main.split_once('/') else {
            bail!(
                "An SSH URL needs the SSH server, then the database: driver+ssh://ssh_user@ssh_host/user@host/database."
            );
        };
        let authority = authority(ssh_authority, "SSH server")?;
        if let Some(why) = ssh_host_problem(&authority.host) {
            bail!("The SSH server name {why}.");
        }
        c.ssh = SshConfig {
            enabled: true,
            host: authority.host,
            port: authority.port.unwrap_or(0),
            user: authority.user.unwrap_or_default(),
            ..SshConfig::default()
        };
        ssh_password = authority.password;
        database_part
    } else {
        main
    };
    let (db_authority, database) = database_part.split_once('/').unwrap_or((database_part, ""));
    let authority = authority(db_authority, "database server")?;
    c.host = if authority.host.is_empty() {
        "127.0.0.1".into()
    } else {
        authority.host
    };
    c.port = authority.port.unwrap_or_else(|| default_port(driver));
    c.user = authority.user.unwrap_or_default();
    c.database = decode(database.trim_end_matches('/'), "database name")?;
    apply_options(&mut c, query)?;
    Ok(UrlImport {
        config: c,
        password: authority.password,
        ssh_password,
    })
}

struct Authority {
    user: Option<String>,
    password: Option<String>,
    host: String,
    port: Option<u16>,
}

fn authority(text: &str, what: &str) -> Result<Authority> {
    // An `@` or `:` inside a user name or password must be percent-encoded; the last `@`
    // separates them from the host.
    let (userinfo, hostport) = match text.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, text),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((name, pass)) => (
                Some(decode(name, "user name")?),
                Some(decode(pass, "password")?),
            ),
            None => (Some(decode(u, "user name")?), None),
        },
        None => (None, None),
    };
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let Some((host, after)) = v6.split_once(']') else {
            bail!("The {what}'s IPv6 address is missing its closing ].");
        };
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').unwrap_or("x")),
        };
        (host.to_owned(), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (decode(h, what)?, Some(p)),
            None => (decode(hostport, what)?, None),
        }
    };
    let port = match port {
        None | Some("") => None,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => Some(n),
            _ => bail!("The {what}'s port must be a number from 1 to 65535."),
        },
    };
    if host.chars().any(|c| c.is_whitespace() || c == '\0') {
        bail!("The {what}'s host name can't contain spaces.");
    }
    Ok(Authority {
        user,
        password,
        host,
        port,
    })
}

fn apply_options(c: &mut ConnectionConfig, query: &str) -> Result<()> {
    let mut safe_level = None;
    let mut advanced = 0;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode(&value.replace('+', " "), "option")?;
        match key {
            "name" => c.name = value,
            "statusColor" => {
                let hex = value.trim().trim_start_matches('#');
                if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    bail!("statusColor must be a hex colour such as 007F3D.");
                }
                c.color = format!("#{}", hex.to_ascii_lowercase());
            }
            "env" => c.tag = value.trim().to_ascii_lowercase(),
            "tLSMode" | "sslmode" | "sslMode" | "ssl-mode" => c.ssl_mode = tls_mode(&value)?,
            "usePrivateKey" => c.ssh.use_key = value.eq_ignore_ascii_case("true") || value == "1",
            "safeModeLevel" => safe_level = Some(value),
            "advancedSafeModeLevel" => {
                advanced = match value.as_str() {
                    "0" => 0,
                    "1" => 1,
                    _ => bail!("advancedSafeModeLevel must be 0 or 1."),
                }
            }
            _ => {}
        }
    }
    if let Some(level) = safe_level {
        c.safe_mode = safe_mode(&level, advanced)?;
    }
    Ok(())
}

fn tls_mode(value: &str) -> Result<SslMode> {
    if let Ok(i) = value.parse::<usize>()
        && let Some(mode) = TLS_MODES.get(i)
    {
        return Ok(*mode);
    }
    Ok(
        match value.to_ascii_uppercase().replace('_', "-").as_str() {
            "PREFERRED" | "PREFER" => SslMode::Preferred,
            "DISABLED" | "DISABLE" => SslMode::Disabled,
            "REQUIRED" | "REQUIRE" => SslMode::Required,
            "ALLOW" => SslMode::Allow,
            "VERIFY-CA" => SslMode::VerifyCa,
            "VERIFY-FULL" | "VERIFY-IDENTITY" => SslMode::VerifyFull,
            _ => bail!("Unknown SSL mode in tLSMode."),
        },
    )
}

/// `safeModeLevel` 0 is Silent, 1 Alert and 2 Safe (TablePlus: 2 is "Safe Mode 1");
/// `advancedSafeModeLevel` 1 picks the variant that lets SELECT, EXPLAIN and SHOW through.
fn safe_mode(level: &str, advanced: u8) -> Result<SafeMode> {
    Ok(match (level, advanced) {
        ("0", _) => SafeMode::Silent,
        ("1", 0) => SafeMode::Alert1,
        ("1", _) => SafeMode::Alert2,
        ("2", 0) => SafeMode::Safe1,
        ("2", _) => SafeMode::Safe2,
        _ => match serde_json::from_value(serde_json::Value::String(level.to_ascii_lowercase())) {
            Ok(mode) => mode,
            Err(_) => bail!("safeModeLevel must be 0, 1 or 2."),
        },
    })
}

fn safe_mode_levels(mode: SafeMode) -> (u8, u8) {
    match mode {
        SafeMode::Silent => (0, 0),
        SafeMode::Alert1 => (1, 0),
        SafeMode::Alert2 => (1, 1),
        SafeMode::Safe1 => (2, 0),
        SafeMode::Safe2 => (2, 1),
    }
}

/// The URL of `c`, with the passwords given (`None` leaves a password out).
pub fn connection_url(
    c: &ConnectionConfig,
    password: Option<&str>,
    ssh_password: Option<&str>,
) -> String {
    let scheme = match c.driver {
        Driver::Mysql => "mysql",
        Driver::Postgres => "postgresql",
    };
    let mut url = String::from(scheme);
    if c.ssh.enabled {
        url.push_str("+ssh://");
        push_authority(
            &mut url,
            &c.ssh.user,
            ssh_password,
            &c.ssh.host,
            (c.ssh.port != 0).then_some(c.ssh.port),
        );
        url.push('/');
    } else {
        url.push_str("://");
    }
    push_authority(&mut url, &c.user, password, &c.host, Some(c.port));
    url.push('/');
    url.push_str(&encode(&c.database));
    let (level, advanced) = safe_mode_levels(c.safe_mode);
    let tls = serde_json::to_value(c.ssl_mode)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut options = vec![
        (
            "statusColor",
            c.color.trim_start_matches('#').to_ascii_uppercase(),
        ),
        ("env", c.tag.clone()),
        ("name", c.name.clone()),
        ("tLSMode", tls),
        ("usePrivateKey", c.ssh.use_key.to_string()),
        ("safeModeLevel", level.to_string()),
        ("advancedSafeModeLevel", advanced.to_string()),
    ];
    options.retain(|(_, v)| !v.is_empty());
    url.push('?');
    let query: Vec<String> = options
        .into_iter()
        .map(|(k, v)| format!("{k}={}", encode(&v)))
        .collect();
    url.push_str(&query.join("&"));
    url
}

fn push_authority(
    url: &mut String,
    user: &str,
    password: Option<&str>,
    host: &str,
    port: Option<u16>,
) {
    let password = password.filter(|p| !p.is_empty());
    if !user.is_empty() || password.is_some() {
        url.push_str(&encode(user));
        if let Some(p) = password {
            url.push(':');
            url.push_str(&encode(p));
        }
        url.push('@');
    }
    if host.contains(':') && !host.starts_with('/') {
        url.push('[');
        url.push_str(host);
        url.push(']');
    } else {
        url.push_str(&encode(host));
    }
    if let Some(port) = port {
        url.push(':');
        url.push_str(&port.to_string());
    }
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str, what: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            let Some(b) = hex else {
                bail!("The URL's {what} has an invalid % escape.");
            };
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    match String::from_utf8(out) {
        Ok(s) => Ok(s),
        Err(_) => bail!("The URL's {what} isn't valid UTF-8."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_urls() {
        let u = parse_connection_url(
            "postgresql://alice:p%40ss%3Aw0rd@db.example.com:6543/app%20db?name=Acme%20Prod&statusColor=007F3D&env=production&tLSMode=2&safeModeLevel=2",
        )
        .unwrap();
        let c = &u.config;
        assert_eq!(c.driver, Driver::Postgres);
        assert_eq!((c.host.as_str(), c.port), ("db.example.com", 6543));
        assert_eq!(c.user, "alice");
        assert_eq!(u.password.as_deref(), Some("p@ss:w0rd"));
        assert_eq!(c.database, "app db");
        assert_eq!(c.name, "Acme Prod");
        assert_eq!(c.color, "#007f3d");
        assert_eq!(c.tag, "production");
        assert_eq!(c.ssl_mode, SslMode::Required);
        assert_eq!(c.safe_mode, SafeMode::Safe1);
        assert!(!c.ssh.enabled);
        assert_eq!(u.ssh_password, None);

        let u = parse_connection_url("MySQL://root@localhost").unwrap();
        assert_eq!(u.config.driver, Driver::Mysql);
        assert_eq!(u.config.port, 3306);
        assert_eq!(u.password, None);
        assert_eq!(u.config.database, "");
        let u = parse_connection_url("mariadb://[::1]:3307/zixma").unwrap();
        assert_eq!((u.config.host.as_str(), u.config.port), ("::1", 3307));
        assert_eq!(u.config.user, "");
        let u =
            parse_connection_url("postgres://u@h/d?sslmode=verify-full&windowMode=tabbed").unwrap();
        assert_eq!(u.config.ssl_mode, SslMode::VerifyFull);
        assert_eq!(u.config.port, 5432);
    }

    #[test]
    fn ssh_urls() {
        let u = parse_connection_url(
            "mysql+ssh://deploy:s%2Fsh%40pw@bastion.example.com:2200/root:secret@10.0.0.5:3306/zixma?usePrivateKey=true&safeModeLevel=1&advancedSafeModeLevel=1",
        )
        .unwrap();
        let s = &u.config.ssh;
        assert!(s.enabled && s.use_key);
        assert_eq!(
            (s.host.as_str(), s.port, s.user.as_str()),
            ("bastion.example.com", 2200, "deploy")
        );
        assert_eq!(u.ssh_password.as_deref(), Some("s/sh@pw"));
        assert_eq!((u.config.host.as_str(), u.config.port), ("10.0.0.5", 3306));
        assert_eq!(u.password.as_deref(), Some("secret"));
        assert_eq!(u.config.safe_mode, SafeMode::Alert2);
        let u = parse_connection_url("postgresql+ssh://bastion/postgres@db/main").unwrap();
        assert_eq!(u.config.ssh.port, 0, "no port: ~/.ssh/config or 22");
        assert_eq!(u.config.ssh.user, "");
        assert_eq!(u.config.host, "db");
    }

    #[test]
    fn round_trip() {
        let mut c = empty_connection(Driver::Postgres);
        c.name = "Prod & Co / 東京".into();
        c.host = "db.internal".into();
        c.port = 5433;
        c.user = "app@corp".into();
        c.database = "main/db".into();
        c.tag = "staging".into();
        c.color = "#ff3ad9".into();
        c.ssl_mode = SslMode::VerifyCa;
        c.safe_mode = SafeMode::Safe2;
        c.ssh = SshConfig {
            enabled: true,
            host: "bastion".into(),
            port: 2222,
            user: "ops".into(),
            use_key: true,
            ..SshConfig::default()
        };
        let pw = "p@ss:/?#&=+ wörd%";
        let url = connection_url(&c, Some(pw), Some("ssh pw"));
        assert!(url.starts_with("postgresql+ssh://ops:ssh%20pw@bastion:2222/app%40corp:"));
        assert!(!url.contains(' '));
        let back = parse_connection_url(&url).unwrap();
        assert_eq!(back.password.as_deref(), Some(pw));
        assert_eq!(back.ssh_password.as_deref(), Some("ssh pw"));
        assert_eq!(back.config, c);

        let mut plain = empty_connection(Driver::Mysql);
        plain.host = "::1".into();
        plain.port = 3307;
        let url = connection_url(&plain, None, None);
        assert_eq!(
            url,
            "mysql://[::1]:3307/?statusColor=3A7D44&tLSMode=PREFERRED&usePrivateKey=false&safeModeLevel=0&advancedSafeModeLevel=0"
        );
        let back = parse_connection_url(&url).unwrap();
        assert_eq!(back.config, plain);
        assert_eq!(back.password, None);
    }

    #[test]
    fn errors_never_repeat_the_input() {
        for bad in [
            "hunter2",
            "ftp://u:hunter2@h/d",
            "mysql://u:hunter2@h:99999/d",
            "mysql://u:hunter2%zz@h/d",
            "mysql+ssh://u:hunter2@h",
            "mysql://u:hunter2@[::1/d",
            "mysql://u:hunter2@h/d?statusColor=hunter2",
            "mysql://u:hunter2@h/d?tLSMode=hunter2",
            "mysql://u:hunter2@h/d?safeModeLevel=hunter2",
            "mysql://u:hunter2@h/d?advancedSafeModeLevel=9",
            "mysql://u:hunter2@ho st/d",
        ] {
            let e = parse_connection_url(bad).unwrap_err().to_string();
            assert!(!e.contains("hunter2"), "{bad}: {e}");
        }
    }

    #[test]
    fn ssh_server_names_that_would_act_as_known_hosts_patterns_are_refused() {
        for host in [
            "*,staging.attacker.example",
            "%2A",
            "a%2Cb",
            "h%21",
            "h%7C1",
            "h%23",
        ] {
            let url = format!("mysql+ssh://deploy@{host}/root@10.0.0.5/app?name=Staging");
            let e = parse_connection_url(&url).unwrap_err().to_string();
            assert!(e.starts_with("The SSH server name "), "{host}: {e}");
        }
        let ok = parse_connection_url("mysql+ssh://deploy@[fe80::1]:22/root@db/app").unwrap();
        assert_eq!(ok.config.ssh.host, "fe80::1");
    }

    #[test]
    fn encoding_keeps_only_unreserved_characters() {
        assert_eq!(encode("aZ09-._~"), "aZ09-._~");
        assert_eq!(encode("a b/c@d:e"), "a%20b%2Fc%40d%3Ae");
        assert_eq!(encode("é"), "%C3%A9");
        assert_eq!(decode("%C3%A9%2f", "x").unwrap(), "é/");
        assert!(decode("%", "x").is_err());
        assert!(decode("%FF", "x").is_err());
    }
}
