//! Feature: "New Connection…" from a group's context menu creates the connection in that group.
use std::sync::Arc;
use telecat_app::{
    credentials::MemoryKeychain,
    store::{Profile, group_menu, new_connection},
};
use telecat_core::types::*;

fn profile() -> Profile {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../.cache/group-connection-profiles")
        .join(uuid_like());
    std::fs::create_dir_all(&root).unwrap();
    Profile::new(root, Arc::new(MemoryKeychain::default()))
}
fn uuid_like() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
fn seeded() -> Profile {
    let p = profile();
    let mut other = empty_connection(Driver::Mysql);
    other.id = "existing".into();
    other.name = "Existing".into();
    other.group_id = Some("g1".into());
    p.seed(&StoreSnapshot {
        connections: vec![other],
        groups: vec![
            ConnectionGroup {
                id: "g1".into(),
                name: "Team".into(),
                collapsed: Some(true),
            },
            ConnectionGroup {
                id: "g2".into(),
                name: "Other".into(),
                collapsed: None,
            },
        ],
    })
    .unwrap();
    p
}

#[test]
fn group_menu_offers_new_connection_first() {
    let menu = group_menu();
    let ids: Vec<_> = menu
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap_or("-"))
        .collect();
    assert_eq!(ids, ["new", "-", "rename", "delete"]);
    assert_eq!(menu[0]["label"], "New Connection…");
}

#[test]
fn a_connection_created_from_a_group_lands_at_its_end_and_opens_it() {
    let p = seeded();
    let mut c = new_connection(Driver::Postgres, Some("g1".into()));
    c.name = "New".into();
    let saved = p.save_connection(c, Some("pw")).unwrap();
    let s = p.snapshot();
    let in_group: Vec<_> = s
        .connections
        .iter()
        .filter(|c| c.group_id.as_deref() == Some("g1"))
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        in_group,
        ["Existing", "New"],
        "appended to the group's order"
    );
    assert_eq!(
        s.groups[0].collapsed,
        Some(false),
        "the collapsed group is opened"
    );
    assert_eq!(s.groups[1].collapsed, None, "other groups untouched");
    // Persisted in the same `groupId` field the app and the Electron import use.
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.root.join("connections.json")).unwrap())
            .unwrap();
    let stored = raw["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == saved.id.as_str())
        .unwrap();
    assert_eq!(stored["groupId"], "g1");
}

#[test]
fn plain_new_connection_and_cancel_are_unchanged() {
    let p = seeded();
    let before = std::fs::read_to_string(p.root.join("connections.json")).unwrap();
    // Cancelling the picker or the form never calls save: nothing changes on disk.
    let _ = new_connection(Driver::Mysql, Some("g1".into()));
    assert_eq!(
        std::fs::read_to_string(p.root.join("connections.json")).unwrap(),
        before
    );
    let plain = new_connection(Driver::Mysql, None);
    assert_eq!(
        plain,
        empty_connection(Driver::Mysql),
        "⌘N / + create the same blank connection as before"
    );
    let saved = p.save_connection(plain, None).unwrap();
    assert_eq!(p.connection(&saved.id).unwrap().group_id, None);
    assert_eq!(
        p.snapshot().groups[0].collapsed,
        Some(true),
        "no group opened"
    );
}

#[test]
fn a_group_deleted_while_the_form_was_open_leaves_the_connection_ungrouped() {
    let p = seeded();
    p.delete_group("g2").unwrap();
    let saved = p
        .save_connection(new_connection(Driver::Mysql, Some("g2".into())), None)
        .unwrap();
    assert_eq!(p.connection(&saved.id).unwrap().group_id, None);
}
