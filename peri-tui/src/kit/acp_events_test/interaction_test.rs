use super::*;

// ── [Slice 4 §6.8] Interaction block：生产创建点 + 结果回写 + 折叠表 ──

fn make_interaction_state() -> BridgeState {
    make_fold_test_state()
}

fn hitl_event(payload: HitlPending) -> AcpEventData {
    AcpEventData::HitlPending(crate::kit::acp_types::PendingInteraction {
        owner: Default::default(),
        request_id_json: serde_json::to_string(&"hitl-1").unwrap(),
        payload,
    })
}

fn ask_user_event(payload: AskUser) -> AcpEventData {
    AcpEventData::AskUser(crate::kit::acp_types::PendingInteraction {
        owner: Default::default(),
        request_id_json: serde_json::to_string(&"ask-1").unwrap(),
        payload,
    })
}

fn ask_user_block_of(snapshot: &ViewModelsSnapshot, idx: usize) -> &TuiAskUserBlock {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiAskUserBlock(a) => a,
        other => panic!("expected TuiAskUserBlock at [{idx}], got {other:?}"),
    }
}

/// HitlPending 到达 → block 按事件位置 push 到 committed（不进 CurrentTurn
/// 缓存——sync_cache 段对齐不可破坏），pending + 选项 [Allow once, Deny]。
#[test]
#[serial]
fn test_hitl_pending_injects_pending_permission_block() {
    let mut state = make_interaction_state();
    let hp = HitlPending {
        tool_name: "Bash".into(),
        tool_input: serde_json::json!({"command": "cargo test"}),
        batch: None,
    };
    dispatch_and_notify(&mut state, &hitl_event(hp));

    let snap = VIEW_MODELS.state().read().clone();
    // committed 末尾是 interaction block（无 current_turn 内容 → 快照即 committed）
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert!(block.pending, "等待响应 → pending=true");
    assert_eq!(block.kind, InteractionKind::Permission);
    assert_eq!(block.verb, "Bash");
    assert_eq!(block.question, "Bash wants to run: cargo test");
    assert_eq!(block.options, vec!["Allow once", "Deny"], "D6：仅两项");
    assert_eq!(
        block.fold,
        FoldState::Expanded,
        "§7 interaction Running → Expanded（可聚焦）"
    );
    assert!(
        block.request_id.as_deref() == Some(&serde_json::to_string(&"hitl-1").unwrap()),
        "request_id 与 payload 由同一 composite 事件提供"
    );
    assert_eq!(
        block.request_id.as_deref(),
        crate::kit::atoms::HITL_PENDING
            .state()
            .read()
            .as_ref()
            .map(|p| p.request_id_json.as_str())
    );
}

#[test]
#[serial]
fn test_hitl_handler_publishes_same_composite_to_atom_and_block() {
    let mut state = make_interaction_state();
    dispatch_and_notify(
        &mut state,
        &hitl_event(HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command":"cargo test"}),
            batch: None,
        }),
    );
    let atom_id = crate::kit::atoms::HITL_PENDING
        .state()
        .read()
        .as_ref()
        .unwrap()
        .request_id_json
        .clone();
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert_eq!(block.request_id.as_deref(), Some(atom_id.as_str()));
    assert!(block.question_ids.is_empty());
}

/// [§6.8 模态互斥] 同 request_id 的 pending block 重复注入（事件重放/重连/重试
/// 重复到达）→ 跳过第二次注入：重复 pending 块永远不会被 resolve（单响应
/// 事件只匹配首个），会以「可聚焦假象」永久滞留 transcript（review TEST MEDIUM）。
#[test]
#[serial]
fn test_hitl_pending_duplicate_request_id_not_reinjected() {
    let mut state = make_interaction_state();
    let hp = || HitlPending {
        tool_name: "Bash".into(),
        tool_input: serde_json::json!({"command": "cargo test"}),
        batch: None,
    };
    dispatch_and_notify(&mut state, &hitl_event(hp()));
    dispatch_and_notify(&mut state, &hitl_event(hp()));

    let snap = VIEW_MODELS.state().read().clone();
    let pending = snap
        .items
        .iter()
        .filter(|vm| matches!(vm, TuiRenderUnit::TuiAskUserBlock(a) if a.pending))
        .count();
    assert_eq!(snap.items.len(), 1, "重复 pending 事件不注入第二个 block");
    assert_eq!(pending, 1, "transcript 至多一个 pending 块（模态互斥）");
}

