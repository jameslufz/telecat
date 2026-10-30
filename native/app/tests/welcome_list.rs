//! The welcome list's keyboard order, Enter target and delete confirmations; the custom colour.
use telecat_app::{
    form::parse_hex_color,
    store::{
        delete_connection_prompt, delete_group_prompt, enter_target, listed_connections,
        step_selection,
    },
};
use telecat_core::types::*;

fn conn(id: &str, name: &str, group: Option<&str>) -> ConnectionConfig {
    let mut c = empty_connection(Driver::Mysql);
    c.id = id.into();
    c.name = name.into();
    c.group_id = group.map(Into::into);
    c
}
fn snapshot() -> StoreSnapshot {
    StoreSnapshot {
        connections: vec![
            conn("a", "Alpha", None),
            conn("b", "Beta", Some("g1")),
            conn("c", "Gamma", Some("g2")),
            conn("d", "Delta", Some("gone")),
            conn("e", "Epsilon", Some("g1")),
        ],
        groups: vec![
            ConnectionGroup {
                id: "g1".into(),
                name: "One".into(),
                collapsed: None,
            },
            ConnectionGroup {
                id: "g2".into(),
                name: "Two".into(),
                collapsed: Some(true),
            },
        ],
    }
}
fn ids(list: &[&ConnectionConfig]) -> Vec<String> {
    list.iter().map(|c| c.id.clone()).collect()
}
#[test]
fn listed_in_the_order_the_list_shows_them() {
    let s = snapshot();
    // Groups first (a collapsed group's members are hidden), then the ungrouped and orphans.
    assert_eq!(ids(&listed_connections(&s, "")), ["b", "e", "a", "d"]);
    // Searching opens collapsed groups.
    assert_eq!(ids(&listed_connections(&s, "gam")), ["c"]);
    assert!(listed_connections(&s, "nothing").is_empty());
}
#[test]
fn arrows_move_and_enter_connects_the_selection_or_the_only_match() {
    let s = snapshot();
    let listed = listed_connections(&s, "");
    assert_eq!(step_selection(&listed, None, true).as_deref(), Some("b"));
    assert_eq!(step_selection(&listed, None, false).as_deref(), Some("d"));
    assert_eq!(
        step_selection(&listed, Some("e"), true).as_deref(),
        Some("a")
    );
    assert_eq!(
        step_selection(&listed, Some("d"), true).as_deref(),
        Some("d")
    );
    assert_eq!(
        step_selection(&listed, Some("b"), false).as_deref(),
        Some("b")
    );
    assert_eq!(step_selection(&[], None, true), None);
    assert_eq!(
        enter_target(&listed, Some("a")).map(|c| c.id.as_str()),
        Some("a")
    );
    // Nothing selected and several listed: Enter does nothing.
    assert!(enter_target(&listed, None).is_none());
    // A selection the search hides isn't connected; the one match is.
    let one = listed_connections(&s, "eps");
    assert_eq!(
        enter_target(&one, Some("a")).map(|c| c.id.as_str()),
        Some("e")
    );
}
#[test]
fn deleting_asks_and_says_what_happens() {
    let mut c = conn("a", "Prod", None);
    let (title, detail) = delete_connection_prompt(&c);
    assert_eq!(title, "Delete the connection Prod?");
    assert!(detail.contains("Keychain"), "{detail}");
    c.password_mode = PasswordMode::Ask;
    assert!(delete_connection_prompt(&c).1.contains("Keychain"));
    c.name.clear();
    assert_eq!(
        delete_connection_prompt(&c).0,
        "Delete the connection Untitled?"
    );
    let g = ConnectionGroup {
        id: "g".into(),
        name: "Team".into(),
        collapsed: None,
    };
    assert_eq!(delete_group_prompt(&g, 0).1, "The group is empty.");
    assert!(delete_group_prompt(&g, 3).1.contains("3 connections stay"));
    assert!(!delete_group_prompt(&g, 1).1.contains("deleted"));
}
#[test]
fn custom_colours_are_six_hex_digits() {
    assert_eq!(parse_hex_color("#3A7D44").unwrap(), "#3a7d44");
    assert_eq!(parse_hex_color(" 3a7d44 ").unwrap(), "#3a7d44");
    for bad in ["", "#", "#12345", "#1234567", "#gg0000", "red", "#๑๒๓๔๕๖"] {
        let e = parse_hex_color(bad).unwrap_err().to_string();
        assert!(e.contains("#RRGGBB"), "{bad}: {e}");
    }
}
