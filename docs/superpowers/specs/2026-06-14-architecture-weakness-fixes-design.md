# 架构弱点修复设计（Architecture Weakness Fixes）

**Spec ID**: 2026-06-14-architecture-weakness-fixes
**输入**: `docs/review/2026-06-14-architecture-review.md`（架构审查报告）
**目标**: 顺序修复报告中识别的 23 个 P1/P2/P3 弱点，分三批 commit
**派发方式**: 单一 workflow，顺序 for-await，每任务 edit + cargo test 验证
**日期**: 2026-06-14

---

## 一、执行策略

### 1.1 三阶段顺序

```
phase('P1')  →  7 个任务顺序修复（for-await）  →  commit agent  →  phase('P1-commit')
phase('P2')  →  7 个任务顺序修复               →  commit agent  →  phase('P2-commit')
phase('P3')  →  9 个任务顺序修复               →  commit agent  →  phase('P3-commit')
                                                                          ↓
                                              workflow 完成 → 主循环输出中文报告
```

**为何顺序而非并行**：
- 弱点 1（SessionManager 统一）影响 P2 的 w6（AgentComm）、w10（FrozenSessionData）
- 弱点 3（execute_prompt 重构）影响后续多个修改到 executor.rs 的任务
- 同 crate 的连续修改若并行会冲突；顺序 + 每任务 cargo test 才能定位失败

### 1.2 单元任务流程

每个任务由单个 agent 完成：

1. Read 相关文件确认现状（必读，不读不改）
2. 按报告重构草图修改代码
3. **必须运行 `git diff --name-only`** 获取实际修改文件清单，填入 `result.filesChanged`（不靠记忆）
4. `cargo test -p <crate>`（timeout 10 分钟，单 crate 范围）
5. 若失败：**不让 agent 自行修复**，记录 `testPassed=false` + testOutput 后返回
6. 若成功：记录 `testPassed=true`

### 1.3 错误处理

| 情况 | 处理 |
|---|---|
| cargo test 通过 | 加入本批次 commit 文件清单 |
| cargo test 失败 | 标记 skipped，继续下一任务；文件残留 working tree |
| agent 异常 | catch 异常，标记 error，继续下一任务 |
| 整批次全失败 | commit agent 跳过 commit（不报错），workflow 进下一批次 |

### 1.4 失败任务残留文件处理

采用**精准 git add**（方案 A）：
- commit agent 通过 `git diff --name-only` 不能信任（包含失败任务的改动）
- 改为：commit agent **仅 add succeeded 任务的 result.filesChanged** 中报告的文件
- 失败任务的文件保留在 working tree 作为诊断素材
- workflow 完成后由主循环（用户）决定 `git checkout .` 清理或保留

### 1.5 Commit 策略

- 每批次 1 个 commit agent，调用 `git add <specific files>` + `git commit`
- **不 push**
- commit message 模板：
  ```
  <P1|P2|P3>: <一句话总结>

  完成：
  - <task.id>: <task.name>

  跳过（失败）：
  - <task.id>: <task.name>

  Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
  ```

---

## 二、任务清单（23 个，按执行顺序）

### P1 阶段（7 个任务，预计 1-2 周）

#### `p1-w1`：TUI/stdio/ACP 会话管理三合一到 SessionManager

- **弱点**: 报告弱点 1（P0 级架构债务）
- **crate**: `peri-acp`, `peri-tui`
- **位置**:
  - `peri-tui/src/acp_server/mod.rs:34-47`（SessionState）
  - `peri-acp/src/session/mod.rs:37-56`（AcpSession）
  - `peri-tui/src/acp_server/requests.rs:81,255,371,424`（frozen 构建 4 处）
  - `peri-tui/src/acp_stdio/freeze.rs:16`（stdio frozen）
  - `peri-tui/src/acp_server/prompt.rs:133`、`prompt_exec.rs:88`（`None, // session_manager`）
