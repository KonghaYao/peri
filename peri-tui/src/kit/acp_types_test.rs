//! Tests for acp_types

use super::*;

// -- CurrentTurn tests ----------------------------------------------------

#[test]
fn test_default_empty() {
    let mut ct = CurrentTurn::default();
    assert!(ct.text.is_empty());
    assert!(ct.reasoning.is_empty());
    assert!(ct.tool_cards.is_empty());
    assert!(!ct.active);
    assert!(ct.view_models().is_empty());
}

#[test]
fn test_new_equals_default() {
    let a = CurrentTurn::new();
    let b = CurrentTurn::default();
    assert_eq!(a.text, b.text);
    assert_eq!(a.active, b.active);
}

#[test]
fn test_append_text_sets_active() {
    let mut ct = CurrentTurn::new();
    assert!(!ct.active);
    ct.append_text("hello ", None);
    ct.append_text("world", None);
    assert_eq!(ct.text, "hello world");
    assert!(ct.active);
}

#[test]
fn test_append_reasoning_sets_active() {
    let mut ct = CurrentTurn::new();
    ct.append_reasoning("thinking...", None);
    assert_eq!(ct.reasoning, "thinking...");
    assert!(ct.active);
}

#[test]
fn test_start_then_end_tool() {
    let mut ct = CurrentTurn::new();
    ct.start_tool(ToolCardAccumulator::new(
        "tc-1".into(),
        "Edit".into(),
        "path: foo.rs".into(),
    ));
    assert_eq!(ct.tool_cards.len(), 1);
    assert!(ct.active);

    ct.end_tool("tc-1", "updated 3 lines".into(), false);
    let card = &ct.tool_cards[0];
    assert_eq!(card.output_summary.as_deref(), Some("updated 3 lines"));
    assert!(!card.is_error);
}

#[test]
fn test_start_tool_duplicate_id_upserts_input() {
    // [Fix think-end] agent 侧提前 ToolStarted（工具块开始即发，参数尚未
    // 流式生成 → raw_input=Null）与 dispatch 的正式 ToolStarted（参数完整）
    // 同 id 先后到达：只升级 input，不重建卡片（保留 started_at/时长语义）。
    let mut ct = CurrentTurn::new();
    ct.append_reasoning("thinking...", None);
    ct.start_tool(ToolCardAccumulator::with_input(
        "tc-1".into(),
        "Edit".into(),
        String::new(),
        serde_json::Value::Null,
        None,
    ));
    assert_eq!(ct.tool_cards.len(), 1);
    assert_eq!(ct.tool_cards[0].raw_input, serde_json::Value::Null);
    assert!(ct.tool_cards[0].input_summary.is_empty());

    // 正式发：同 id，input 完整 → upsert input
    ct.start_tool(ToolCardAccumulator::with_input(
        "tc-1".into(),
        "Edit".into(),
        "path: foo.rs".into(),
        serde_json::json!({"path": "foo.rs"}),
        None,
    ));
    assert_eq!(ct.tool_cards.len(), 1, "同 id 不应重复建卡");
    assert_eq!(
        ct.tool_cards[0].raw_input,
        serde_json::json!({"path": "foo.rs"})
    );
    assert_eq!(ct.tool_cards[0].input_summary, "path: foo.rs");
}

#[test]
fn test_end_tool_unknown_id_is_noop() {
    let mut ct = CurrentTurn::new();
    ct.start_tool(ToolCardAccumulator::new(
        "tc-1".into(),
        "Edit".into(),
        "x".into(),
    ));
    ct.end_tool("does-not-exist", "out".into(), true);
    assert!(ct.tool_cards[0].output_summary.is_none());
    assert!(!ct.tool_cards[0].is_error);
}