/// AskUser 到达 → pending block 用首问 header/options 摘要（双轨 D5）。
#[test]
#[serial]
fn test_ask_user_injects_pending_ask_user_block() {
    let mut state = make_interaction_state();
    let au = AskUser {
        questions: vec![
            Question {
                id: "q1".into(),
                header: "Pick a strategy".into(),
                question: "How to proceed?".into(),
                options: vec![
                    QuestionOption {
                        label: "Fast".into(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "Careful".into(),
                        description: String::new(),
                    },
                ],
                multi_select: false,
            },
            Question {
                id: "q2".into(),
                header: "Second".into(),
                question: "Second question".into(),
                options: vec![],
                multi_select: false,
            },
        ],
    };
    dispatch_and_notify(&mut state, &ask_user_event(au));

    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert!(block.pending);
    assert_eq!(block.kind, InteractionKind::AskUser);
    assert_eq!(block.question, "Pick a strategy", "首问 header 摘要");
    assert_eq!(block.options, vec!["Fast", "Careful"]);
    assert_eq!(block.question_ids, vec!["q1", "q2"]);
    assert_eq!(block.fold, FoldState::Expanded);
}

#[test]
#[serial]
fn test_ask_user_handler_publishes_same_composite_and_question_ids() {
    let mut state = make_interaction_state();
    let payload = AskUser {
        questions: vec![Question {
            id: "q1".into(),
            header: "H".into(),
            question: "Q".into(),
            options: vec![],
            multi_select: false,
        }],
    };
    dispatch_and_notify(&mut state, &ask_user_event(payload));
    let atom_id = crate::kit::atoms::ASK_USER_PENDING
        .state()
        .read()
        .as_ref()
        .unwrap()
        .request_id_json
        .clone();
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert_eq!(block.request_id.as_deref(), Some(atom_id.as_str()));
    assert_eq!(block.question_ids, vec!["q1"]);
}

#[test]
#[serial]
fn test_queued_interaction_blocks_keep_origin_request_ids() {
    let mut state = make_interaction_state();
    for (token, id) in [(1, "A"), (2, "B")] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::HitlPending(crate::kit::acp_types::PendingInteraction {
                owner: crate::acp_client::InteractionOwner {
                    token,
                    ..Default::default()
                },
                request_id_json: id.into(),
                payload: HitlPending {
                    tool_name: id.into(),
                    tool_input: serde_json::Value::Null,
                    batch: None,
                },
            }),
        );
    }
    let ids: Vec<_> = state
        .committed
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAskUserBlock(block) => block.request_id.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["A", "B"]);
    assert_eq!(
        crate::kit::atoms::HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "B"
    );
}

