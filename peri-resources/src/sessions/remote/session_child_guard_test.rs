//! child 快照输入一致性与「新建只接受 root」的**远程入口**回归（不连网）。
//!
//! 判定本身是纯函数（`data::ensure_child_relation`、`write_new_session` 的 root 校验），门面、
//! 本机 adapter 与远程 adapter 引用的是同一份代码——「只有一条规则」由编译期引用保证，不靠
//! 各处各写一遍。这里补的是远程入口自己的两件事：
//!
//! - 输入不自洽时 `save_child` 在**任何远端读取之前**就拒绝：本机一侧没有可写的记录（v10
//!   之后本机也不持有远端操作日志），拒绝因此不带任何副作用；
//! - 带父的 `save_new_session` 同样在**任何远端读取之前**被拒绝：远程新建只造 root，有父必须
//!   走 `save_child`；
//! - 同一入口对自洽的输入不设额外门槛：它照常走到连接（本装配下连接已关闭，因此失败于
//!   `Internal` 而不是 `InvalidInput`）。
//!
//! 边界：远端数据事实（不落行、原 root 不变）无法在这套离线装配上断言——那需要真实云库，
//! 而共享库的身份不可重置。远端落库语句里的父子列与 `target.meta` 同源由 `session_sql`
//! 与 `session_shape_test` 覆盖；本文件能证明的是「校验先于 I/O，被拒时不发任何请求」。
//! 真云对照：`cloud_limit_test.rs::cloud_remote_create_refuses_parent_input`（类型化拒绝 +
//! 远端零行 + 远端零 child 行）。

use peri_acp_types::session_resources::{
    ChildSnapshot, FrozenSnapshotBytes, NewSession, NewSessionDraft, NewSessionMeta,
    SessionResourceErrorKind,
};
use peri_acp_types::store::InheritedContext;

use super::cloud_tests::{synth_binding, synth_thread};
use super::schema::StoreId;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;

/// 连接已关闭的远程 adapter（与 `close` 之后的状态同一个形状）。
struct ClosedRemote {
    adapter: RemoteSessionData,
}

impl ClosedRemote {
    async fn open() -> Self {
        Self {
            adapter: RemoteSessionData::closed_for_test(StoreId::mint()),
        }
    }
}

/// 合成一份新建输入：默认是 root（`parent_thread_id: None`），由调用方按需改成带父。
fn root_session(thread: &str) -> NewSession {
    NewSession {
        thread_id: synth_thread(thread),
        created_at: "2026-09-26T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {thread}")),
            cwd: "/tmp/peri-child-guard".to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: synth_binding(),
        frozen: FrozenSnapshotBytes::new(r#"{"frozen":"guard"}"#),
    }
}

/// 合成一份 child 快照：`meta_parent` 是落库用的那一份父子关系。
fn child_snapshot(
    child: &str,
    parent: &str,
    root: &str,
    meta_parent: Option<&str>,
) -> ChildSnapshot {
    ChildSnapshot {
        target: NewSession {
            thread_id: synth_thread(child),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: Some(format!("child {child}")),
                cwd: "/tmp/peri-child-guard".to_owned(),
                parent_thread_id: meta_parent.map(synth_thread),
                hidden: true,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: synth_binding(),
            frozen: FrozenSnapshotBytes::new(r#"{"frozen":"guard"}"#),
        },
        parent_id: synth_thread(parent),
        root_id: synth_thread(root),
        inherited: InheritedContext::default(),
    }
}

#[tokio::test]
async fn test_remote_child_entry_rejects_a_disagreeing_relation_before_any_io() {
    let remote = ClosedRemote::open().await;

    // 合法父/根，但目标 meta 里没有父（旧行为：写成一条独立 root）；以及把自己当根。
    let refused = [
        (
            "target without a parent",
            child_snapshot("rel-child", "rel-root", "rel-root", None),
        ),
        (
            "own root",
            child_snapshot("rel-child", "rel-root", "rel-child", Some("rel-root")),
        ),
    ];
    for (label, snapshot) in &refused {
        let error = remote.adapter.save_child(snapshot).await.unwrap_err();
        assert!(
            matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. }),
            "{label}: expected InvalidInput, got {error:?}"
        );
    }
    // 拒绝发生在任何远端读取之前：本机不再留任何记录（v10 之后也没有可留的记录）。
    // 自洽的输入不被额外拦住：同一入口照常走到连接（连接已关闭，因此是 Internal）。
    let legal = child_snapshot("rel-child", "rel-root", "rel-root", Some("rel-root"));
    let error = remote.adapter.save_child(&legal).await.unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::Internal { .. }),
        "a consistent child must reach the store path, got {error:?}"
    );
}

/// 远程新建只接受 root：带父的输入在**任何远端读取之前**被类型化拒绝。
///
/// 与 `save_child` 的输入一致性判定同一性质——只看纯输入，因此拒绝不带任何副作用。放行会在
/// 远端写出一条**没有经过 child 通路判定**的父关系（父子/根归属、root owner 门禁、frozen
/// 继承全都没走）。这条判定原先挂在已撤销的远程执行面上，v10 撤销时连同文件一起被删掉了。
#[tokio::test]
async fn test_remote_root_entry_rejects_a_parent_before_any_io() {
    let remote = ClosedRemote::open().await;

    let mut with_parent = root_session("root-with-parent");
    with_parent.meta.parent_thread_id = Some(synth_thread("root-parent"));
    let error = remote
        .adapter
        .save_new_session(&with_parent)
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. }),
        "a parent-bearing input must be refused as input, got {error:?}"
    );

    // 判定不读存储：父会话根本不必存在，也不会有任何远端读取。
    // 同一入口对 root 输入不设额外门槛：照常走到连接（连接已关闭，因此是 Internal）。
    let error = remote
        .adapter
        .save_new_session(&root_session("root-plain"))
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::Internal { .. }),
        "a root input must reach the store path, got {error:?}"
    );
}

#[tokio::test]
async fn test_remote_draft_reaches_store_without_an_owner_token() {
    let remote = ClosedRemote::open().await;
    let root = root_session("draft-without-owner");
    let draft = NewSessionDraft {
        thread_id: root.thread_id,
        created_at: root.created_at,
        meta: root.meta,
        binding: root.binding,
    };

    // A draft precedes owner acquisition. The closed connection proves that the
    // request passed admission and reached the store path without a token.
    let error = remote
        .adapter
        .save_new_session_draft(&draft)
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::Internal { .. }),
        "draft creation should reach the store before owner acquisition, got {error:?}"
    );
}