- **detail**: TUI 自维护 `SessionState`（7 字段），与 peri-acp 的 `AcpSession`（12 字段）平行，frozen 构建重复 5 处。`session_manager=None` 导致 cascade cancel 子 agent 逻辑失效。
- **pattern**: Shared Kernel / Delegation Pattern
- **refactor**:
  1. `SessionManager::new_session_with_id/load/resume/fork` 内部统一构建 frozen
  2. TUI 的 `SessionState` 和 stdio 的 `SessionInfo` 合并为对 `AcpSession` 的引用
  3. 替换 `prompt.rs:133` / `prompt_exec.rs:88` 的 `None` 为实际 SessionManager
  4. 补全 `goal_state` 字段到 TUI/stdio
- **验证**: `cargo test -p peri-acp && cargo test -p peri-tui`
- **工作量**: L
- **注意**: L 级任务保持单个 agent，agent 需有耐心分步骤迁移。失败则跳过，不重试。

#### `p1-w2`：AcpAgentConfig 第一阶段分组（Facade + Builder）

- **弱点**: 报告弱点 2
- **crate**: `peri-acp`
- **位置**: `peri-acp/src/agent/builder.rs:52-103`（struct）、`:124-563`（build_agent）
- **detail**: 34 字段 God Struct，build_agent 440 行，近 20 次 commit 高频热点。
- **pattern**: 渐进式子配置分组（Facade + Builder），**不要一次性拆分全部 34 字段**
- **refactor**:
  1. 抽取 `FrozenData { claude_md, claude_local_md, skill_summary, date }`（4 字段，零跨依赖）
  2. 抽取 `CompactSettings { config, budget, model, event_tx }`（4 字段，零跨依赖）
  3. 抽取 `ThreadPersistence { store, parent_thread_id, register_runtime, deregister_runtime }`（4 字段，零跨依赖）
  4. 共 12 字段分组。保留 `build_agent` 单体函数（中间件顺序是 [TRAP] 守护契约）
- **不要做**: Hook/MCP/LSP/Cron 保持平铺（跨字段依赖）；不拆中间件顺序
- **验证**: `cargo test -p peri-acp`

#### `p1-w3`：execute_prompt Parameter Object 重构

- **弱点**: 报告弱点 3
- **crate**: `peri-acp`
- **位置**: `peri-acp/src/session/executor.rs:102-634`
- **detail**: 533 行 + 30 参数，3 调用点各传 6 个占位符，零测试。
- **pattern**: Parameter Object + Extract Method
- **refactor**:
  1. 引入 `PromptExecutionContext { session, middleware, resources }` 三组
  2. 拆 4 个私有方法：`intercept_immediate_command`、`spawn_event_pump`、`build_and_execute_agent`、`collect_result`
  3. `execute_prompt` 保留为编排器
  4. 补 `executor_test.rs` 覆盖 `intercept_immediate_command`
  5. 移除 `#[allow(clippy::too_many_arguments)]`
- **验证**: `cargo test -p peri-acp`

#### `p1-w4`：Prediction 功能 Facade 到 peri-acp

- **弱点**: 报告弱点 4
- **crate**: `peri-acp`, `peri-tui`
- **位置**: `peri-tui/src/acp_server/mod.rs:205-218`
- **detail**: TUI 层直接构建 ReActAgent，违反 CLAUDE.md [TRAP]。
- **pattern**: Facade Pattern
- **refactor**:
  1. 在 `peri-acp/src/session/executor.rs` 新增 `pub async fn execute_prediction(provider, history, cwd) -> Result<String, _>`
  2. 将 mod.rs:205-218 的构造逻辑和 30s 超时（L228-238）移入
  3. TUI 改为 `let text = peri_acp::session::executor::execute_prediction(...).await`
  4. 保留 `peri/prediction_ready` 通知（L267-275）
- **验证**: `cargo test -p peri-tui`

#### `p1-w5a`：rewind.rs 修复 UTF-8 边界 panic（紧急 bug）

