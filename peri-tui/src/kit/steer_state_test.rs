use super::*;
use peri_acp_types::session::UserInputQueueItem;

fn make_input(id: &str) -> UserInput {
    UserInput {
        input_id: id.to_owned(),
        content: MessageContent::text("  中文\n@image /tmp/test.png\n"),
        original_draft: "  中文\n@image /tmp/test.png\n".to_owned(),
    }
}

fn make_snapshot(revision: u64) -> UserInputQueueSnapshot {
    let input = make_input("a");
    UserInputQueueSnapshot {
        session_id: "s".to_owned(),
        generation: "g".to_owned(),
        revision,
        active_request_id: None,
        items: vec![UserInputQueueItem {
            input_id: input.input_id,
            content: input.content,
            original_draft: input.original_draft,
            state: UserInputState::Queued,
        }],
    }
}

fn make_command(kind: SteerCommandKind) -> SteerCommand {
    SteerCommand {
        session_id: "s".to_owned(),
        epoch: 1,
        command_id: "c".to_owned(),
        generation: Some("g".to_owned()),
        kind,
    }
}

fn make_state() -> SteerState {
    let mut state = SteerState::default();
    state.reset_session("s", 1);
    state.accept_snapshot(make_snapshot(1), 1, true);
    state
}

#[test]
fn test_steer_projection_rejects_old_revision_and_generation() {
    let mut state = make_state();
    let mut other_generation = make_snapshot(8);
    other_generation.generation = "old-instance".to_owned();
    assert!(
        !state.accept_snapshot(make_snapshot(0), 1, false),
        "旧 revision 不得覆盖当前投影"
    );
    assert!(
        !state.accept_snapshot(other_generation, 1, true),
        "不同实例回执不得覆盖当前投影"
    );
    assert_eq!(
        state.snapshot("s", 1).unwrap().revision,
        1,
        "当前 revision 应保留"
    );
}

#[test]
fn test_steer_projection_session_reset_rejects_old_epoch() {
    let mut state = make_state();
    state.reset_session("s", 2);
    assert!(
        !state.accept_snapshot(make_snapshot(99), 1, true),
        "相同 sessionId 重载仍要拒绝旧 epoch"
    );
    assert!(state.snapshot("s", 2).is_none(), "旧回执不得重新建立实例");
}

#[test]
fn test_steer_projection_notifications_need_established_generation() {
    let mut state = SteerState::default();
    state.reset_session("s", 1);
    assert!(
        !state.accept_snapshot(make_snapshot(4), 1, false),
        "首个通知不能代替可信 snapshot 查询"
    );
    assert!(
        state.accept_snapshot(make_snapshot(5), 1, true),
        "明确查询结果可建立实例"
    );
}

#[test]
fn test_steer_pending_dispatch_disables_repeat_takeback() {
    let mut state = make_state();
    state.begin(make_command(SteerCommandKind::Dispatch(vec![
        "a".to_owned(),
    ])));
    assert_eq!(
        state.rows("s")[0].state,
        SteerItemState::Dispatching,
        "发出意图后同条不得继续取回"
    );
}

#[test]
fn test_steer_takeback_waits_for_receipt_and_preserves_raw_draft() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::TakeBack {
        id: "a".to_owned(),
        restore_draft: true,
    });
    state.begin(command.clone());
    assert!(
        state.recover("s", 1, true).is_none(),
        "服务端未确认前不得恢复"
    );
    state.settle(
        &command,
        UserInputQueueReceipt {
            snapshot: UserInputQueueSnapshot {
                items: Vec::new(),
                ..make_snapshot(2)
            },
            results: Vec::new(),
            taken_back: Some(make_input("a")),
        },
    );
    let recovered = state.recover("s", 1, true).unwrap();
    assert_eq!(
        recovered.original_draft, "  中文\n@image /tmp/test.png\n",
        "空白、多行和图片引用应原样返回"
    );
    assert!(state.rows("s").is_empty(), "恢复完成后队列应为空");
}

#[test]
fn test_steer_takeback_with_existing_draft_only_withdraws() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::TakeBack {
        id: "a".to_owned(),
        restore_draft: false,
    });
    state.begin(command.clone());
    assert_eq!(state.rows("s")[0].state, SteerItemState::Withdrawing);
    state.settle(&command, takeback_receipt());
    assert!(state.rows("s").is_empty(), "有草稿也应从队列撤回");
    assert!(state.recover("s", 1, false).is_none());
    assert!(
        state.recover("s", 1, true).is_none(),
        "后来清空输入框不能再补恢复"
    );
}

#[test]
fn test_steer_takeback_does_not_restore_after_new_input_arrives() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::TakeBack {
        id: "a".to_owned(),
        restore_draft: true,
    });
    state.begin(command.clone());
    state.settle(&command, takeback_receipt());
    assert!(
        state.recover("s", 1, false).is_none(),
        "回执前新输入不能被覆盖"
    );
    assert!(state.rows("s").is_empty(), "撤回完成后不应留下等待恢复行");
    assert!(
        state.recover("s", 1, true).is_none(),
        "新稿清空后也不能补恢复"
    );
}