#[test]
fn test_bash_timer_hash_changes_over_time() {
    // [设计变更] ToolCard content_hash 现在纳入 duration（按秒向下取整）——
    // 这是为了让按 hash 分片的渲染缓存每秒刷新一次 duration 文本。
    // 此测试验证：跨秒后 content_hash 变化（触发缓存失效 + duration 文本更新）。
    let mut ct = CurrentTurn::new();
    ct.start_tool(ToolCardAccumulator::new(
        "tc-bash".into(),
        "Bash".into(),
        "cargo test".into(),
    ));

    let first_hash = match &ct.view_models()[0] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert!(card.is_running);
            assert!(card.running_duration_ms.is_some());
            card.content_hash
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    };

    std::thread::sleep(std::time::Duration::from_millis(1_100));
    ct.invalidate_cache();

    let second_hash = match &ct.view_models()[0] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert!(card.is_running);
            assert!(card.running_duration_ms.unwrap() >= 1_000);
            card.content_hash
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    };

    // 跨秒后 duration_secs 从 0 变为 1，content_hash 必须变化
    assert_ne!(
        first_hash, second_hash,
        "跨秒后 duration_secs 变化，content_hash 必须变化以触发缓存失效"
    );
}

#[test]
fn test_completed_bash_hash_stays_same() {
    let mut ct = CurrentTurn::new();
    ct.start_tool(ToolCardAccumulator::new(
        "tc-bash".into(),
        "Bash".into(),
        "cargo test".into(),
    ));
    ct.end_tool("tc-bash", "ok".into(), false);

    let first_hash = match &ct.view_models()[0] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert!(!card.is_running);
            assert_eq!(card.running_duration_ms, None);
            card.content_hash
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    };

    std::thread::sleep(std::time::Duration::from_millis(1_100));
    ct.invalidate_cache();

    let second_hash = match &ct.view_models()[0] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert!(!card.is_running);
            assert_eq!(card.running_duration_ms, None);
            card.content_hash
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    };

    assert_eq!(first_hash, second_hash);
}

#[test]
fn test_deactivate() {
    let mut ct = CurrentTurn::new();
    ct.append_text("x", None);
    assert!(ct.active);
    ct.deactivate();
    assert!(!ct.active);
}

// -- AcpEventData decode tests -------------------------------------------

#[test]
fn test_current_turn_subagent_streaming_builds_nested_group() {
    let mut ct = CurrentTurn::new();
    ct.start_subagent("agent-1".into(), "researcher".into());
    assert!(ct.append_subagent_text("agent-1", "hello"));
    assert!(ct.start_subagent_tool(
        "agent-1",
        ToolCardAccumulator::new("tc-1".into(), "Read".into(), "path: foo.rs".into()),
    ));
    assert!(ct.end_subagent_tool("agent-1", "tc-1", "10 lines".into(), false));

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 1);
    match &vms[0] {
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            assert_eq!(group.agent_id, "agent-1");
            assert_eq!(group.agent_name, "researcher");
            assert_eq!(group.view_models.len(), 2);
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

#[test]
fn test_current_turn_subagent_unknown_route_returns_false() {
    let mut ct = CurrentTurn::new();
    assert!(!ct.append_subagent_text("missing", "hello"));
    assert!(ct.view_models().is_empty());
}

/// [回归测试] ToolStarted 后无 ToolEnded 直接 SubagentStopped：
/// stop_subagent 必须 deactivate child_turn，否则无 output_summary 的
/// 工具卡保持 Running（is_running = turn_active && 无输出），渲染为永久进行中。
#[test]
fn test_stop_subagent_without_tool_ended_deactivates_child_turn() {
    let mut ct = CurrentTurn::new();
    ct.start_subagent("agent-1".into(), "researcher".into());
    assert!(ct.start_subagent_tool(
        "agent-1",
        ToolCardAccumulator::new("tc-1".into(), "Read".into(), "path: foo.rs".into()),
    ));
    // 无 end_subagent_tool，直接 stop
    ct.stop_subagent("agent-1", false, "");

    let s = ct
        .subagents
        .iter_mut()
        .find(|s| s.agent_id == "agent-1")
        .expect("subagent 应存在");
    assert!(
        !s.child_turn.active,
        "stop_subagent 后 child_turn 必须 deactivate（ToolStarted 无 ToolEnded 场景）"
    );
    let vms: Vec<_> = s.child_turn.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 1, "child_turn 应仍保留工具卡");
    match &vms[0] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert!(
                !card.is_running,
                "ToolStarted 无 ToolEnded 时停止，tool card 不应保持 Running"
            );
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    }
}

