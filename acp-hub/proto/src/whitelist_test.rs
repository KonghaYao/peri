//! M1 帧集白名单测试：§4.8 向量 6（未知 t / 方向约束）+ 收窄矩阵。

use crate::frame::FrameTag;
use crate::whitelist::{m1_allows, m1_allows_action_type, m1_check, Direction, M1Check, Role};

/// 客户端面：C→S 允许集合（§9.1）。
#[test]
fn client_inbound_m1_set() {
    for tag in [
        "action",
        "ysync.subscribe",
        "ysync.unsubscribe",
        "pong",
        "auth",
    ] {
        assert_eq!(
            m1_check(FrameTag(tag), Role::Client, Direction::Inbound),
            M1Check::Allowed,
            "{tag} C→S 应允许"
        );
    }
    // 布尔便捷形式一致
    assert!(m1_allows(
        FrameTag("action"),
        Role::Client,
        Direction::Inbound
    ));
}

/// 客户端面：S→C 允许集合（§9.1）。
#[test]
fn client_outbound_m1_set() {
    for tag in [
        "action_ack",
        "action_error",
        "ysync.update",
        "ready",
        "keep_alive",
    ] {
        assert_eq!(
            m1_check(FrameTag(tag), Role::Client, Direction::Outbound),
            M1Check::Allowed,
            "{tag} S→C 应允许"
        );
    }
}

/// 向量 6：客户端上行 `ysync.update` → 方向违反（§5.6「客户端上行一律拒绝」）。
#[test]
fn client_uplink_ysync_update_rejected() {
    assert_eq!(
        m1_check(FrameTag("ysync.update"), Role::Client, Direction::Inbound),
        M1Check::DirectionRejected
    );
    assert!(!m1_allows(
        FrameTag("ysync.update"),
        Role::Client,
        Direction::Inbound
    ));
}

/// 已知非 M1 帧（类型保留）：任何角色/方向均不在白名单（§9.2 收窄）。
#[test]
fn non_m1_frames_are_not_in_frame_set() {
    for tag in ["event", "ysync.sync", "ysync.awareness"] {
        for role in [Role::Client, Role::Instance] {
            for dir in [Direction::Inbound, Direction::Outbound] {
                assert_eq!(
                    m1_check(FrameTag(tag), role, dir),
                    M1Check::NotInM1,
                    "{tag} 任何 (role, dir) 均应 NotInM1"
                );
            }
        }
    }
}

/// instance 面：M1 即全量 9 帧 + auth_response（§9.2 注）。
#[test]
fn instance_m1_set() {
    // M→S
    for tag in [
        "instance/hello",
        "instance/heartbeat",
        "instance/event",
        "instance/buffer_sync",
        "instance/spawn_ack",
        "instance/kill_ack",
        "instance/process_exit",
    ] {
        assert_eq!(
            m1_check(FrameTag(tag), Role::Instance, Direction::Inbound),
            M1Check::Allowed,
            "{tag} M→S 应允许"
        );
    }
    // S→M
    for tag in ["instance/spawn", "instance/kill", "auth_response"] {
        assert_eq!(
            m1_check(FrameTag(tag), Role::Instance, Direction::Outbound),
            M1Check::Allowed,
            "{tag} S→M 应允许"
        );
    }
}

/// instance 帧在 client 面一律拒绝（角色隔离）。
#[test]
fn instance_frames_rejected_on_client_role() {
    for tag in [
        "instance/hello",
        "instance/spawn",
        "instance/kill_ack",
        "instance/process_exit",
    ] {
        for dir in [Direction::Inbound, Direction::Outbound] {
            assert_eq!(
                m1_check(FrameTag(tag), Role::Client, dir),
                M1Check::DirectionRejected,
                "{tag} client 面应拒绝"
            );
        }
    }
}

/// client 帧在 instance 面一律拒绝（角色隔离）。
#[test]
fn client_frames_rejected_on_instance_role() {
    for tag in ["action", "action_ack", "pong", "ready", "ysync.subscribe"] {
        for dir in [Direction::Inbound, Direction::Outbound] {
            assert_eq!(
                m1_check(FrameTag(tag), Role::Instance, dir),
                M1Check::DirectionRejected,
                "{tag} instance 面应拒绝"
            );
        }
    }
}

/// action `type` 子集收窄（§9.2）：M1 共 10 种（含 workspace 管理面、
/// session/list 按需查询、§8.5 启用的 chat/load 会话切换与 chat/session-new
/// 当前对话内新建会话）；M3 类型白名单外。
#[test]
fn m1_action_type_subset() {
    for t in [
        "chat/create",
        "chat/load",
        "chat/prompt",
        "chat/session-new",
        "chat/cancel",
        "chat/close",
        "permission/resolve",
        "workspace/create",
        "workspace/remove",
        "session/list",
        "project/create",
        "project/archive",
        "session/create",
        "session/open",
        "session/rename",
        "session/import",
    ] {
        assert!(m1_allows_action_type(t), "{t} 应在 M1");
    }
    for t in ["events/subscribe", "events/unsubscribe"] {
        assert!(!m1_allows_action_type(t), "{t} 应不在 M1");
    }
    assert_eq!(crate::whitelist::M1_ACTION_TYPES.len(), 16);
}

/// 全量注册表：26 个 tag 且与 §3.2 表一致（含 M2/M3 保留帧与
/// instance/forward 系，冲突 1 裁决）。
#[test]
fn frame_tag_registry_completeness() {
    let tags: Vec<&str> = crate::whitelist::FRAME_TAGS.iter().map(|t| t.0).collect();
    assert_eq!(tags.len(), 26);
    for expected in [
        "action",
        "action_ack",
        "action_error",
        "event",
        "keep_alive",
        "pong",
        "ready",
        "auth",
        "auth_response",
        "ysync.subscribe",
        "ysync.unsubscribe",
        "ysync.update",
        "ysync.sync",
        "ysync.awareness",
        "instance/hello",
        "instance/heartbeat",
        "instance/event",
        "instance/buffer_sync",
        "instance/spawn",
        "instance/kill",
        "instance/forward",
        "instance/spawn_ack",
        "instance/kill_ack",
        "instance/forward_ack",
        "instance/process_exit",
        "session_list",
    ] {
        assert!(tags.contains(&expected), "缺少注册 tag {expected}");
    }
}
