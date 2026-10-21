//! Staged edits live only in memory until ⌘S. If the UI thread panics, the process aborts (a
//! panic can't unwind through AppKit), so the panic hook writes the staged SQL of every open
//! workspace to the profile and the next launch says where it is. Nothing is lost silently.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

static STAGED: Mutex<BTreeMap<u64, (String, String)>> = Mutex::new(BTreeMap::new());
const NOTICE: &str = "recovered-changes.notice";

/// Record the staged changes of workspace `id` (`None` when it has none or closed).
pub fn remember(id: u64, connection: &str, staged_sql: Option<String>) {
    let mut staged = STAGED.lock().unwrap_or_else(|e| e.into_inner());
    match staged_sql.filter(|s| !s.trim().is_empty()) {
        Some(sql) => {
            staged.insert(id, (connection.to_owned(), sql));
        }
        None => {
            staged.remove(&id);
        }
    }
}

/// Write the remembered staged changes, if any, into `root`. Returns the file written.
pub fn write(root: &Path) -> Option<PathBuf> {
    let staged = match STAGED.try_lock() {
        Ok(s) => s,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    if staged.is_empty() {
        return None;
    }
    let mut text = String::from(
        "-- Telecat closed unexpectedly. These changes were staged but NOT saved to the database.\n-- Review them and run what you still need.\n",
    );
    for (connection, sql) in staged.values() {
        text += &format!("\n-- Connection: {connection}\n{sql}\n");
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = root.join(format!("recovered-changes-{stamp}.sql"));
    crate::store::atomic_write(&path, text.as_bytes()).ok()?;
    let _ = crate::store::atomic_write(&root.join(NOTICE), path.to_string_lossy().as_bytes());
    Some(path)
}

/// Writes the statements of a save the connection dropped during into `root`, so they stay
/// at hand whatever the check finds. Returns the file written.
pub fn write_interrupted(root: &Path, connection: &str, sql: &str) -> Option<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = root.join(format!("interrupted-save-{stamp}.sql"));
    let text = format!(
        "-- The connection to {connection} was lost while these statements were being saved,\n-- before the server confirmed them. They may or may not have been applied: check the\n-- data before running any of them again.\n\n{sql}\n"
    );
    crate::store::atomic_write(&path, text.as_bytes()).ok()?;
    Some(path)
}

/// The message to show once after a crash that recovered staged changes.
pub fn take_notice(root: &Path) -> Option<String> {
    let path = std::fs::read_to_string(root.join(NOTICE)).ok()?;
    let _ = std::fs::remove_file(root.join(NOTICE));
    Some(format!(
        "Telecat closed unexpectedly last time. The changes you had staged but not saved were written to:\n{path}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_changes_are_written_and_announced_once() {
        let root = std::env::temp_dir().join(format!("telecat-recovery-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        remember(
            1,
            "Local",
            Some("UPDATE `t` SET `v` = '1' WHERE `id` = '2';".into()),
        );
        remember(2, "Other", Some("  ".into()));
        let path = write(&root).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("-- Connection: Local") && text.contains("UPDATE `t`"));
        assert!(!text.contains("Other"));
        let notice = take_notice(&root).unwrap();
        assert!(notice.contains(&path.to_string_lossy().to_string()));
        assert!(take_notice(&root).is_none(), "shown once");
        remember(1, "Local", None);
        assert!(write(&root).is_none(), "nothing staged, nothing written");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
