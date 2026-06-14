# Architecture Weakness Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 顺序修复 23 个架构弱点（P1/P2/P3 三批），通过单一大 workflow 派发，每任务跑 cargo test 验证，每批后 commit，全自动模式。

**Architecture:** 主循环（Claude Code 主 agent）负责前置验证、派发 workflow、收尾处理；workflow 内部 23 个 task agent 顺序 for-await 执行 edit + cargo test，每批次结束由 commit agent 提交。失败任务自动跳过，文件残留 working tree 由主循环收尾处理。

**Tech Stack:** Rust workspace（9 crates）, tokio async, tracing, anyhow/thiserror, parking_lot, tempfile（测试）, cargo test, git。

**Spec Reference:** `docs/superpowers/specs/2026-06-14-architecture-weakness-fixes-design.md`
**Review Reference:** `docs/review/2026-06-14-architecture-review.md`

---

## File Structure

**会被修改的文件（按 crate 分组，来自 spec §2 的 23 个任务）：**

| Crate | 涉及文件（修改/创建） | 任务 ID |
|-------|---------------------|---------|
| `peri-acp` | `src/session/mod.rs`, `src/session/executor.rs`, `src/agent/builder.rs`, `src/session/command/rewind.rs`, `src/session/command/rewind_test.rs`(新), `src/session/command/compact.rs`, `src/session/executor_test.rs`(新) | p1-w1, p1-w2, p1-w3, p1-w5a, p1-w5b, p1-w6, p2-w10, p3-event-tx-mutex |
| `peri-tui` | `src/acp_server/mod.rs`, `src/acp_server/requests.rs`, `src/acp_server/prompt.rs`, `src/acp_stdio/freeze.rs`, `src/acp_server/prompt_exec.rs`, `src/app/agent_comm.rs`, `src/app/service_registry.rs`, `src/main.rs`, `src/app/panel_manager.rs`, `src/app/setup_wizard/mod.rs`, `src/app/setup_wizard/setup_wizard_test.rs`, `src/acp_client/`, `src/event/` | p1-w1, p1-w4, p2-w6, p2-w9, p2-w11, p3-setup-wizard, p3-acp-error, p3-dispatch-anyhow |
| `peri-middlewares` | `src/cron/mod.rs`, `src/cron/tools.rs`, `src/background.rs` | p2-w7 |
| `peri-agent` | `src/thread/types.rs`, `src/agent/executor/tool_dispatch.rs`, `src/agent/executor/tool_dispatch_test.rs`, `src/llm/mod.rs`, `src/llm/react_adapter.rs`, `src/agent/compact/config.rs` | p2-w12, p2-w13, p3-clean-deadlock, p3-capability-query, p3-extract-reasoning, p3-compact-defaults |
| `langfuse-client` | `src/`（locate `ingestion_events_to_otel`） | p3-extract-spanid |

**plan 本身的产出物：**
- 本文档：`docs/superpowers/plans/2026-06-14-architecture-weakness-fixes.md`
- workflow script（内联在 Task 2 中，调用 Workflow tool 时由系统持久化到 `.claude/workflow-runs/<run_id>/script.js`）
- 主循环执行后：3 个 commit（P1/P2/P3）+ 中文报告

---

## Pre-flight Tasks

### Task 0: 基线验证

**Files:**
- 验证：`docs/superpowers/specs/2026-06-14-architecture-weakness-fixes-design.md`（已存在）
- 验证：working tree 状态

- [ ] **Step 1: 验证 spec 存在且为最新版本**

Run:
```bash
ls -la docs/superpowers/specs/2026-06-14-architecture-weakness-fixes-design.md
git log -1 --format="%h %s" docs/superpowers/specs/2026-06-14-architecture-weakness-fixes-design.md
```
Expected: 文件存在，最近 commit 为 `81a5667f docs: add architecture review + weakness fixes spec`。

- [ ] **Step 2: 验证 working tree 仅含可解释改动**

Run:
```bash
git status --short
```
Expected: 无与本次任务冲突的改动。如果有未提交改动（特别是 23 个任务涉及的文件），需先 stash 或 commit。

- [ ] **Step 3: 记录 baseline commit hash**

Run:
```bash
git rev-parse HEAD
```
Expected: 输出 40 字符 SHA-1，记录为 `BASELINE=<sha>`。workflow 失败时可 `git reset --hard $BASELINE` 回滚（**仅在工作树干净时**，破坏性操作需用户授权）。

- [ ] **Step 4: 验证基线 cargo test 通过（关键 crate）**

Run:
```bash
cargo test -p peri-acp --lib 2>&1 | tail -5
cargo test -p peri-tui --lib 2>&1 | tail -5
cargo test -p peri-agent --lib 2>&1 | tail -5
cargo test -p peri-middlewares --lib 2>&1 | tail -5
```
Expected: 每个 crate 末尾 `test result: ok. N passed;`. 如果基线就有 fail，需先修复基线，否则 workflow 中所有 cargo test 都会失败。

- [ ] **Step 5: 提交 plan 文档**

