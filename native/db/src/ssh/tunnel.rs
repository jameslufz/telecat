use super::{
    HostKeyKind, HostKeyProblem, SshEnv, SshError, SshSecrets,
    config::{self, expand_tilde, expand_tokens},
    known_hosts::{self, HostKey, Verdict},
};
use russh::{
    Channel, ChannelOpenFailure, Disconnect, MethodKind,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse, Msg},
    keys::{
        self, Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey,
        agent::{AgentIdentity, client::AgentClient},
    },
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use telecat_core::types::{ConnectionConfig, Driver};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
use zeroize::Zeroizing;

const TCP_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// An ssh-agent may wait for the user to approve the key (1Password, Secretive).
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(5);
/// With three unanswered keepalives the link counts as dead: about 40 s on a silent network.
const KEEPALIVE: Duration = Duration::from_secs(10);
/// A channel opened for a database connection that never reached the listener is dropped
/// after this long.
const PENDING_TTL: Duration = Duration::from_secs(30);
/// OpenSSH servers disconnect after 6 failed attempts (MaxAuthTries).
const MAX_KEY_TRIES: usize = 5;
const MAX_KEY_FILE: u64 = 256 * 1024;

/// Where and as whom to connect, with ~/.ssh/config applied.
#[derive(Debug, Clone)]
struct Plan {
    driver: Driver,
    host: String,
    port: u16,
    user: String,
    /// The name host keys are recorded under.
    key_host: String,
    explicit_key: Option<PathBuf>,
    identity_files: Vec<PathBuf>,
    identities_only: bool,
    agent: Option<PathBuf>,
    user_known_hosts: Vec<PathBuf>,
    target_host: String,
    target_port: u16,
}
impl Plan {
    fn new(c: &ConnectionConfig, env: &SshEnv) -> Result<Self, SshError> {
        let alias = c.ssh.host.trim();
        if alias.is_empty() {
            return Err(SshError::Config("enter the SSH server.".into()));
        }
        if c.host.starts_with('/') {
            return Err(SshError::Config(
                "over SSH, Host must be the database's host name or address as the SSH server sees it, not a socket path.".into(),
            ));
        }
        let named = |what: &str, name: &str| match telecat_core::types::ssh_host_problem(name) {
            Some(why) => Err(SshError::Config(format!("the {what} {why}."))),
            None => Ok(()),
        };
        named("SSH server name", alias)?;
        let settings = env
            .config
            .as_deref()
            .map(|p| config::resolve(p, &env.home, alias))
            .unwrap_or_default();
        if let Some(proxy) = settings.proxy.filter(|p| !p.is_empty()) {
            return Err(SshError::Config(format!(
                "~/.ssh/config sets {proxy} for {alias}, which Telecat doesn't support. Enter the server Telecat can reach directly."
            )));
        }
        let port = if c.ssh.port != 0 {
            c.ssh.port
        } else {
            settings.port.unwrap_or(22)
        };
        let user = if !c.ssh.user.trim().is_empty() {
            c.ssh.user.trim().to_owned()
        } else {
            settings
                .user
                .clone()
                .unwrap_or_else(|| env.local_user.clone())
        };
        let host = match &settings.host_name {
            Some(h) => expand_tokens(h, &env.home, &env.local_user, alias, &user, port, alias),
            None => alias.to_owned(),
        };
        let expand = |p: &str| {
            PathBuf::from(expand_tokens(
                p,
                &env.home,
                &env.local_user,
                &host,
                &user,
                port,
                alias,
            ))
        };
        let mut identity_files: Vec<PathBuf> =
            settings.identity_files.iter().map(|p| expand(p)).collect();
        if identity_files.is_empty() {
            identity_files = ["id_ed25519", "id_ecdsa", "id_rsa"]
                .iter()
                .map(|n| env.home.join(".ssh").join(n))
                .filter(|p| p.exists())
                .collect();
        }
        let agent = match settings.identity_agent.as_deref() {
            None | Some("SSH_AUTH_SOCK") => env.agent.clone(),
            Some(none) if none.eq_ignore_ascii_case("none") => None,
            Some(path) => Some(expand(path)),
        };
        let mut user_known_hosts: Vec<PathBuf> = match &settings.user_known_hosts_files {
            Some(files) => files
                .iter()
                .filter(|f| !f.eq_ignore_ascii_case("none"))
                .map(|f| expand(f))
                .collect(),
            None => env.user_known_hosts.clone(),
        };
        user_known_hosts.extend(env.global_known_hosts.iter().cloned());
        let key_host = settings
            .host_key_alias
            .clone()
            .unwrap_or_else(|| host.clone());
        // ~/.ssh/config can name the host too: neither name may act as a known_hosts pattern.
        named("HostName from ~/.ssh/config", &host)?;
        named("HostKeyAlias from ~/.ssh/config", &key_host)?;
        Ok(Self {
            driver: c.driver,
            key_host,
            host,
            port,
            user,
            explicit_key: c
                .ssh
                .key_file()
                .map(|p| PathBuf::from(expand_tilde(p, &env.home))),
            identity_files,
            identities_only: settings.identities_only.unwrap_or(false),
            agent,
            user_known_hosts,
            target_host: c.host.trim().to_owned(),
            target_port: c.port,
        })
    }
    fn endpoint(&self) -> String {
        if self.port == 22 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
    fn target(&self) -> String {
        format!("{}:{}", self.target_host, self.target_port)
    }
}

/// A logged-in SSH session forwarding one local port to the database.
pub(crate) struct Tunnel {
    port: u16,
    shared: Arc<Shared>,
    accept: tokio::task::JoinHandle<()>,
}
type Handle = client::Handle<Client>;
struct Shared {
    plan: Plan,
    env: SshEnv,
    secrets: Mutex<SshSecrets>,
    fenced: bool,
    session: tokio::sync::Mutex<Option<Arc<Handle>>>,
    pending: Mutex<VecDeque<(Instant, Channel<Msg>)>>,
    links: Mutex<JoinSet<()>>,
}
impl Tunnel {
    /// Logs in to the SSH server of `c` and starts listening. `fenced` (tests) only lets it
    /// reach the test SSH server and, through it, the test databases.
    pub(crate) async fn open(
        c: &ConnectionConfig,
        secrets: SshSecrets,
        env: &SshEnv,
        fenced: bool,
    ) -> Result<Arc<Self>, SshError> {
        let plan = Plan::new(c, env)?;
        let shared = Arc::new(Shared {
            plan,
            env: env.clone(),
            secrets: Mutex::new(secrets),
            fenced,
            session: tokio::sync::Mutex::new(None),
            pending: Mutex::new(VecDeque::new()),
            links: Mutex::new(JoinSet::new()),
        });
        shared.fence()?;
        shared.live().await?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| {
            SshError::Config(format!("couldn't open a local port for the tunnel ({e})"))
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| SshError::Config(format!("couldn't open a local port ({e})")))?
            .port();
        let accept = tokio::spawn(accept_loop(listener, Arc::downgrade(&shared)));
        Ok(Arc::new(Self {
            port,
            shared,
            accept,
        }))
    }
    /// The local port database connections go to.
    pub(crate) fn port(&self) -> u16 {
        self.port
    }
    /// Opens a channel to the database for the next connection to the local port, logging in
    /// again first if the SSH connection dropped.
    pub(crate) async fn prepare(&self) -> Result<(), SshError> {
        let s = &self.shared;
        s.fence()?;
        let handle = s.live().await?;
        let opened = tokio::time::timeout(
            CHANNEL_TIMEOUT,
            handle.channel_open_direct_tcpip(
                s.plan.target_host.clone(),
                u32::from(s.plan.target_port),
                "127.0.0.1",
                u32::from(self.port),
            ),
        )
        .await;
        let channel = match opened {
            Ok(Ok(channel)) => channel,
            Ok(Err(e)) => return Err(forward_error(&s.plan, e)),
            Err(_) => {
                return Err(SshError::Lost(format!(
                    "the SSH server {} didn't answer in time.",
                    s.plan.endpoint()
                )));
            }
        };
        let mut pending = s.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|(at, _)| at.elapsed() < PENDING_TTL);
        pending.push_back((Instant::now(), channel));
        Ok(())
    }
    /// Before reopening a session: checks the SSH connection still answers, and logs in again
    /// when it doesn't.
    pub(crate) async fn refresh(&self) -> Result<(), SshError> {
        let s = &self.shared;
        let current = s.session.lock().await.clone();
        if let Some(handle) = current {
            let alive = !handle.is_closed()
                && tokio::time::timeout(PING_TIMEOUT, handle.send_ping())
                    .await
                    .is_ok_and(|r| r.is_ok())
                && !handle.is_closed();
            if alive {
                return Ok(());
            }
            let mut slot = s.session.lock().await;
            if slot.as_ref().is_some_and(|h| Arc::ptr_eq(h, &handle)) {
                slot.take();
            }
            disconnect(handle);
        }
        s.live().await.map(|_| ())
    }
    /// The SSH connection is known to be gone (and nobody is logging in again right now).
    pub(crate) fn is_down(&self) -> bool {
        self.shared
            .session
            .try_lock()
            .is_ok_and(|slot| slot.as_ref().is_none_or(|h| h.is_closed()))
    }
    /// The secrets to log in with from now on (reconnecting with a password read again).
    pub(crate) fn set_secrets(&self, secrets: SshSecrets) {
        *self
            .shared
            .secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = secrets;
    }
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.accept.abort();
        let s = &self.shared;
        s.links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .abort_all();
        s.pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
        if let Ok(mut slot) = s.session.try_lock()
            && let Some(handle) = slot.take()
        {
            disconnect(handle);
        }
    }
}
/// Ends the SSH connection; dropping the last handle ends it too when no runtime is at hand.
fn disconnect(handle: Arc<Handle>) {
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                handle.disconnect(Disconnect::ByApplication, "", "en"),
            )
            .await;
        });
    }
}
impl Shared {
    fn fence(&self) -> Result<(), SshError> {
        if !(self.fenced || crate::wire::fence_enabled()) {
            return Ok(());
        }
        let p = &self.plan;
        if !crate::wire::test_ssh_endpoint(&p.host, p.port)
            || !crate::wire::test_ssh_target(p.driver, &p.target_host, p.target_port)
        {
            return Err(SshError::Config(
                "Test safety: only the test SSH server and test databases are permitted".into(),
            ));
        }
        Ok(())
    }
    /// The SSH connection, logging in again when it dropped.
    async fn live(&self) -> Result<Arc<Handle>, SshError> {
        let mut slot = self.session.lock().await;
        if let Some(h) = slot.as_ref().filter(|h| !h.is_closed()) {
            return Ok(h.clone());
        }
        if let Some(dead) = slot.take() {
            disconnect(dead);
        }
        let secrets = self
            .secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let handle = Arc::new(connect(&self.plan, &self.env, &secrets).await?);
        *slot = Some(handle.clone());
        Ok(handle)
    }
    fn take_pending(&self) -> Option<Channel<Msg>> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while let Some((at, channel)) = pending.pop_front() {
            if at.elapsed() < PENDING_TTL {
                return Some(channel);
            }
        }
        None
    }
}

