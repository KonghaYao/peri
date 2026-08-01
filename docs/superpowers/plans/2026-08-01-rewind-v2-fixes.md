# Rewind v2 Code Review Fixes 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 code review 发现的 P0（`session/rewind` 执行链路恒静默空转，核心功能不可用）与全部 11 项 P1 问题。

**Architecture:** 双管齐下修 P0——TUI `build_execute_params` 显式传 `revert_files: true`，服务端 `RewindArgs` 加 `#[serde(default)]` 容错并在 dispatch 层预验证参数（不再静默）；P1 按服务端（历史回写/双重计数/阻塞 git/候选排序）→ TUI 链路（竞态/失败提示）→ TUI 交互（Esc 优先级/回填保留/条件关闭）→ i18n 的顺序推进，每 Task 独立可测可提交。

**Tech Stack:** Rust workspace（peri-acp / peri-tui）、serde_json、ratatui-kit atoms、Fluent i18n、tokio。

**审查基准：** commit `fa15f18f` + `6f5a1a82`；设计文档 `docs/superpowers/specs/2026-08-01-rewind-v2-design.md`。

**执行须知：**
- 每个 Task 改完跑对应 crate 测试：`cargo test -p peri-acp --lib` / `cargo test -p peri-tui --lib`
- 每 Task 末尾 commit（lefthook pre-commit 会跑 rustfmt/clippy/typos，`cargo clippy --workspace --all-targets -- -D warnings` 需在 Task 10 全量跑一次）
- `BaseMessage::ai_from_blocks` 会把 `ContentBlock::ToolUse` 同步到 `tool_calls` 字段（`peri-agent/src/messages/message.rs:137`）——这是双重计数问题的根源，修复时以 ToolUse **id** 去重，不依赖消息构造路径
- 测试环境 i18n 默认语言为 **en**（`peri-tui/src/i18n/mod.rs:59-66`），i18n 化后的文案断言必须用 `i18n::tr(...)` 动态期望，不能写死中文
- 既有测试 `command/rewind_test.rs::test_extract_file_changes_anthropic_write_format` / `_edit_format` 与 `dispatch/rewind_test.rs::test_preview_extracts_anthropic_tool_use` 的注释已声明"未来若修复去重，此断言需同步更新"——Task 3 一并更新

---

### Task 1: P0 服务端容错 —— RewindArgs.revert_files 默认值 + dispatch 参数预验证

**问题：** `command/rewind.rs:27-31` 的 `RewindArgs.revert_files` 必填无默认值；TUI 只发 `{sessionId, target_message_id}` → serde 解析失败 → `emit_rewind_parse_error`（仅 CompactError 事件）→ `dispatch/rewind.rs::rewind_execute` 仍返回 `{"status":"executed"}` 成功。截断/文件回退/持久化删除全部未执行。

**Files:**
- Modify: `peri-acp/src/session/command/rewind.rs:27-31`（RewindArgs 加默认值）
- Modify: `peri-acp/src/dispatch/rewind.rs:23-27`（dispatch::RewindArgs 加默认值）+ `rewind_execute` 开头（参数预验证返回 -32602）
- Test: `peri-acp/src/session/command/rewind_test.rs`（新增 execute 测试）
- Test: `peri-acp/src/dispatch/rewind_test.rs`（新增 args 默认值测试）

- [x] **Step 1: 写失败测试（command/rewind_test.rs）**

在 `peri-acp/src/session/command/rewind_test.rs` 文件末尾追加（`make_ctx` helper 与 `MockEventSink` 已存在于该文件）：

```rust
/// P0：参数缺 revert_files 时（TUI 旧版本/第三方客户端）应默认回退文件，
/// 而不是进入解析失败静默路径。
#[tokio::test]
async fn test_execute_missing_revert_files_defaults_true() {
    let sink: Arc<dyn crate::session::event_sink::EventSink> = Arc::new(MockEventSink::new());
    let history = vec![
        BaseMessage::human("第一轮问题"),
        BaseMessage::ai("第一轮回答"),
        BaseMessage::human("第二轮问题"),
    ];
    let target_id = history[0].id().as_uuid().to_string();
    let ctx = make_ctx(
        Arc::clone(&sink),
        history.clone(),
        std::env::temp_dir().to_string_lossy().to_string(),
        // 只传 target_message_id，缺 revert_files
        serde_json::json!({ "target_message_id": target_id }).to_string(),
    );

    let result = RewindCommand.execute(ctx).await;

    let events = sink.events();
    assert!(
        !events
            .iter()
            .any(|(_, json)| json.contains("参数解析失败")),
        "缺 revert_files 不应进入解析失败路径"
    );
    assert_eq!(result.messages.len(), 0, "回退到第一条 → 保留 0 条（截断已执行）");
}
```

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib rewind -- --nocapture`
Expected: `test_execute_missing_revert_files_defaults_true` FAIL——事件包含 "参数解析失败"，`result.messages.len() == 3`。

- [x] **Step 3: 实现默认值（command/rewind.rs）**

`peri-acp/src/session/command/rewind.rs:27-31` 改为：

```rust
#[derive(serde::Deserialize)]
struct RewindArgs {
    target_message_id: String,
    /// P0 修复：默认回退文件。TUI 早期版本/第三方客户端可能只传
    /// target_message_id——缺失时不得进入解析失败静默路径。
    #[serde(default = "default_true")]
    revert_files: bool,
}

fn default_true() -> bool {
    true
}
```

- [x] **Step 4: dispatch 层参数预验证（dispatch/rewind.rs）**

`peri-acp/src/dispatch/rewind.rs:23-27` 的 `RewindArgs` 改为：

```rust
/// 解析 `session/rewind` 系请求的公共参数。
#[derive(serde::Deserialize)]
pub struct RewindArgs {
    pub target_message_id: String,
    /// 与 command/rewind.rs::RewindArgs 保持同一默认语义（P0 双保险）。
    #[serde(default = "default_true")]
    pub revert_files: bool,
}

