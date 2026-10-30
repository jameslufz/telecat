use anyhow::{Result, bail};
use telecat_core::types::*;
/// Electron permits blank names, users and database names. Port 0 uses the driver default.
/// Reject nonrepresentable ports/NULs before saving or passing values to native APIs.
pub fn validate(c: &ConnectionConfig, port_text: &str) -> Result<ConnectionConfig> {
    let digits: String = port_text.chars().filter(char::is_ascii_digit).collect();
    let port = if digits.is_empty() {
        0
    } else {
        digits
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("Port must be between 1 and 65535"))?
    };
    if port > 65535 {
        bail!("Port must be between 1 and 65535");
    }
    for (field, s) in [
        ("Name", &c.name),
        ("Host/Socket", &c.host),
        ("User", &c.user),
        ("Database", &c.database),
    ] {
        if s.contains('\0') {
            bail!("{field} cannot contain a NUL character");
        }
    }
    let mut c = c.clone();
    c.port = if port == 0 {
        default_port(c.driver)
    } else {
        port as u16
    };
    Ok(c)
}
/// The SSH part of the form and the startup SQL: a server is required over SSH, its port is
/// 1–65535 or left empty (from ~/.ssh/config, else 22), and the startup SQL must be usable.
pub fn validate_ssh(c: &ConnectionConfig, port_text: &str) -> Result<ConnectionConfig> {
    let mut c = c.clone();
    telecat_core::startup::startup_statements(&c.startup_sql, c.driver)?;
    if !c.ssh.enabled {
        return Ok(c);
    }
    let digits: String = port_text.chars().filter(char::is_ascii_digit).collect();
    let port = if digits.is_empty() {
        0
    } else {
        match digits.parse::<u32>() {
            Ok(p) if (1..=65535).contains(&p) => p as u16,
            _ => bail!("SSH port must be between 1 and 65535"),
        }
    };
    c.ssh.port = port;
    c.ssh.host = c.ssh.host.trim().to_owned();
    c.ssh.user = c.ssh.user.trim().to_owned();
    for (field, s) in [
        ("SSH server", c.ssh.host.as_str()),
        ("SSH user", c.ssh.user.as_str()),
        ("SSH key", c.ssh.key_path.as_deref().unwrap_or("")),
    ] {
        if s.contains('\0') {
            bail!("{field} cannot contain a NUL character");
        }
    }
    if c.ssh.host.is_empty() {
        bail!("Enter the SSH server, or turn off Over SSH.");
    }
    if let Some(why) = ssh_host_problem(&c.ssh.host) {
        bail!("The SSH server name {why}.");
    }
    if c.host.starts_with('/') {
        bail!(
            "Over SSH, Host must be the database's host name or address as the SSH server sees it, not a socket path."
        );
    }
    if c.ssh.use_key && c.ssh.key_path.as_deref().is_none_or(str::is_empty) {
        bail!("Choose the SSH private key, or turn off Use SSH key.");
    }
    Ok(c)
}
/// What the form shows for the startup SQL: how many statements, or why it can't be used.
pub fn startup_summary(c: &ConnectionConfig) -> String {
    match telecat_core::startup::startup_statements(&c.startup_sql, c.driver) {
        Ok(s) if s.is_empty() => "None".into(),
        Ok(s) if s.len() == 1 => "1 statement".into(),
        Ok(s) => format!("{} statements", s.len()),
        Err(_) => "Can't be used: see Edit…".into(),
    }
}
pub fn ssh_password_placeholder(mode: PasswordMode) -> &'static str {
    match mode {
        PasswordMode::Ask => "asked on connect (used for Test)",
        PasswordMode::None => "no password: key, ssh-agent or ~/.ssh/config",
        PasswordMode::Command => "command that prints the SSH password",
        PasswordMode::Keychain => "password",
    }
}
pub fn passphrase_placeholder(mode: PassphraseMode) -> &'static str {
    match mode {
        PassphraseMode::Ask => "asked on connect if the key is encrypted",
        PassphraseMode::Keychain => "key passphrase, if it has one",
    }
}
/// A status colour typed in the custom colour sheet, as `#rrggbb`; the `#` may be left out.
pub fn parse_hex_color(input: &str) -> Result<String> {
    let hex = input.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Type a colour as #RRGGBB, for example #3A7D44.");
    }
    Ok(format!("#{}", hex.to_ascii_lowercase()))
}
pub fn password_placeholder(mode: PasswordMode) -> &'static str {
    match mode {
        PasswordMode::Ask => "asked on connect (used for Test)",
        PasswordMode::None => "no password",
        PasswordMode::Command => {
            "command that prints the password, e.g. op read op://vault/db/password"
        }
        PasswordMode::Keychain => "password",
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormStatus {
    Idle,
    Busy(String),
    Ok(String),
    Error(String),
}
impl FormStatus {
    pub fn busy(&self) -> bool {
        matches!(self, Self::Busy(_))
    }
}