async fn accept_loop(listener: TcpListener, shared: std::sync::Weak<Shared>) {
    loop {
        let accepted = listener.accept().await;
        let Some(s) = shared.upgrade() else {
            return;
        };
        let socket = match accepted {
            Ok((socket, _)) => socket,
            Err(_) => {
                // Out of file descriptors and the like: back off instead of spinning.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        // A connection Telecat didn't prepare a channel for (another local program) is closed.
        let Some(channel) = s.take_pending() else {
            continue;
        };
        let _ = socket.set_nodelay(true);
        let mut links = s.links.lock().unwrap_or_else(|e| e.into_inner());
        while links.try_join_next().is_some() {}
        links.spawn(async move {
            let mut socket = socket;
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
        });
    }
}

fn forward_error(plan: &Plan, e: russh::Error) -> SshError {
    let target = plan.target();
    match e {
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited) => {
            SshError::Forward {
                message: format!(
                    "the SSH server doesn't allow forwarding to {target} (administratively prohibited)."
                ),
                retryable: false,
            }
        }
        russh::Error::ChannelOpenFailure(reason) => SshError::Forward {
            message: format!(
                "the SSH server couldn't connect to the database at {target} ({}).",
                match reason {
                    ChannelOpenFailure::ConnectFailed => "connection failed".to_owned(),
                    ChannelOpenFailure::Other { reason, .. } if !reason.is_empty() => reason,
                    other => format!("{other:?}"),
                }
            ),
            retryable: true,
        },
        other => SshError::Lost(format!(
            "the connection to the SSH server {} was lost ({other}).",
            plan.endpoint()
        )),
    }
}

/// Checks the server's host key and remembers what it found, for the error to show.
struct Client {
    telecat_known_hosts: PathBuf,
    user_known_hosts: Vec<PathBuf>,
    key_host: String,
    port: u16,
    seen: Arc<Mutex<Option<(Verdict, HostKey)>>>,
}
impl client::Handler for Client {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public = match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            russh::keys::PublicKeyOrCertificate::Certificate(cert) => {
                PublicKey::new(cert.public_key().clone(), "")
            }
        };
        let Some(host_key) = public.to_bytes().ok().and_then(HostKey::from_blob) else {
            return Ok(false);
        };
        let verdict = known_hosts::verify(
            &self.telecat_known_hosts,
            &self.user_known_hosts,
            &self.key_host,
            self.port,
            &host_key,
        );
        let trusted = verdict == Verdict::Trusted;
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some((verdict, host_key));
        Ok(trusted)
    }
}

