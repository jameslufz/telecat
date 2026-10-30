//! The parts of OpenSSH's client configuration (~/.ssh/config) that decide where and as whom
//! Telecat connects: `Host` blocks, `HostName`, `User`, `Port`, `IdentityFile`,
//! `IdentitiesOnly`, `IdentityAgent`, `HostKeyAlias`, `UserKnownHostsFile` and `Include`.
//!
//! As in OpenSSH, the first value found for a keyword wins. `Match` blocks can't be evaluated
//! here and are skipped. A host that needs `ProxyJump` or `ProxyCommand` is refused rather than
//! reached directly.
use super::known_hosts::glob;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostSettings {
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<String>,
    pub identities_only: Option<bool>,
    pub identity_agent: Option<String>,
    pub host_key_alias: Option<String>,
    pub user_known_hosts_files: Option<Vec<String>>,
    /// `ProxyJump` or `ProxyCommand` when one applies; `Some("")` when the first one found is
    /// `none`.
    pub proxy: Option<&'static str>,
}

/// Nested `Include`s deeper than this are ignored, which also stops include loops.
const MAX_DEPTH: usize = 8;

/// The settings `path` gives for `alias` (the host as typed in the form). A missing file gives
/// none.
pub fn resolve(path: &Path, home: &Path, alias: &str) -> HostSettings {
    let mut out = HostSettings::default();
    let mut identity_files_seen = false;
    let alias = alias.to_ascii_lowercase();
    read(path, home, &alias, 0, &mut out, &mut identity_files_seen);
    out
}

fn read(
    path: &Path,
    home: &Path,
    alias: &str,
    depth: usize,
    out: &mut HostSettings,
    identity_seen: &mut bool,
) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    // Lines before the first Host apply to every host.
    let mut active = true;
    for line in text.lines() {
        let Some((keyword, args)) = split(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => active = host_matches(&args, alias),
            "match" => active = args.first().is_some_and(|a| a.eq_ignore_ascii_case("all")),
            _ if !active => {}
            "include" if depth < MAX_DEPTH => {
                for pattern in &args {
                    for file in include_files(pattern, home) {
                        read(&file, home, alias, depth + 1, out, identity_seen);
                    }
                }
            }
            _ => apply(&keyword, &args, out, identity_seen),
        }
    }
}

fn apply(keyword: &str, args: &[String], out: &mut HostSettings, identity_seen: &mut bool) {
    let first = args.first().cloned();
    match keyword {
        "hostname" => {
            out.host_name
                .get_or_insert_with(|| first.unwrap_or_default());
        }
        "user" => {
            out.user.get_or_insert_with(|| first.unwrap_or_default());
        }
        "port" => {
            if out.port.is_none() {
                out.port = first.and_then(|p| p.parse().ok()).filter(|p| *p != 0);
            }
        }
        // Every IdentityFile line adds a key to try.
        "identityfile" => {
            *identity_seen = true;
            out.identity_files.extend(
                args.iter()
                    .filter(|f| !f.eq_ignore_ascii_case("none"))
                    .cloned(),
            );
        }
        "identitiesonly" => {
            out.identities_only
                .get_or_insert(first.is_some_and(|v| yes(&v)));
        }
        "identityagent" => {
            out.identity_agent
                .get_or_insert_with(|| first.unwrap_or_default());
        }
        "hostkeyalias" => {
            out.host_key_alias
                .get_or_insert_with(|| first.unwrap_or_default());
        }
        "userknownhostsfile" => {
            out.user_known_hosts_files
                .get_or_insert_with(|| args.to_vec());
        }
        "proxyjump" | "proxycommand" => {
            if out.proxy.is_none()
                && first
                    .as_deref()
                    .is_some_and(|v| !v.eq_ignore_ascii_case("none"))
            {
                out.proxy = Some(if keyword == "proxyjump" {
                    "ProxyJump"
                } else {
                    "ProxyCommand"
                });
            } else if out.proxy.is_none() {
                // `none` is a value too: later lines can't set a proxy for this host.
                out.proxy = Some("");
            }
        }
        _ => {}
    }
}

fn yes(v: &str) -> bool {
    v.eq_ignore_ascii_case("yes") || v.eq_ignore_ascii_case("true")
}

/// `Keyword value…` or `Keyword=value`, with double-quoted arguments; `None` for blank lines and
/// comments.
fn split(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = line[..end].to_ascii_lowercase();
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    let mut args = vec![];
    let mut current = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in rest.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    args.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            c => {
                current.push(c);
                any = true;
            }
        }
    }
    if any {
        args.push(current);
    }
    Some((keyword, args))
}

/// OpenSSH's `match_pattern_list` for a `Host` line: any positive pattern matches and no
/// negated one does.
fn host_matches(patterns: &[String], alias: &str) -> bool {
    let mut matched = false;
    for p in patterns {
        let p = p.to_ascii_lowercase();
        let (negated, p) = match p.strip_prefix('!') {
            Some(rest) => (true, rest.to_owned()),
            None => (false, p),
        };
        if glob(&p, alias) {
            if negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

fn include_files(pattern: &str, home: &Path) -> Vec<PathBuf> {
    let expanded = expand_tilde(pattern, home);
    let path = if Path::new(&expanded).is_absolute() {
        PathBuf::from(expanded)
    } else {
        home.join(".ssh").join(expanded)
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| glob(&name, &n.to_string_lossy()))
        })
        .collect();
    files.sort();
    files
}

pub fn expand_tilde(path: &str, home: &Path) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.display())
        }
        _ => path.to_owned(),
    }
}