Run:
```bash
git add docs/superpowers/plans/2026-06-14-architecture-weakness-fixes.md
git commit -m "$(cat <<'EOF'
docs: add implementation plan for architecture weakness fixes

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Execution Tasks

### Task 1: 准备 workflow script

**Files:**
- 创建临时文件（如需）：不需要，Workflow tool 接受 inline script，会自动持久化

- [ ] **Step 1: 检查 workflow 工具可用性**

直接调用 `Workflow` 工具（已是 core tool），无需 SearchExtraTools。

- [ ] **Step 2: 准备 script（无需写入磁盘，inline 传入 Workflow tool）**

Script 完整内容见 Task 2 的 Step 1（直接传入 Workflow tool 的 `script` 参数）。

---

### Task 2: 派发 workflow（核心执行）

**Files:**
- 间接修改：见 File Structure 表中所有 23 个任务涉及的文件
- 自动产出：`.claude/workflow-runs/<run_id>/script.js`、`journal.jsonl`、`state.json`

- [ ] **Step 1: 调用 Workflow tool，传入完整 script**

调用 `Workflow` 工具，参数：
- `description`: "顺序修复 23 个架构弱点"
- `title`: "Peri 架构弱点修复"
- `maxConcurrency`: 3（默认，顺序执行不需要更高）
- `script`: 完整 script（见下方代码块）

完整 workflow script：

```javascript
export const meta = {
  name: 'fix-arch-weaknesses',
  description: '顺序修复 23 个架构弱点（P1/P2/P3 三批，每批后 commit）',
  phases: [
    { title: 'P1', detail: '7 个 P1 任务顺序修复' },
    { title: 'P1-commit', detail: 'P1 批次 commit' },
    { title: 'P2', detail: '7 个 P2 任务顺序修复' },
    { title: 'P2-commit', detail: 'P2 批次 commit' },
    { title: 'P3', detail: '9 个 P3 任务顺序修复' },
    { title: 'P3-commit', detail: 'P3 批次 commit' },
  ],
}

const TASK_RESULT_SCHEMA = {
  type: 'object',
  properties: {
    taskId: { type: 'string' },
    name: { type: 'string' },
    filesChanged: { type: 'array', items: { type: 'string' }, description: 'git diff --name-only 输出' },
    testPassed: { type: 'boolean' },
    testOutput: { type: 'string', description: 'cargo test 输出（截断 500 字符）' },
    notes: { type: 'string' },
  },
  required: ['taskId', 'name', 'filesChanged', 'testPassed'],
}

const COMMIT_SCHEMA = {
  type: 'object',
  properties: {
    commitHash: { type: 'string' },
    filesAdded: { type: 'array', items: { type: 'string' } },
    leftover: { type: 'array', items: { type: 'string' }, description: 'git commit 后仍未提交的文件' },
    message: { type: 'string' },
    skipped: { type: 'boolean' },
  },
  required: ['skipped'],
}

