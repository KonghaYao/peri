# P1：OpenAI-compatible 协议丢弃 MCP 工具图片

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：错误（核心多模态路径）。
**创建日期**：2026-11-11。
**来源**：用户报告 + explorer 只读确认。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `795ff49`）。

## 确认证据

- 复现路径：OpenAI-compatible 会话用 Read 读取 PNG，模型只收到文本
  `Image read: <path> (image/png).`，下一次请求的 `messages` 中没有图片字节。
- Read 已产出带 base64 的 raw 结果：
  `mcp-packages/workspace/src/filesystem/read.rs:84-91`（`RawToolResult::new`）。
- MCP 结果把 raw 提升为 image block：
  `mcp-packages/common/src/helpers.rs:61-62`（`raw_image_blocks`）。
- 工具桥把完整结果保存为 `ToolOutcome.raw`：
  `peri-middlewares/src/mcp/tool_bridge.rs:508-514`。
- 调度按当前协议决定是否保留图片：
  `peri-agent/src/agent/stages/tool_dispatch.rs:243-246`（`allow_tool_images` 仅
  `Anthropic | OpenAiResponses`）。
- OpenAI-compatible 分支只生成文本消息：
  `peri-agent/src/agent/stages/tool_dispatch/execution.rs:204-227`
  （`settled_tool_message` 在 `allow_tool_images=false` 时清空 media）。
- 落库后 raw 被置为 `Unreported`，图片不可恢复：
  `peri-agent/src/agent/stages/tool_dispatch.rs:250-253`。
- 请求层 tool 角色没有图片投影：
  `peri-model/src/openai_compatible/request.rs:158-162`（旧实现直接
  `content_to_openai(&result.content)`，而 Chat Completions 的 tool 角色不接受
  image part）。
- 结论：P1 成立。图片在 canonical 写入前被协议白名单丢弃，请求层即使想投影也
  拿不到 canonical 图片。

## 根因链

1. Read 读取图片文件，返回带 `text + image(base64)` 的 `RawToolResult`
   （观察：read.rs）。
2. MCP 公共层把 raw 中的 image 还原为 `ContentBlock::Image`
   （观察：helpers.rs）。
3. 工具桥把 MCP typed result 序列化进 `ToolOutcome.raw`
   （观察：tool_bridge.rs）。
4. 工具调度按 `ProviderProtocol` 白名单决定 canonical 是否保留图片；OpenAI
   Chat Completions 不在白名单（观察：tool_dispatch.rs）。
5. `settled_tool_message` 只写入文本投影，图片块从未进入消息
   （观察：execution.rs）。
6. 工具交换原子落库后 `outcome = Unreported`，raw 副本释放；后续请求投影无法
   再取回图片（观察：tool_dispatch.rs）。
7. OpenAI-compatible 请求层对 tool 角色只做文本投影，协议本身也不允许 tool
   角色携带 image part；因此即使用户改用“再次发送图片”，也只能走 `@image`
   人类附件（观察：request.rs + OpenAI Chat Completions 规范）。

用户可见后果：所有 OpenAI-compatible 端点（含自定义 `base_url`）的 MCP 工具
图片被静默丢弃，工具状态仍为成功，模型只能看到 `Image read` 文本。

## 影响面

- 用户影响：使用 OpenAI-compatible 端点时，Read 图片、以及任何返回图片的 MCP
  工具结果都无法被视觉模型看到；容易误判为模型不支持视觉或图片损坏。
- 受影响模块/调用点：`mcp-packages/workspace`（Read）、`mcp-packages/common`
  （raw → image）、`peri-middlewares`（tool bridge）、`peri-agent`
  （tool dispatch canonical）、`peri-model`（OpenAI-compatible 请求构建）。
- 不受影响：Anthropic 与 OpenAI Responses 的工具图片路径；`@image` 人类附件。
- 是否可规避：切换到 Anthropic / Responses；或用 `@image` 手动附加图片。

