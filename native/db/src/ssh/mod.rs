//! Reaching a database through an SSH server (pure Rust, `russh`).
//!
//! A [`Tunnel`] logs in to the SSH server and listens on 127.0.0.1 at a port the system picks.
//! Before each database connection Telecat opens a `direct-tcpip` channel to the database as
//! the SSH server sees it; the connection then reaches the listener and is paired with that
//! channel. A local connection nobody asked for gets no channel and is closed.
pub mod config;
pub mod known_hosts;
mod tunnel;

pub(crate) use tunnel::Tunnel;

use known_hosts::{HostKey, Recorded};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The SSH secrets a connection was opened with, kept to reopen it. Never printed.
#[derive(Clone, Default)]
pub struct SshSecrets {
    pub password: Option<Zeroizing<String>>,
    pub passphrase: Option<Zeroizing<String>>,
}
impl std::fmt::Debug for SshSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshSecrets")
            .field("password", &self.password.as_ref().map(|_| "…"))
            .field("passphrase", &self.passphrase.as_ref().map(|_| "…"))
            .finish()
    }
}
impl SshSecrets {
    pub fn new(password: Option<String>, passphrase: Option<String>) -> Self {
        let keep = |s: Option<String>| s.filter(|s| !s.is_empty()).map(Zeroizing::new);
        Self {
            password: keep(password),
            passphrase: keep(passphrase),
        }
    }
}

/// Where SSH settings, keys and known hosts come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshEnv {
    pub home: PathBuf,
    pub local_user: String,
    /// ~/.ssh/config; read only.
    pub config: Option<PathBuf>,
    /// Telecat's own known_hosts file: the only one Telecat writes.
    pub known_hosts: PathBuf,
    /// The user's known_hosts files (when ~/.ssh/config names none); read only.
    pub user_known_hosts: Vec<PathBuf>,
    pub global_known_hosts: Vec<PathBuf>,
    /// The ssh-agent socket (SSH_AUTH_SOCK).
    pub agent: Option<PathBuf>,
}
impl SshEnv {
    /// The user's SSH setup, with Telecat's known_hosts file at `known_hosts`.
    pub fn system(known_hosts: PathBuf) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let local_user = std::env::var("USER").unwrap_or_default();
        let agent = std::env::var_os("SSH_AUTH_SOCK")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        Self {
            config: Some(home.join(".ssh/config")),
            user_known_hosts: vec![
                home.join(".ssh/known_hosts"),
                home.join(".ssh/known_hosts2"),
            ],
            global_known_hosts: vec![
                PathBuf::from("/etc/ssh/ssh_known_hosts"),
                PathBuf::from("/etc/ssh/ssh_known_hosts2"),
            ],
            home,
            local_user,
            known_hosts,
            agent,
        }
    }
    /// Everything under `dir`: no real home, config, known hosts or agent. For tests.
    pub fn isolated(dir: &Path) -> Self {
        Self {
            home: dir.to_owned(),
            local_user: "telecat".into(),
            config: Some(dir.join(".ssh/config")),
            known_hosts: dir.join("telecat_known_hosts"),
            user_known_hosts: vec![dir.join(".ssh/known_hosts")],
            global_known_hosts: vec![],
            agent: None,
        }
    }
}

/// What went wrong on the SSH side of a connection. Shown as "SSH: …".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    /// The SSH server can't be reached (refused, timed out, name not found).
    Unreachable(String),
    /// The SSH connection dropped.
    Lost(String),
    /// The SSH server couldn't reach the database, or wouldn't forward to it.
    Forward {
        message: String,
        retryable: bool,
    },
    /// The server refused every login method tried.
    Auth(String),
    /// A private key couldn't be used: unreadable, wrong passphrase, unsupported type.
    Key(String),
    /// The settings can't work (no server, a ProxyJump in ~/.ssh/config, a socket path…).
    Config(String),
    HostKey(Box<HostKeyProblem>),
}
impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SSH: ")?;
        match self {
            Self::Unreachable(m)
            | Self::Lost(m)
            | Self::Auth(m)
            | Self::Key(m)
            | Self::Config(m)
            | Self::Forward { message: m, .. } => f.write_str(m),
            Self::HostKey(p) => f.write_str(&p.summary()),
        }
    }
}
impl std::error::Error for SshError {}
impl SshError {
    /// Whether trying again later may work: the network, not the settings or a host key.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unreachable(_) | Self::Lost(_) => true,
            Self::Forward { retryable, .. } => *retryable,
            Self::Auth(_) | Self::Key(_) | Self::Config(_) | Self::HostKey(_) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyKind {
    /// Not on record: ask before trusting it.
    Unknown,
    /// A different key is on record: someone may be intercepting the connection.
    Changed,
    /// Marked `@revoked`: never trusted.
    Revoked,
}

/// The SSH server's host key couldn't be trusted without asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyProblem {
    pub kind: HostKeyKind,
    /// The name the key is recorded under (`HostKeyAlias`, else the host connected to).
    pub host: String,
    pub port: u16,
    pub key: HostKey,
    /// The keys on record for the host (`Changed`), or the revocation (`Revoked`).
    pub recorded: Vec<Recorded>,
}
impl HostKeyProblem {
    pub fn fingerprint(&self) -> String {
        self.key.fingerprint()
    }
    pub fn endpoint(&self) -> String {
        if self.port == 22 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
    pub fn summary(&self) -> String {
        match self.kind {
            HostKeyKind::Unknown => format!(
                "the host key of {} isn't known yet ({} {})",
                self.endpoint(),
                self.key.algorithm,
                self.fingerprint()
            ),
            HostKeyKind::Changed => format!(
                "host key mismatch for {}: the server presented {} {}, which differs from the key on record",
                self.endpoint(),
                self.key.algorithm,
                self.fingerprint()
            ),
            HostKeyKind::Revoked => format!(
                "the host key of {} ({}) is marked as revoked",
                self.endpoint(),
                self.fingerprint()
            ),
        }
    }
}

/// Records the presented key of `problem` in Telecat's known_hosts file, replacing what Telecat
/// had for the host. A revoked key is refused.
pub fn trust_host_key(env: &SshEnv, problem: &HostKeyProblem) -> anyhow::Result<()> {
    anyhow::ensure!(
        problem.kind != HostKeyKind::Revoked,
        "A revoked host key can't be trusted."
    );
    known_hosts::trust(&env.known_hosts, &problem.host, problem.port, &problem.key)?;
    Ok(())
}

/// Whether the private key at `path` is encrypted, so its passphrase must be asked for.
pub fn key_is_encrypted(path: &Path) -> anyhow::Result<bool> {
    let text = tunnel::read_key_file(path).map_err(anyhow::Error::new)?;
    Ok(tunnel::encrypted(&text))
}

/// The `SshError` in `e`'s chain, if any.
pub fn ssh_error(e: &anyhow::Error) -> Option<&SshError> {
    e.chain().find_map(|c| c.downcast_ref::<SshError>())
}