// 23 个任务数据：详细 location/detail/pattern/refactor 见
// docs/superpowers/specs/2026-06-14-architecture-weakness-fixes-design.md §2
const P1_TASKS = [
  {
    id: 'p1-w1', name: 'TUI/stdio/ACP 会话管理三合一到 SessionManager',
    crate: 'peri-acp',
    location: 'peri-tui/src/acp_server/mod.rs:34-47; peri-acp/src/session/mod.rs:37-56; peri-tui/src/acp_server/requests.rs:81,255,371,424; peri-tui/src/acp_stdio/freeze.rs:16; peri-tui/src/acp_server/prompt.rs:133; peri-tui/src/acp_server/prompt_exec.rs:88',
    pattern: 'Shared Kernel / Delegation Pattern',
    detail: 'TUI 自维护 SessionState（7 字段）与 peri-acp AcpSession（12 字段）平行，frozen 构建重复 5 处。session_manager=None 导致 cascade cancel 子 agent 逻辑失效。commit 9d74169b 加 goal_state 字段未同步到 TUI/stdio。',
    refactor: '1) SessionManager::new_session_with_id/load/resume/fork 内部统一构建 frozen  2) TUI SessionState 和 stdio SessionInfo 合并为对 AcpSession 的引用  3) 替换 prompt.rs:133 / prompt_exec.rs:88 的 None 为实际 SessionManager  4) 补全 goal_state 字段到 TUI/stdio',
    extraTest: 'cargo test -p peri-acp && cargo test -p peri-tui',
  },
  {
    id: 'p1-w2', name: 'AcpAgentConfig 第一阶段分组（Facade + Builder）',
    crate: 'peri-acp',
    location: 'peri-acp/src/agent/builder.rs:52-103（struct）, :124-563（build_agent）',
    pattern: '渐进式子配置分组（Facade + Builder），不要一次性拆分全部 34 字段',
    detail: '34 字段 God Struct，build_agent 440 行，近 20 次 commit 高频热点。',
    refactor: '抽取三组共 12 字段（零跨依赖）：FrozenData { claude_md, claude_local_md, skill_summary, date }、CompactSettings { config, budget, model, event_tx }、ThreadPersistence { store, parent_thread_id, register_runtime, deregister_runtime }。保留 build_agent 单体函数（中间件顺序是 [TRAP] 守护契约）。不要做 Hook/MCP/LSP/Cron 分组（跨字段依赖）。',
    extraTest: 'cargo test -p peri-acp',
  },
  {
    id: 'p1-w3', name: 'execute_prompt Parameter Object 重构',
    crate: 'peri-acp',
    location: 'peri-acp/src/session/executor.rs:102-634',
    pattern: 'Parameter Object + Extract Method',
    detail: '533 行 + 30 参数，3 调用点各传 6 个占位符，零测试，#[allow(clippy::too_many_arguments)] 抑制警告。',
    refactor: '1) 引入 PromptExecutionContext { session, middleware, resources } 三组  2) 拆 4 个私有方法：intercept_immediate_command、spawn_event_pump、build_and_execute_agent、collect_result  3) execute_prompt 保留为编排器  4) 补 executor_test.rs 覆盖 intercept_immediate_command  5) 移除 #[allow(clippy::too_many_arguments)]',
    extraTest: 'cargo test -p peri-acp',
  },
  {
    id: 'p1-w4', name: 'Prediction 功能 Facade 到 peri-acp',
    crate: 'peri-tui',
    location: 'peri-tui/src/acp_server/mod.rs:205-218',
    pattern: 'Facade Pattern',
    detail: 'TUI 层直接构建 ReActAgent（BaseModelReactLLM::new + RetryableLLM::new + ReActAgent::new），违反 CLAUDE.md [TRAP]。',
    refactor: '1) 在 peri-acp/src/session/executor.rs 新增 pub async fn execute_prediction(provider, history, cwd) -> Result<String, _>  2) 将 mod.rs:205-218 的构造逻辑和 30s 超时（L228-238）移入  3) TUI 改为 let text = peri_acp::session::executor::execute_prediction(...).await  4) 保留 peri/prediction_ready 通知（L267-275）',
    extraTest: 'cargo test -p peri-tui',
  },
  {
    id: 'p1-w5a', name: 'rewind.rs 修复 UTF-8 边界 panic（紧急 bug）',
    crate: 'peri-acp',
    location: 'peri-acp/src/session/command/rewind.rs:252-258（revert_files Edit 分支）',
    pattern: 'Bug fix（char_indices 替代字节切片）',
    detail: 'content.find(new_string) 返回字节索引，&content[..idx] 若 new_string 跨 UTF-8 字符边界会 panic。违反 CLAUDE.md "字符串截断必须用字符级操作"。',
    refactor: '将 L252-258 的 &content[..byte_idx] 改用字符级操作。可选方案：1) 用 str::char_indices + byte position 安全切片（find 返回的 byte_idx 在 char boundary 上时是安全的，但跨多字节 char 时需检查） 2) 用 content.replacen(new_string, "", 1) 替换法反向取前缀。修复时保留函数签名不变。',
    extraTest: 'cargo test -p peri-acp',
  },
  {
    id: 'p1-w5b', name: 'rewind.rs 补 rewind_test.rs 测试',
    crate: 'peri-acp',
    location: 'peri-acp/src/session/command/rewind.rs（345 行零测试）',
    pattern: 'Strategy Pattern + Test Data Builder',
    detail: '最复杂的 slash command 完全未测试，含文件系统破坏性操作（std::fs::write/remove_file + git checkout）。',
    refactor: '1) 创建 rewind_test.rs，注册到 mod.rs 的 #[cfg(test)]  2) 用 tempfile::tempdir() 作为 cwd 测试 revert_files 的 Write/Edit 分支（含跨 UTF-8 边界场景，回归保护 w5a 的修复）  3) 构造未配对的 ToolUse/ToolResult 消息测试 validate_tool_pairing  4) 用 MockEventSink 测试 execute 的未找到目标、末尾截断、中间截断三种场景',
    extraTest: 'cargo test -p peri-acp',
  },
  {
    id: 'p1-w6', name: 'compact 命令核心路径 Contract Test',
    crate: 'peri-acp',
    location: 'peri-acp/src/session/command/compact.rs（已有 28 个内联测试，缺 Contract Test）',
    pattern: 'Contract Test',
    detail: '验证 [TRAP] 不变量"compact 后消息必须以 BaseMessage::human(summary + continuation) 开头"。',
    refactor: '1) 在 compact.rs 现有测试旁新增 contract test  2) 构造典型 compact 输入（含 System/Ai/Historical 消息）  3) 断言输出 [Human(摘要+续接指令), System(文件)..., System(Skills)...] 结构  4) 断言不出现孤立的 ToolUse 或 System 摘要',
    extraTest: 'cargo test -p peri-acp',
  },
]