fn default_true() -> bool {
    true
}
```

`rewind_execute` 开头（`let session_id = ...` 之前）插入预验证——参数错误显式返回 -32602，TUI 收到错误响应后走 consumer 失败路径展示，不再静默：

```rust
    // P0 修复：参数预验证。RewindCommand 内部解析失败只发 CompactError 事件
    // 且本函数仍返回成功——这里前置解析，参数错误直接以 RPC 错误形式返回，
    // TUI 才能感知并展示失败。
    let _args: RewindArgs = serde_json::from_value(params.clone())
        .map_err(|e| AcpError::new(-32602, format!("rewind 参数解析失败: {e}")))?;
```

- [x] **Step 5: 写 dispatch 层默认值测试（dispatch/rewind_test.rs）**

`peri-acp/src/dispatch/rewind_test.rs` 文件末尾追加：

```rust
/// P0：dispatch 层参数缺 revert_files 时默认 true（与 command RewindArgs 双保险）。
#[test]
fn test_execute_args_missing_revert_files_defaults_true() {
    let args: super::RewindArgs = serde_json::from_value(serde_json::json!({
        "target_message_id": "msg-1",
    }))
    .unwrap();
    assert!(args.revert_files, "缺省应回退文件");
    assert_eq!(args.target_message_id, "msg-1");
}

/// P0：target_message_id 也缺失时返回参数错误（不再静默成功）。
#[test]
fn test_execute_args_missing_target_id_fails() {
    let result = serde_json::from_value::<super::RewindArgs>(serde_json::json!({}));
    assert!(result.is_err(), "缺 target_message_id 应解析失败");
}
```

- [x] **Step 6: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib rewind`
Expected: 全部 PASS（含 Task 1 Step 1 新测试 + 既有 rewind 测试）。

- [x] **Step 7: Commit**

```bash
git add peri-acp/src/session/command/rewind.rs peri-acp/src/session/command/rewind_test.rs peri-acp/src/dispatch/rewind.rs peri-acp/src/dispatch/rewind_test.rs
git commit -m "fix(acp): P0 rewind 参数缺 revert_files 时默认回退文件并显式报错"
```

---

### Task 2: P0 TUI 补 revert_files 字段

**问题：** `peri-tui/src/kit/rewind_action.rs:45-50` 的 `build_execute_params` 只发 `{sessionId, target_message_id}`。与服务端默认值构成双保险，且参数语义自文档化。

**Files:**
- Modify: `peri-tui/src/kit/rewind_action.rs:45-51`
- Test: `peri-tui/src/kit/rewind_action_test.rs`

- [x] **Step 1: 写失败测试（rewind_action_test.rs）**

`peri-tui/src/kit/rewind_action_test.rs` 文件末尾追加：

```rust
/// P0：执行参数必须携带 revert_files=true——缺失会导致服务端解析失败
/// （虽有服务端默认值兜底，TUI 侧仍应显式声明回退文件语义）。
#[test]
fn test_build_execute_params_includes_revert_files() {
    let params = build_execute_params("sid-1", "msg-1");
    assert_eq!(params["sessionId"], "sid-1");
    assert_eq!(params["target_message_id"], "msg-1");
    assert_eq!(params["revert_files"], true, "revert_files 缺失 = P0 静默空转");
}
```

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-tui --lib rewind_action`
Expected: FAIL——`params["revert_files"]` 为 Null。

- [x] **Step 3: 实现**

`peri-tui/src/kit/rewind_action.rs` 的 `build_execute_params` 改为：

```rust
/// 构造执行参数。
pub fn build_execute_params(sid: &str, target_message_id: &str) -> Value {
    json!({
        "sessionId": sid,
        "target_message_id": target_message_id,
        // P0 修复：RewindArgs.revert_files 必填（服务端已加 #[serde(default)] 双保险）。
        // 恒回退文件——与 Rewind v2 设计一致：预算确认后执行即包含文件复原。
        "revert_files": true,
    })
}
```

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-tui --lib rewind_action`
Expected: PASS。

- [x] **Step 5: Commit**

```bash
git add peri-tui/src/kit/rewind_action.rs peri-tui/src/kit/rewind_action_test.rs
git commit -m "fix(tui): P0 rewind 执行参数显式携带 revert_files=true"
```

---

### Task 3: P1 双重计数 —— extract_file_changes 按 ToolUse id 去重

**问题：** `BaseMessage::ai_from_blocks` 会把 `ContentBlock::ToolUse` 同步到 `tool_calls` 字段；`extract_file_changes`（`command/rewind.rs:159-195`）先遍历 `tool_calls`（OpenAI 格式）再遍历 `content.content_blocks()`（Anthropic 格式），对同一变更计 2 次——预算列表和文件恢复操作数都翻倍（Edit 恢复执行两次同向替换，第二次因 new_string 已不存在而误报警告）。

**Files:**
- Modify: `peri-acp/src/session/command/rewind.rs:159-195`
- Test: `peri-acp/src/session/command/rewind_test.rs`（2 个断言 2→1）
- Test: `peri-acp/src/dispatch/rewind_test.rs`（`test_preview_extracts_anthropic_tool_use` 断言 2→1）

- [x] **Step 1: 更新既有断言为期望值（red）**

`command/rewind_test.rs` 两处（`test_extract_file_changes_anthropic_write_format` 与 `_edit_format`）改为：

```rust
    // Assert: 修复后同一变更只计一次（按 ToolUse id 去重）
    assert_eq!(
        changes.len(),
        1,
        "ai_from_blocks 构造的消息在 tool_calls + content_blocks 双路径应去重"
    );
```

`dispatch/rewind_test.rs` 的 `test_preview_extracts_anthropic_tool_use` 改为：