## P1 依据

- 核心多模态路径错误，稳定可复现，并有单测可锁定。
- 影响主要路径（所有 OpenAI-compatible 端点），但存在规避（换协议、手动
  `@image`），且不会崩溃或损坏既有数据，因此不是 P0。
- 历史：`19f19e221` 引入协议白名单时即写明“降级为文本”，属于既有设计缺陷，
  不是新回归。

## 修复方案

- canonical 工具结果保留通过既有 MIME / 空值 / 大小校验的图片，不按当前
  provider 协议删除；Anthropic / Responses 继续使用原生工具图片。
- OpenAI-compatible 请求适配层：tool 角色只发送文本；把工具图片投影为紧随
  **完整**并行工具结果批次之后的 `user` 附件，附件文本携带 `tool_call_id`、
  `name`、`is_error`。该 user 消息只存在于 wire 投影，不写入用户历史，也不能插
  在并行工具结果之间。image-only 工具结果的 tool `content` 使用占位提示文本，
  避免严格兼容端点拒绝空 content。
- 明确不做：不新增视觉能力门禁；不修改数据库 schema；不改 Read 工具参数与既有
  图片大小上限；不修改 `OpenAiAdapter`（见未覆盖项）。
- 已接受风险：不声明视觉能力的文本兼容端点可能对 `image_url` 返回 400。这是用户
  已确认接受的兼容性风险，修复不做模型能力探测。
- 回归测试：
  - `peri-model/src/openai_compatible/request_image_test.rs`：并行批次后投影、
    多图、base64/URL、跨批次顺序、错误图片、纯文本形状、用户图片原位、重复投影
    幂等。
  - `peri-agent/src/agent/stages/tool_image_dispatch_test.rs`：工具返回 →
    dispatch → 历史序列化往返 → OpenAI-compatible 请求，图片字节保留且不伪造
    持久 user 消息。
  - `peri-agent/src/agent/stages/tool_dispatch/execution_test.rs`：canonical
    工具消息始终保留图片。
  - `peri-model/src/anthropic/mod_test.rs`：Anthropic 工具图片仍在
    `tool_result.content` 原生位置；Responses 由既有
    `openai_responses/request_test.rs` 覆盖。

## 验收标准

1. OpenAI-compatible 会话 Read 图片后，下一次模型请求的 `user` 附件包含完整
   `image_url`（base64 或 URL），tool 角色 `content` 不含 image part。
2. Anthropic / Responses 的工具图片请求形状不变。
3. 目标测试与 `scripts/check-changed.sh` 通过。

## 未覆盖与遗留风险

- `peri-agent/src/messages/adapters/openai.rs:142-163`（`OpenAiAdapter`）是同类
  文本投影，但当前无生产调用方，仅测试使用；本次不改，记录为潜伏 API 风险。
- 历史恢复验证是 serde 序列化往返，未执行 SQLite 冷启动 / 真实数据库重开。
- 未运行真实视觉模型推理与现场 TUI 验收。

## 验证记录

基线 `795ff49`，在独立 worktree 内应用补丁并补充 Anthropic 回归后执行：

- `cargo fmt --check` → 通过（exit 0）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-model --lib -- openai_compatible`
  → 33 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- agent::stages::tool_dispatch::image_tests`
  → 1 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-model --lib`
  → 257 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib`
  → 1518 passed，0 failed（首轮有 1 项
  `session::user_input_mailbox::tests::control_tests::test_mailbox_dispatch_takeback_race_has_one_winner`
  失败，该用例位于未改动的 user input mailbox 竞态测试；单独重跑与整包重跑均通过，
  判定为既有偶发，与本次改动无关）。
- `bash scripts/check-changed.sh` → 通过（exit 0，反向依赖闭包 `cargo check` 完成）。
- `git diff --check` → 通过。
- `lefthook run pre-commit` → 通过（fmt、check、staged clippy、layer-imports 全部成功）。