#[test]
fn test_decode_turn_done() {
    let decoded = AcpEventData::decode("turn-done", serde_json::json!({}));
    match decoded {
        AcpEventData::TurnDone => {}
        _ => panic!("expected TurnDone"),
    }
}

#[test]
fn test_decode_turn_interrupted() {
    let data = serde_json::json!({"reason": "user cancelled"});
    let decoded = AcpEventData::decode("turn-interrupted", data);
    match decoded {
        AcpEventData::TurnInterrupted { reason, request_id } => {
            assert_eq!(reason, "user cancelled");
            assert_eq!(request_id, None, "requestId 缺失时应为 None");
        }
        _ => panic!("expected TurnInterrupted"),
    }
}

#[test]
fn test_decode_turn_interrupted_with_request_id() {
    let data = serde_json::json!({"reason": "user cancelled", "requestId": "rid-1"});
    let decoded = AcpEventData::decode("turn-interrupted", data);
    match decoded {
        AcpEventData::TurnInterrupted { reason, request_id } => {
            assert_eq!(reason, "user cancelled");
            assert_eq!(request_id.as_deref(), Some("rid-1"));
        }
        _ => panic!("expected TurnInterrupted"),
    }
}

#[test]
fn test_decode_tool_count() {
    let data = serde_json::json!({"count": 3});
    let decoded = AcpEventData::decode("tool-count", data);
    match decoded {
        AcpEventData::ToolCount(tc) => assert_eq!(tc.count, 3),
        _ => panic!("expected ToolCount"),
    }
}

#[test]
fn test_decode_budget_warning() {
    let data = serde_json::json!({
        "used": 85000,
        "limit": 100000,
        "threshold": "0.85"
    });
    let decoded = AcpEventData::decode("budget-warning", data);
    match decoded {
        AcpEventData::BudgetWarning(bw) => assert_eq!(bw.threshold, "0.85"),
        _ => panic!("expected BudgetWarning"),
    }
}

#[test]
fn test_decode_system_notification() {
    let data = serde_json::json!({"text": "model switched", "level": "info"});
    let decoded = AcpEventData::decode("system-notification", data);
    match decoded {
        AcpEventData::SystemNotification(sn) => assert_eq!(sn.level, "info"),
        _ => panic!("expected SystemNotification"),
    }
}

#[test]
fn test_decode_prediction() {
    let data = serde_json::json!({"text": "fix typo"});
    let decoded = AcpEventData::decode("prediction", data);
    match decoded {
        AcpEventData::Prediction(p) => assert_eq!(p.text, "fix typo"),
        _ => panic!("expected Prediction"),
    }
}

#[test]
fn test_decode_file_suggestions() {
    let data = serde_json::json!({"files": ["src/main.rs", "src/lib.rs"]});
    let decoded = AcpEventData::decode("file-suggestions", data);
    match decoded {
        AcpEventData::FileSuggestions(fs) => assert_eq!(fs.files.len(), 2),
        _ => panic!("expected FileSuggestions"),
    }
}

#[test]
fn test_decode_rewind_preview() {
    let data = serde_json::json!({"files": [], "messages": []});
    let decoded = AcpEventData::decode("rewind-preview", data);
    match decoded {
        AcpEventData::RewindPreview(rp) => assert!(rp.files.is_empty()),
        _ => panic!("expected RewindPreview"),
    }
}

#[test]
fn test_decode_oauth_needed() {
    let data = serde_json::json!({
        "server_name": "github-mcp",
        "auth_url": "https://github.com/login/oauth"
    });
    let decoded = AcpEventData::decode("oauth-needed", data);
    match decoded {
        AcpEventData::OauthNeeded(on) => assert_eq!(on.server_name, "github-mcp"),
        _ => panic!("expected OauthNeeded"),
    }
}