/// Expands `~` and the `%` tokens OpenSSH allows in `IdentityFile` and similar paths.
pub fn expand_tokens(
    path: &str,
    home: &Path,
    local_user: &str,
    host: &str,
    user: &str,
    port: u16,
    alias: &str,
) -> String {
    let path = expand_tilde(path, home);
    let mut out = String::new();
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('d') => out.push_str(&home.display().to_string()),
            Some('u') => out.push_str(local_user),
            Some('h') => out.push_str(host),
            Some('r') => out.push_str(user),
            Some('p') => out.push_str(&port.to_string()),
            Some('n') => out.push_str(alias),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "telecat-ssh-config-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".ssh/conf.d")).unwrap();
        d
    }

    #[test]
    fn first_value_wins_and_blocks_match_by_pattern() {
        let h = home();
        let config = h.join(".ssh/config");
        std::fs::write(
            &config,
            r#"
# global
IdentitiesOnly yes

Host prod-db bastion-*
    HostName 10.1.2.3
    User deploy
    Port=2200
    IdentityFile ~/.ssh/id_prod
    IdentityFile "~/.ssh/with space"

Host *.internal !secret.internal
    ProxyJump jump.example.com

Match host whatever
    User nobody

Host *
    User fallback
    Port 22
    IdentityFile ~/.ssh/id_default
    HostKeyAlias shared-alias
    IdentityAgent ~/agent.sock
    UserKnownHostsFile ~/.ssh/kh1 ~/.ssh/kh2
"#,
        )
        .unwrap();
        let s = resolve(&config, &h, "PROD-DB");
        assert_eq!(s.host_name.as_deref(), Some("10.1.2.3"));
        assert_eq!(s.user.as_deref(), Some("deploy"));
        assert_eq!(s.port, Some(2200));
        assert_eq!(
            s.identity_files,
            ["~/.ssh/id_prod", "~/.ssh/with space", "~/.ssh/id_default"]
        );
        assert_eq!(s.identities_only, Some(true));
        assert_eq!(s.host_key_alias.as_deref(), Some("shared-alias"));
        assert_eq!(s.identity_agent.as_deref(), Some("~/agent.sock"));
        assert_eq!(
            s.user_known_hosts_files.as_deref(),
            Some(&["~/.ssh/kh1".to_owned(), "~/.ssh/kh2".to_owned()][..])
        );
        assert_eq!(s.proxy, None);

        let s = resolve(&config, &h, "db.internal");
        assert_eq!(s.proxy, Some("ProxyJump"));
        assert_eq!(
            s.user.as_deref(),
            Some("fallback"),
            "Match blocks are skipped"
        );
        assert_eq!(resolve(&config, &h, "secret.internal").proxy, None);
        assert_eq!(resolve(&config, &h, "bastion-2").port, Some(2200));

        assert_eq!(
            resolve(&h.join("missing"), &h, "x"),
            HostSettings::default()
        );
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn includes_are_relative_to_dot_ssh_and_globbed() {
        let h = home();
        let config = h.join(".ssh/config");
        std::fs::write(
            &config,
            "Include conf.d/*.conf\nHost loop\n  Include config\n",
        )
        .unwrap();
        std::fs::write(h.join(".ssh/conf.d/b.conf"), "Host app\n  User second\n").unwrap();
        std::fs::write(
            h.join(".ssh/conf.d/a.conf"),
            "Host app\n  User first\n  HostName app.example.com\n",
        )
        .unwrap();
        std::fs::write(h.join(".ssh/conf.d/c.txt"), "Host app\n  Port 9\n").unwrap();
        let s = resolve(&config, &h, "app");
        assert_eq!(s.user.as_deref(), Some("first"));
        assert_eq!(s.host_name.as_deref(), Some("app.example.com"));
        assert_eq!(s.port, None);
        // A file including itself stops at the depth limit.
        assert_eq!(resolve(&config, &h, "loop"), HostSettings::default());
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn proxy_none_blocks_later_proxies() {
        let h = home();
        let config = h.join(".ssh/config");
        std::fs::write(
            &config,
            "Host direct\n  ProxyJump none\nHost *\n  ProxyCommand nc %h %p\n",
        )
        .unwrap();
        assert_eq!(resolve(&config, &h, "direct").proxy, Some(""));
        assert_eq!(resolve(&config, &h, "other").proxy, Some("ProxyCommand"));
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn tokens() {
        let home = Path::new("/Users/me");
        assert_eq!(
            expand_tokens(
                "~/.ssh/%r@%h:%p-%n-%u-%d-%%-%x",
                home,
                "me",
                "10.0.0.1",
                "deploy",
                22,
                "prod"
            ),
            "/Users/me/.ssh/deploy@10.0.0.1:22-prod-me-/Users/me-%-%x"
        );
        assert_eq!(expand_tilde("~user/x", home), "~user/x");
        assert_eq!(expand_tilde("~", home), "/Users/me");
    }
}