/// Host key algorithms, with the ones on record for this host first, so the server presents a
/// key that can be checked (OpenSSH does the same).
fn preferred(known: &[String]) -> russh::Preferred {
    let family = |a: &Algorithm| match a {
        Algorithm::Rsa { .. } => "ssh-rsa".to_owned(),
        other => other.as_str().to_owned(),
    };
    let mut p = russh::Preferred::default();
    let mut keys = p.key.to_vec();
    keys.sort_by_key(|a| {
        known
            .iter()
            .position(|k| *k == family(a))
            .unwrap_or(usize::MAX)
    });
    p.key = keys.into();
    p
}

async fn connect(plan: &Plan, env: &SshEnv, secrets: &SshSecrets) -> Result<Handle, SshError> {
    let endpoint = plan.endpoint();
    let stream = tcp(plan).await?;
    let seen = Arc::new(Mutex::new(None));
    let handler = Client {
        telecat_known_hosts: env.known_hosts.clone(),
        user_known_hosts: plan.user_known_hosts.clone(),
        key_host: plan.key_host.clone(),
        port: plan.port,
        seen: seen.clone(),
    };
    let known = known_hosts::known_algorithms(
        &env.known_hosts,
        &plan.user_known_hosts,
        &plan.key_host,
        plan.port,
    );
    let config = Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE),
        keepalive_max: 3,
        nodelay: true,
        preferred: preferred(&known),
        ..Default::default()
    });
    let started = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        client::connect_stream(config, stream, handler),
    )
    .await;
    let mut handle = match started {
        Ok(Ok(handle)) => handle,
        Ok(Err(russh::Error::UnknownKey)) => {
            let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).take();
            return Err(match seen {
                Some((verdict, key)) => host_key_error(plan, verdict, key),
                None => SshError::Unreachable(format!(
                    "the SSH server {endpoint} presented a host key Telecat can't read."
                )),
            });
        }
        Ok(Err(e)) => {
            return Err(SshError::Unreachable(format!(
                "couldn't start an SSH session with {endpoint} ({e})."
            )));
        }
        Err(_) => {
            return Err(SshError::Unreachable(format!(
                "the SSH server {endpoint} didn't complete the handshake in time."
            )));
        }
    };
    match tokio::time::timeout(AUTH_TIMEOUT, authenticate(&mut handle, plan, secrets)).await {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(SshError::Auth(format!(
            "logging in to {endpoint} took too long."
        ))),
    }
}

