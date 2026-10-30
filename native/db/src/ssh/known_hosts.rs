//! Host key verification against OpenSSH known_hosts files.
//!
//! The user's own files (~/.ssh/known_hosts and the like) are only ever read. Keys the user
//! trusts in Telecat go to Telecat's own file, which is checked first and wins: once it has an
//! entry for a host, a different key recorded for that host in the user's files is ignored, so
//! "Trust New Key" after a warning is final. A key marked `@revoked` in any file is refused.
//!
//! Telecat's file only ever holds exact names: entries are written hashed (`|1|salt|hmac`), and
//! when reading it, wildcard and negated patterns are ignored. An entry there can therefore
//! never stand for another host, even one written by an earlier version from a host name that
//! looked like a pattern (`*,attacker.example`).
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use std::path::{Path, PathBuf};

/// A host key: its algorithm name (`ssh-ed25519`, `rsa-sha2-512` keys are `ssh-rsa`) and its
/// SSH wire encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub algorithm: String,
    pub blob: Vec<u8>,
}
impl HostKey {
    /// `SHA256:…`, as `ssh-keygen -l` prints it.
    pub fn fingerprint(&self) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA256, &self.blob);
        format!("SHA256:{}", STANDARD_NO_PAD.encode(digest.as_ref()))
    }
    /// `algorithm base64`, the key part of a known_hosts line.
    pub fn openssh(&self) -> String {
        format!("{} {}", self.algorithm, STANDARD.encode(&self.blob))
    }
    /// A key from its wire encoding, which starts with its algorithm name.
    pub fn from_blob(blob: Vec<u8>) -> Option<Self> {
        let len = u32::from_be_bytes(blob.get(..4)?.try_into().ok()?) as usize;
        let algorithm = std::str::from_utf8(blob.get(4..4usize.checked_add(len)?)?).ok()?;
        Some(Self {
            algorithm: algorithm.to_owned(),
            blob,
        })
    }
    fn parse(algorithm: &str, base64: &str) -> Option<Self> {
        // A line whose algorithm and key blob disagree is malformed.
        Self::from_blob(STANDARD.decode(base64).ok()?).filter(|k| k.algorithm == algorithm)
    }
}

/// Where a key for the host was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub path: PathBuf,
    pub line: usize,
    pub algorithm: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Trusted,
    /// No key on record for the host.
    Unknown,
    /// Keys are on record for the host, none of them this one.
    Changed(Vec<Recorded>),
    /// This key is marked `@revoked`.
    Revoked(Recorded),
}

#[derive(Debug)]
struct Entry {
    marker: Option<String>,
    hosts: String,
    key: HostKey,
    line: usize,
}

