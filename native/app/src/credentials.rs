use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, sync::Mutex, time::Duration};
use telecat_core::types::{ConnectionConfig, PasswordMode};
// Keep the original service name so the Telecat rename preserves saved passwords.
pub const NATIVE_SERVICE: &str = "Telecat Native";
pub const ELECTRON_SERVICE: &str = "Telecat Safe Storage";
pub const ELECTRON_ACCOUNT: &str = "Telecat Key";
pub trait Keychain: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, service: &str, account: &str, value: &[u8]) -> Result<()>;
    fn delete(&self, service: &str, account: &str) -> Result<()>;
}
/// Scenarios and every test inject this implementation, never the real Keychain.
#[derive(Default)]
pub struct MemoryKeychain(Mutex<BTreeMap<(String, String), Vec<u8>>>);
impl Keychain for MemoryKeychain {
    fn get(&self, s: &str, a: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(&(s.into(), a.into())).cloned())
    }
    fn set(&self, s: &str, a: &str, v: &[u8]) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert((s.into(), a.into()), v.to_vec());
        Ok(())
    }
    fn delete(&self, s: &str, a: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(s.into(), a.into()));
        Ok(())
    }
}
/// Only instantiated by an ordinary, non-scenario production launch.
pub struct MacKeychain;
fn allowed() -> Result<()> {
    if std::env::var_os("TELECAT_NO_KEYCHAIN").is_some()
        || std::env::var_os("TELECAT_SCENARIO").is_some()
    {
        bail!("Keychain is disabled in this launch");
    }
    Ok(())
}
impl Keychain for MacKeychain {
    fn get(&self, s: &str, a: &str) -> Result<Option<Vec<u8>>> {
        allowed()?;
        match security_framework::passwords::get_generic_password(s, a) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.code() == -25300 => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn set(&self, s: &str, a: &str, v: &[u8]) -> Result<()> {
        allowed()?;
        security_framework::passwords::set_generic_password(s, a, v)?;
        Ok(())
    }
    fn delete(&self, s: &str, a: &str) -> Result<()> {
        allowed()?;
        match security_framework::passwords::delete_generic_password(s, a) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
pub async fn run_password_command(command: &str) -> Result<String> {
    run_password_command_options(command, Duration::from_secs(15), None).await
}
/// The tests supply an empty, private ZDOTDIR, so they never execute real shell startup files.
pub async fn run_password_command_options(
    command: &str,
    timeout: Duration,
    zdotdir: Option<&std::path::Path>,
) -> Result<String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    if telecat_core::sql::trim(command).is_empty() {
        bail!("Password command is empty");
    }
    let mut process = tokio::process::Command::new("/bin/zsh");
    process
        .args(["-lc", command])
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(dir) = zdotdir {
        process.env("ZDOTDIR", dir);
    }
    let mut child = process.spawn().context("Password command failed")?;
    let pid = child.id();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let work = async {
        let read = |stream: tokio::process::ChildStdout| async move {
            let mut out = vec![];
            stream.take(1_048_577).read_to_end(&mut out).await?;
            if out.len() > 1_048_576 {
                bail!("Password command failed: output exceeds 1 MiB");
            }
            Ok::<_, anyhow::Error>(out)
        };
        let err = async move {
            let mut out = vec![];
            stderr.take(1_048_577).read_to_end(&mut out).await?;
            if out.len() > 1_048_576 {
                bail!("Password command failed: output exceeds 1 MiB");
            }
            Ok::<_, anyhow::Error>(out)
        };
        let (stdout, stderr) = tokio::try_join!(read(stdout), err)?;
        if stdout.len() > 1_048_576 || stderr.len() > 1_048_576 {
            bail!("Password command failed: output exceeds 1 MiB");
        }
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status, stdout, stderr))
    };
    let result = tokio::time::timeout(timeout, work).await;
    let output = match result {
        Ok(Ok(out)) => out,
        other => {
            if let Some(pid) = pid {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.wait().await;
            return match other {
                Err(_) => Err(anyhow::anyhow!("Password command failed: timed out")),
                Ok(Err(e)) => Err(e),
                _ => unreachable!(),
            };
        }
    };
    if !output.0.success() {
        let stderr = String::from_utf8_lossy(&output.2);
        bail!(
            "Password command failed: {}",
            if stderr.trim().is_empty() {
                output.0.to_string()
            } else {
                stderr.trim().into()
            }
        );
    }
    let mut out = String::from_utf8_lossy(&output.1).into_owned();
    if out.ends_with('\n') {
        out.pop();
        if out.ends_with('\r') {
            out.pop();
        }
    }
    Ok(out)
}
pub async fn resolve_password(
    c: &ConnectionConfig,
    typed: Option<&str>,
    keychain: &dyn Keychain,
) -> Result<String> {
    Ok(match c.password_mode {
        PasswordMode::None => String::new(),
        PasswordMode::Ask => typed.unwrap_or("").into(),
        PasswordMode::Command => {
            run_password_command(c.password_command.as_deref().unwrap_or("")).await?
        }
        PasswordMode::Keychain => match typed {
            Some(v) => v.into(),
            None => keychain
                .get(NATIVE_SERVICE, &c.id)?
                .map(String::from_utf8)
                .transpose()?
                .unwrap_or_default(),
        },
    })
}