fn host_key_error(plan: &Plan, verdict: Verdict, key: HostKey) -> SshError {
    let (kind, recorded) = match verdict {
        Verdict::Unknown | Verdict::Trusted => (HostKeyKind::Unknown, vec![]),
        Verdict::Changed(r) => (HostKeyKind::Changed, r),
        Verdict::Revoked(r) => (HostKeyKind::Revoked, vec![r]),
    };
    SshError::HostKey(Box::new(HostKeyProblem {
        kind,
        host: plan.key_host.clone(),
        port: plan.port,
        key,
        recorded,
    }))
}

async fn tcp(plan: &Plan) -> Result<TcpStream, SshError> {
    let endpoint = plan.endpoint();
    let connect = async {
        let addrs: Vec<_> = tokio::net::lookup_host((plan.host.as_str(), plan.port))
            .await
            .map_err(|_| {
                SshError::Unreachable(format!("can't find the SSH server {}.", plan.host))
            })?
            .collect();
        let mut last = None;
        for addr in addrs {
            match TcpStream::connect(addr).await {
                Ok(s) => return Ok(s),
                Err(e) => last = Some(e),
            }
        }
        Err(SshError::Unreachable(match last {
            Some(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                format!("the SSH server {endpoint} refused the connection.")
            }
            Some(e) => format!("can't reach the SSH server {endpoint} ({e})."),
            None => format!("can't find the SSH server {}.", plan.host),
        }))
    };
    let stream = tokio::time::timeout(TCP_TIMEOUT, connect)
        .await
        .map_err(|_| {
            SshError::Unreachable(format!(
                "the SSH server {endpoint} didn't answer within {} s.",
                TCP_TIMEOUT.as_secs()
            ))
        })??;
    let _ = stream.set_nodelay(true);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(15))
        .with_interval(Duration::from_secs(5));
    let _ = socket2::SockRef::from(&stream).set_tcp_keepalive(&keepalive);
    Ok(stream)
}