- **弱点**: 报告弱点 5（拆分子任务 a，紧急 bug 修复）
- **crate**: `peri-acp`
- **位置**: `peri-acp/src/session/command/rewind.rs:252-258`（revert_files 的 Edit 分支）
- **detail**: `content.find(new_string)` 返回字节索引，`&content[..idx]` 若 `new_string` 跨 UTF-8 字符边界会 panic。违反 CLAUDE.md 编码规范"字符串截断必须用字符级操作"。
- **pattern**: Bug fix（使用 char_indices）
- **refactor**:
  ```rust
  // 替换 L252-258 的字节切片
  let char_idx = content.char_indices()
      .find(|(_, _)| /* 等价于 content.find 的匹配逻辑 */)
      .map(|(byte_idx, _)| byte_idx);
  // 或更简单：直接用 content.char_indices().nth(...) 但需要计算字符位置
  // 实际最稳的修复：使用 str::replace_indices 或重写为基于 char 的查找
  ```
  修复时请保留函数签名不变。
- **验证**: `cargo test -p peri-acp`（即使没有专门测试，也要确保现有测试不回归）

#### `p1-w5b`：rewind.rs 补 rewind_test.rs 测试

- **弱点**: 报告弱点 5（拆分子任务 b，补测试）
- **crate**: `peri-acp`
- **位置**: `peri-acp/src/session/command/rewind.rs`（345 行零测试）
- **detail**: 最复杂的 slash command 完全未测试，含文件系统破坏性操作。
- **pattern**: Strategy Pattern + Test Data Builder
- **refactor**:
  1. 创建 `rewind_test.rs`，注册到 `mod.rs` 的 `#[cfg(test)]`
  2. 用 `tempfile::tempdir()` 作为 cwd 测试 `revert_files` 的 Write/Edit 分支（含跨 UTF-8 边界场景，回归保护 w5a 的修复）
  3. 构造未配对的 ToolUse/ToolResult 消息测试 `validate_tool_pairing`
  4. 用 `MockEventSink` 测试 `execute` 的未找到目标、末尾截断、中间截断三种场景
- **验证**: `cargo test -p peri-acp`（新增测试必须通过）

#### `p1-w6`：compact 命令核心路径 Contract Test

- **弱点**: 报告 P1 路线图第 6 项
- **crate**: `peri-acp`
- **位置**: `peri-acp/src/session/command/compact.rs`（已有 28 个内联测试，缺 Contract Test）
- **detail**: 验证 [TRAP] 不变量"compact 后消息必须以 BaseMessage::human(summary + continuation) 开头"。
- **pattern**: Contract Test
- **refactor**:
  1. 在 `compact.rs` 现有测试旁新增 contract test
  2. 构造典型 compact 输入（含 System/Ai/Historical 消息）
  3. 断言输出 `[Human(摘要+续接指令), System(文件)..., System(Skills)...]` 结构
  4. 断言不出现孤立的 ToolUse 或 System 摘要
- **验证**: `cargo test -p peri-acp`

---

### P2 阶段（7 个任务，预计 2-4 周）

#### `p2-w6`：AgentComm 抽取 BgTaskState + LspDiagnostics

- **弱点**: 报告弱点 6
- **crate**: `peri-tui`
- **位置**: `peri-tui/src/app/agent_comm.rs:26-87`
- **detail**: 30 字段混合 6 个关注点，134 处访问点。
- **pattern**: Value Object Composition，**不要一次拆完 5 组**
- **refactor**:
  1. 抽取 `BgTaskState { pending_bg_continuation, agent_done_pending_bg, pre_done_bg_completions, pre_done_bg_results }`，新增 `reset_for_new_round()` 方法替换散落重置语句
  2. 抽取 `LspDiagnostics { errors, warnings, files_with_errors }`（总是一起重置）
  3. 不要做其他 3 组拆分（ACP通信/HITL/Token指标/消息历史）
- **验证**: `cargo test -p peri-tui`

#### `p2-w7`：中间件 thiserror 枚举

- **弱点**: 报告弱点 7
- **crate**: `peri-middlewares`
- **位置**:
  - `peri-middlewares/src/cron/mod.rs:50`（`Result<String, String>`）
  - `peri-middlewares/src/cron/tools.rs:50`（`Box<dyn Error>`）
  - `peri-middlewares/src/background.rs:52,100`（`Result<(), String>`）
