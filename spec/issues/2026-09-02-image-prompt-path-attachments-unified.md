# ACP prompt 参数双路径：顶层 attachments 在主执行路径丢失

**状态**：Fixed  
**优先级**：高  
**类型**：Bug — ACP host / prompt 归一化  
**创建日期**：2026-09-02  

## 问题描述

Peri 存在两条并行的「用户 prompt → `MessageContent`」解析路径，行为不一致，导致 **wire 级图片输入** 在常见主路径上静默失效。

### 路径 A — TUI（`@image` 文本，当前可用）

1. `Ctrl+V` 图片 → 写入 `~/.peri/images/`，输入框插入 `@image <path>`（`peri-tui/src/kit/input_area/image.rs`）。
2. `submit_consumer` 提交 `MessageContent::text(...)`，**无**顶层 `attachments` 字段（`peri-tui/src/kit/submit_consumer.rs:187`）。
3. `ImageMiddleware::before_agent` 将 `@image` 行替换为 `ContentBlock::Image`（`peri-middlewares/src/middleware/image/mod.rs`）。

该路径不依赖 `merge_attachments_into_content`，只要 `run_prompt` 把 `message.content` 原样传入 executor 即可。

### 路径 B — stdio / ACP 客户端（`prompt` 块或 `attachments`）

1. **`message.content` 或 `prompt: Vec<ContentBlock>`**：`prompt_blocks_to_content` 可解析 inline `type: image`（base64 + mimeType）。
2. **顶层 `attachments`**：规范允许客户端只把图片放在 `attachments`，而不嵌入 `message.content`。

`extract_prompt_params`（`peri-acp/src/dispatch/prompt.rs:79-107`）已实现 `merge_attachments_into_content`，并有单测 `test_extract_prompt_params_with_attachments`。

### 缺陷：主路径未走 `extract_prompt_params`

| 调用点 | 是否 merge `attachments` |
| --- | --- |
| `dispatch::handle_prompt` | ✅ `extract_and_validate_run_prompt_params`（与 `run_prompt` 同守卫） |
| `host/mod.rs` **idle_suspended 注入**（~883） | ✅ `extract_and_validate_run_prompt_params` |
| **`host/prompt.rs::run_prompt` 主路径**（~154-176） | ❌ 自行解析 `message` / `prompt`，**忽略** `attachments` |

TUI 与 Mpsc 正常 turn 均经 `dispatch_prompt_turn` → `run_prompt`（`host/mod.rs:961`）。因此：

- **TUI `@image`**：仍可用（路径 A）。
- **stdio / IDE 仅发 `attachments`**：主路径图片丢失；仅当 session 处于 `idle_suspended` 注入分支时偶然正确。
- **双路径维护成本**：`run_prompt` 与 `extract_prompt_params` 注释均声称「以 run_prompt 为基座」，事实相反，易再次分叉。

### 关联缺口（本 issue 不阻塞 P0，见下文）

- `build_initialize_response` 使用 `PromptCapabilities::new()`（`dispatch/init.rs:24`），未显式声明 image 能力；客户端可能不发 `attachments` / image blocks。
- `cli_print` 仅 `MessageContent::text`，无图片通道（`spec/issues/2026-07-25-stdio-missing-features.md` 问题 1 范畴）。
- `PENDING_ATTACHMENTS`（`peri-tui/src/kit/atoms.rs`）为 UI 占位，从未写入，与 wire `attachments` 无关。

## 根因

`run_prompt` 在 2026-08 批 3 合并 stdio `prompt` 字段时，在函数内复制了参数提取逻辑，**未**复用已含 `merge_attachments_into_content` 的 `extract_prompt_params`。

## 修复方案（最小改动）

### P0：统一主路径参数提取

**文件**：`peri-acp/src/host/prompt.rs`

1. 删除 `run_prompt` 内重复的 `session_id` + `content` 手工解析（约 154-176 行）。
2. 改为单次调用：
   ```rust
   let (session_id, content, _attachments) = crate::dispatch::prompt::extract_prompt_params(&params)?;
   ```
3. **保留既有 stdio 严格语义**（与当前 `run_prompt` 一致，避免无意行为变更）：
   - 当请求中 **既无** `message` **也无** `prompt` 键，且 merge 后的 `content` 无任何 block（纯空）时，仍返回 `-32602` / `"missing message"`。
   - 当存在 `prompt: []`（空数组）时，仍允许空文本 turn（与今日 `prompt_blocks_to_content` 行为一致）；判定条件应基于 **键是否存在**，而非 content 是否为空。
4. `bgResults` / `requestId` / continuation 等字段解析 **不动**。
5. `host/mod.rs` idle 注入分支已调用 `extract_prompt_params`：P0 完成后可保留（重复 extract 开销可忽略），或后续 refactor 在注入前预提取 content——**非 P0 要求**。

**不修改** TUI `submit_consumer`（TUI  intentionally 走 `@image` 文本 + ImageMiddleware）。