const P2_TASKS = [
  {
    id: 'p2-w6', name: 'AgentComm 抽取 BgTaskState + LspDiagnostics',
    crate: 'peri-tui',
    location: 'peri-tui/src/app/agent_comm.rs:26-87',
    pattern: 'Value Object Composition，不要一次拆完 5 组',
    detail: '30 字段混合 6 个关注点，134 处访问点。',
    refactor: '1) 抽取 BgTaskState { pending_bg_continuation, agent_done_pending_bg, pre_done_bg_completions, pre_done_bg_results }，新增 reset_for_new_round() 方法替换散落重置语句  2) 抽取 LspDiagnostics { errors, warnings, files_with_errors }（总是一起重置）  3) 不要做其他 3 组拆分',
    extraTest: 'cargo test -p peri-tui',
  },
  {
    id: 'p2-w7', name: '中间件 thiserror 枚举（CronError/BackgroundRegistryError）',
    crate: 'peri-middlewares',
    location: 'peri-middlewares/src/cron/mod.rs:50; src/cron/tools.rs:50; src/background.rs:52,100',
    pattern: 'thiserror（参考已有 lsp/tool.rs:11-28 LspToolError 模式）',
    detail: '中间件内部 API 大面积 String 化，丢失结构化错误。',
    refactor: '1) 新增 CronError { InvalidExpression, TaskLimitReached, TaskNotFound, ... }  2) 新增 BackgroundRegistryError { ConcurrentLimit, TaskNotFound, ... }  3) 替换 String 错误类型。不要做 BaseTool 关联类型改造。',
    extraTest: 'cargo test -p peri-middlewares',
  },
  {
    id: 'p2-w9', name: '双 config 共享 Arc 统一（Single Source of Truth）',
    crate: 'peri-tui',
    location: 'peri-tui/src/app/service_registry.rs:63; src/acp_server/mod.rs:54; src/main.rs:709-711; 已损坏：command/session/lang.rs:47-50, app/config_panel.rs:88-98',
    pattern: 'Single Source of Truth（共享 Arc）',
    detail: '两个独立存储（Option<PeriConfig> vs Arc<RwLock<PeriConfig>>），写入前者无法传播到后者。',
    refactor: '1) ServiceRegistry.peri_config 改为 Arc<parking_lot::RwLock<PeriConfig>>  2) main.rs:709 克隆 Arc 而非新建  3) 消除 sync_acp_config() 和 panel_manager.rs:282 的手动写通  4) 修复 lang.rs:47-50 和 config_panel.rs:88-98 的损坏点  5) 保留 session/set_config_option 处理器（有重建 LlmProvider 副作用）',
    extraTest: 'cargo test -p peri-tui',
  },
  {
    id: 'p2-w10', name: 'FrozenSessionData Immutable Value Object',
    crate: 'peri-acp',
    location: 'peri-acp/src/session/executor.rs:62-84',
    pattern: 'Immutable Value Object',
    detail: '7 字段全 pub，仅 derive Clone，无类型保护。',
    refactor: '1) 字段改为 pub(crate)  2) String 字段改为 Arc<str>（clone 零成本）  3) 提供 accessor 方法  4) 封装 build() 唯一构造路径（吸收现有 build_frozen_session_data）  5) 不要把 FrozenSessionData 存入 AcpSession（创建第三存储点）',
    extraTest: 'cargo test -p peri-acp',
  },
  {
    id: 'p2-w11', name: 'PanelState macro dispatch',
    crate: 'peri-tui',
    location: 'peri-tui/src/app/panel_manager.rs:136-544',
    pattern: '声明式宏 dispatch',
    detail: '13 变体 × 13 方法 = ~169 个 match arm，纯样板。',
    refactor: '1) 定义 macro_rules! dispatch_panel  2) 替换 13 个 dispatch 方法为宏调用  3) 处理 dispatch_desired_height 的 Some(match state) 形态差异  4) 不要改为 Box<dyn PanelComponent>（失去 enum 穷举保证）',
    extraTest: 'cargo test -p peri-tui',
  },
  {
    id: 'p2-w12', name: 'ThreadMeta 强类型枚举（Making Illegal States Unrepresentable）',
    crate: 'peri-agent',
    location: 'peri-agent/src/thread/types.rs:31,39（定义在 peri-agent 而非 peri-acp 避免循环依赖）',
    pattern: 'Type State + 强类型枚举',
    detail: 'agent_status: String、cancel_policy: String 允许任意字符串。peri-acp/src/session/agent_runtime.rs:6-62 已有强类型枚举但持久化层未用，且 from_str 用 match { _ => default } 静默 fallback。',
    refactor: '1) 定义 enum AgentStatus { Active, Done, Cancelled, Error }  2) 定义 enum CancelPolicy { Cascade, Independent }  3) ThreadMeta 改用枚举类型  4) TryFrom<&str> 返回 Result（不静默 fallback）  5) 不要做 SQLite CHECK 约束（独立任务）',
    extraTest: 'cargo test -p peri-agent',
  },
  {
    id: 'p2-w13', name: 'tool_dispatch_test.rs Test Fixture Factory',
    crate: 'peri-agent',
    location: 'peri-agent/src/agent/executor/tool_dispatch_test.rs（EchoTool ×4 at L75-95,196-216,439-459,917-937; ThreeToolLLM ×3 at L97-122,169-194,412-437）',
    pattern: 'Test Fixture Factory（文件内共享）',
    detail: '~200 行 Mock 重复。',
    refactor: '1) 顶部定义 struct EchoTool { name_str: &\'static str } + 实现  2) 顶部定义 struct ThreeToolLLM { final_answer: &\'static str } + 实现  3) 提供 make_echo_tool(name) / make_three_tool_llm(answer) 工厂函数  4) 各测试函数改用工厂',
    extraTest: 'cargo test -p peri-agent',
  },
]