- **detail**: 中间件内部 API 大面积 String 化，丢失结构化错误信息。
- **pattern**: thiserror（参考已有 `lsp/tool.rs:11-28` `LspToolError` 模式）
- **refactor**:
  1. 新增 `CronError { InvalidExpression, TaskLimitReached, TaskNotFound, ... }`
  2. 新增 `BackgroundRegistryError { ConcurrentLimit, TaskNotFound, ... }`
  3. 替换 String 错误类型
  4. **不要做** BaseTool 关联类型改造（P3 之外，且影响面太大）
- **验证**: `cargo test -p peri-middlewares`

#### `p2-w9`：双 config 共享 Arc 统一

- **弱点**: 报告建议 6（Single Source of Truth）
- **crate**: `peri-tui`
- **位置**:
  - `peri-tui/src/app/service_registry.rs:63`（`Option<PeriConfig>`）
  - `peri-tui/src/acp_server/mod.rs:54`（`Arc<RwLock<PeriConfig>>`）
  - `peri-tui/src/main.rs:709-711`（新建 Arc）
  - 已确认损坏：`command/session/lang.rs:47-50`、`app/config_panel.rs:88-98`
- **detail**: 两个独立存储，写入前者无法传播到后者。
- **pattern**: Single Source of Truth（共享 Arc）
- **refactor**:
  1. `ServiceRegistry.peri_config` 改为 `Arc<parking_lot::RwLock<PeriConfig>>`
  2. `main.rs:709` 克隆 Arc 而非新建
  3. 消除 `sync_acp_config()` 和 `panel_manager.rs:282` 的手动写通
  4. 修复 `lang.rs:47-50` 和 `config_panel.rs:88-98` 的损坏点
  5. 保留 `session/set_config_option` 处理器（有重建 LlmProvider 副作用）
- **验证**: `cargo test -p peri-tui`

#### `p2-w10`：FrozenSessionData Immutable Value Object

- **弱点**: 报告建议 5
- **crate**: `peri-acp`
- **位置**: `peri-acp/src/session/executor.rs:62-84`
- **detail**: 7 字段全 pub，仅 derive Clone，无类型保护。
- **pattern**: Immutable Value Object
- **refactor**:
  1. 字段改为 `pub(crate)`
  2. String 字段改为 `Arc<str>`（clone 零成本）
  3. 提供 accessor 方法
  4. 封装 `build()` 唯一构造路径（吸收现有 `build_frozen_session_data`）
  5. **不要做**: 把 FrozenSessionData 存入 AcpSession（会创建第三存储点）
- **验证**: `cargo test -p peri-acp`

#### `p2-w11`：PanelState macro dispatch

- **弱点**: 报告建议 7
- **crate**: `peri-tui`
- **位置**: `peri-tui/src/app/panel_manager.rs:136-544`
- **detail**: 13 变体 × 13 方法 = ~169 个 match arm，纯样板。
- **pattern**: 声明式宏 dispatch
- **refactor**:
  1. 定义 `macro_rules! dispatch_panel`
  2. 替换 13 个 dispatch 方法为宏调用
  3. 处理 `dispatch_desired_height` 的 `Some(match state)` 形态差异
  4. **不要做**: 改为 `Box<dyn PanelComponent>`（会失去 enum 穷举保证）
- **验证**: `cargo test -p peri-tui`

#### `p2-w12`：ThreadMeta 强类型枚举

- **弱点**: 报告建议 8（Making Illegal States Unrepresentable）
- **crate**: `peri-agent`（注意：定义在 peri-agent 而非 peri-acp，避免循环依赖）
- **位置**: `peri-agent/src/thread/types.rs:31,39`
- **detail**: `agent_status: String`、`cancel_policy: String` 允许任意字符串。
- **pattern**: Type State + 强类型枚举
- **refactor**:
  1. 定义 `enum AgentStatus { Active, Done, Cancelled, Error }`
  2. 定义 `enum CancelPolicy { Cascade, Independent }`
  3. ThreadMeta 改用枚举类型
  4. `TryFrom<&str>` 返回 Result（不静默 fallback）
  5. **不要做**: SQLite CHECK 约束（迁移代价大，独立任务）
- **验证**: `cargo test -p peri-agent`

#### `p2-w13`：tool_dispatch_test.rs Test Fixture Factory

