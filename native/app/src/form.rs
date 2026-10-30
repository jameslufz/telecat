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