fn entries(path: &Path) -> Vec<Entry> {
    // A missing or unreadable file has no entries; a malformed line is skipped, as OpenSSH does.
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let mut out = vec![];
    for (i, line) in text.lines().enumerate() {
        let line_text = line.trim();
        if line_text.is_empty() || line_text.starts_with('#') {
            continue;
        }
        let mut fields = line_text.split_whitespace();
        let mut first = fields.next().unwrap_or_default();
        let marker = first.starts_with('@').then(|| {
            let m = first.to_ascii_lowercase();
            first = "";
            m
        });
        let hosts = if marker.is_some() {
            fields.next().unwrap_or_default()
        } else {
            first
        };
        let (Some(algorithm), Some(base64)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let Some(key) = HostKey::parse(algorithm, base64) {
            out.push(Entry {
                marker,
                hosts: hosts.into(),
                key,
                line: i + 1,
            });
        }
    }
    out
}

/// The name known_hosts files record for `host` on `port`.
pub fn host_pattern_name(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

/// OpenSSH's `match_hostname`: a comma-separated list of patterns (`*`, `?`, `!negation`) or
/// hashed names (`|1|salt|hash`). A negated match wins over any other.
fn hosts_match(patterns: &str, name: &str) -> bool {
    let mut matched = false;
    for pattern in patterns.split(',') {
        if let Some(hashed) = pattern.strip_prefix("|1|") {
            matched |= hashed_match(hashed, name);
            continue;
        }
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, pattern),
        };
        if glob(&pattern.to_ascii_lowercase(), name) {
            if negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Telecat's own file: only an exact name or a hashed one matches; elements that are patterns
/// (`*`, `?`, `!`) never do.
fn exact_match(hosts: &str, name: &str) -> bool {
    hosts.split(',').any(|h| match h.strip_prefix("|1|") {
        Some(hashed) => hashed_match(hashed, name),
        None => !h.contains(['*', '?', '!']) && h.eq_ignore_ascii_case(name),
    })
}
fn hashed_match(hashed: &str, name: &str) -> bool {
    let Some((salt, hash)) = hashed.split_once('|') else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (STANDARD.decode(salt), STANDARD.decode(hash)) else {
        return false;
    };
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &salt);
    ring::hmac::verify(&key, name.as_bytes(), &hash).is_ok()
}

/// `*` matches any run of characters, `?` any one.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Checks `key` for `host`:`port` against Telecat's file, then the user's files.
pub fn verify(
    telecat: &Path,
    user_files: &[PathBuf],
    host: &str,
    port: u16,
    key: &HostKey,
) -> Verdict {
    let name = host_pattern_name(host, port);
    let files: Vec<(&Path, Vec<Entry>)> = std::iter::once(telecat)
        .chain(user_files.iter().map(PathBuf::as_path))
        .map(|p| (p, entries(p)))
        .collect();
    for (path, list) in &files {
        if let Some(e) = list
            .iter()
            .find(|e| e.marker.as_deref() == Some("@revoked") && e.key == *key)
        {
            return Verdict::Revoked(recorded(path, e));
        }
    }
    let mut on_record = vec![];
    for (i, (path, list)) in files.iter().enumerate() {
        let for_host: Vec<&Entry> = list
            .iter()
            .filter(|e| {
                e.marker.is_none()
                    && if i == 0 {
                        exact_match(&e.hosts, &name)
                    } else {
                        hosts_match(&e.hosts, &name)
                    }
            })
            .collect();
        if for_host.iter().any(|e| e.key == *key) {
            return Verdict::Trusted;
        }
        on_record.extend(for_host.iter().map(|e| recorded(path, e)));
        if i == 0 && !on_record.is_empty() {
            // Telecat's file decides for the hosts it has entries for.
            break;
        }
    }
    if on_record.is_empty() {
        Verdict::Unknown
    } else {
        Verdict::Changed(on_record)
    }
}

/// Algorithms of the keys on record for the host, to ask the server for one of those first.
pub fn known_algorithms(
    telecat: &Path,
    user_files: &[PathBuf],
    host: &str,
    port: u16,
) -> Vec<String> {
    let name = host_pattern_name(host, port);
    let mut out: Vec<String> = vec![];
    for (i, path) in std::iter::once(telecat)
        .chain(user_files.iter().map(PathBuf::as_path))
        .enumerate()
    {
        for e in entries(path) {
            let matches = if i == 0 {
                exact_match(&e.hosts, &name)
            } else {
                hosts_match(&e.hosts, &name)
            };
            if e.marker.is_none() && matches && !out.contains(&e.key.algorithm) {
                out.push(e.key.algorithm);
            }
        }
    }
    out
}

fn recorded(path: &Path, e: &Entry) -> Recorded {
    Recorded {
        path: path.to_owned(),
        line: e.line,
        algorithm: e.key.algorithm.clone(),
        fingerprint: e.key.fingerprint(),
    }
}

/// Records `key` as the key of `host`:`port` in Telecat's file, hashed, replacing the earlier
/// entries for exactly that name. Lines that are patterns (which Telecat never writes, and no
/// longer honours) are dropped. A host name that could act as a pattern is refused. The file
/// is owner-only and replaced atomically.
pub fn trust(telecat: &Path, host: &str, port: u16, key: &HostKey) -> std::io::Result<()> {
    if let Some(why) = telecat_core::types::ssh_host_problem(host) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the host name {why}"),
        ));
    }
    let name = host_pattern_name(host, port);
    let existing = std::fs::read_to_string(telecat).unwrap_or_default();
    let mut kept: Vec<&str> = vec![];
    for line in existing.lines() {
        let mut fields = line.split_whitespace();
        let first = fields.next().unwrap_or_default();
        if first.is_empty() || first.starts_with('#') || first.starts_with('@') {
            kept.push(line);
            continue;
        }
        let pattern = first
            .split(',')
            .any(|h| !h.starts_with("|1|") && h.contains(['*', '?', '!']));
        if pattern || exact_match(first, &name) {
            continue;
        }
        kept.push(line);
    }
    let mut text = kept.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("{} {}\n", hashed_name(&name)?, key.openssh()));
    write_private(telecat, text.as_bytes())
}