- **弱点**: 报告建议 4
- **crate**: `peri-agent`
- **位置**: `peri-agent/src/agent/executor/tool_dispatch_test.rs`（L75-95、L196-216、L439-459、L917-937 EchoTool × 4；L97-122、L169-194、L412-437 ThreeToolLLM × 3）
- **detail**: ~200 行 Mock 重复。
- **pattern**: Test Fixture Factory（文件内共享）
- **refactor**:
  1. 顶部定义 `struct EchoTool { name_str: &'static str }` + 实现
  2. 顶部定义 `struct ThreeToolLLM { final_answer: &'static str }` + 实现
  3. 提供 `make_echo_tool(name)` / `make_three_tool_llm(answer)` 工厂函数
  4. 各测试函数改用工厂
- **验证**: `cargo test -p peri-agent`

---

### P3 阶段（9 个任务，预计 1-2 周）

#### `p3-clean-deadlock`：tool_dispatch.rs [DEADLOCK] 日志清理 + metrics

- **弱点**: 报告弱点 9
- **crate**: `peri-agent`
- **位置**: `peri-agent/src/agent/executor/tool_dispatch.rs:109,128,173,178,187,405,419`（[DEADLOCK]）、`:387`（metrics `duration_ms: ()`）
- **refactor**:
  1. 删除 6 处 `[DEADLOCK] debug!`（L108-116、L127-130、L187-190、L403-408、L417-422）
  2. L173/L178 的 `warn!` 去掉前缀保留
  3. metrics `duration_ms`：在并发闭包内记录 `Instant::now()` 携带 duration，或移除字段
- **验证**: `cargo test -p peri-agent`

#### `p3-setup-wizard`：setup_wizard_test.rs Constructor Injection

- **弱点**: 报告弱点 10
- **crate**: `peri-tui`
- **位置**: `peri-tui/src/app/setup_wizard/setup_wizard_test.rs:64-72`、`mod.rs:232`
- **refactor**:
  1. `SetupWizardPanel` 添加 `home_dir_override: Option<PathBuf>`
  2. `migrate_from_claude_code()` 改用 override fallback
  3. 重写测试用 TempDir 隔离
- **验证**: `cargo test -p peri-tui`

#### `p3-event-tx-mutex`：event_tx 改 parking_lot::Mutex

- **弱点**: 报告建议 9
- **crate**: `peri-acp`
- **位置**: `peri-acp/src/session/executor.rs:271`（4 处 `.lock().unwrap()`）
- **refactor**:
  1. `Arc<std::sync::Mutex<Option<UnboundedSender>>>` → `Arc<parking_lot::Mutex<...>>`
  2. 所有 `.lock().unwrap()` → `.lock()`
  3. **不要做**: 改用 AtomicBool + Sender（TOCTOU）
- **验证**: `cargo test -p peri-acp`

#### `p3-capability-query`：BaseModel supports_streaming Capability Query

- **弱点**: 报告建议 10
- **crate**: `peri-agent`
- **位置**: `peri-agent/src/llm/mod.rs:35-41`
- **refactor**:
  1. trait 加 `fn supports_streaming(&self) -> bool { false }`
  2. 默认 `invoke_streaming` fallback 加 `tracing::debug!` 降级日志
  3. ChatAnthropic/ChatOpenAI override 返回 true
- **验证**: `cargo test -p peri-agent`

#### `p3-extract-reasoning`：BaseModelReactLLM::generate_reasoning Extract Method

- **crate**: `peri-agent`
- **位置**: `peri-agent/src/llm/react_adapter.rs`（generate_reasoning 函数）
- **refactor**: 提取 Reasoning 块构建辅助函数
- **验证**: `cargo test -p peri-agent`

#### `p3-extract-spanid`：ingestion_events_to_otel 提取 build_span_id

- **crate**: `langfuse-client`
- **位置**: `langfuse-client/src/` (locate `ingestion_events_to_otel`)
- **refactor**: 提取 `build_span_id` 辅助函数
- **验证**: `cargo test -p langfuse-client`

#### `p3-compact-defaults`：CompactConfig 默认值单一来源

