//! Opening a connection's session, SSH included, and what to tell the user about its host key.
use crate::credentials::{Keychain, Typed, resolve_credentials};
use anyhow::Result;
use telecat_core::types::ConnectionConfig;
use telecat_db::{
    Session,
    ssh::{HostKeyKind, HostKeyProblem, SshEnv, SshError, ssh_error},
};

/// Resolves every secret `c` needs (typed ones first) and opens the session, through its SSH
/// server when it has one.
pub async fn open_session(
    c: &ConnectionConfig,
    typed: &Typed,
    keychain: &dyn Keychain,
    ssh_env: &SshEnv,
) -> Result<Session> {
    let credentials = resolve_credentials(c, typed, keychain).await?;
    Session::connect_with(c, credentials, Some(ssh_env)).await
}

/// The host key problem that stopped a connect, which the user has to decide on.
pub fn host_key_problem(e: &anyhow::Error) -> Option<HostKeyProblem> {
    match ssh_error(e) {
        Some(SshError::HostKey(p)) => Some((**p).clone()),
        _ => None,
    }
}

/// The sheet shown for a host key problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyPrompt {
    pub title: String,
    pub detail: String,
    /// The presented key: type and SHA256 fingerprint.
    pub presented: String,
    /// Keys on record for the host, with where they are recorded.
    pub recorded: Vec<String>,
    /// The button that trusts the presented key; `None` when it can't be trusted.
    pub trust: Option<&'static str>,
    /// A key change: Cancel is the default and the sheet is a warning.
    pub warning: bool,
}

pub fn host_key_prompt(p: &HostKeyProblem) -> HostKeyPrompt {
    let endpoint = p.endpoint();
    let presented = format!(
        "{} key fingerprint: {}",
        key_type(&p.key.algorithm),
        p.fingerprint()
    );
    let recorded = p
        .recorded
        .iter()
        .map(|r| {
            format!(
                "{} {} ({}:{})",
                key_type(&r.algorithm),
                r.fingerprint,
                r.path.display(),
                r.line
            )
        })
        .collect();
    match p.kind {
        HostKeyKind::Unknown => HostKeyPrompt {
            title: format!("Trust the SSH server {endpoint}?"),
            detail: "Telecat hasn't connected to this server before. Check that the fingerprint below matches the server's (ask its administrator, or run ssh-keygen -lf on its host key) before trusting it.".into(),
            presented,
            recorded,
            trust: Some("Trust"),
            warning: false,
        },
        HostKeyKind::Changed => HostKeyPrompt {
            title: format!("Warning: the SSH host key of {endpoint} has changed"),
            detail: "The server presented a different key from the one on record. Someone may be intercepting the connection (a man-in-the-middle attack), or the server's key was replaced. Only trust the new key if you know it changed.".into(),
            presented,
            recorded,
            trust: Some("Trust New Key"),
            warning: true,
        },
        HostKeyKind::Revoked => HostKeyPrompt {
            title: format!("The SSH host key of {endpoint} is revoked"),
            detail: "This key is marked @revoked in a known_hosts file, so Telecat won't connect to it.".into(),
            presented,
            recorded,
            trust: None,
            warning: true,
        },
    }
}

fn key_type(algorithm: &str) -> &str {
    match algorithm {
        "ssh-ed25519" => "ED25519",
        "ssh-rsa" => "RSA",
        a if a.starts_with("ecdsa-") => "ECDSA",
        a => a,
    }
}