#[test]
fn test_decode_subagent_started() {
    let data = serde_json::json!({
        "agent_id": "sa-1",
        "agent_name": "file-searcher"
    });
    let decoded = AcpEventData::decode("subagent-started", data);
    match decoded {
        AcpEventData::SubagentStarted { agent_name, .. } => {
            assert_eq!(agent_name, "file-searcher")
        }
        _ => panic!("expected SubagentStarted"),
    }
}

#[test]
fn test_decode_subagent_stopped() {
    // legacy 通道缺省：无 result/is_error 字段 → 空字符串 / false（向后兼容）
    let data = serde_json::json!({"agent_id": "sa-1"});
    let decoded = AcpEventData::decode("subagent-stopped", data);
    match decoded {
        AcpEventData::SubagentStopped {
            agent_id,
            result,
            is_error,
        } => {
            assert_eq!(agent_id, "sa-1");
            assert_eq!(result, "", "legacy 缺省 result 应为空");
            assert!(!is_error, "legacy 缺省 is_error 应为 false");
        }
        _ => panic!("expected SubagentStopped"),
    }
    // 显式字段（canonical 主通道 peri/agent_event）
    let data = serde_json::json!({
        "agent_id": "sa-2",
        "result": "loop failed: llm error",
        "is_error": true
    });
    let decoded = AcpEventData::decode("subagent-stopped", data);
    match decoded {
        AcpEventData::SubagentStopped {
            agent_id,
            result,
            is_error,
        } => {
            assert_eq!(agent_id, "sa-2");
            assert_eq!(result, "loop failed: llm error");
            assert!(is_error);
        }
        _ => panic!("expected SubagentStopped"),
    }
}

#[test]
fn test_decode_unknown_event_name() {
    let data = serde_json::json!({"foo": "bar"});
    let decoded = AcpEventData::decode("future-event", data);
    match decoded {
        AcpEventData::Unknown { event, data } => {
            assert_eq!(event, "future-event");
            assert_eq!(data["foo"], "bar");
        }
        _ => panic!("expected Unknown"),
    }
}

#[test]
fn test_decode_malformed_data_falls_to_unknown() {
    let data = serde_json::json!("not an object");
    let decoded = AcpEventData::decode("future-event-xyz", data);
    match decoded {
        AcpEventData::Unknown { event, .. } => assert_eq!(event, "future-event-xyz"),
        _ => panic!("expected Unknown for malformed data"),
    }
}

// ── Segment interleaving tests ─────────────────────────────────────────

/// 工具调用之间由 message_id 变化驱动的文本段分隔。
///
/// 场景：Agent 说"1"（message_A）→ Read → 说"2"（message_B）→ Bash。
/// 期望 view_models 产出 4 项，顺序为
/// [TuiAssistantBubble("1"), TuiToolCard(Read), TuiAssistantBubble("2"), TuiToolCard(Bash)]
#[test]
fn test_build_view_models_interleaves_text_and_tools() {
    let mut ct = CurrentTurn::new();
    ct.append_text("1", Some("msg_A"));
    ct.start_tool(ToolCardAccumulator::new(
        "tc-1".into(),
        "Read".into(),
        "file: a.rs".into(),
    ));
    ct.end_tool("tc-1", "ok".into(), false);
    ct.append_text("2", Some("msg_B"));
    ct.start_tool(ToolCardAccumulator::new(
        "tc-2".into(),
        "Bash".into(),
        "echo hi".into(),
    ));
    ct.end_tool("tc-2", "hi".into(), false);

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 4, "应为 4 项：Text→Tool→Text→Tool");
    assert!(
        matches!(&vms[0], TuiRenderUnit::TuiAssistantBubble(_)),
        "[0] 应为 Text bubble (1)"
    );
    assert!(
        matches!(&vms[1], TuiRenderUnit::TuiToolCard(_)),
        "[1] 应为 Tool card (Read)"
    );
    assert!(
        matches!(&vms[2], TuiRenderUnit::TuiAssistantBubble(_)),
        "[2] 应为 Text bubble (2)"
    );
    assert!(
        matches!(&vms[3], TuiRenderUnit::TuiToolCard(_)),
        "[3] 应为 Tool card (Bash)"
    );

    // 验证文本内容是否正确分离（不是整体拼接）
    match &vms[0] {
        TuiRenderUnit::TuiAssistantBubble(b) => assert_eq!(b.text, "1"),
        _ => unreachable!(),
    }
    match &vms[2] {
        TuiRenderUnit::TuiAssistantBubble(b) => assert_eq!(b.text, "2"),
        _ => unreachable!(),
    }
}

