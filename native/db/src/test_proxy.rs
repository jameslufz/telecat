//! A TCP proxy in front of the local test servers, for tests that need the network to fail: it
//! can cut every link (and refuse new ones), blackhole them (accept and hold, forward nothing,
//! like a VPN that went away) and restore them. It only ever forwards to 127.0.0.1:3307 or
//! 127.0.0.1:5434, and the endpoint fence admits only live proxies in front of those.
use anyhow::{Result, bail};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Up,
    /// Bytes are held, neither forwarded nor answered; new links hang in the handshake.
    Blackhole,
    /// Every link closed; new ones are closed as soon as they are accepted.
    Down,
}

/// Live proxies: listening port → the test server it forwards to.
static PROXIES: Mutex<BTreeMap<u16, u16>> = Mutex::new(BTreeMap::new());

/// Whether 127.0.0.1:`port` is a live proxy in front of 127.0.0.1:`upstream`.
pub(crate) fn forwards(port: u16, upstream: u16) -> bool {
    PROXIES.lock().unwrap_or_else(|e| e.into_inner()).get(&port) == Some(&upstream)
}

/// Goes dark when a client sends `pattern` (say the COMMIT of a save), either just before
/// those bytes reach the server or just after.
#[derive(Debug, Clone)]
struct Trigger {
    pattern: Vec<u8>,
    /// `None`: hold the matching bytes. `Some(n)`: deliver them and the client's next `n`
    /// writes (PostgreSQL sends a statement's Parse and its Execute separately), then go dark.
    deliver: Option<usize>,
}
pub struct TestProxy {
    port: u16,
    link: Arc<watch::Sender<Link>>,
    trigger: Arc<Mutex<Option<Trigger>>>,
    tripped: Arc<std::sync::atomic::AtomicBool>,
    links: Arc<Mutex<JoinSet<()>>>,
    accept: tokio::task::JoinHandle<()>,
}
impl TestProxy {
    /// Starts a proxy on a free loopback port in front of 127.0.0.1:`upstream`.
    pub async fn start(upstream: u16) -> Result<Self> {
        if !matches!(upstream, 3307 | 5434) {
            bail!("Test safety: the proxy only forwards to 127.0.0.1:3307 or 127.0.0.1:5434");
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let (link, watch) = watch::channel(Link::Up);
        let link = Arc::new(link);
        let trigger: Arc<Mutex<Option<Trigger>>> = Arc::default();
        let tripped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let links = Arc::new(Mutex::new(JoinSet::new()));
        let accept = tokio::spawn({
            let links = links.clone();
            let arm = Arm {
                link: link.clone(),
                trigger: trigger.clone(),
                tripped: tripped.clone(),
            };
            async move {
                while let Ok((client, _)) = listener.accept().await {
                    if *watch.borrow() == Link::Down {
                        drop(client);
                        continue;
                    }
                    let watch = watch.clone();
                    links
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .spawn(forward(client, upstream, watch, arm.clone()));
                }
            }
        });
        PROXIES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(port, upstream);
        Ok(Self {
            port,
            link,
            trigger,
            tripped,
            links,
            accept,
        })
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    /// Closes every link and refuses new ones until [`Self::restore`].
    pub fn cut(&self) {
        self.link.send_replace(Link::Down);
        self.links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .abort_all();
    }
    /// Holds every byte in both directions, keeping the sockets open.
    pub fn blackhole(&self) {
        self.link.send_replace(Link::Blackhole);
    }
    /// Forwards again, held bytes included.
    pub fn restore(&self) {
        self.link.send_replace(Link::Up);
    }
    /// Blackholes every link once a client sends `pattern`: with `deliver: Some(n)`, after
    /// those bytes and the client's next `n` writes reach the server (it acts, its answer
    /// never arrives); with `None`, right before.
    pub fn blackhole_on(&self, pattern: &[u8], deliver: Option<usize>) {
        self.tripped
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *self.trigger.lock().unwrap_or_else(|e| e.into_inner()) = Some(Trigger {
            pattern: pattern.to_vec(),
            deliver,
        });
    }
    /// Whether the [`Self::blackhole_on`] pattern was seen.
    pub fn tripped(&self) -> bool {
        self.tripped.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl Drop for TestProxy {
    fn drop(&mut self) {
        PROXIES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.port);
        self.accept.abort();
        self.links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .abort_all();
    }
}
#[derive(Clone)]
struct Arm {
    link: Arc<watch::Sender<Link>>,
    trigger: Arc<Mutex<Option<Trigger>>>,
    tripped: Arc<std::sync::atomic::AtomicBool>,
}
impl Arm {
    /// Whether `chunk` trips the trigger, and if so how many writes to deliver first.
    fn check(&self, chunk: &[u8]) -> Option<Option<usize>> {
        let mut trigger = self.trigger.lock().unwrap_or_else(|e| e.into_inner());
        let deliver = trigger
            .as_ref()
            .filter(|t| chunk.windows(t.pattern.len()).any(|w| w == t.pattern))?
            .deliver;
        trigger.take();
        self.tripped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Some(deliver)
    }
}
async fn forward(client: TcpStream, upstream: u16, mut watch: watch::Receiver<Link>, arm: Arm) {
    if watch.wait_for(|l| *l == Link::Up).await.is_err() {
        return;
    }
    let Ok(server) = TcpStream::connect(("127.0.0.1", upstream)).await else {
        return;
    };
    let (client_read, client_write) = client.into_split();
    let (server_read, server_write) = server.into_split();
    tokio::select! {
        _ = pump(client_read, server_write, watch.clone(), Some(arm)) => {}
        _ = pump(server_read, client_write, watch, None) => {}
    }
}
async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    mut watch: watch::Receiver<Link>,
    arm: Option<Arm>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    // Writes still to deliver before going dark, once tripped.
    let mut countdown: Option<usize> = None;
    loop {
        if watch.wait_for(|l| *l == Link::Up).await.is_err() {
            return;
        }
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if let Some(a) = &arm {
            match countdown {
                Some(left) => countdown = Some(left.saturating_sub(1)),
                None => match a.check(&buf[..n]) {
                    Some(Some(more)) => countdown = Some(more),
                    Some(None) => {
                        a.link.send_replace(Link::Blackhole);
                    }
                    None => {}
                },
            }
            if countdown == Some(0) {
                countdown = None;
                if to.write_all(&buf[..n]).await.is_err() {
                    return;
                }
                a.link.send_replace(Link::Blackhole);
                continue;
            }
        }
        // Bytes read just as the link went dark are held until it comes back.
        if watch.wait_for(|l| *l == Link::Up).await.is_err()
            || to.write_all(&buf[..n]).await.is_err()
        {
            return;
        }
    }
}