const P3_TASKS = [
  {
    id: 'p3-clean-deadlock', name: 'tool_dispatch.rs [DEADLOCK] 日志清理 + metrics',
    crate: 'peri-agent',
    location: 'peri-agent/src/agent/executor/tool_dispatch.rs:109,128,173,178,187,405,419（[DEADLOCK]）; :387（metrics duration_ms: ()）',
    pattern: '清理',
    detail: '7 处 [DEADLOCK] 调试残留；L387 duration_ms 序列化为 null。',
    refactor: '1) 删除 6 处 [DEADLOCK] debug!（L108-116,127-130,187-190,403-408,417-422）  2) L173/L178 的 warn! 去掉前缀保留  3) metrics duration_ms：在并发闭包内记录 Instant::now() 携带 duration，或移除字段',
    extraTest: 'cargo test -p peri-agent',
  },
  {
    id: 'p3-setup-wizard', name: 'setup_wizard_test.rs Constructor Injection',
    crate: 'peri-tui',
    location: 'peri-tui/src/app/setup_wizard/setup_wizard_test.rs:64-72; mod.rs:232',
    pattern: 'Constructor Injection',
    detail: '依赖 dirs_next::home_dir() 读真实 ~/.claude/settings.json，测试结果取决于运行环境。',
    refactor: '1) SetupWizardPanel 添加 home_dir_override: Option<PathBuf>  2) migrate_from_claude_code() 改用 override fallback  3) 重写测试用 TempDir 隔离',
    extraTest: 'cargo test -p peri-tui',
  },
  {
    id: 'p3-event-tx-mutex', name: 'event_tx 改 parking_lot::Mutex',
    crate: 'peri-acp',
    location: 'peri-acp/src/session/executor.rs:271（4 处 .lock().unwrap()）',
    pattern: 'parking_lot::Mutex 替换',
    detail: 'std::sync::Mutex 的 poisoning panic 风险。',
    refactor: '1) Arc<std::sync::Mutex<Option<UnboundedSender>>> → Arc<parking_lot::Mutex<...>>  2) 所有 .lock().unwrap() → .lock()  3) 不要改用 AtomicBool + Sender（TOCTOU）',
    extraTest: 'cargo test -p peri-acp',
  },
  {
    id: 'p3-capability-query', name: 'BaseModel supports_streaming Capability Query',
    crate: 'peri-agent',
    location: 'peri-agent/src/llm/mod.rs:35-41',
    pattern: 'Capability Query',
    detail: 'invoke_streaming 默认回退到 invoke()，调用方无法知道是否真正流式。',
    refactor: '1) trait 加 fn supports_streaming(&self) -> bool { false }  2) 默认 invoke_streaming fallback 加 tracing::debug! 降级日志  3) ChatAnthropic/ChatOpenAI override 返回 true。不要 typestate 重构（破坏 Box<dyn BaseModel>）',
    extraTest: 'cargo test -p peri-agent',
  },
  {
    id: 'p3-extract-reasoning', name: 'BaseModelReactLLM::generate_reasoning Extract Method',
    crate: 'peri-agent',
    location: 'peri-agent/src/llm/react_adapter.rs（locate generate_reasoning）',
    pattern: 'Extract Method',
    detail: 'Reasoning 块构建逻辑内联，可读性差。',
    refactor: '提取 Reasoning 块构建辅助函数',
    extraTest: 'cargo test -p peri-agent',
  },
  {
    id: 'p3-extract-spanid', name: 'ingestion_events_to_otel 提取 build_span_id',
    crate: 'langfuse-client',
    location: 'langfuse-client/src/（locate ingestion_events_to_otel）',
    pattern: 'Extract Method',
    detail: 'span_id 构建逻辑内联。',
    refactor: '提取 build_span_id 辅助函数',
    extraTest: 'cargo test -p langfuse-client',
  },
  {
    id: 'p3-compact-defaults', name: 'CompactConfig 默认值单一来源',
    crate: 'peri-agent',
    location: 'peri-agent/src/agent/compact/config.rs',
    pattern: 'SSoT',
    detail: '11 个 default_xxx serde 函数与 Default impl 重复定义。',
    refactor: '1) 保留 serde 函数  2) Default impl 调用这些函数（消除重复定义）  3) 或用 serde_with #[serde(default)] + Default 派生',
    extraTest: 'cargo test -p peri-agent',
  },
  {
    id: 'p3-acp-error', name: 'AcpTuiClient 保留 AcpError 类型',
    crate: 'peri-tui',
    location: 'peri-tui/src/acp_client/（locate AcpTuiClient error conversion）',
    pattern: '类型保留',
    detail: 'AcpTuiClient 把 AcpError 转 String，丢失类型信息。',
    refactor: 'AcpTuiClient 不再把 AcpError 转 String，保留类型信息',
    extraTest: 'cargo test -p peri-tui',
  },
  {
    id: 'p3-dispatch-anyhow', name: 'dispatch 层 String 错误改 anyhow::Result',
    crate: 'peri-tui',
    location: 'peri-tui/src/event/（dispatch 相关）',
    pattern: 'anyhow::Result',
    detail: 'dispatch 层 String 错误丢失上下文。',
    refactor: 'dispatch 层 String 错误 → anyhow::Result',
    extraTest: 'cargo test -p peri-tui',
  },
]