/// 结果回写：InteractionTerminal 按 semantic owner 匹配 pending block → clone +
/// pending=false + result + 重算 hash + 原位 set（COW）；completed → Collapsed。
#[test]
#[serial]
fn test_interaction_resolved_writes_back_pending_block() {
    let mut state = make_interaction_state();
    dispatch_and_notify(
        &mut state,
        &hitl_event(HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "cargo test"}),
            batch: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let idx = snap.items.len() - 1;
    let before = ask_user_block_of(&snap, idx).clone();
    let hash_before = before.content_hash;

    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionTerminal {
            owner: Default::default(),
            outcome: crate::acp_client::InteractionUiOutcome::Resolved {
                result: "Allowed once".into(),
            },
        },
    );

    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, idx);
    assert!(!block.pending, "结果回写 → pending=false");
    assert_eq!(block.result.as_deref(), Some("Allowed once"));
    assert_eq!(
        block.fold,
        FoldState::Expanded,
        "答毕保持 Expanded 完整展示（用户需求，不再自动收束）"
    );
    assert_ne!(
        block.content_hash, hash_before,
        "结果回写必须重算 hash（触发分片缓存重建）"
    );

    // 幂等：重复到达（迟到/重复事件）不改变结果（matched 条件 pending=false 不再命中）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionTerminal {
            owner: Default::default(),
            outcome: crate::acp_client::InteractionUiOutcome::Resolved {
                result: "Allowed once".into(),
            },
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, idx);
    assert!(!block.pending);
    assert_eq!(block.result.as_deref(), Some("Allowed once"));
}

/// semantic owner 不匹配的 InteractionTerminal → no-op（防御）。
#[test]
#[serial]
fn test_interaction_resolved_mismatched_request_id_noop() {
    let mut state = make_interaction_state();
    dispatch_and_notify(
        &mut state,
        &hitl_event(HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "ls"}),
            batch: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionTerminal {
            owner: crate::acp_client::InteractionOwner {
                token: 999,
                ..Default::default()
            },
            outcome: crate::acp_client::InteractionUiOutcome::Resolved {
                result: "Denied".into(),
            },
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert!(block.pending, "不匹配的 id 不回写");
    assert!(block.result.is_none());
}

/// 折叠 pass：pending → Running → Expanded（覆盖免疫）；结果回写后 Completed
/// 默认 Expanded 完整展示（用户需求）；手动折叠覆盖（FoldKey::Interaction）优先。
#[test]
#[serial]
fn test_fold_pass_interaction_pending_expanded_override_priority() {
    let mut state = make_interaction_state();
    dispatch_and_notify(
        &mut state,
        &hitl_event(HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "cargo test"}),
            batch: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let idx = snap.items.len() - 1;
    assert_eq!(ask_user_block_of(&snap, idx).fold, FoldState::Expanded);

    // 结果回写 → Completed → Expanded（不自动收束）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionTerminal {
            owner: Default::default(),
            outcome: crate::acp_client::InteractionUiOutcome::Resolved {
                result: "Denied".into(),
            },
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(ask_user_block_of(&snap, idx).fold, FoldState::Expanded);

    // 手动折叠覆盖：FOLD_OVERRIDES 写入 Interaction(rid) → 折叠 pass 恢复 Collapsed
    //（默认策略已是 Expanded，覆盖必须优先）
    let rid = serde_json::to_string(&"hitl-1").unwrap();
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Interaction(rid.clone()), FoldState::Collapsed);
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, idx);
    assert_eq!(block.fold, FoldState::Collapsed, "用户覆盖优先于自动策略");
    assert!(block.user_modified, "覆盖后 user_modified=true（免疫）");
}

/// hitl_input_summary 提取矩阵：优先主要对象字段，fallback 紧凑 JSON。
#[test]
fn test_hitl_input_summary_extraction_matrix() {
    use crate::kit::acp_events::system::hitl_input_summary;
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"command": "cargo test"})),
        "cargo test"
    );
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"path": "src/main.rs"})),
        "src/main.rs"
    );
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"query": "fn main"})),
        "fn main"
    );
    // 空字符串字段跳过，继续找下一个候选
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"command": "", "path": "Cargo.toml"})),
        "Cargo.toml"
    );
    // 无候选字段 → 紧凑 JSON
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"a": 1, "b": true})),
        r#"{"a":1,"b":true}"#
    );
    // null 输入 → 兜底文案
    assert_eq!(
        hitl_input_summary(&serde_json::Value::Null),
        crate::i18n::tr("render-interaction-tool-unknown")
    );
}