- **弱点**: 报告弱点 8
- **crate**: `peri-agent`
- **位置**: `peri-agent/src/agent/compact/config.rs`
- **refactor**:
  1. 保留 serde 函数（11 个 `default_xxx`）
  2. `Default` impl 调用这些函数（消除重复定义）
  3. 或用 `serde_with` `#[serde(default)]` + Default 派生
- **验证**: `cargo test -p peri-agent`

#### `p3-acp-error`：AcpTuiClient 保留 AcpError 类型

- **crate**: `peri-tui`
- **位置**: `peri-tui/src/acp_client/`（locate AcpTuiClient error conversion）
- **refactor**: AcpTuiClient 不再把 AcpError 转 String，保留类型信息
- **验证**: `cargo test -p peri-tui`

#### `p3-dispatch-anyhow`：dispatch 层 String 错误改 anyhow::Result

- **crate**: `peri-tui`
- **位置**: `peri-tui/src/event/`（dispatch 相关）
- **refactor**: dispatch 层 String 错误 → anyhow::Result
- **验证**: `cargo test -p peri-tui`

---

## 三、Workflow 脚本骨架

```js
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
    filesChanged: { type: 'array', items: { type: 'string' } },
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
    leftover: { type: 'array', items: { type: 'string' }, description: 'git commit 后仍未提交的文件（失败任务残留或 agent 漏报）' },
    message: { type: 'string' },
    skipped: { type: 'boolean', description: 'true 表示无 succeeded 任务，跳过 commit' },
  },
  required: ['skipped'],
}

const P1_TASKS = [ /* p1-w1 .. p1-w6 完整数据见 §2 */ ]
const P2_TASKS = [ /* p2-w6 .. p2-w13 */ ]
const P3_TASKS = [ /* p3-clean-deadlock .. p3-dispatch-anyhow */ ]

async function runTask(task, phaseName) {
  return agent(
    `任务 ${task.id}: ${task.name}\n` +
    `位置：${task.location}\n` +
    `目标 crate：${task.crate}\n` +
    `弱点描述：${task.detail}\n` +
    `建议模式：${task.pattern}\n` +
    `重构草图：${task.refactor}\n\n` +
    `步骤：\n` +
    `1. Read 相关文件确认现状\n` +
    `2. 按重构草图修改代码\n` +
    `3. Bash: git diff --name-only（必须用此命令获取真实修改清单，不靠记忆）填入 result.filesChanged\n` +
    `4. Bash: cargo test -p ${task.crate}（timeout 10 分钟，使用 timeout: 600000）\n` +
    `5. 若 cargo test 失败，记录 testOutput 但不重试，返回 testPassed=false\n\n` +
    `强制约束：\n` +
    `- 严格遵守 CLAUDE.md 所有 [TRAP]\n` +
    `- 测试 ≥30 行必须分离到 _test.rs 文件\n` +
    `- 字符串截断用 char_indices（CJK 安全）\n` +
    `- 终端列宽用 unicode-width\n` +
    `- 日志用 tracing，禁止 println! / eprintln!\n` +
    `- 不修改与本任务无关的代码\n` +
    `- 不 git commit / push（由 commit agent 处理）`,
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
      log(result?.testPassed
        ? `[${phaseName}] ${task.id} ✓ cargo test 通过`
        : `[${phaseName}] ${task.id} ✗ cargo test 失败，跳过`)
    } catch (e) {
      results.push({ task, result: null, error: String(e) })
      log(`[${phaseName}] ${task.id} ✗ agent 异常: ${String(e).slice(0, 100)}`)
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
    failed.map(r => `- ${r.task.id}: ${r.task.name}${r.result ? ` (cargo test 失败)` : ` (agent 异常)`}`).join('\n') +
    `\n\n步骤：\n` +
    `1. 仅 git add 以下文件（不要 git add -A）：\n   ${filesToCommit.join('\n   ')}\n` +
    `2. git commit（不要 push）\n` +
    `3. git status --short 审计：列出任何仍未提交的修改文件，记录到 result.leftover（这些是失败任务残留或 agent 漏报）\n` +
    `4. commit message 格式：\n` +
    `   ${phaseName}: <一句话总结>\n\n` +
    `   完成：\n` +
    succeeded.map(r => `   - ${r.task.id}: ${r.task.name}`).join('\n') + '\n\n' +
    (failed.length > 0 ? `   跳过（失败）：\n` + failed.map(r => `   - ${r.task.id}: ${r.task.name}`).join('\n') + '\n\n' : '') +
    `   Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>`,
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

return {
  P1: { tasks: p1Results.map(r => ({ id: r.task.id, name: r.task.name, passed: r.result?.testPassed || false })), commit: p1Commit },
  P2: { tasks: p2Results.map(r => ({ id: r.task.id, name: r.task.name, passed: r.result?.testPassed || false })), commit: p2Commit },
  P3: { tasks: p3Results.map(r => ({ id: r.task.id, name: r.task.name, passed: r.result?.testPassed || false })), commit: p3Commit },
  summary: {
    totalTasks: P1_TASKS.length + P2_TASKS.length + P3_TASKS.length,
    passed: [...p1Results, ...p2Results, ...p3Results].filter(r => r.result?.testPassed).length,
    skipped: [...p1Results, ...p2Results, ...p3Results].filter(r => !r.result?.testPassed).length,
  },
}
```