/// `|1|salt|HMAC-SHA1(salt, name)`, as `ssh-keygen -H` writes: it matches only that name.
fn hashed_name(name: &str) -> std::io::Result<String> {
    use ring::rand::SecureRandom;
    let mut salt = [0u8; 20];
    ring::rand::SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| std::io::Error::other("no random bytes for the host entry"))?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &salt);
    let tag = ring::hmac::sign(&key, name.as_bytes());
    Ok(format!(
        "|1|{}|{}",
        STANDARD.encode(salt),
        STANDARD.encode(tag.as_ref())
    ))
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("known_hosts path has no directory"))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    let tmp = parent.join(format!(
        ".known_hosts.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(algorithm: &str, seed: u8) -> HostKey {
        let mut blob = (algorithm.len() as u32).to_be_bytes().to_vec();
        blob.extend_from_slice(algorithm.as_bytes());
        blob.extend_from_slice(&[0, 0, 0, 32]);
        blob.extend(std::iter::repeat_n(seed, 32));
        HostKey {
            algorithm: algorithm.into(),
            blob,
        }
    }
    fn hashed(name: &str, salt: &[u8]) -> String {
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, salt);
        let tag = ring::hmac::sign(&k, name.as_bytes());
        format!(
            "|1|{}|{}",
            STANDARD.encode(salt),
            STANDARD.encode(tag.as_ref())
        )
    }
    fn dir() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "telecat-known-hosts-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn patterns_ports_hashes_and_negation() {
        assert!(hosts_match("db.example.com,10.0.0.5", "db.example.com"));
        assert!(hosts_match("*.example.com", "db.example.com"));
        assert!(hosts_match("db?.example.com", "db1.example.com"));
        assert!(!hosts_match(
            "*.example.com,!db.example.com",
            "db.example.com"
        ));
        assert!(!hosts_match("*.example.com", "[db.example.com]:2222"));
        assert!(hosts_match(
            "[db.example.com]:2222",
            "[db.example.com]:2222"
        ));
        assert!(hosts_match("[*.example.com]:2222", "[db.example.com]:2222"));
        assert!(hosts_match("DB.Example.com", "db.example.com"));
        assert!(hosts_match(
            &hashed("[h]:2222", b"0123456789abcdef0123"),
            "[h]:2222"
        ));
        assert!(!hosts_match(
            &hashed("[h]:2222", b"0123456789abcdef0123"),
            "[h]:2223"
        ));
        assert!(!hosts_match("|1|garbage", "h"));
        assert_eq!(host_pattern_name("Bastion", 22), "bastion");
        assert_eq!(host_pattern_name("::1", 2222), "[::1]:2222");
    }

    #[test]
    fn verdicts() {
        let d = dir();
        let (ours, user) = (d.join("telecat_known_hosts"), d.join("known_hosts"));
        let (a, b) = (key("ssh-ed25519", 1), key("ssh-ed25519", 2));
        let files = [user.clone(), d.join("missing")];
        assert_eq!(verify(&ours, &files, "h", 2222, &a), Verdict::Unknown);
        std::fs::write(
            &user,
            format!(
                "# comment\n\nnot a valid line\n[other]:2222 {}\n{} {}\n",
                b.openssh(),
                hashed("[h]:2222", b"salt-salt-salt-salt!"),
                a.openssh()
            ),
        )
        .unwrap();
        assert_eq!(verify(&ours, &files, "h", 2222, &a), Verdict::Trusted);
        assert_eq!(
            verify(&ours, &files, "other", 2222, &a),
            Verdict::Changed(vec![Recorded {
                path: user.clone(),
                line: 4,
                algorithm: "ssh-ed25519".into(),
                fingerprint: b.fingerprint(),
            }])
        );
        assert_eq!(
            verify(&ours, &files, "h", 22, &a),
            Verdict::Unknown,
            "port 22 is another entry"
        );
        assert_eq!(known_algorithms(&ours, &files, "h", 2222), ["ssh-ed25519"]);

        // Trusting the new key in Telecat's file overrides the user's file, and replaces only
        // that host's entry there.
        trust(&ours, "other", 2222, &key("ssh-ed25519", 9)).unwrap();
        trust(&ours, "other", 2222, &a).unwrap();
        trust(&ours, "third", 22, &b).unwrap();
        let text = std::fs::read_to_string(&ours).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert_eq!(verify(&ours, &files, "other", 2222, &a), Verdict::Trusted);
        assert!(
            matches!(verify(&ours, &files, "other", 2222, &b), Verdict::Changed(r) if r.len() == 1 && r[0].path == ours)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&ours).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // A revoked key is refused even where it is trusted.
        std::fs::write(
            &user,
            format!("@revoked * {}\n[h]:2222 {}\n", a.openssh(), a.openssh()),
        )
        .unwrap();
        assert!(matches!(verify(&ours, &files, "h", 2222, &a), Verdict::Revoked(r) if r.line == 1));
        // @cert-authority lines aren't host keys.
        std::fs::write(&user, format!("@cert-authority [h]:2222 {}\n", a.openssh())).unwrap();
        assert_eq!(verify(&ours, &files, "h", 2222, &a), Verdict::Unknown);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// SEC-001: a host name that looks like a pattern must never trust its key for others.
    #[test]
    fn a_pattern_shaped_host_name_never_trusts_a_key_for_other_hosts() {
        let d = dir();
        let ours = d.join("telecat_known_hosts");
        let k = key("ssh-ed25519", 3);
        for port in [22, 2222] {
            let e = trust(&ours, "*,x.example", port, &k).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        }
        for host in ["x?", "!x", "[x]", "a b", "x|1", "", "x\n* ssh-ed25519 AAAA"] {
            assert!(trust(&ours, host, 22, &k).is_err(), "{host:?}");
        }
        for port in [22, 2200] {
            assert_ne!(
                verify(&ours, &[], "other.example", port, &k),
                Verdict::Trusted
            );
        }

        // Lines an earlier version may have written from such names stay harmless: patterns
        // in Telecat's own file are ignored, and the next trust drops them.
        let pinned = key("ssh-ed25519", 4);
        std::fs::write(
            &ours,
            format!(
                "*,x.example {k}\n[*,x.example]:2222 {k}\n* {k}\n!nothing,* {k}\nprod.corp {p}\n",
                k = k.openssh(),
                p = pinned.openssh()
            ),
        )
        .unwrap();
        for (host, port) in [
            ("other.example", 22),
            ("other.example", 2200),
            ("db.corp", 2222),
        ] {
            assert_eq!(
                verify(&ours, &[], host, port, &k),
                Verdict::Unknown,
                "{host}:{port}"
            );
        }
        assert!(matches!(
            verify(&ours, &[], "prod.corp", 22, &k),
            Verdict::Changed(_)
        ));
        assert_eq!(
            verify(&ours, &[], "prod.corp", 22, &pinned),
            Verdict::Trusted
        );
        assert!(known_algorithms(&ours, &[], "other.example", 22).is_empty());
        trust(&ours, "third.example", 22, &k).unwrap();
        let text = std::fs::read_to_string(&ours).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains("prod.corp "));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn trusted_entries_are_hashed_and_replaced_by_exact_name_only() {
        let d = dir();
        let ours = d.join("telecat_known_hosts");
        let (a, b) = (key("ssh-ed25519", 5), key("ssh-ed25519", 6));
        trust(&ours, "db.example", 2222, &a).unwrap();
        trust(&ours, "db.example.org", 2222, &a).unwrap();
        let text = std::fs::read_to_string(&ours).unwrap();
        assert!(text.lines().all(|l| l.starts_with("|1|")), "{text}");
        assert!(!text.contains("db.example"), "{text}");
        assert_eq!(verify(&ours, &[], "DB.example", 2222, &a), Verdict::Trusted);
        assert_eq!(verify(&ours, &[], "db.example", 22, &a), Verdict::Unknown);
        assert_eq!(
            verify(&ours, &[], "db.examplex", 2222, &a),
            Verdict::Unknown
        );
        // Replacing one host's key leaves the other host's entry alone.
        trust(&ours, "db.example", 2222, &b).unwrap();
        assert_eq!(std::fs::read_to_string(&ours).unwrap().lines().count(), 2);
        assert_eq!(verify(&ours, &[], "db.example", 2222, &b), Verdict::Trusted);
        assert!(matches!(
            verify(&ours, &[], "db.example", 2222, &a),
            Verdict::Changed(_)
        ));
        assert_eq!(
            verify(&ours, &[], "db.example.org", 2222, &a),
            Verdict::Trusted
        );
        // Entries from 0.7.0 (plain exact names) still count.
        std::fs::write(&ours, format!("[legacy.example]:2222 {}\n", a.openssh())).unwrap();
        assert_eq!(
            verify(&ours, &[], "legacy.example", 2222, &a),
            Verdict::Trusted
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn fingerprints_match_ssh_keygen() {
        // ssh-keygen -lf for this public key prints this fingerprint.
        let line =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGe1jPeDE1Ru7ZyD/cBffYTsNInjZkQ4AzOampxcenhS";
        let mut f = line.split(' ');
        let k = HostKey::parse(f.next().unwrap(), f.next().unwrap()).unwrap();
        assert_eq!(
            k.fingerprint(),
            "SHA256:ewzFoOcRn3u9/iVhovoAk8nncT1rp/7Rex751id4bd4"
        );
        assert_eq!(k.openssh(), line);
        assert!(
            HostKey::parse(
                "ssh-rsa",
                "AAAAC3NzaC1lZDI1NTE5AAAAIGe1jPeDE1Ru7ZyD/cBffYTsNInjZkQ4AzOampxcenhS"
            )
            .is_none()
        );
    }

    #[test]
    fn globs() {
        assert!(glob("*", ""));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b", "aXXc"));
        assert!(glob("??", "ab"));
        assert!(!glob("??", "abc"));
    }
}