```rust
    let changes = result["file_changes"].as_array().unwrap();
    // P1 修复：ai_from_blocks 双路径（tool_calls + content_blocks）按 id 去重，
    // 同一变更只计一次。
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["path"], "docs/readme.md");
```

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib rewind`
Expected: 3 个测试 FAIL——`changes.len() == 2`。

- [x] **Step 3: 写混合格式回归测试（command/rewind_test.rs 末尾追加）**

```rust
/// P1：tool_calls 与 content_blocks 双路径对同一 id 只计一次；
/// 不同 id 的调用仍全部计入。
#[test]
fn test_extract_file_changes_deduplicates_by_id() {
    // ai_from_blocks：ToolUse 同步到 tool_calls，同 id 双路径
    let msgs = vec![
        make_ai_write_block("a.txt", "hello"),
        make_ai_edit_call("b.txt", "old", "new"), // ai_with_tool_calls：仅 tool_calls 路径
    ];
    let changes = extract_file_changes(&msgs);
    assert_eq!(changes.len(), 2, "两个不同 id 的调用各计一次");
}
```

- [x] **Step 4: 实现去重**

`command/rewind.rs::extract_file_changes` 改为（关键：以 id 为去重键，`tool_calls` 与 blocks 中 id 相同的调用只计一次）：

```rust
/// 从被移除的消息中提取所有 Write/Edit 工具调用。
pub(crate) fn extract_file_changes(messages: &[BaseMessage]) -> Vec<FileChange> {
    let mut changes = Vec::new();
    // P1 修复：BaseMessage::ai_from_blocks 会把 ContentBlock::ToolUse 同步到
    // tool_calls 字段——同一调用在两条路径各出现一次。按 ToolUse id 去重，
    // 不依赖消息构造路径（OpenAI 反序列化只有 tool_calls、Anthropic 双路径）。
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in messages {
        if let BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } = msg
        {
            // OpenAI 格式: tool_calls 字段
            for tc in tool_calls {
                if (tc.name == "Write" || tc.name == "Edit")
                    && seen_ids.insert(tc.id.clone())
                    && let Some(change) = parse_tool_call(&tc.name, &tc.arguments)
                {
                    changes.push(change);
                }
            }

            // Anthropic 格式: ContentBlock::ToolUse（content_blocks 返回 owned Vec）
            for block in content.content_blocks() {
                if let ContentBlock::ToolUse {
                    ref name,
                    ref input,
                    ref id,
                    ..
                } = block
                {
                    if (name == "Write" || name == "Edit")
                        && seen_ids.insert(id.clone())
                        && let Some(change) = parse_tool_call(name, input)
                    {
                        changes.push(change);
                    }
                }
            }
        }
    }
    changes
}
```

- [x] **Step 5: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib rewind`
Expected: 全部 PASS（含既有 OpenAI 格式测试——`ai_with_tool_calls` 的消息 content 为 Text，blocks 路径无 ToolUse，单路径不受影响）。

- [x] **Step 6: Commit**

```bash
git add peri-acp/src/session/command/rewind.rs peri-acp/src/session/command/rewind_test.rs peri-acp/src/dispatch/rewind_test.rs
git commit -m "fix(acp): rewind 文件变更提取按工具调用 id 去重，消除双路径重复计数"
```

---

### Task 4: P1 阻塞 git 子进程移出 async 上下文

**问题：** `revert_files`（`command/rewind.rs:227-287`）在 `RewindCommand::execute` 的 async 上下文内同步执行 `std::process::Command::new("git").output()`——阻塞 tokio worker（tokio worker_threads 仅 4，见 CHANGELOG）。文件数量多时卡死整个事件泵。

**Files:**
- Modify: `peri-acp/src/session/command/rewind.rs:113-118`（execute Step 3）
- Test: 既有 execute 测试保持通过（行为不变，仅执行位置改变）

- [x] **Step 1: 实现 spawn_blocking**

`command/rewind.rs` 的 execute Step 3 改为：

```rust
        // Step 3: 提取文件变更并逆向恢复
        let mut revert_warnings = Vec::new();
        if args.revert_files {
            let changes = extract_file_changes(removed_messages);
            // P1 修复：revert_files 内含同步 git checkout 子进程，直接调用会
            // 阻塞 tokio worker（tokio worker_threads=4）——移出 async 上下文。
            let cwd_owned = ctx.cwd.clone();
            revert_warnings = tokio::task::spawn_blocking(move || {
                let mut warnings = Vec::new();
                revert_files(&changes, &cwd_owned, &mut warnings);
                warnings
            })
            .await
            .unwrap_or_else(|e| {
                warn!("rewind: spawn_blocking join 失败: {e}");
                Vec::new()
            });
        }
```

- [x] **Step 2: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib rewind`
Expected: 全部 PASS（execute 的 revert 场景测试覆盖行为不变）。

- [x] **Step 3: Commit**

```bash
git add peri-acp/src/session/command/rewind.rs
git commit -m "fix(acp): rewind 文件复原移入 spawn_blocking，避免 git 子进程阻塞 tokio worker"
```

---

### Task 5: P1 history 回写 —— rewind 后 SessionState.history 必须截断

**问题：** `requests.rs:1072-1077` 把 `s.history.clone()` 传给 `rewind_execute`，但响应只有 `{"status":"executed"}`——`SessionState.history` 保持旧值。再次双击 Esc 查询候选/预算用的是**已删除消息的旧 history**，第二次回退会 not found。

**Files:**
- Modify: `peri-acp/src/dispatch/rewind.rs:147`（响应携带 history）
- Modify: `peri-tui/src/acp_server/requests.rs:1084-1102`（写回 sessions）
- Test: `peri-tui/src/acp_server/requests_test.rs`（扩展 `test_rewind_routes_to_dispatch`）

- [x] **Step 1: 写失败测试（requests_test.rs）**

在 `test_rewind_routes_to_dispatch`（`requests_test.rs:387`）末尾追加断言（该测试 target 为 `history[0]`，截断后应保留 0 条）：

```rust
    assert_eq!(result.unwrap()["status"], "executed");

    // P1：rewind 后 SessionState.history 必须截断——它是后续候选/预算查询的
    // 数据源，不写回会导致第二次回退 not found。
    let s = sessions.get(&sid).unwrap();
    assert_eq!(s.history.len(), 0, "回退到第一条后 history 应为空");
