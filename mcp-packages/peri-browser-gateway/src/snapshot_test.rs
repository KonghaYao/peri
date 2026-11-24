use serde_json::json;

use super::*;

fn tree() -> Vec<serde_json::Value> {
    vec![
        json!({
            "nodeId": "root",
            "role": {"value": "RootWebArea"},
            "name": {"value": "Example"},
            "childIds": ["button"],
            "backendDOMNodeId": 1
        }),
        json!({
            "nodeId": "button",
            "role": {"value": "button"},
            "name": {"value": "Sign in"},
            "children": [],
            "backendDOMNodeId": 7,
            "properties": [{"name": "focused", "value": {"value": true}}]
        }),
        json!({
            "nodeId": "ignored",
            "ignored": true,
            "role": {"value": "generic"},
            "childIds": ["text"],
            "backendDOMNodeId": 0
        }),
        json!({
            "nodeId": "text",
            "role": {"value": "StaticText"},
            "name": {"value": "Welcome"},
            "backendDOMNodeId": 9
        }),
    ]
}

#[test]
fn uids_have_unique_namespaces_and_map_to_backend_nodes() {
    let built = build_ax_snapshot(&tree(), 3);
    let other = build_ax_snapshot(&tree(), 3);
    assert_eq!(built.generation, 3);
    let root = built.uids.iter().find(|(_, id)| **id == 1).unwrap().0;
    let button = built.uids.iter().find(|(_, id)| **id == 7).unwrap().0;
    let text = built.uids.iter().find(|(_, id)| **id == 9).unwrap().0;
    let (namespace, _) = root.rsplit_once('_').unwrap();
    assert_eq!(root, &format!("{namespace}_0"));
    assert_eq!(button, &format!("{namespace}_1"));
    assert_eq!(text, &format!("{namespace}_2"));
    assert!(built.uids.keys().all(|uid| !other.uids.contains_key(uid)));
    assert_eq!(built.node_count, 3);
    assert!(built
        .text
        .contains(&format!("uid={root} RootWebArea \"Example\"")));
    assert!(built
        .text
        .contains(&format!("  uid={button} button \"Sign in\"")));
    assert!(built.text.contains("focused"));
    assert!(built
        .text
        .contains(&format!("uid={text} StaticText \"Welcome\"")));
}

#[test]
fn ignored_nodes_pass_their_children_through() {
    let built = build_ax_snapshot(&tree(), 1);
    assert!(!built.text.contains("generic"), "{}", built.text);
    assert!(
        built.text.contains("StaticText \"Welcome\""),
        "{}",
        built.text
    );
}

#[test]
fn an_empty_tree_names_the_empty_state() {
    let built = build_ax_snapshot(&[], 1);
    assert_eq!(built.text, "(empty accessibility tree)");
    assert!(built.uids.is_empty());
    assert_eq!(built.node_count, 0);
}

#[test]
fn a_snapshot_without_backend_ids_has_no_uids() {
    let nodes = vec![json!({
        "nodeId": "text",
        "role": {"value": "StaticText"},
        "name": {"value": "only text"}
    })];
    let built = build_ax_snapshot(&nodes, 4);
    assert!(built.uids.is_empty());
    assert_eq!(built.text.trim(), "StaticText \"only text\"");
}

#[test]
fn huge_trees_are_truncated_with_a_marker() {
    let nodes: Vec<serde_json::Value> = (0..MAX_SNAPSHOT_NODES + 5)
        .map(|index| {
            json!({
                "nodeId": format!("n{index}"),
                "role": {"value": "button"},
                "name": {"value": format!("b{index}")},
                "backendDOMNodeId": index + 1
            })
        })
        .collect();
    let built = build_ax_snapshot(&nodes, 1);
    assert_eq!(built.node_count, MAX_SNAPSHOT_NODES);
    assert!(
        built.text.ends_with("... snapshot truncated"),
        "{}",
        built.text
    );
}

#[test]
fn the_book_resolves_latest_uids_only() {
    let mut book = SnapshotBook::default();
    assert!(matches!(
        book.resolve("1_0"),
        Err(BrowserError::StaleUid(_))
    ));

    let built = build_ax_snapshot(&tree(), 1);
    let uid = built
        .uids
        .iter()
        .find(|(_, backend)| **backend == 1)
        .unwrap()
        .0;
    book.store(&built);
    assert!(book.is_valid());
    assert_eq!(book.resolve(uid).ok(), Some(1));
    assert_eq!(book.len(), 3);
    let replacement = build_ax_snapshot(&tree(), 2);
    let next = replacement
        .uids
        .iter()
        .find(|(_, backend)| **backend == 1)
        .unwrap()
        .0;
    book.store(&replacement);
    assert_eq!(book.resolve(next).ok(), Some(1));
    assert!(matches!(book.resolve(uid), Err(BrowserError::StaleUid(_))));

    book.invalidate();
    assert!(!book.is_valid());
    assert!(matches!(book.resolve(next), Err(BrowserError::StaleUid(stale)) if stale == *next));
    assert_eq!(book.generation(), 3);
    assert!(book.is_empty());
}

#[test]
fn quotes_and_backslashes_are_escaped() {
    let nodes = vec![json!({
        "nodeId": "n",
        "role": {"value": "heading"},
        "name": {"value": "say \"hi\" \\ now"},
        "backendDOMNodeId": 2
    })];
    let built = build_ax_snapshot(&nodes, 1);
    assert!(
        built.text.contains(r#"heading "say \"hi\" \\ now""#),
        "{}",
        built.text
    );
}