#[test]
fn test_steer_takeback_discard_keeps_rejected_enqueue_recoverable() {
    let mut state = make_state();
    let rejected = make_command(SteerCommandKind::Enqueue(make_input("b")));
    state.reject(&rejected, true);
    let before = state.pending_recovery_ids("s");
    let command = make_command(SteerCommandKind::TakeBack {
        id: "a".to_owned(),
        restore_draft: true,
    });
    state.begin(command.clone());
    state.settle(&command, takeback_receipt());
    assert_ne!(
        state.pending_recovery_ids("s"),
        before,
        "新回执必须触发恢复检查"
    );
    assert!(state.recover("s", 1, false).is_none());
    assert_eq!(state.pending_recovery_ids("s"), vec!["b"]);
    assert_eq!(
        state.recover("s", 1, true).unwrap().input_id,
        "b",
        "提交失败仍须保留原稿"
    );
    assert!(state.recover("s", 1, true).is_none());
}

fn takeback_receipt() -> UserInputQueueReceipt {
    UserInputQueueReceipt {
        snapshot: UserInputQueueSnapshot {
            items: Vec::new(),
            ..make_snapshot(2)
        },
        results: Vec::new(),
        taken_back: Some(make_input("a")),
    }
}

#[test]
fn test_steer_uncertain_enqueue_keeps_same_input_identity() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("b")));
    state.begin(command.clone());
    state.reject(&command, false);
    assert_eq!(state.rows("s")[1].id, "b", "回执不明保留原输入 ID");
    assert!(
        state.recover("s", 1, true).is_none(),
        "未确认拒绝前不得恢复成可重复提交的草稿"
    );
}

#[test]
fn test_steer_rejected_enqueue_keeps_draft_for_safe_recovery() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("b")));
    state.begin(command.clone());
    state.reject(&command, true);
    assert!(
        !state.pending_recovery_ids("s").is_empty(),
        "明确拒绝后保留完整原稿"
    );
    assert_eq!(
        state.recover("s", 1, true).unwrap().input_id,
        "b",
        "取回身份不得变换"
    );
}

#[test]
fn test_steer_delivered_and_replay_keep_one_identity() {
    let mut state = make_state();
    assert!(state.claim_delivery("s", "a"), "首次 canonical 消息应接受");
    state.reset_session("s", 2);
    assert!(
        !state.claim_delivery("s", "a"),
        "相同会话重放后迟到 Delivered 不得重复"
    );
}

#[test]
fn test_steer_attachment_content_survives_roundtrip() {
    let attachments = vec![PendingAttachment {
        label: "image".to_owned(),
        media_type: "image/png".to_owned(),
        base64_data: "AQID".to_owned(),
    }];
    let content = content_for_draft("  image\n", &attachments);
    let recovered = attachments_from_content(&content);
    assert_eq!(
        content.text_content(),
        "  image\n",
        "附件发送不得 trim 原稿"
    );
    assert_eq!(recovered[0].base64_data, "AQID", "附件数据须完整恢复");
    assert_eq!(recovered[0].media_type, "image/png", "附件类型须完整恢复");
}

#[test]
fn test_steer_confirmed_takeback_survives_session_reload() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::TakeBack {
        id: "a".to_owned(),
        restore_draft: true,
    });
    state.begin(command.clone());
    state.settle(
        &command,
        UserInputQueueReceipt {
            snapshot: UserInputQueueSnapshot {
                items: Vec::new(),
                ..make_snapshot(2)
            },
            results: Vec::new(),
            taken_back: Some(make_input("a")),
        },
    );
    state.reset_session("other", 2);
    assert!(
        state.pending_recovery_ids("other").is_empty(),
        "取回稿不能跨会话覆盖其他编辑器"
    );
    state.reset_session("s", 3);
    assert!(
        !state.pending_recovery_ids("s").is_empty(),
        "原会话重载后仍须能访问已确认取回的原稿"
    );
    assert_eq!(
        state.recover("s", 3, true).unwrap().original_draft,
        make_input("a").original_draft,
        "已从服务端移除的原稿不能被旧 epoch 过滤丢失"
    );
}

#[test]
fn test_steer_takeback_receipt_after_reload_remains_recoverable() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::TakeBack {
        id: "a".to_owned(),
        restore_draft: true,
    });
    state.begin(command.clone());
    state.reset_session("s", 2);
    state.settle(
        &command,
        UserInputQueueReceipt {
            snapshot: UserInputQueueSnapshot {
                items: Vec::new(),
                ..make_snapshot(2)
            },
            results: Vec::new(),
            taken_back: Some(make_input("a")),
        },
    );
    assert!(state.snapshot("s", 2).is_none(), "旧快照仍不能跨重载绑定");
    assert_eq!(
        state.recover("s", 2, true).unwrap().input_id,
        "a",
        "确定取回的原稿归属原会话并继续可恢复"
    );
}

#[test]
fn test_steer_unknown_input_after_instance_change_stays_visible_without_retry() {
    let mut state = make_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("b")));
    state.begin(command);
    state.reset_session("s", 2);
    let mut snapshot = make_snapshot(1);
    snapshot.generation = "new-instance".into();
    state.accept_snapshot(snapshot, 2, true);
    assert!(
        state.resume_pending("s", 2).is_empty(),
        "不同实例不得重投旧未知请求"
    );
    assert!(
        state.rows("s").iter().any(|item| item.id == "b"),
        "尚未核实结果的原稿必须仍可见"
    );
    assert!(
        state.pending_recovery_ids("s").is_empty(),
        "不能假设未发送而生成可重复提交草稿"
    );
    state.claim_delivery("s", "b");
    assert!(
        !state.rows("s").iter().any(|item| item.id == "b"),
        "重放确认已进入历史后应消除未知投影"
    );
}