```

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-tui --lib requests_test::test_rewind_routes_to_dispatch`
Expected: FAIL——`s.history.len() == 3`。

- [x] **Step 3: 实现 dispatch 响应携带 history**

`dispatch/rewind.rs::rewind_execute` 末尾（`Ok(json!({ "status": "executed" }))` 处）改为：

```rust
    let history = result.messages;
    Ok(json!({
        "status": "executed",
        // P1：携带截断后的 history，调用方（TUI 进程内 ACP server）回写
        // SessionState.history，保证后续候选/预算查询与事件一致。
        "history": history,
    }))
```

注意：`result.stop_reason` 的读取在 `let history = result.messages;` 之前完成（保持现有代码顺序：先 `if result.stop_reason == PromptStopReason::Cancelled` 判断，再移动 `result.messages`）。

- [x] **Step 4: 实现 requests.rs 回写**

`requests.rs` 的 `"session/rewind"` 分支末尾改为：

```rust
            let resp = dispatch::rewind_execute(
                params,
                history,
                &cwd,
                &peri_config_snapshot,
                &event_sink,
                None, // auxiliary_model：RewindCommand 不使用
                &peri_agent::agent::AgentCancellationToken::new(),
                Some(cfg.thread_store.clone()),
                Some(session_id.clone()),
                None, // bg_event_tx
                None, // bg_registry
                None,
                None,
                None,
                None, // frozen_*：RewindCommand 不使用
            )
            .await?;
            // P1：回写截断后的 history——SessionState.history 是后续
            // session/rewind-candidates 与 session/rewind-preview 的数据源，
            // 必须与 RewindCompleted 事件中的结果一致。
            if let Some(h) = resp.get("history").and_then(|v| v.as_array())
                && let Ok(msgs) = serde_json::from_value::<
                    Vec<peri_agent::messages::BaseMessage>,
                >(resp.get("history").unwrap().clone())
                && let Some(s) = sessions.get_mut(&session_id)
            {
                s.history = msgs;
            }
            Ok(resp)
```

- [x] **Step 5: 跑测试确认通过**

Run: `cargo test -p peri-tui --lib requests_test`
Expected: 全部 PASS（3 个 rewind 路由测试 + history 断言）。

- [x] **Step 6: Commit**

```bash
git add peri-acp/src/dispatch/rewind.rs peri-tui/src/acp_server/requests.rs peri-tui/src/acp_server/requests_test.rs
git commit -m "fix: rewind 执行后回写 SessionState.history，二次回退候选不再失效"
```

---

### Task 6: P1 候选排序语义 + 重建过滤 —— 最新在前、只含 user

**问题 1（排序矛盾）：** 服务端 `rewind_candidates` 按历史正序返回（最旧在前），弹窗 `msg_sel=0` 默认选中第一条 = **最旧的 user 消息 = 回退整个会话**；而 `rewind_popup.rs` 注释声称"最新一条 = 回退一步"。最常见的"撤销上一轮"操作需要用户手动翻到最后。

**问题 2（重建含 assistant）：** `system.rs::handle_rewind_completed` 重建 `REWIND_PREVIEW` 时 filter_map **所有** role（含 assistant），与 `rewind-candidates` 的 user-only 口径不一致——回退完成后弹窗候选混入 AI 消息。

**Files:**
- Modify: `peri-acp/src/dispatch/rewind_candidates.rs:14-28`（`.rev()` 最新在前）
- Modify: `peri-tui/src/kit/acp_events/system.rs:134-154`（user-only + 排除 `<system-reminder>` + 逆序）
- Test: `peri-acp/src/dispatch/rewind_candidates_test.rs`（新增顺序测试）
- Test: `peri-tui/src/kit/acp_events_test.rs`（`test_rewind_completed_replaces_committed` 重建断言更新）

- [x] **Step 1: 写失败测试（dispatch/rewind_candidates_test.rs 末尾追加）**

```rust
/// P1：候选按时间逆序返回——弹窗第一条 = 最近一次 user 消息 = 回退一步。
#[test]
fn test_candidates_newest_first() {
    let history = vec![
        BaseMessage::human("第一轮问题"),
        BaseMessage::ai("第一轮回答"),
        BaseMessage::human("第二轮问题"),
    ];
    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["preview"], "第二轮问题", "最新在前");
    assert_eq!(messages[1]["preview"], "第一轮问题");
}
```

（该测试文件顶部需有 `use peri_agent::messages::BaseMessage;` 与 `use super::rewind_candidates;`——按文件既有 import 补全。）

- [x] **Step 2: 更新 acp_events_test.rs 重建断言（red）**

`test_rewind_completed_replaces_committed` 中（`acp_events_test.rs:526-533`）的 preview 断言改为：

```rust
    // P1：重建只含 user 消息、最新在前（与 rewind-candidates 口径一致）
    let preview = crate::kit::atoms::REWIND_PREVIEW.state().read().clone();
    let preview = preview.expect("rewind 后 REWIND_PREVIEW 应被重建");
    assert_eq!(preview.messages.len(), 1, "preview 只含 user 候选（assistant 被过滤）");
    assert_eq!(preview.messages[0].id, "msg-1");
    assert_eq!(preview.messages[0].role, "user");
    assert_eq!(preview.messages[0].preview, "rewound user msg");
```

- [x] **Step 3: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib rewind_candidates && cargo test -p peri-tui --lib acp_events_test`
Expected: 2 个测试 FAIL。

- [x] **Step 4: 实现服务端逆序**

`dispatch/rewind_candidates.rs` 改为：

```rust
    let messages: Vec<Value> = session_history
        .iter()
        .rev() // P1：最新在前——弹窗第一条 = 最近一次 user 消息 = 回退一步
        .filter(|m| matches!(m, BaseMessage::Human { .. }))
        .filter(|m| !m.content().contains("<system-reminder>"))
        .map(|m| {
            json!({
                "id": m.id().as_uuid().to_string(),
                "preview": m.content().chars().take(200).collect::<String>(),
            })
        })
        .collect();
