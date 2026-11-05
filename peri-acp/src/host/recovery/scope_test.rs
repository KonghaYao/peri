//! 范围分区的边界判定。

use super::*;
use peri_acp_types::recovery::{RecoveryCandidate, RecoveryScope};

fn candidate(thread: &str, parent: Option<&str>) -> RecoveryCandidate {
    RecoveryCandidate {
        thread_id: thread.to_owned(),
        parent_thread_id: parent.map(str::to_owned),
        context_usage_tokens: None,
    }
}

#[test]
fn root_candidate_is_split_from_children_and_order_is_kept() {
    let scope = RecoveryScope {
        root_thread_id: "root".to_owned(),
        candidates: vec![
            candidate("root", None),
            candidate("child-a", Some("root")),
            candidate("child-b", Some("root")),
        ],
    };
    let partition = partition(scope);
    assert_eq!(partition.root_thread_id, "root");
    assert_eq!(
        partition.root.as_ref().map(|c| c.thread_id.as_str()),
        Some("root")
    );
    assert_eq!(
        partition
            .children
            .iter()
            .map(|c| c.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["child-a", "child-b"]
    );
    assert_eq!(
        partition.direct_children_of(&"root".to_owned()),
        ["child-a".to_owned(), "child-b".to_owned()]
    );
}

#[test]
fn idle_root_leaves_no_root_candidate_but_keeps_children() {
    let scope = RecoveryScope {
        root_thread_id: "root".to_owned(),
        candidates: vec![candidate("child-a", Some("root"))],
    };
    let partition = partition(scope);
    assert!(partition.root.is_none());
    assert_eq!(partition.children.len(), 1);
}

#[test]
fn direct_children_are_scoped_to_the_asked_thread() {
    let scope = RecoveryScope {
        root_thread_id: "root".to_owned(),
        candidates: vec![
            candidate("child-a", Some("root")),
            candidate("grandchild", Some("child-a")),
        ],
    };
    let partition = partition(scope);
    assert_eq!(
        partition.direct_children_of(&"child-a".to_owned()),
        ["grandchild".to_owned()]
    );
    assert!(partition
        .direct_children_of(&"grandchild".to_owned())
        .is_empty());
}