const COMMON_CONSTRAINTS = `强制约束（必须遵守）：
- 严格遵守 CLAUDE.md 所有 [TRAP]（特别是 18 个中间件顺序、deferred_error 模式、cleanup_prepended、字符串字符级操作）
- 测试与源码分离为同目录 _test.rs 文件（≥30 行必须分离）
- 字符串截断必须用字符级操作：s.chars().take(N).collect() 或 s.char_indices().nth(N)，禁止 &s[..N] 对 CJK（会 panic）
- 终端列宽用 unicode-width crate（CJK 占 2 列）
- 鼠标坐标转换：逐字符累加 unicode-width
- 日志用 tracing，禁止 println! / eprintln!
- 库用 thiserror，应用层用 anyhow::Result
- std::sync::RwLockReadGuard 不是 Send，async 中不能跨 .await 持有，用 parking_lot::RwLock
- 跨平台 spawn 必须通过 shell_command() wrapper
- 不修改与本任务无关的代码
- 不 git commit / push（由 commit agent 处理）
- 注释、断言消息用中文；命名 test_<被测对象>_<场景>
- Mock 命名 make_ 前缀（函数），Mock 前缀（结构体），不跨文件共享
- 测试隔离：禁止写入全局配置`

async function runTask(task, phaseName) {
  return agent(
    `任务 ${task.id}: ${task.name}\n\n` +
    `目标 crate：${task.crate}\n` +
    `位置：${task.location}\n` +
    `弱点描述：${task.detail}\n` +
    `建议模式：${task.pattern}\n` +
    `重构方向：${task.refactor}\n\n` +
    `执行步骤：\n` +
    `1. Read 相关文件确认现状（必须，不读不改）\n` +
    `2. 按重构方向修改代码\n` +
    `3. Bash: git diff --name-only（必须用此命令获取真实修改清单，不靠记忆）填入 result.filesChanged\n` +
    `4. Bash: cargo test -p ${task.crate}（timeout 10 分钟，使用 timeout: 600000）\n` +
    `5. 若 cargo test 失败，记录 testOutput 但不重试，返回 testPassed=false\n\n` +
    COMMON_CONSTRAINTS,
    { label: task.id, phase: phaseName, schema: TASK_RESULT_SCHEMA }
  )
}

async function runBatch(tasks, phaseName) {
  const results = []
  for (const task of tasks) {
    log(`[${phaseName}] 开始 ${task.id}: ${task.name}`)
    try {
      const result = await runTask(task, phaseName)
      results.push({ task, result })
      if (result?.testPassed) {
        log(`[${phaseName}] ✓ ${task.id} cargo test 通过`)
      } else {
        log(`[${phaseName}] ✗ ${task.id} cargo test 失败，跳过`)
      }
    } catch (e) {
      results.push({ task, result: null, error: String(e) })
      log(`[${phaseName}] ✗ ${task.id} agent 异常: ${String(e).slice(0, 100)}`)
    }
  }
  return results
}

async function commitBatch(results, phaseName) {
  const succeeded = results.filter(r => r.result?.testPassed)
  const failed = results.filter(r => !r.result?.testPassed)
  if (succeeded.length === 0) {
    log(`[${phaseName}-commit] 无 succeeded 任务，跳过 commit`)
    return { skipped: true, succeeded: [], failed }
  }
  const filesToCommit = [...new Set(succeeded.flatMap(r => r.result.filesChanged || []))]
  return agent(
    `提交 ${phaseName} 批次。\n\n` +
    `完成的任务（${succeeded.length}）：\n` +
    succeeded.map(r => `- ${r.task.id}: ${r.task.name} → files: ${(r.result.filesChanged || []).join(', ')}`).join('\n') +
    `\n\n跳过的任务（${failed.length}）：\n` +
    failed.map(r => `- ${r.task.id}: ${r.task.name}${r.result ? ` (cargo test 失败: ${(r.result.testOutput || '').slice(0, 100)})` : ` (agent 异常: ${(r.error || '').slice(0, 100)})`}`).join('\n') +
    `\n\n执行步骤：\n` +
    `1. 仅 git add 以下文件（不要 git add -A，禁止 -A 防止误添加失败任务残留）：\n   ${filesToCommit.join('\n   ')}\n` +
    `2. git commit（不要 push，不要 amend）\n` +
    `3. git status --short 审计：列出任何仍未提交的修改文件，记录到 result.leftover\n` +
    `4. commit message 格式（必须用 HEREDOC，避免 shell 转义问题）：\n` +
    `   ${phaseName}: <一句话总结，如"修复 5 个 P1 架构弱点">\n\n` +
    `   完成：\n` +
    succeeded.map(r => `   - ${r.task.id}: ${r.task.name}`).join('\n') + '\n\n' +
    (failed.length > 0 ? `   跳过（失败）：\n` + failed.map(r => `   - ${r.task.id}: ${r.task.name}`).join('\n') + '\n\n' : '') +
    `   Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>\n\n` +
    `5. 记录 commitHash（git rev-parse HEAD）、filesAdded（实际 add 的文件）、leftover（git status --short 输出）`,
    { label: `commit:${phaseName}`, phase: `${phaseName}-commit`, schema: COMMIT_SCHEMA }
  )
}

phase('P1')
const p1Results = await runBatch(P1_TASKS, 'P1')
const p1Commit = await commitBatch(p1Results, 'P1')

phase('P2')
const p2Results = await runBatch(P2_TASKS, 'P2')
const p2Commit = await commitBatch(p2Results, 'P2')