```

- [x] **Step 5: 实现重建过滤（system.rs）**

`system.rs:134-154` 的 preview 重建改为：

```rust
            // 同步重建 REWIND_PREVIEW：rewind 后消息列表已变，旧 preview 中的
            // 消息 id 已从服务端 history 删除——不重建会导致连续第二次回滚
            // 时 target 找不到（服务端 emit_rewind_not_found）。从回滚后的
            // 消息 JSON 直接提取 id/role/preview，保证候选列表与消息区一致。
            // P1：只保留 user 消息且排除系统注入（与 rewind-candidates 口径
            // 一致），并逆序（最新在前）——弹窗第一条 = 回退一步。
            let preview = RewindPreview {
                files: vec![],
                messages: msgs
                    .iter()
                    .rev()
                    .filter_map(|msg| {
                        let id = msg.get("id").and_then(|v| v.as_str())?.to_string();
                        let role = msg
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let text = super::extract_message_text(msg);
                        if role != "user" || text.contains("<system-reminder>") {
                            return None;
                        }
                        Some(RewindMessage {
                            id,
                            role,
                            preview: text.chars().take(200).collect(),
                        })
                    })
                    .collect(),
            };
            *crate::kit::atoms::REWIND_PREVIEW.state().write() = Some(preview);
```

- [x] **Step 6: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib rewind_candidates && cargo test -p peri-tui --lib acp_events_test`
Expected: 全部 PASS。

- [x] **Step 7: Commit**

```bash
git add peri-acp/src/dispatch/rewind_candidates.rs peri-acp/src/dispatch/rewind_candidates_test.rs peri-tui/src/kit/acp_events/system.rs peri-tui/src/kit/acp_events_test.rs
git commit -m "fix: rewind 候选最新在前且只含 user 消息，重建口径与服务端一致"
```

---

### Task 7: P1 弹窗交互 —— Esc 优先级 High、执行中态保回填、close_popup 条件化

**问题 1（Esc 死代码）：** `rewind_popup.rs` 事件 handler 用 `EventPriority::Normal`，与根层 `register_root_handlers`（`event_handlers.rs:149`，同样 Current+Normal）同优先级——根层先注册先消费 Esc：弹窗内 Budget 视图 Esc 返回候选的代码是死代码（实际行为：根层 Esc 直接关闭弹窗）。

**问题 2（执行中 Esc 丢回填）：** Executing 视图 Esc 清 `REWIND_TARGET_TEXT`——但 RPC 已发出、服务端已执行，`RewindCompleted` 到达后 `handle_rewind_completed` 读不到目标文本，输入框不回填。

**问题 3（无条件 close_popup）：** `handle_rewind_completed:182` 无条件 `close_popup()`——执行期间用户若打开了其他弹窗（HITL/OAuth 等事件），会被误关。

**Files:**
- Modify: `peri-tui/src/kit/popups/rewind_popup.rs`（Esc High + Executing 保回填）
- Modify: `peri-tui/src/kit/acp_events/system.rs:182`（条件化 close_popup）
- Test: 既有测试保持通过（键盘行为属组件级，无法单测；Task 10 手动清单验证）

- [x] **Step 1: Esc 优先级改 High**

`rewind_popup.rs` 的 `hooks.use_event_handler_with_options(...)` 调用中：

```rust
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High, // P1：根层 Esc 为 Normal（event_handlers.rs:149），
                             // 同优先级下根层先注册先消费——弹窗内 Esc 分支变死代码。
                             // 改 High 后弹窗自管理 Esc（关闭/返回候选）。
        EventOptions { hit_test: true },
        move |event| {
```

- [x] **Step 2: Executing 视图 Esc 保留目标文本**

`rewind_popup.rs` 的 `Some(ListNavAction::Cancel)` 分支改为：

```rust
                    Some(ListNavAction::Cancel) => match view {
                        RewindView::Executing => {
                            // P1：执行中态 Esc——RPC 已发出、服务端正在回退，
                            // RewindCompleted 必达。保留 REWIND_TARGET_TEXT 等待
                            // 回填；仅回候选视图。若 RPC 失败，rewind_consumer
                            // 失败路径会清目标文本并显示错误。
                            *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
                            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                            return EventResult::Consumed;
                        }
                        RewindView::Budget => {
                            // 预算确认前 Esc：尚未执行，目标文本不再需要
                            *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
                            *REWIND_TARGET_TEXT.state().write() = None;
                            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                            return EventResult::Consumed;
                        }
                        RewindView::Candidates => {
                            close_popup();
                            *REWIND_TARGET_TEXT.state().write() = None;
                            *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
                            *REWIND_QUERY_ERROR.state().write() = None;
                            return EventResult::Consumed;
                        }
                    },
```

- [x] **Step 3: close_popup 条件化（system.rs:182）**

`handle_rewind_completed` 末尾的 `crate::kit::popup_overlay::close_popup();` 改为：

```rust
    // P1：仅当 rewind 弹窗仍在显示时关闭——执行期间用户可能已 Esc 关闭弹窗
    // 或打开了其他弹窗（HITL/OAuth 事件），无条件 close 会误关。
    if *crate::kit::atoms::POPUP_KIND.state().read()
        == Some(crate::kit::atoms::PopupKind::Rewind)
    {
        crate::kit::popup_overlay::close_popup();
    }
```

- [x] **Step 4: 编译 + 既有测试**

Run: `cargo test -p peri-tui --lib rewind_popup && cargo test -p peri-tui --lib acp_events_test && cargo test -p peri-tui --lib popup_overlay_test`
Expected: 全部 PASS。

- [x] **Step 5: Commit**

```bash
git add peri-tui/src/kit/popups/rewind_popup.rs peri-tui/src/kit/acp_events/system.rs
git commit -m "fix(tui): rewind 弹窗 Esc 提权 High、执行中态保留回填文本、完成事件条件化关闭"
```

---

### Task 8: P1 失败提示 + 查询竞态防护

**问题 1（失败静默）：** `rewind_consumer` 失败路径只 `error!` 日志 + 清 atom——用户看到弹窗闪回候选视图，无任何错误说明。

