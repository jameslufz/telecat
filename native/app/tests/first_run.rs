//! Exercise the production path resolver in a child with a synthetic HOME.
//! Test-only entry point: neither this mode nor the injected keys ship in the app.
use std::{fs, path::PathBuf, process::Command, sync::Arc};
use telecat_app::{
    credentials::*,
    import::*,
    store::{Profile, atomic_json},
};
use telecat_core::types::*;

#[test]
fn synthetic_home_import_uses_production_defaults() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../.cache/first-run")
        .join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&root).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "synthetic_home_child", "--nocapture"])
        .env("HOME", &root)
        .env("TELECAT_FIRST_RUN_TEST_CHILD", "1")
        .env("TELECAT_NO_KEYCHAIN", "1")
        .env_remove("TELECAT_USER_DATA")
        .env_remove("TELECAT_SCENARIO")
        // Finder launches do not have the repository as their current directory.
        .current_dir(&root)
        .status()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(status.success());
}

#[test]
fn synthetic_home_child() -> anyhow::Result<()> {
    if std::env::var_os("TELECAT_FIRST_RUN_TEST_CHILD").is_none() {
        return Ok(());
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert!(
        home.canonicalize()?.starts_with(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../.cache/first-run")
                .canonicalize()?
        )
    );
    assert!(std::env::var_os("TELECAT_USER_DATA").is_none());
    let destination = Profile::default_path()?;
    assert_eq!(
        destination,
        home.join("Library/Application Support/Telecat Native")
    );
    let source = electron_default_path().unwrap();
    assert_eq!(source, home.join("Library/Application Support/Telecat"));
    fs::create_dir_all(&source)?;
    let mut connection = empty_connection(Driver::Mysql);
    connection.id = "synthetic-first-run".into();
    connection.name = "ธนาคารกรุงเทพ 😺".into();
    connection.port = 3307;
    connection.password_mode = PasswordMode::Keychain;
    atomic_json(
        &source.join("connections.json"),
        &StoreSnapshot {
            connections: vec![connection.clone()],
            groups: vec![],
        },
    )?;
    atomic_json(&source.join("queries.json"), &serde_json::json!({}))?;
    atomic_json(&source.join("drafts.json"), &serde_json::json!({}))?;
    let fx: serde_json::Value = serde_json::from_str(include_str!("fixtures/oscrypt.json"))?;
    atomic_json(
        &source.join("secrets.json"),
        &serde_json::json!({connection.id.clone():fx["cases"][2]["encrypted"]}),
    )?;
    let names = [
        "connections.json",
        "queries.json",
        "drafts.json",
        "secrets.json",
    ];
    let before: Vec<_> = names
        .iter()
        .map(|name| fs::read(source.join(name)).unwrap())
        .collect();
    let keys = Arc::new(MemoryKeychain::default());
    // Literal pre-rename service names guard compatibility with existing secrets.
    keys.set(
        "Telecat Safe Storage",
        "Telecat Key",
        fx["key"].as_str().unwrap().as_bytes(),
    )?;
    let profile = Arc::new(Profile::new(destination, keys.clone()));
    let runtime: telecat_app::worker::Runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .into();
    let p = profile.clone();
    let s = source.clone();
    let report = futures::executor::block_on(
        runtime.spawn_blocking(move || import_electron(&p, &s, p.keychain.as_ref())),
    )?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.passwords, 1);
    assert_eq!(profile.snapshot().connections, vec![connection.clone()]);
    assert_eq!(
        keys.get("Telecat Native", &connection.id)?.unwrap(),
        fx["cases"][2]["plain"].as_str().unwrap().as_bytes()
    );
    for (name, original) in names.iter().zip(before) {
        assert_eq!(fs::read(source.join(name))?, original);
    }
    assert!(import_electron(&profile, &source, keys.as_ref()).already_attempted);
    Ok(())
}