enum Attempt {
    Done,
    Failed,
}
fn outcome(r: AuthResult) -> Attempt {
    match r {
        AuthResult::Success => Attempt::Done,
        // Partial success (a second factor is required) moves on to the next method.
        AuthResult::Failure { .. } => Attempt::Failed,
    }
}
fn lost(plan: &Plan, e: impl std::fmt::Display) -> SshError {
    SshError::Lost(format!(
        "the SSH server {} closed the connection while logging in ({e}).",
        plan.endpoint()
    ))
}

/// Logs in with the key chosen in the form, then the password; with neither, with the
/// ssh-agent's keys and the identity files from ~/.ssh/config (or the default ones).
async fn authenticate(
    handle: &mut Handle,
    plan: &Plan,
    secrets: &SshSecrets,
) -> Result<(), SshError> {
    let user = plan.user.clone();
    let mut tried: Vec<String> = vec![];
    // Encrypted identity files left out for want of a passphrase, to say so if nothing works.
    let mut skipped: Vec<String> = vec![];
    let rsa_hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(|e| lost(plan, e))?
        .unwrap_or(Some(HashAlg::Sha512));
    let passphrase = secrets.passphrase.as_ref().map(|p| p.as_str());
    if let Some(path) = &plan.explicit_key {
        let key = load_key(path, passphrase)?;
        tried.push(format!("key {}", file_name(path)));
        if let Attempt::Done = try_key(handle, &user, key, rsa_hash, plan).await? {
            return Ok(());
        }
    }
    if let Some(password) = &secrets.password {
        tried.push("password".into());
        let r = handle
            .authenticate_password(user.clone(), password.as_str())
            .await
            .map_err(|e| lost(plan, e))?;
        let interactive = matches!(&r, AuthResult::Failure { remaining_methods, .. } if remaining_methods.contains(&MethodKind::KeyboardInteractive));
        if let Attempt::Done = outcome(r) {
            return Ok(());
        }
        if interactive && keyboard_interactive(handle, &user, password, plan).await? {
            return Ok(());
        }
    }
    if plan.explicit_key.is_none() && secrets.password.is_none() {
        let mut budget = MAX_KEY_TRIES;
        if let Some(sock) = plan.agent.as_ref().filter(|_| !plan.identities_only)
            && let Ok(mut agent) = AgentClient::connect_uds(sock).await
        {
            let identities = agent.request_identities().await.unwrap_or_default();
            if !identities.is_empty() {
                tried.push("ssh-agent".into());
            }
            for identity in identities {
                if budget == 0 {
                    break;
                }
                let AgentIdentity::PublicKey { key, .. } = identity else {
                    continue;
                };
                budget -= 1;
                let hash = key.algorithm().is_rsa().then_some(rsa_hash).flatten();
                match handle
                    .authenticate_publickey_with(user.clone(), key, hash, &mut agent)
                    .await
                {
                    Ok(AuthResult::Success) => return Ok(()),
                    Ok(_) => {}
                    // The agent declined or lost the key: try the next one.
                    Err(russh::AgentAuthError::Key(_)) => {}
                    Err(e) => return Err(lost(plan, e)),
                }
            }
        }
        for path in &plan.identity_files {
            if budget == 0 {
                break;
            }
            // Keys that can't be used without a passphrase nobody gave are skipped, as
            // `ssh -o BatchMode=yes` does.
            let key = match load_key(path, passphrase) {
                Ok(key) => key,
                Err(_) => {
                    if read_key_file(path).is_ok_and(|text| encrypted(&text)) {
                        skipped.push(file_name(path));
                    }
                    continue;
                }
            };
            budget -= 1;
            tried.push(format!("key {}", file_name(path)));
            if let Attempt::Done = try_key(handle, &user, key, rsa_hash, plan).await? {
                return Ok(());
            }
        }
    }
    let mut message = if tried.is_empty() {
        format!(
            "no password, key or ssh-agent key to log in to {} as {user}. Enter the SSH password or choose a private key.",
            plan.endpoint()
        )
    } else {
        format!(
            "authentication failed for {user}@{} (tried {}).",
            plan.endpoint(),
            tried.join(", ")
        )
    };
    if !skipped.is_empty() {
        message.push_str(&format!(
            " Skipped encrypted key {} (no passphrase): add it to ssh-agent with ssh-add, or choose it under Use SSH key with its passphrase.",
            skipped.join(", ")
        ));
    }
    Err(SshError::Auth(message))
}