/// 同一 message_id 的多段文本不拆开，保持为一个 bubble。
#[test]
fn test_same_message_id_keeps_text_contiguous() {
    let mut ct = CurrentTurn::new();
    ct.append_text("part1", Some("msg_A"));
    ct.append_text(" part2", Some("msg_A"));
    ct.start_tool(ToolCardAccumulator::new(
        "tc-1".into(),
        "Read".into(),
        "f: x.rs".into(),
    ));

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 2, "1 个 Text bubble + 1 个 Tool card");
    match &vms[0] {
        TuiRenderUnit::TuiAssistantBubble(b) => {
            assert_eq!(b.text, "part1 part2", "同 message_id 不应拆分");
        }
        _ => panic!("[0] 应为 Text bubble"),
    }
}

/// 无 message_id（旧事件或协议不携带）时，依赖 tool/subagent 边界分段。
#[test]
fn test_no_message_id_uses_tool_boundaries() {
    let mut ct = CurrentTurn::new();
    ct.append_text("a", None);
    ct.start_tool(ToolCardAccumulator::new(
        "tc-1".into(),
        "Read".into(),
        "f: x.rs".into(),
    ));
    ct.end_tool("tc-1", "ok".into(), false);
    ct.append_text("b", None);

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 3, "Text→Tool→Text");
    assert!(matches!(&vms[0], TuiRenderUnit::TuiAssistantBubble(_)));
    assert!(matches!(&vms[1], TuiRenderUnit::TuiToolCard(_)));
    assert!(matches!(&vms[2], TuiRenderUnit::TuiAssistantBubble(_)));
}

/// M1: SubAgentAccumulator content_hash 随 child VM 内容变化。
/// 相同结构（1 个 child）但不同文本 → 不同 content_hash。
#[test]
fn test_subagent_content_hash_changes_with_child_content() {
    let mut acc1 = SubAgentAccumulator::new("agent-1".into(), "worker".into());
    acc1.append_text("hello");
    let vm1 = acc1.view_model();
    let hash1 = match &vm1 {
        TuiRenderUnit::TuiSubAgentGroup(g) => g.content_hash,
        _ => panic!("expected TuiSubAgentGroup"),
    };

    let mut acc2 = SubAgentAccumulator::new("agent-1".into(), "worker".into());
    acc2.append_text("world");
    let vm2 = acc2.view_model();
    let hash2 = match &vm2 {
        TuiRenderUnit::TuiSubAgentGroup(g) => g.content_hash,
        _ => panic!("expected TuiSubAgentGroup"),
    };

    assert_ne!(
        hash1, hash2,
        "不同 child 内容应产出不同 content_hash（M1 修复前会相等）"
    );
}

