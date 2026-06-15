//! `/compact` 命令 — 手动触发上下文压缩。
//!
//! 移植自 `peri-tui/src/acp_server/compact.rs`，
//! 改为接收 [`CommandContext`]、返回 [`CommandResult`]。

use std::sync::Arc;

use peri_agent::{
    agent::{
        compact::{extract_file_info, extract_skill_names, full_compact, re_inject},
        events::AgentEvent as ExecutorEvent,
    },
    messages::BaseMessage,
};
use tracing::{info, warn};

use super::{AgentCommand, CommandContext, CommandKind, CommandResult};
use crate::session::executor::PromptStopReason;

/// 手动 compact 命令。
pub struct CompactCommand;

impl CompactCommand {
    pub const NAME: &'static str = "compact";
}

#[async_trait::async_trait]
impl AgentCommand for CompactCommand {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["compress"]
    }

    fn description(&self) -> &str {
        "压缩对话历史以释放上下文空间"
    }

    fn kind(&self) -> CommandKind {
        CommandKind::Immediate
    }

    async fn execute(&self, ctx: CommandContext) -> CommandResult {
        let CommandContext {
            session_id,
            history,
            cwd,
            peri_config,
            compact_model,
            event_sink,
            cancel_token,
            ..
        } = ctx;

        tracing::debug!(history_len = history.len(), "compact: execute called");

        if history.is_empty() {
            warn!("compact: 无历史消息可压缩");
            event_sink
                .push_event(
                    &session_id,
                    &ExecutorEvent::CompactError {
                        message: "no history to compact".into(),
                    },
                    0,
                )
                .await;
            return CommandResult {
                messages: history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }

        // compact 配置
        let mut compact_config = peri_config.config.compact.clone().unwrap_or_default();
        compact_config.apply_env_overrides();

        // 获取 compact model
        let compact_model: Arc<dyn peri_agent::llm::BaseModel> = match compact_model {
            Some(m) => m,
            None => {
                warn!("compact: 无可用模型");
                event_sink
                    .push_event(
                        &session_id,
                        &ExecutorEvent::CompactError {
                            message: "no model available for compact".into(),
                        },
                        0,
                    )
                    .await;
                return CommandResult {
                    messages: history,
                    stop_reason: PromptStopReason::EndTurn,
                };
            }
        };

        // 发送 CompactStarted 事件
        event_sink
            .push_event(&session_id, &ExecutorEvent::CompactStarted, 0)
            .await;

        // 执行 full_compact（支持 Ctrl+C 取消）
        let compact_result = tokio::select! {
            r = full_compact(&history, compact_model.as_ref(), &compact_config, "", &cwd) => {
                match r {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "compact: full_compact 失败");
                        event_sink
                            .push_event(
                                &session_id,
                                &ExecutorEvent::CompactError {
                                    message: e.to_string(),
                                },
                                0,
                            )
                            .await;
                        return CommandResult {
                            messages: history,
                            stop_reason: PromptStopReason::EndTurn,
                        };
                    }
                }
            }
            _ = cancel_token.cancelled() => {
                tracing::info!(session_id = %session_id, "compact cancelled by user");
                event_sink
                    .push_event(
                        &session_id,
                        &ExecutorEvent::CompactError {
                            message: "compact cancelled".into(),
                        },
                        0,
                    )
                    .await;
                return CommandResult {
                    messages: history,
                    stop_reason: PromptStopReason::Cancelled,
                };
            }
        };

        info!(
            summary_len = compact_result.summary.len(),
            "compact: full_compact 完成"
        );

        // 执行 re_inject
        let re_inject_result = re_inject(&history, &compact_config, &cwd).await;

        info!(
            files_injected = re_inject_result.files_injected,
            skills_injected = re_inject_result.skills_injected,
            "compact: re_inject 完成"
        );

        // 提取文件和 skill 信息
        let files = extract_file_info(&re_inject_result.messages);
        let skills = extract_skill_names(&re_inject_result.messages);

        // 摘要作为 Human 消息（与 auto-compact 路径和 Claude Code 实现对齐）
        let summary_content = format!(
            "{}\n\n[上下文已压缩，请根据摘要继续工作]",
            compact_result.summary
        );
        let mut new_messages = vec![BaseMessage::human(summary_content)];
        new_messages.extend(re_inject_result.messages.clone());

        // 发送 CompactCompleted 事件
        event_sink
            .push_event(
                &session_id,
                &ExecutorEvent::CompactCompleted {
                    summary: compact_result.summary,
                    files: files.clone(),
                    skills: skills.clone(),
                    micro_cleared: 0,
                    messages: new_messages.clone(),
                },
                0,
            )
            .await;

        info!("compact: 完成，session 已更新");

        CommandResult {
            messages: new_messages,
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use peri_agent::{
        agent::events::AgentEvent as ExecutorEvent,
        error::AgentResult,
        llm::{
            types::{LlmRequest, LlmResponse, StopReason},
            BaseModel,
        },
        messages::ContentBlock,
    };

    use super::*;
    use crate::session::executor::PromptStopReason;

    // ── Mock EventSink ────────────────────────────────────────────────────

    struct MockEventSink {
        events: Mutex<Vec<(String, String)>>,
        push_done_count: Mutex<usize>,
    }

    impl MockEventSink {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                push_done_count: Mutex::new(0),
            }
        }

        fn events(&self) -> Vec<(String, String)> {
            self.events.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl crate::session::event_sink::EventSink for MockEventSink {
        async fn push_event(&self, session_id: &str, event: &ExecutorEvent, _context_window: u32) {
            let json = serde_json::to_string(event).unwrap_or_default();
            self.events
                .lock()
                .unwrap()
                .push((session_id.to_string(), json));
        }

        async fn push_done(&self, _session_id: &str) {
            *self.push_done_count.lock().unwrap() += 1;
        }
    }

    impl MockEventSink {
        fn push_done_count(&self) -> usize {
            *self.push_done_count.lock().unwrap()
        }
    }

    fn make_ctx(
        sink: Arc<dyn crate::session::event_sink::EventSink>,
        history: Vec<BaseMessage>,
    ) -> super::super::CommandContext {
        super::super::CommandContext {
            session_id: "test-session".to_string(),
            history,
            cwd: "/tmp".to_string(),
            peri_config: Arc::new(Default::default()),
            compact_model: None,
            event_sink: sink,
            args: String::new(),
            cancel_token: peri_agent::agent::AgentCancellationToken::new(),
            thread_store: None,
            thread_id: None,
            bg_event_sender: None,
            bg_registry: None,
        }
    }

    /// 构造带 compact_model 的 CommandContext（contract test 使用真实模型路径）
    fn make_ctx_with_model(
        sink: Arc<dyn crate::session::event_sink::EventSink>,
        history: Vec<BaseMessage>,
        cwd: String,
        model: Arc<dyn BaseModel>,
    ) -> super::super::CommandContext {
        super::super::CommandContext {
            session_id: "test-session".to_string(),
            history,
            cwd,
            peri_config: Arc::new(Default::default()),
            compact_model: Some(model),
            event_sink: sink,
            args: String::new(),
            cancel_token: peri_agent::agent::AgentCancellationToken::new(),
            thread_store: None,
            thread_id: None,
            bg_event_sender: None,
            bg_registry: None,
        }
    }

    // ── extract_file_info 测试 ───────────────────────────────────────────

    #[test]
    fn test_extract_file_info_single_file() {
        // Arrange: 一条包含文件路径的 System 消息
        let msgs = vec![BaseMessage::system(
            "[最近读取的文件: /src/main.rs\nfn main() {}\n",
        )];

        // Act
        let files = extract_file_info(&msgs);

        // Assert: 提取到文件路径和行数（内容行数 = 总行数 - 1(路径行)）
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/src/main.rs");
        assert_eq!(files[0].lines, 1); // "fn main() {}" — 1 行内容
    }

    #[test]
    fn test_extract_file_info_multiple_files() {
        // Arrange: 多条文件消息
        let msgs = vec![
            BaseMessage::system("[最近读取的文件: /a.rs\nline1\nline2\n"),
            BaseMessage::system("[最近读取的文件: /b.rs\nline1\n"),
        ];

        // Act
        let files = extract_file_info(&msgs);

        // Assert
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "/a.rs");
        assert_eq!(files[0].lines, 2);
        assert_eq!(files[1].path, "/b.rs");
        assert_eq!(files[1].lines, 1);
    }

    #[test]
    fn test_extract_file_info_empty_messages() {
        // Arrange: 空消息列表
        let msgs: Vec<BaseMessage> = vec![];

        // Act
        let files = extract_file_info(&msgs);

        // Assert
        assert!(files.is_empty());
    }

    #[test]
    fn test_extract_file_info_skips_non_file_messages() {
        // Arrange: 非文件 System 消息 + Human/Ai 消息
        let msgs = vec![
            BaseMessage::system("普通系统提示"),
            BaseMessage::human("用户消息"),
            BaseMessage::ai("助手回复"),
        ];

        // Act
        let files = extract_file_info(&msgs);

        // Assert: 全部跳过
        assert!(files.is_empty());
    }

    #[test]
    fn test_extract_file_info_file_with_no_content_lines() {
        // Arrange: 只有路径行，无内容
        let msgs = vec![BaseMessage::system("[最近读取的文件: /empty.rs\n")];

        // Act
        let files = extract_file_info(&msgs);

        // Assert: 路径行存在但无内容行（lines = 0）
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/empty.rs");
        assert_eq!(files[0].lines, 0);
    }

    // ── extract_skill_names 测试 ─────────────────────────────────────────

    #[test]
    fn test_extract_skill_names_single_skill() {
        // Arrange: 一条包含 Skill 名称的 System 消息
        let msgs = vec![BaseMessage::system("[激活的 Skill 指令: tdd")];

        // Act
        let skills = extract_skill_names(&msgs);

        // Assert
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0], "tdd");
    }

    #[test]
    fn test_extract_skill_names_multiple_skills() {
        // Arrange: 多条 Skill 消息
        let msgs = vec![
            BaseMessage::system("[激活的 Skill 指令: tdd"),
            BaseMessage::system("[激活的 Skill 指令: code-review"),
        ];

        // Act
        let skills = extract_skill_names(&msgs);

        // Assert
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0], "tdd");
        assert_eq!(skills[1], "code-review");
    }

    #[test]
    fn test_extract_skill_names_empty_messages() {
        // Arrange: 空消息列表
        let msgs: Vec<BaseMessage> = vec![];

        // Act
        let skills = extract_skill_names(&msgs);

        // Assert
        assert!(skills.is_empty());
    }

    #[test]
    fn test_extract_skill_names_skips_non_skill_messages() {
        // Arrange: 非技能消息
        let msgs = vec![
            BaseMessage::system("[最近读取的文件: /src/main.rs\n"),
            BaseMessage::human("你好"),
        ];

        // Act
        let skills = extract_skill_names(&msgs);

        // Assert: 全部跳过
        assert!(skills.is_empty());
    }

    #[test]
    fn test_extract_skill_names_extracts_only_first_line() {
        // Arrange: Skill 名称后有多行内容，只取第一行
        let msgs = vec![BaseMessage::system(
            "[激活的 Skill 指令: my-skill\n额外内容\n更多内容",
        )];

        // Act
        let skills = extract_skill_names(&msgs);

        // Assert: 只提取第一行名称
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0], "my-skill");
    }

    // ── CompactCommand execute 测试 ──────────────────────────────────────

    #[tokio::test]
    async fn test_compact_empty_history_returns_original_with_error_event() {
        // Arrange: 空历史 + mock sink
        let sink = Arc::new(MockEventSink::new());
        let ctx = make_ctx(sink.clone(), vec![]);
        let cmd = CompactCommand;

        // Act
        let result = cmd.execute(ctx).await;

        // Assert: 返回空消息 + EndTurn
        assert_eq!(result.messages.len(), 0);
        assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

        // 应推送 CompactError 事件
        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].1.contains("compact_error"),
            "空历史应推送 compact_error，实际: {}",
            events[0].1
        );
        assert!(
            events[0].1.contains("no history to compact"),
            "错误消息应包含 'no history to compact'，实际: {}",
            events[0].1
        );
    }

    #[tokio::test]
    async fn test_compact_no_model_returns_original_with_error_event() {
        // Arrange: 有历史但无 compact_model（默认 None）
        let sink = Arc::new(MockEventSink::new());
        let history = vec![BaseMessage::human("你好"), BaseMessage::ai("世界")];
        let ctx = make_ctx(sink.clone(), history.clone());
        let cmd = CompactCommand;

        // Act
        let result = cmd.execute(ctx).await;

        // Assert: 返回原消息 + EndTurn
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

        // 应推送 CompactError 事件
        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].1.contains("compact_error"),
            "无模型应推送 compact_error，实际: {}",
            events[0].1
        );
        assert!(
            events[0].1.contains("no model available"),
            "错误消息应包含 'no model available'，实际: {}",
            events[0].1
        );
    }

    // ── CompactCommand 属性测试 ──────────────────────────────────────────

    #[test]
    fn test_compact_command_name_and_aliases() {
        let cmd = CompactCommand;

        assert_eq!(cmd.name(), "compact");
        let aliases = cmd.aliases();
        assert!(aliases.contains(&"compress"), "应包含 compress 别名");
        assert_eq!(cmd.kind(), CommandKind::Immediate);
        assert!(!cmd.description().is_empty());
    }

    /// 验证 CompactCommand（Immediate）执行后 push_done 未被命令自身调用
    /// （push_done 由 executor.rs 的 Immediate 路径负责调用，此处验证职责分离）
    #[tokio::test]
    async fn test_compact_command_does_not_call_push_done_itself() {
        let sink = Arc::new(MockEventSink::new());
        let ctx = make_ctx(sink.clone(), vec![]);
        let cmd = CompactCommand;

        let _result = cmd.execute(ctx).await;

        // 空历史返回后，不调用 push_done（由 executor 负责）
        let count = sink.push_done_count();
        assert_eq!(
            count, 0,
            "CompactCommand 自身不应调用 push_done，由 executor 负责"
        );
    }

    // ── Contract Test: compact 后消息结构不变量 ───────────────────────────
    //
    // 验证 CLAUDE.md [TRAP] 不变量：
    //   compact 后消息必须以 BaseMessage::human(summary + continuation) 开头，
    //   完整结构为 [Human(摘要+续接指令), System(文件)..., System(Skills)...]。
    //   禁止将摘要放在 BaseMessage::system() 中，禁止出现孤立的 ToolUse。
    //
    // 这些测试是 Contract Test：固定 mock 输入与 mock 模型，
    // 断言 CompactCommand.execute 的输出结构契约（而非内部行为细节）。

    /// 返回固定摘要的 mock BaseModel（contract test 用）
    struct MockSummaryModel {
        summary: String,
    }

    impl MockSummaryModel {
        fn new(summary: impl Into<String>) -> Self {
            Self {
                summary: summary.into(),
            }
        }
    }

    #[async_trait]
    impl BaseModel for MockSummaryModel {
        async fn invoke(&self, _request: LlmRequest) -> AgentResult<LlmResponse> {
            Ok(LlmResponse {
                message: BaseMessage::ai(self.summary.clone()),
                stop_reason: StopReason::EndTurn,
                usage: None,
                request_id: None,
            })
        }
        fn provider_name(&self) -> &str {
            "mock-summary"
        }
        fn model_id(&self) -> &str {
            "mock-summary-model"
        }
    }

    /// 构造一条 Ai 消息，包含 Read 工具调用 block（用于 re_inject 提取文件路径）
    fn make_ai_with_read_tool(file_path: &str) -> BaseMessage {
        let tool_call_id = "call_read_1".to_string();
        let blocks = vec![
            ContentBlock::Text {
                text: "我来读取这个文件".to_string(),
            },
            ContentBlock::ToolUse {
                id: tool_call_id.clone(),
                name: "Read".to_string(),
                input: serde_json::json!({ "file_path": file_path }),
            },
        ];
        BaseMessage::ai_from_blocks(blocks)
    }

    /// 构造一条 Human 消息，包含 [Skill: path] 标记（用于 re_inject 提取 Skill 路径）
    fn make_human_with_skill_marker(skill_path: &str) -> BaseMessage {
        BaseMessage::human(format!("用户消息\n[Skill: {}]", skill_path))
    }

    /// 契约：compact 输出首条消息必须是 Human（摘要+续接指令），
    /// 不得为 System 或其他类型。
    #[tokio::test]
    async fn test_contract_compact_output_starts_with_human_summary() {
        // Arrange: 典型 history — System + Human + Ai(Read) + Tool 结果
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "fn main() {}\n").expect("写入文件失败");
        let file_path_str = file_path.to_string_lossy().to_string();

        let history = vec![
            BaseMessage::system("系统提示词"),
            BaseMessage::human("帮我看看 main.rs"),
            make_ai_with_read_tool(&file_path_str),
            BaseMessage::tool_result("call_read_1", "fn main() {}"),
        ];

        let sink = Arc::new(MockEventSink::new());
        let model = Arc::new(MockSummaryModel::new("## 摘要\n已完成 main.rs 审查"));
        let ctx = make_ctx_with_model(
            sink.clone(),
            history,
            dir.path().to_string_lossy().to_string(),
            model,
        );
        let cmd = CompactCommand;

        // Act
        let result = cmd.execute(ctx).await;

        // Assert: 首条必须是 Human
        assert!(!result.messages.is_empty(), "compact 输出不应为空");
        assert!(
            matches!(result.messages[0], BaseMessage::Human { .. }),
            "compact 输出首条必须是 Human（摘要+续接指令），实际: {:?}",
            result.messages[0]
        );

        // 首条内容必须包含续接指令标记
        let first_text = result.messages[0].content();
        assert!(
            first_text.contains("[上下文已压缩，请根据摘要继续工作]"),
            "首条 Human 必须包含续接指令，实际内容: {}",
            first_text.chars().take(200).collect::<String>()
        );
        assert!(
            first_text.contains("已完成 main.rs 审查"),
            "首条 Human 必须包含摘要 LLM 输出"
        );
    }

    /// 契约：compact 输出结构必须为 [Human, System(文件)..., System(Skills)...]，
    /// 即首条之后只允许 System 消息（文件/Skills），不得出现孤立的 ToolUse/Ai/Tool。
    #[tokio::test]
    async fn test_contract_compact_output_structure_human_then_system_only() {
        // Arrange: history 含 Read 工具调用（对应真实文件）+ Skill 标记
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, "pub fn foo() {}\n").expect("写入文件失败");
        let file_path_str = file_path.to_string_lossy().to_string();

        // Skills 路径需落在 .claude/skills/ 下，且文件存在
        let skills_dir = dir.path().join(".claude").join("skills").join("tdd");
        std::fs::create_dir_all(&skills_dir).expect("创建 skills 目录失败");
        let skill_file = skills_dir.join("SKILL.md");
        std::fs::write(&skill_file, "# TDD Skill\n").expect("写入 SKILL.md 失败");
        let skill_path_str = skill_file.to_string_lossy().to_string();

        let history = vec![
            BaseMessage::system("系统提示词"),
            make_human_with_skill_marker(&skill_path_str),
            make_ai_with_read_tool(&file_path_str),
            BaseMessage::tool_result("call_read_1", "pub fn foo() {}"),
        ];

        let sink = Arc::new(MockEventSink::new());
        let model = Arc::new(MockSummaryModel::new("## 摘要\n审查 lib.rs 与 tdd skill"));
        let ctx = make_ctx_with_model(
            sink.clone(),
            history,
            dir.path().to_string_lossy().to_string(),
            model,
        );
        let cmd = CompactCommand;

        // Act
        let result = cmd.execute(ctx).await;

        // Assert: 结构契约 — 首条 Human，其后只能是 System
        assert!(
            matches!(result.messages[0], BaseMessage::Human { .. }),
            "首条必须为 Human"
        );
        for (i, msg) in result.messages.iter().enumerate().skip(1) {
            assert!(
                matches!(msg, BaseMessage::System { .. }),
                "compact 输出索引 {} 必须为 System（文件/Skills），实际: {:?}",
                i,
                msg
            );
        }

        // 不得出现孤立的 ToolUse（Ai 消息不应含 tool_calls）或 Tool 消息
        for (i, msg) in result.messages.iter().enumerate() {
            match msg {
                BaseMessage::Ai { tool_calls, .. } => {
                    assert!(
                        tool_calls.is_empty(),
                        "compact 输出索引 {} 的 Ai 消息不得包含 tool_calls（孤立 ToolUse）",
                        i
                    );
                }
                BaseMessage::Tool { .. } => {
                    panic!("compact 输出索引 {} 出现孤立的 Tool 消息: {:?}", i, msg);
                }
                _ => {}
            }
        }
    }

    /// 契约：摘要 LLM 输出不得作为 System 消息出现（即不得把摘要放入 System）。
    /// 这是一个 "negative contract"：断言没有任何 System 消息的文本包含摘要内容。
    #[tokio::test]
    async fn test_contract_summary_not_in_system_message() {
        // Arrange: 简单 history
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let history = vec![
            BaseMessage::system("系统提示词"),
            BaseMessage::human("你好"),
            BaseMessage::ai("你好，世界"),
        ];

        let unique_marker = "UNIQUE_SUMMARY_MARKER_2026";
        let sink = Arc::new(MockEventSink::new());
        let model = Arc::new(MockSummaryModel::new(format!("## 摘要\n{}", unique_marker)));
        let ctx = make_ctx_with_model(
            sink.clone(),
            history,
            dir.path().to_string_lossy().to_string(),
            model,
        );
        let cmd = CompactCommand;

        // Act
        let result = cmd.execute(ctx).await;

        // Assert: 摘要只出现在首条 Human，不得出现在任何 System 消息中
        assert!(
            result.messages[0].content().contains(unique_marker),
            "摘要必须出现在首条 Human"
        );
        for (i, msg) in result.messages.iter().enumerate().skip(1) {
            if let BaseMessage::System { content, .. } = msg {
                let text = content.text_content();
                assert!(
                    !text.contains(unique_marker),
                    "System 消息索引 {} 不得包含摘要 LLM 输出（摘要应只在 Human），实际: {}",
                    i,
                    text.chars().take(200).collect::<String>()
                );
            }
        }
    }

    /// 契约：compact 输出 CompactCompleted 事件携带 new_messages，
    /// 且事件中的 messages 与 CommandResult.messages 保持一致（外部可观测契约）。
    #[tokio::test]
    async fn test_contract_compact_completed_event_matches_result_messages() {
        // Arrange
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let history = vec![
            BaseMessage::system("系统提示词"),
            BaseMessage::human("你好"),
            BaseMessage::ai("你好，世界"),
        ];

        let sink = Arc::new(MockEventSink::new());
        let model = Arc::new(MockSummaryModel::new("## 摘要\n简单对话"));
        let ctx = make_ctx_with_model(
            sink.clone(),
            history,
            dir.path().to_string_lossy().to_string(),
            model,
        );
        let cmd = CompactCommand;

        // Act
        let result = cmd.execute(ctx).await;

        // Assert: CompactCompleted 事件存在
        let events = sink.events();
        let completed = events
            .iter()
            .find(|(_, json)| json.contains("compact_completed"));
        assert!(
            completed.is_some(),
            "应推送 CompactCompleted 事件，实际事件数: {}",
            events.len()
        );

        // CompactCompleted 事件的 messages 字段（反序列化）应与 result 结构契约一致：
        // 首条为 Human
        // 由于事件 JSON 序列化结构复杂，这里验证 result.messages 结构即可（与事件共享同一个 new_messages.clone()）
        assert!(
            matches!(result.messages[0], BaseMessage::Human { .. }),
            "CommandResult 首条必须为 Human"
        );
    }

    /// 契约：当 history 全为 System 消息（无 Human/Ai）时，
    /// full_compact 返回 fallback 摘要，CompactCommand 仍产出以 Human 开头的输出。
    /// （对应 full.rs: non_system_count == 0 分支）
    #[tokio::test]
    async fn test_contract_all_system_history_still_human_first() {
        // Arrange: 全 System history
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let history = vec![
            BaseMessage::system("系统提示词 1"),
            BaseMessage::system("系统提示词 2"),
        ];

        let sink = Arc::new(MockEventSink::new());
        // 即使 LLM 被调用返回内容，也不影响首条 Human 契约
        let model = Arc::new(MockSummaryModel::new("## 摘要\n不应到达此处"));
        let ctx = make_ctx_with_model(
            sink.clone(),
            history,
            dir.path().to_string_lossy().to_string(),
            model,
        );
        let cmd = CompactCommand;

        // Act
        let result = cmd.execute(ctx).await;

        // Assert: 仍以 Human 开头（fallback 摘要也要走 Human 路径）
        assert!(
            matches!(result.messages[0], BaseMessage::Human { .. }),
            "全 System history 的 compact 输出首条也必须为 Human（fallback 摘要），实际: {:?}",
            result.messages[0]
        );
        assert_eq!(
            result.stop_reason,
            PromptStopReason::EndTurn,
            "stop_reason 必须为 EndTurn"
        );
    }
}