phase('P3')
const p3Results = await runBatch(P3_TASKS, 'P3')
const p3Commit = await commitBatch(p3Results, 'P3')

const allResults = [...p1Results, ...p2Results, ...p3Results]
const passedCount = allResults.filter(r => r.result?.testPassed).length
const failedCount = allResults.length - passedCount

return {
  P1: {
    tasks: p1Results.map(r => ({ id: r.task.id, name: r.task.name, passed: r.result?.testPassed || false, filesChanged: r.result?.filesChanged || [], testOutput: r.result?.testOutput?.slice(0, 300) })),
    commit: p1Commit,
  },
  P2: {
    tasks: p2Results.map(r => ({ id: r.task.id, name: r.task.name, passed: r.result?.testPassed || false, filesChanged: r.result?.filesChanged || [], testOutput: r.result?.testOutput?.slice(0, 300) })),
    commit: p2Commit,
  },
  P3: {
    tasks: p3Results.map(r => ({ id: r.task.id, name: r.task.name, passed: r.result?.testPassed || false, filesChanged: r.result?.filesChanged || [], testOutput: r.result?.testOutput?.slice(0, 300) })),
    commit: p3Commit,
  },
  summary: {
    totalTasks: allResults.length,
    passed: passedCount,
    skipped: failedCount,
    passRate: `${passedCount}/${allResults.length}`,
  },
}
```

- [ ] **Step 2: 等待 workflow 完成**

Workflow 在后台运行（`run_in_background` 默认 false——前台等待完成）。
- 预计耗时：~3-6 小时（23 个任务，每任务含 cargo test 5-10 分钟，顺序执行）
- 期间不要做其他改动到目标文件的修改
- 完成后会收到 `<task-notification>`

- [ ] **Step 3: 读取 workflow 返回值**

读取 workflow 完成后返回的 JSON（在 task notification 中），结构：
```json
{
  "P1": { "tasks": [...], "commit": { "commitHash": "...", "filesAdded": [...], "leftover": [...] } },
  "P2": { ... },
  "P3": { ... },
  "summary": { "totalTasks": 23, "passed": N, "skipped": M, "passRate": "N/23" }
}
```

---

### Task 3: 验证 workflow 结果

**Files:**
- 验证：3 个 commit 的内容
- 验证：working tree 状态

- [ ] **Step 1: 列出本次 workflow 新增的 commits**

Run:
```bash
git log $BASELINE..HEAD --oneline
```
Expected: 看到 0-3 个 commit（P1/P2/P3 批次）。如果某批次全部失败，对应 commit 不存在。

- [ ] **Step 2: 逐个 commit 验证 stat**

对每个 commit hash（从 workflow result 的 `commit.commitHash` 读取）：

Run:
```bash
git show <commitHash> --stat
```
Expected: 改动文件都在预期范围（task agent 报告的 filesChanged 内）。

- [ ] **Step 3: 验证 working tree 残留**

Run:
```bash
git status --short
```
Expected: 失败任务的文件残留（M/??）。如果没有失败任务，working tree 应为 clean。

- [ ] **Step 4: 对 [TRAP] 守护代码做 spot-check**

Run:
```bash
# 检查 18 个中间件顺序未被破坏
git diff $BASELINE..HEAD -- peri-acp/src/agent/builder.rs | grep -E "add_middleware" | head -20
# 检查 deferred_error 模式未被破坏
git diff $BASELINE..HEAD -- peri-agent/src/agent/executor/tool_dispatch.rs | grep -E "deferred_error|try_break"
```
Expected: 中间件 add 顺序与 baseline 一致；deferred_error/try_break 仍存在。如果异常，需要手动审查并可能 revert 受影响的 commit。

---

### Task 4: 生成中文报告

**Files:**
- 创建：`docs/review/2026-06-14-weakness-fixes-report.md`

- [ ] **Step 1: 写中文总结报告**

报告结构：
```markdown
# 架构弱点修复执行报告

**日期**: 2026-06-14
**Workflow run_id**: <从 task notification 读取>
**总耗时**: <从 journal.jsonl 计算或估算>

## 总览
- 总任务数：23
- 通过：N
- 跳过：M
- 通过率：N/23 (xx%)

## P1 阶段（7 个任务）
- Commit: <hash>
- 通过：N1 / 跳过：M1
- 详细：
  - [✓/✗] p1-w1: <name>
  - ...

## P2 阶段（7 个任务）
...

## P3 阶段（9 个任务）
...

## Working tree 残留
<git status --short 输出>

## 失败任务原因汇总
<每个失败任务的 testOutput（截断 200 字符）>

## 建议下一步
- 失败任务：单独 brainstorming 后修复
- 残留文件：git checkout . 清理 / 手动修复
- [TRAP] 验证：手动 spot-check 已通过/未通过
```

- [ ] **Step 2: 提交报告**

Run:
```bash
git add docs/review/2026-06-14-weakness-fixes-report.md
git commit -m "$(cat <<'EOF'
docs: add weakness fixes execution report

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

### Task 5: 处理失败任务和 working tree 残留

**仅当 workflow summary.skipped > 0 时执行此任务**

- [ ] **Step 1: 列出所有失败任务及其错误**