async fn try_key(
    handle: &mut Handle,
    user: &str,
    key: PrivateKey,
    rsa_hash: Option<HashAlg>,
    plan: &Plan,
) -> Result<Attempt, SshError> {
    let hash = key.algorithm().is_rsa().then_some(rsa_hash).flatten();
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
    handle
        .authenticate_publickey(user.to_owned(), key)
        .await
        .map(outcome)
        .map_err(|e| lost(plan, e))
}

/// Keyboard-interactive login answering a single hidden prompt with the password, the way
/// servers that only allow PAM ask for it. More prompts (a one-time code) aren't answered.
async fn keyboard_interactive(
    handle: &mut Handle,
    user: &str,
    password: &Zeroizing<String>,
    plan: &Plan,
) -> Result<bool, SshError> {
    let mut response = handle
        .authenticate_keyboard_interactive_start(user.to_owned(), None)
        .await
        .map_err(|e| lost(plan, e))?;
    for _ in 0..4 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let hidden = prompts.iter().filter(|p| !p.echo).count();
                if prompts.len() > 1 || (prompts.len() == 1 && hidden != 1) {
                    return Ok(false);
                }
                let answers = prompts.iter().map(|_| password.to_string()).collect();
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|e| lost(plan, e))?;
            }
        }
    }
    Ok(false)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