**问题 2（异步竞态）：** `spawn_candidates_query` 是 fire-and-forget `tokio::spawn`——快速关闭再打开面板触发两次查询，慢的旧响应后到，覆盖新响应（展示陈旧候选）。

**Files:**
- Modify: `peri-tui/src/kit/atoms.rs`（新增 `REWIND_QUERY_GEN`）
- Modify: `peri-tui/src/kit/rewind_candidates.rs`（代次捕获 + 过期丢弃）
- Modify: `peri-tui/src/kit/rewind_action.rs`（失败路径提取 `on_action_failed` 纯函数，写 REWIND_QUERY_ERROR）
- Test: `peri-tui/src/kit/rewind_action_test.rs`（`on_action_failed` 断言）

- [x] **Step 1: 写失败测试（rewind_action_test.rs 末尾追加）**

```rust
/// P1：执行失败后弹窗回到候选视图并展示错误（不再静默）。
#[test]
fn test_on_action_failed_writes_query_error() {
    crate::kit::atoms::init_atoms();
    *REWIND_TARGET_TEXT.state().write() = Some("target".to_string());
    *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Executing;
    *REWIND_QUERY_ERROR.state().write() = None;

    on_action_failed("RPC timeout");

    assert!(REWIND_TARGET_TEXT.state().read().is_none(), "目标文本应清空");
    assert_eq!(
        *REWIND_BUDGET_STATE.state().read(),
        RewindBudgetState::Idle,
        "预算状态应复位（回候选视图）"
    );
    assert_eq!(
        REWIND_QUERY_ERROR.state().read().as_deref(),
        Some("RPC timeout"),
        "错误应写入 REWIND_QUERY_ERROR 供弹窗展示"
    );
}
```

（文件顶部需补 import：`REWIND_QUERY_ERROR`、`on_action_failed`——按既有 `use super::*` 或显式 import 补全。）

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-tui --lib rewind_action`
Expected: FAIL——`on_action_failed` 未定义。

- [x] **Step 3: 实现 atoms.rs 新增代次 atom**

`peri-tui/src/kit/atoms.rs` 的 rewind atom 区块追加：

```rust
/// 候选查询代次——`spawn_candidates_query` 每次递增并捕获；响应到达时与
/// 当前代次比对，不一致（已发起新查询）则丢弃，防止慢响应覆盖新数据
/// （P1 竞态防护）。
pub static REWIND_QUERY_GEN: AtomStatic<u64> = AtomStatic::new(|| 0);
```

- [x] **Step 4: 实现竞态防护（rewind_candidates.rs）**

`spawn_candidates_query` 改为：

```rust
    // P1 竞态防护：捕获代次，响应到达时比对——过期响应丢弃。
    let gen = crate::kit::atoms::REWIND_QUERY_GEN.get().wrapping_add(1);
    crate::kit::atoms::REWIND_QUERY_GEN.set(gen);

    tokio::spawn(async move {
        let resp = client
            .send_raw_request(
                "session/rewind-candidates",
                serde_json::json!({ "sessionId": sid }),
            )
            .await;
        match resp {
            Ok(value) => match parse_candidates_response(&value) {
                Ok(candidates) => {
                    if crate::kit::atoms::REWIND_QUERY_GEN.get() != gen {
                        return; // 已有新查询，丢弃过期响应
                    }
                    *REWIND_QUERY_ERROR.state().write() = None;
                    apply_candidates(&candidates);
                }
                Err(e) => {
                    if crate::kit::atoms::REWIND_QUERY_GEN.get() != gen {
                        return;
                    }
                    *REWIND_QUERY_ERROR.state().write() = Some(e);
                    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                }
            },
            Err(e) => {
                if crate::kit::atoms::REWIND_QUERY_GEN.get() != gen {
                    return;
                }
                *REWIND_QUERY_ERROR.state().write() = Some(format!("候选查询失败: {e}"));
                RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
            }
        }
    });
```

- [x] **Step 5: 实现 on_action_failed（rewind_action.rs）**

`rewind_action.rs` 新增纯函数（放 `spawn_rewind_consumer` 之前）：

```rust
/// 执行失败恢复（纯函数，测试友好）：清目标文本与预算状态，回候选视图
/// 并写 REWIND_QUERY_ERROR——弹窗 Candidates 视图据此展示错误，不再静默。
pub fn on_action_failed(error: &str) {
    *REWIND_TARGET_TEXT.state().write() = None;
    *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
    *REWIND_QUERY_ERROR.state().write() = Some(error.to_string());
    crate::kit::atoms::RENDER_HEARTBEAT.set(
        crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1),
    );
}
```

`spawn_rewind_consumer` 的失败分支改为：

```rust
                        Some(action) => {
                            if let Err(e) = handle_action(&acp_client, action).await {
                                error!(error = %e, "kit rewind_consumer: rewind RPC failed");
                                // P1：失败不再静默——回候选视图并展示错误
                                on_action_failed(&e.to_string());
                            }
                        }