### P0：测试

**文件**：`peri-acp/src/dispatch/prompt_test.rs`（必要时 `host/prompt_test.rs`）

| 测试意图 | 说明 |
| --- | --- |
| 已有 | `test_extract_prompt_params_with_attachments` 等保持通过 |
| **新增** | `message` + 顶层 `attachments`：merge 后 block 数与 image block 类型 |
| **新增** | 仅 `prompt` 文本块 + `attachments` image（stdio 形态） |
| **新增** | 无 `message`、无 `prompt` 键、无 `attachments` → `extract` 成功但 **run_prompt 守卫**应 error（若守卫实现在 `run_prompt`，用小型 `pub(crate)` 纯函数 `prompt_body_guard(params, &content)` 便于单测，避免拉全 async executor） |

不要求本阶段新增 e2e；middleware 回归可选。

### P1（可选，本 workflow 可单独 PR）

**文件**：`peri-acp/src/dispatch/init.rs`

- 查阅 `agent_client_protocol_schema::v1::PromptCapabilities` builder（如 `.image(...)` / 等价字段），在 `build_initialize_response` 中声明 agent 支持 prompt 图片。
- 更新 `host/unify_wire_baseline_test.rs` 中 `test_initialize_response_wire_baseline` 对 `promptCapabilities` 的断言（若 wire 形态变化）。
- 与 TUI Mpsc 初始化路径核对：确保 stdio / TUI **同一** `build_initialize_response` 产物（已共享则只改一处）。

## 验收标准

实现完成后必须全部通过：

```bash
cargo test -p peri-acp --lib dispatch::prompt::tests
cargo test -p peri-acp --lib host::prompt::tests
cargo test -p peri-acp --lib host::unify_wire_baseline_test
cargo clippy -p peri-acp --all-targets -- -D warnings
```

若改动 `init.rs`（P1）：

```bash
cargo test -p peri-acp --lib test_initialize_response_wire_baseline
```

可选回归（未改 middleware 时可跳过）：

```bash
cargo test -p peri-middlewares --lib middleware::image
```

**手工冒烟（建议）**

1. TUI：粘贴图片 → `@image` 行 → agent 能描述图片（路径 A 无回归）。
2. stdio：对 active session 发送 `session/prompt`，仅顶层 `attachments` 含 base64 image block → turn 内 LLM 收到 image content（路径 B 修复验证）。

## 本 workflow 明确不做（P2 / 其他 issue）

| 项 | 理由 |
| --- | --- |
| TUI `PENDING_ATTACHMENTS` 写入与 wire 同步 | UI 占位，与 P0 attachments 丢失无关 |
| `cli_print` 图片 / 多模态 | 见 `2026-07-25-stdio-missing-features.md` |
| ImageMiddleware 行为变更 | 已满足 TUI `@image`；P0 只修 ACP 提取 |
| TUI 改为发 `attachments` 而非 `@image` | 违背既有「transport 轻量、路径在 middleware」设计（`spec/archive-issues/agent-core/2026-07-29-image-input-support.md`） |

## 涉及文件

| 文件 | 变更类型 |
| --- | --- |
| `peri-acp/src/host/prompt.rs` | P0 主修复 |
| `peri-acp/src/dispatch/prompt.rs` | 可选：抽出 `missing message` 守卫纯函数 |
| `peri-acp/src/dispatch/prompt_test.rs` | P0 新增用例 |
| `peri-acp/src/dispatch/init.rs` | P1 可选 |
| `peri-acp/src/host/unify_wire_baseline_test.rs` | P1 可选 |

## 执行步骤（实现者 checklist）

1. Read 本 issue + `peri-acp/src/host/prompt.rs` + `peri-acp/src/dispatch/prompt.rs`。
2. `run_prompt` 改用 `extract_prompt_params`；实现/单测 `missing message` 守卫。
3. 补充 prompt_test（attachments merge + 守卫）。
4. 跑验收 `cargo test` / `clippy` 列表。
5. 更新本 issue **状态变更记录**（Open → Fixed），填写验证命令与手工冒烟结果。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-02 | — | Open | agent | 审计结论落盘；仅计划，未改实现 |
| 2026-09-02 | Fixed | Fixed | composer-2.5 | Review follow-up：idle 注入 + `handle_prompt` 统一 `extract_and_validate_run_prompt_params`；`validate_run_prompt_body` 改用 `MessageContent::is_empty()`；+2 管道单测。验证：`cargo test -p peri-acp --lib dispatch::prompt::tests`、`host::prompt::tests` |
| 2026-09-02 | Open | Fixed | composer-2.5 | P0：`run_prompt` 统一 `extract_prompt_params` + `validate_run_prompt_body`；P1：`PromptCapabilities::image(true)`。验证：`cargo test -p peri-acp --lib dispatch::prompt::tests`、`host::prompt::tests`、`test_initialize_response_wire_baseline`；`cargo clippy -p peri-acp --all-targets -- -D warnings` 通过 |