pub(super) fn read_key_file(path: &Path) -> Result<Zeroizing<String>, SshError> {
    let name = file_name(path);
    let unreadable =
        |e: std::io::Error| SshError::Key(format!("can't read the private key file {name} ({e})."));
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if meta.len() > MAX_KEY_FILE {
        return Err(SshError::Key(format!(
            "{name} is too large to be a private key."
        )));
    }
    let bytes = Zeroizing::new(std::fs::read(path).map_err(unreadable)?);
    match std::str::from_utf8(&bytes) {
        Ok(text) => Ok(Zeroizing::new(text.to_owned())),
        Err(_) => Err(SshError::Key(format!(
            "{name} isn't a private key in OpenSSH or PEM format."
        ))),
    }
}

/// Whether a private key needs a passphrase: OpenSSH format with a cipher, or encrypted PEM.
pub(super) fn encrypted(text: &str) -> bool {
    if text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        return PrivateKey::from_openssh(text.trim()).is_ok_and(|k| k.is_encrypted());
    }
    text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
        || text.contains("Proc-Type: 4,ENCRYPTED")
}

fn load_key(path: &Path, passphrase: Option<&str>) -> Result<PrivateKey, SshError> {
    let name = file_name(path);
    let text = read_key_file(path)?;
    let is_encrypted = encrypted(&text);
    if is_encrypted && passphrase.is_none() {
        return Err(SshError::Key(format!(
            "the private key {name} is encrypted: enter its passphrase."
        )));
    }
    let decoded = keys::decode_secret_key(&text, passphrase.filter(|_| is_encrypted));
    match decoded {
        Ok(key) => Ok(key),
        Err(_) if is_encrypted => Err(SshError::Key(format!(
            "couldn't decrypt the private key {name}: the passphrase is wrong."
        ))),
        Err(e) => Err(SshError::Key(format!(
            "can't use the private key {name} ({e})."
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dir: &Path) -> SshEnv {
        let mut env = SshEnv::isolated(dir);
        env.agent = Some(dir.join("agent.sock"));
        env
    }
    fn temp() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "telecat-ssh-plan-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".ssh")).unwrap();
        d
    }
    fn connection() -> ConnectionConfig {
        let mut c = telecat_core::types::empty_connection(Driver::Mysql);
        c.host = "db.internal".into();
        c.port = 3306;
        c.ssh.enabled = true;
        c.ssh.host = "prod".into();
        c
    }

    #[test]
    fn the_form_wins_over_ssh_config_and_config_over_defaults() {
        let d = temp();
        std::fs::write(
            d.join(".ssh/config"),
            "Host prod\n  HostName bastion.example.com\n  User deploy\n  Port 2200\n  IdentityFile ~/.ssh/prod_key\n  IdentityAgent none\n  UserKnownHostsFile ~/.ssh/team_hosts\n  HostKeyAlias prod-bastion\n",
        )
        .unwrap();
        let env = env(&d);
        let p = Plan::new(&connection(), &env).unwrap();
        assert_eq!(p.host, "bastion.example.com");
        assert_eq!((p.port, p.user.as_str()), (2200, "deploy"));
        assert_eq!(p.key_host, "prod-bastion");
        assert_eq!(p.identity_files, [d.join(".ssh/prod_key")]);
        assert_eq!(p.agent, None);
        assert_eq!(p.user_known_hosts, [d.join(".ssh/team_hosts")]);
        assert_eq!(p.target(), "db.internal:3306");
        assert_eq!(p.explicit_key, None);

        let mut c = connection();
        c.ssh.port = 22;
        c.ssh.user = "me".into();
        c.ssh.use_key = true;
        c.ssh.key_path = Some("~/keys/id".into());
        let p = Plan::new(&c, &env).unwrap();
        assert_eq!((p.port, p.user.as_str()), (22, "me"));
        assert_eq!(p.explicit_key, Some(d.join("keys/id")));
        // A key path stays unused while "Use SSH key" is off.
        c.ssh.use_key = false;
        assert_eq!(Plan::new(&c, &env).unwrap().explicit_key, None);

        c.ssh.host = "other".into();
        let p = Plan::new(&c, &env).unwrap();
        assert_eq!(p.host, "other");
        assert_eq!(p.user, "me");
        assert_eq!(p.agent, Some(d.join("agent.sock")));
        assert!(
            p.identity_files.is_empty(),
            "no default keys exist in the temp home"
        );
        assert_eq!(p.user_known_hosts, [d.join(".ssh/known_hosts")]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn settings_that_cant_work_are_refused() {
        let d = temp();
        std::fs::write(d.join(".ssh/config"), "Host jump-*\n  ProxyJump hop\n").unwrap();
        let env = env(&d);
        let mut c = connection();
        c.ssh.host = "jump-1".into();
        assert!(matches!(Plan::new(&c, &env), Err(SshError::Config(m)) if m.contains("ProxyJump")));
        c.ssh.host = " ".into();
        assert!(matches!(Plan::new(&c, &env), Err(SshError::Config(_))));
        // Names that would act as known_hosts patterns, typed or from ~/.ssh/config.
        for host in ["*,x.example", "x?", "a,b", "!x", "[x]:22", "a b", "x|1"] {
            c.ssh.host = host.into();
            assert!(
                matches!(Plan::new(&c, &env), Err(SshError::Config(m)) if m.contains("SSH server name")),
                "{host}"
            );
        }
        std::fs::write(
            d.join(".ssh/config"),
            "Host evil\n  HostName *,x.example\nHost alias\n  HostKeyAlias *\n",
        )
        .unwrap();
        c.ssh.host = "evil".into();
        assert!(matches!(Plan::new(&c, &env), Err(SshError::Config(m)) if m.contains("HostName")));
        c.ssh.host = "alias".into();
        assert!(
            matches!(Plan::new(&c, &env), Err(SshError::Config(m)) if m.contains("HostKeyAlias"))
        );
        let mut c = connection();
        c.host = "/var/run/mysqld/mysqld.sock".into();
        assert!(matches!(Plan::new(&c, &env), Err(SshError::Config(m)) if m.contains("socket")));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn forwarding_refusals_say_whether_to_retry() {
        let d = temp();
        let plan = Plan::new(&connection(), &env(&d)).unwrap();
        let prohibited =
            russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited);
        assert!(
            matches!(forward_error(&plan, prohibited), SshError::Forward { message, retryable: false } if message.contains("db.internal:3306"))
        );
        let refused = russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed);
        assert!(matches!(
            forward_error(&plan, refused),
            SshError::Forward {
                retryable: true,
                ..
            }
        ));
        assert!(matches!(
            forward_error(&plan, russh::Error::Disconnect),
            SshError::Lost(_)
        ));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn known_key_types_are_asked_for_first() {
        let p = preferred(&["ssh-rsa".into()]);
        assert!(matches!(p.key[0], Algorithm::Rsa { .. }));
        let p = preferred(&["ecdsa-sha2-nistp256".into(), "ssh-ed25519".into()]);
        assert_eq!(p.key[0].as_str(), "ecdsa-sha2-nistp256");
        assert_eq!(p.key[1].as_str(), "ssh-ed25519");
        assert_eq!(preferred(&[]).key, russh::Preferred::default().key);
    }

    #[test]
    fn encrypted_keys_are_recognised_without_the_passphrase() {
        assert!(encrypted("-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n"));
        assert!(encrypted(
            "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00\n"
        ));
        assert!(!encrypted("-----BEGIN RSA PRIVATE KEY-----\nMIIE\n"));
        let d = temp();
        let path = d.join("missing");
        assert!(matches!(load_key(&path, None), Err(SshError::Key(m)) if m.contains("can't read")));
        std::fs::write(&path, "not a key").unwrap();
        assert!(matches!(load_key(&path, None), Err(SshError::Key(_))));
        std::fs::remove_dir_all(d).unwrap();
    }
}