/// [回归测试] 每个 batch 的第一个工具调用应在完成后 is_running=false。
///
/// 场景复现 issue #2026-07-20-first-tool-call-per-batch-stuck-running：
/// reasoning → tool1 启动 → 更多 reasoning → tool2 启动 →
/// tool1 结束 → tool2 结束。
/// 预期两个工具完成后 is_running 都为 false。
#[test]
fn test_first_tool_in_batch_is_running_false_after_end() {
    let mut ct = CurrentTurn::new();

    // 第一批 reasoning
    ct.append_reasoning("思考了 653 字符...", None);
    // 第一个工具启动
    ct.start_tool(ToolCardAccumulator::new(
        "tc-shell-1".into(),
        "Shell".into(),
        "git log --oneline -15".into(),
    ));
    // 第二批 reasoning（在工具 1 启动后到达）
    ct.append_reasoning("思考了 302 字符...", None);
    // 第二个工具启动
    ct.start_tool(ToolCardAccumulator::new(
        "tc-shell-2".into(),
        "Shell".into(),
        "git show --stat e5239171".into(),
    ));
    // 第一个工具结束
    ct.end_tool("tc-shell-1", "c4596722 refactor...".into(), false);
    // 第二个工具结束
    ct.end_tool("tc-shell-2", "commit e5239171...".into(), false);

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();

    // 期望：2 个 reasoning bubble + 2 个 tool card = 4 个 VM
    assert_eq!(vms.len(), 4, "应为 2 个 AssistantBubble + 2 个 ToolCard");

    // 验证第一个工具卡片：is_running 应为 false
    match &vms[1] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert_eq!(card.tool_id, "tc-shell-1");
            assert!(
                !card.is_running,
                "[回归测试] 第一个工具调用完成后的 is_running 应为 false，实际为 true"
            );
            assert!(
                !card.output_summary.is_empty(),
                "第一个工具完成后的 output_summary 不应为空"
            );
        }
        _ => panic!("vms[1] 应为 TuiToolCard"),
    }

    // 验证第二个工具卡片：is_running 也应为 false
    match &vms[3] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert_eq!(card.tool_id, "tc-shell-2");
            assert!(
                !card.is_running,
                "第二个工具调用完成后的 is_running 也应为 false"
            );
            assert!(!card.output_summary.is_empty());
        }
        _ => panic!("vms[3] 应为 TuiToolCard"),
    }
}
#[test]
fn test_flush_segment_rebuilds_cached_reasoning_status() {
    // [Fix think-end] 回归测试：思考→工具（无正文）场景下，提前 ToolStarted
    // 触发 flush 切段后，推理段必须以 Completed 形态构建（动画冻结）。
    // 旧 bug：sync_cache 的 `len() <= i` 守卫复用 flush 前缓存的 trailing
    // bubble（Running 形态），推理动画持续到 turn 结束才冻结。
    let mut ct = CurrentTurn::new();
    ct.append_reasoning("thinking...", None);
    // 流式期间：缓存已构建 trailing bubble（推理块 Running）
    let vm = ct.view_models();
    assert_eq!(vm.len(), 1);
    let TuiRenderUnit::TuiAssistantBubble(early) = &vm[0] else {
        panic!("expected assistant bubble");
    };
    let early_reasoning = early.reasoning.as_ref().expect("推理块应存在");
    assert_eq!(early_reasoning.status, EntryStatus::Running);

    // 提前 ToolStarted（工具块开始 = 推理结束）→ flush 切段 + 建卡
    ct.start_tool(ToolCardAccumulator::with_input(
        "tc-1".into(),
        "Edit".into(),
        String::new(),
        serde_json::Value::Null,
        None,
    ));
    assert_eq!(ct.tool_cards.len(), 1);

    // 段切走后：推理块必须转为 Completed（冻结），不再 Running
    let vm = ct.view_models();
    assert_eq!(vm.len(), 2, "段 + 工具卡片");
    let TuiRenderUnit::TuiAssistantBubble(bubble) = &vm[0] else {
        panic!("expected assistant bubble at index 0");
    };
    let reasoning = bubble.reasoning.as_ref().expect("推理块应存在");
    assert_eq!(
        reasoning.status,
        EntryStatus::Completed,
        "flush 切段后推理块必须 Completed（动画冻结）"
    );
    assert!(!reasoning.is_running, "冻结段不可处于 running");
    assert_eq!(bubble.text, "", "思考→工具（无正文）场景正文为空");
}