从 workflow result 提取所有 `passed: false` 的任务，列出 `id` / `name` / `testOutput`。

- [ ] **Step 2: 询问用户处理方式**

用 AskUserQuestion 询问每个失败任务：
- A: 单独重试（派发独立 agent 修复）
- B: 进入新一轮 brainstorming（如果失败原因是设计问题）
- C: 接受现状，跳过
- D: 回滚到 baseline（仅当大失败时）

- [ ] **Step 3: 询问 working tree 残留处理**

Run:
```bash
git status --short
```

用 AskUserQuestion 询问：
- A: `git checkout .` 清理所有残留（破坏性，需用户确认）
- B: 保留残留以便诊断
- C: 选择性 stash 某些文件

- [ ] **Step 4: 如用户选回滚，执行回滚（破坏性，必须用户明确授权）**

**仅在用户明确说"回滚到 baseline"时执行**：

Run:
```bash
# 验证用户授权（口头确认）
git log $BASELINE..HEAD --oneline  # 列出会丢失的 commits
# 等用户再次确认后：
git reset --hard $BASELINE
```

---

### Task 6: 全局回归测试

**仅当通过率 ≥ 80% 时执行**

- [ ] **Step 1: 跑全量 cargo test**

Run:
```bash
cargo test 2>&1 | tail -20
```
Expected: 全 crate 测试通过（或仅失败任务相关 crate 有已知 fail）。

- [ ] **Step 2: 跑 clippy（可选）**

Run:
```bash
cargo clippy --workspace 2>&1 | grep -E "^error" | head -20
```
Expected: 无新增 error（与 baseline 比）。

- [ ] **Step 3: 跑 cargo fmt 检查（可选）**

Run:
```bash
cargo fmt --all -- --check 2>&1 | head -20
```
Expected: 无输出（格式正确）。

---

## Self-Review

按 writing-plans skill 要求自审：

### 1. Spec coverage（每个 spec 要求都映射到 task）

| Spec §  | Plan Task |
|---------|-----------|
| §1.1 三阶段顺序 | Task 2 Step 1（workflow script 含 phase P1/P2/P3） |
| §1.2 单元任务流程（5 步） | Task 2 runTask()（5 步：Read/修改/git diff/cargo test/记录） |
| §1.3 错误处理矩阵 | Task 2 runBatch()（try-catch + 自动跳过） |
| §1.4 失败任务残留文件处理 | Task 2 commitBatch()（精准 git add + git status --short 审计）+ Task 3 Step 3 + Task 5 |
| §1.5 Commit 策略 | Task 2 commitBatch()（每批 commit，不 push） |
| §2 23 个任务清单 | Task 2 P1_TASKS/P2_TASKS/P3_TASKS 完整数据 |
| §3 Workflow 脚本骨架 | Task 2 Step 1 完整 script |
| §4 主循环收尾 | Task 3 + Task 4 + Task 5 |
| §5 风险与缓解（8 条）| COMMON_CONSTRAINTS（[TRAP] 守护）+ Task 3 Step 4（spot-check）+ Task 5（回滚） |
| §6 不在范围内 | plan 未引入额外任务，仅按 spec 23 个任务执行 |
| §7 验收标准 | Task 3 Step 1-4 + Task 6 |

✅ 所有 spec 要求都有对应 task。

### 2. Placeholder scan

搜索 plan 内容：
- 无 "TBD" / "TODO" / "implement later"
- Task 2 Step 1 包含完整 workflow script（500+ 行 JS），无 placeholder
- 每个任务的 detail/refactor 字段从 spec §2 完整复制，无缩写
- Task 4 报告模板用 `<...>` 标注需运行时填充的字段（如 `<commitHash>`），这是正确的模板语法而非 placeholder
- Task 5 的失败任务处理是条件性的（"仅当 skipped > 0 时执行"），不是 placeholder

✅ 无 placeholder。

### 3. Type consistency

- `result.filesChanged` 在 schema、runTask()、commitBatch()、report 中一致使用
- `result.testPassed` 在 schema、runTask()、runBatch()、commitBatch() 一致
- `commit.commitHash` / `commit.leftover` 在 schema、commitBatch()、Task 3 一致
- task 对象字段：`id` / `name` / `crate` / `location` / `pattern` / `detail` / `refactor` / `extraTest` 在 P1/P2/P3_TASKS 数组中一致

✅ 类型一致。

### 4. 任务依赖与执行顺序

- Task 0（pre-flight）→ Task 1（准备）→ Task 2（派发）→ Task 3（验证）→ Task 4（报告）→ Task 5（条件性失败处理）→ Task 6（条件性回归）
- Task 5 和 Task 6 都是条件性，主流程是 0→1→2→3→4

✅ 顺序合理。

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-14-architecture-weakness-fixes.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**

> 注：本 plan 的 Task 2 本质是调用 Workflow tool 派发单一 workflow（内部 23 个 task agents）。"subagent-driven" vs "inline" 在本场景下区别较小——无论哪种方式，Task 2 都会调用 Workflow tool 一次。区别在于：
> - Subagent-Driven：主循环（我）派发一个 subagent 调用 Workflow，结果回传
> - Inline Execution：主循环（我）直接调用 Workflow
>
> Inline 更直接（少一层 subagent 转手），推荐用 Inline。