---

## 四、主循环收尾动作

Workflow 返回后，主循环（Claude Code 主 agent）执行：

1. 输出中文总结报告：每批次完成/跳过任务清单、commit hash
2. 检查 working tree：`git status` 列出未 commit 的失败任务残留文件
3. 提示用户决定：
   - 残留文件如何处理（`git checkout .` 清理 / 保留诊断 / 手动修复）
   - 是否 `git push`
4. 视情况触发后续：如果某 P1 关键任务失败（如 p1-w1），需要专门 brainstorming 后重做

---

## 五、风险与缓解

| 风险 | 缓解 |
|---|---|
| p1-w1（L 级）单 agent 装不下 | spec 已声明"保持单任务"，若失败 spec 自身不阻塞 workflow，由主循环后续单独处理 |
| 顺序执行耗时长 | 用户已接受"全自动"，期间可继续其他工作 |
| 失败任务文件污染 working tree | §1.4 精准 git add，commit agent 不 add 失败任务的文件 |
| cargo test 慢（peri-acp ~10min） | timeout 600000ms，agent 等待 |
| 多个任务改同一文件（如 executor.rs）| 顺序执行天然避免并行冲突 |
| agent 改错文件（误改 [TRAP] 守护代码）| prompt 明确"严格遵守 [TRAP]"；如 p1-w1 失败后 P2 任务基于错误代码 |
| agent 漏报 filesChanged | §1.2 强制 task agent 用 `git diff --name-only` 填字段；commit agent 在 add 后用 `git status --short` 审计，任何 succeeded 任务期间产生的未 add 文件都报警 |
| succeeded 任务文件被误归入失败任务残留 | commit agent 严格按 result.filesChanged 列表 add；非白名单文件保留 working tree |

---

## 六、不在本 spec 范围内

- 报告"不建议采纳的建议"（async_trait 替换 / ThreadId Newtype / AgentError Box / FrozenSessionData 入 AcpSession）—— 显式不做
- SQLite CHECK 约束迁移（p2-w12 派生项）—— 独立任务
- BaseTool 关联类型改造 —— 影响面太大，独立 spec
- 任何新增功能（仅修复，不加功能）

---

## 七、验收标准

- [ ] P1 阶段 ≥5/7 任务 cargo test 通过并 commit
- [ ] P2 阶段 ≥5/7 任务 cargo test 通过并 commit
- [ ] P3 阶段 ≥6/9 任务 cargo test 通过并 commit
- [ ] 每个 commit 仅包含 succeeded 任务的文件（由 commit agent 用 `git status --short` 审计验证）
- [ ] working tree 中残留文件清晰可识别（git status 输出）
- [ ] 最终报告中失败任务有明确错误信息便于后续修复
- [ ] 主循环 spot-check：对每个 commit 跑 `git show <hash> --stat`，确认改动文件在预期范围内；对 [TRAP] 守护的 18 个中间件顺序、tool_dispatch.rs 的 deferred_error 模式、cleanup_prepended 逻辑做差量核对