```

import 补全：`use crate::kit::atoms::{..., REWIND_QUERY_ERROR, ...}`。

- [x] **Step 6: 跑测试确认通过**

Run: `cargo test -p peri-tui --lib rewind_action && cargo test -p peri-tui --lib rewind_candidates`
Expected: 全部 PASS。

- [x] **Step 7: Commit**

```bash
git add peri-tui/src/kit/atoms.rs peri-tui/src/kit/rewind_candidates.rs peri-tui/src/kit/rewind_action.rs peri-tui/src/kit/rewind_action_test.rs
git commit -m "fix(tui): rewind 失败展示错误、候选查询代次防护防旧响应覆盖"
```

---

### Task 9: P1 i18n —— 硬编码中文文案迁移到 Fluent

**问题：** `rewind_popup.rs`（10 处）、`rewind_candidates.rs`（3 处）、`rewind_action.rs`（2 处错误文案）硬编码中文，en locale 下仍显示中文。

**Files:**
- Modify: `peri-tui/locales/zh-CN/main.ftl`（新增 16 keys，追加在 `rewind-title` 区块后）
- Modify: `peri-tui/locales/en/main.ftl`（新增 16 keys）
- Modify: `peri-tui/src/kit/popups/rewind_popup.rs`
- Modify: `peri-tui/src/kit/rewind_candidates.rs`
- Modify: `peri-tui/src/kit/rewind_action.rs`
- Test: `peri-tui/src/kit/popups/rewind_popup_test.rs`（断言改 i18n 动态期望）

- [x] **Step 1: 新增 ftl keys（zh-CN/main.ftl）**

在 `rewind-title = 回滚` 区块（zh-CN main.ftl:562）之后追加：

```ftl
# ---- Rewind v2（弹窗与消费者文案）----
rewind-executing = 正在回退…
rewind-budget-title = 回退将撤销 { $count } 个文件改动：
rewind-budget-more = ... 还有 { $count } 项
rewind-budget-confirm-hint = Enter 确认回退 · Esc 返回候选
rewind-query-failed = 查询失败: { $error }
rewind-loading = 正在加载回退候选…
rewind-empty = 无可回退的消息。
rewind-empty-hint = 完成一轮对话后双击 Esc 即可回滚。
rewind-title-count = 回退到（{ $count }）
rewind-enter-hint = Enter 回退 · Esc 关闭
rewind-error-no-client = ACP client 未初始化，无法查询回退候选
rewind-error-no-session = 无活动会话，无法查询回退候选
rewind-error-query-failed = 候选查询失败: { $error }
rewind-error-budget-missing = rewind-preview 响应缺少 file_changes 数组
rewind-error-path-missing = 预算项缺少 path
rewind-execute-failed = 回退执行失败: { $error }
```

- [x] **Step 2: 新增 ftl keys（en/main.ftl）**

在 `rewind-title = Rewind` 区块（en main.ftl:563）之后追加：

```ftl
rewind-executing = Rewinding…
rewind-budget-title = Rewind will revert { $count } file change(s):
rewind-budget-more = ... and { $count } more
rewind-budget-confirm-hint = Enter to confirm · Esc back to candidates
rewind-query-failed = Query failed: { $error }
rewind-loading = Loading rewind candidates…
rewind-empty = Nothing to rewind.
rewind-empty-hint = Complete a turn, then double-press Esc to rewind.
rewind-title-count = Rewind to ({ $count })
rewind-enter-hint = Enter to rewind · Esc to close
rewind-error-no-client = ACP client not initialized, cannot query candidates
rewind-error-no-session = No active session, cannot query candidates
rewind-error-query-failed = Candidates query failed: { $error }
rewind-error-budget-missing = rewind-preview response missing file_changes array
rewind-error-path-missing = budget item missing path
rewind-execute-failed = Rewind failed: { $error }
```

- [x] **Step 3: 替换 rewind_popup.rs 硬编码**

`build_popup_lines` 内全部替换（`rewind-popup` 文件顶部已有 `use crate::i18n;`；新增 `use fluent_bundle::FluentValue;`）：

| 现状（rewind_popup.rs） | 替换为 |
|---|---|
| `Line::from("  正在回退…")` | `Line::from(format!("  {}", i18n::tr("rewind-executing")))` |
| `Line::from(format!("  回退将撤销 {} 个文件改动：", files.len()))` | `Line::from(format!("  {}", i18n::tr_args("rewind-budget-title", &[("count".into(), FluentValue::from(files.len() as i64))])))` |
| `Line::from(format!("    ... and {} more", files.len() - 8))` | `Line::from(format!("    {}", i18n::tr_args("rewind-budget-more", &[("count".into(), FluentValue::from((files.len() - 8) as i64))])))` |
| `Line::from("  Enter 确认回退 · Esc 返回候选")` | `Line::from(format!("  {}", i18n::tr("rewind-budget-confirm-hint")))` |
| `Line::from(format!("  查询失败: {err}"))` | `Line::from(format!("  {}", i18n::tr_args("rewind-query-failed", &[("error".into(), FluentValue::from(err.as_str()))])))` |
| `Line::from("  正在加载回退候选…")` | `Line::from(format!("  {}", i18n::tr("rewind-loading")))` |
| `Line::from("  无可回退的消息。")` | `Line::from(format!("  {}", i18n::tr("rewind-empty")))` |
| `Line::from("  完成一轮对话后双击 Esc 即可回滚。")` | `Line::from(format!("  {}", i18n::tr("rewind-empty-hint")))` |
| `Line::from(format!("  回退到（{}）", p.messages.len()))` | `Line::from(format!("  {}", i18n::tr_args("rewind-title-count", &[("count".into(), FluentValue::from(p.messages.len() as i64))])))` |
| `Line::from("  Enter 回退 · Esc 关闭")` | `Line::from(format!("  {}", i18n::tr("rewind-enter-hint")))` |

`[write]`/`[edit]` 标签保持英文（专业术语，两 locale 一致）。`i18n::tr("common-esc-close")` 不动。

- [x] **Step 4: 替换 rewind_candidates.rs 硬编码**

`rewind_candidates.rs` 顶部新增 `use crate::i18n;` 与 `use fluent_bundle::FluentValue;`，三处替换：

```rust
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        *REWIND_QUERY_ERROR.state().write() =
            Some(i18n::tr("rewind-error-no-client"));
        ...
    let sid = ...;
    if sid.is_empty() {
        *REWIND_QUERY_ERROR.state().write() = Some(i18n::tr("rewind-error-no-session"));
        ...
                *REWIND_QUERY_ERROR.state().write() = Some(i18n::tr_args(
                    "rewind-error-query-failed",
                    &[("error".into(), FluentValue::from(e.to_string()))],
                ));
```

- [x] **Step 5: 替换 rewind_action.rs 错误文案**

`rewind_action.rs` 的 `parse_budget_response` 两处（保留 String 错误类型，错误最终经 `on_action_failed` 展示）：

```rust
        .ok_or_else(|| i18n::tr("rewind-error-budget-missing"))?;
        ...
                    .ok_or_else(|| i18n::tr("rewind-error-path-missing"))?
```

顶部新增 `use crate::i18n;`。

- [x] **Step 6: 更新 popup 测试断言（动态期望）**

`rewind_popup_test.rs` 中所有硬编码中文断言改为 `i18n::tr(...)` 动态期望（测试环境默认 en）：

```rust
    // 文件顶部新增：use crate::i18n; use fluent_bundle::FluentValue;
    assert!(text.contains(&i18n::tr("rewind-title-count")), "候选视图标题");
    assert!(text.contains(&i18n::tr("rewind-empty")), "空候选提示");
    assert!(text.contains(&i18n::tr_args("rewind-query-failed", &[("error".into(), FluentValue::from("RPC timeout"))])), "错误文案透出");
    assert!(text.contains(&i18n::tr("rewind-loading")), "加载中提示");
    assert!(text.contains(&i18n::tr_args("rewind-budget-title", &[("count".into(), FluentValue::from(2i64))])), "预算数量");
    assert!(text.contains("[edit] src/main.rs") && text.contains("[write] new_file.txt"), "文件列表");
    assert!(text.contains(&i18n::tr("rewind-executing")), "执行中提示");
```

- [x] **Step 7: 跑测试确认通过**

Run: `cargo test -p peri-tui --lib rewind_popup && cargo test -p peri-tui --lib rewind_candidates && cargo test -p peri-tui --lib rewind_action`
Expected: 全部 PASS。

- [x] **Step 8: Commit**

```bash
git add peri-tui/locales/zh-CN/main.ftl peri-tui/locales/en/main.ftl peri-tui/src/kit/popups/rewind_popup.rs peri-tui/src/kit/popups/rewind_popup_test.rs peri-tui/src/kit/rewind_candidates.rs peri-tui/src/kit/rewind_action.rs
git commit -m "fix(tui): rewind 弹窗与消费者文案迁移到 Fluent i18n"
```

---

### Task 10: 全量验证 + 手动清单

**Files:** 无（验证任务）

- [x] **Step 1: workspace 测试**

Run: `cargo test --workspace --lib`
Expected: 全部 PASS（rewind 相关测试包含在 peri-acp / peri-tui）。

- [x] **Step 2: clippy 严格模式**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无警告。

- [x] **Step 3: 手动验证清单（`cargo run -p peri-tui -- -a`）**

1. 完成一轮对话（含一次 Edit 文件操作）后双击 Esc → 弹窗候选列表第一条为**最近** user 消息（逆序验证）
2. 候选列表 Enter → 预算视图显示文件改动列表（数量正确、不重复——双重计数修复验证）
3. 预算视图 Esc → 返回候选视图（Esc High 修复验证）
4. 预算视图 Enter → "正在回退…" → 执行完成后弹窗关闭、消息区截断、**输入框回填目标 user 消息文本**（P0 + 回填验证）
5. 再次双击 Esc → 候选列表已更新（history 回写 + 重建过滤验证——无 assistant 消息混入）
6. 快速双击 Esc 两次（打开→关闭→打开）→ 候选为最新数据（竞态防护验证）
7. 断开/错误 session 场景（可临时改 server 使 RPC 失败）→ 弹窗显示错误文案（失败提示验证）
8. `LANG` 或配置切 en → 弹窗文案为英文（i18n 验证）

- [x] **Step 4: 勾选实现计划**

用脚本勾选本计划全部 `- [ ]` → `- [x]`（或按执行记录逐一更新）。

- [x] **Step 5: Commit（如涉及计划文件更新）**

```bash
git add docs/superpowers/plans/2026-08-01-rewind-v2-fixes.md
git commit -m "docs: rewind v2 修复计划执行完毕"
```

---

## 自审记录（writing-plans Self-Review）

**Spec 覆盖（对照 review 发现）：**

| Review 问题 | Task |
|---|---|
| P0 执行链路静默空转（revert_files 缺失） | Task 1（服务端默认值+预验证）+ Task 2（TUI 显式传参） |
| P1 history 不回写 | Task 5 |
| P1 双重计数（ai_from_blocks 双路径） | Task 3 |
| P1 弹窗 Esc 死代码（EventPriority） | Task 7 Step 1 |
| P1 无条件 close_popup | Task 7 Step 3 |
| P1 执行中 Esc 丢回填 | Task 7 Step 2 |
| P1 锁内同步 git 子进程 | Task 4 |
| P1 异步竞态（迟到响应覆盖） | Task 8 Step 3-4 |
| P1 失败静默 | Task 8 Step 5 |
| P1 候选默认选中矛盾（排序） | Task 6 Step 4 |
| P1 REWIND_PREVIEW 重建含 assistant | Task 6 Step 5 |
| P1 i18n 硬编码中文 | Task 9 |

**Placeholder 扫描：** 全部步骤含完整代码或精确 diff 定位，无 TBD/TODO。

**类型一致性：** `on_action_failed(&str)`、`REWIND_QUERY_GEN: AtomStatic<u64>`（沿用 RENDER_HEARTBEAT 的 get/set API）、`dispatch::RewindArgs` 与 `command::RewindArgs` 的 `default_true()` 同名函数各自独立定义（模块私有，无冲突）；Task 5 中 `result.messages` 移动发生在 `result.stop_reason` 读取之后（borrow 顺序正确）；`i18n::tr_args` 的 `FluentValue` 借用参数在 `format!` 表达式内存活，无生命周期问题。

**风险注记：**
- Task 3 去重后 `dispatch/rewind_test.rs::test_preview_extracts_anthropic_tool_use` 断言从 2 改 1——该测试注释已预告此变更
- Task 6 逆序会影响 `requests_test.rs::test_rewind_candidates_routes_to_dispatch`（只断言 len==2，不受顺序影响，无需改动）
- Task 9 测试断言依赖 i18n 动态期望（测试环境默认 en），严禁写死中文
- 手动清单 Step 3.7 需要临时注入 RPC 失败，验证后还原代码
