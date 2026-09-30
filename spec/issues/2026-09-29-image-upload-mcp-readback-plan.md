# 图片通路改造（上传式上传 + `@image` 归 MCP）实施设计

> 日期：2026-09-29。状态：**待评审（设计稿）**；未实施、未 commit。本次交付仅本文件，不修改任何代码、配置或其他文档。
> 分支基线：`feat/mcp-adaptation-v4-part-3`（读取时工作树含未跟踪文件 `spec/issues/2026-09-29-workspace-mcp-resources-plan.md`；本文件与之存在交叉引用，见 §3.4 / §6.4 / §7-Q3）。
> 行号为本次工作树静态读取的定位信息，实施前须按符号复核（行号会随并行改动漂移）。

## §0 裁决记录与三态口径

### 0.1 用户裁决（原话，作为本设计的硬约束）

1. **J1（ImageMiddleware 解除文件系统依赖）**：
   > 「ImageMiddleware 也需要解除文件系统依赖」
   —— ImageMiddleware 的图片路径读取不得再直接调用 `std::fs` / `tokio::fs`；路径解析、可读根与 tmp 归属交给 MCP 侧操作，由 builtin `workspace` 实例决定 tmp 在哪里。

2. **J2（方向 A）**：三个候选方向中选择 **A：上传式 + `@image` 归 MCP**：
   - TUI `Ctrl+V` 改为 **wire 级上传**：图片 base64 直接进消息（复用已有管道），不再写 `~/.peri/images/<uuid>.png`、不再插入 `@image <path>` 文本；
   - `@image <path>` 文本形式**保留**（用于手输路径引用），但其读取、可读根与 tmp 判定**全部归 workspace 实例**（经 MCP）。

### 0.2 三态口径（全文遵守）

- **现状** = 本次静态源码证据，给 `文件:行号`；**目标/建议** = 待实施方案；**运行时证据** = 本次未取得。
- 不得把设计目标写成已具备能力。凡不能由静态源码确证的点，集中在 §10「未验证假设」，并给出验证方法。

### 0.3 适用规范（原文要点）

| 规范 | 原文要点 | 位置 |
| --- | --- | --- |
| ARC-MIDDLEWARE-001 | 「中间件顺序是行为契约，不得按名称、便利性或局部需求重排；链的唯一事实源是 Agent 层 session 工厂（链序蓝本 `production_blueprint`）」；「内置能力迁移为 MCP 实例后，Web / Artifact / Cron / Filesystem / Terminal **不再是链槽位**……其能力由 builtin 实例经 Mcp 槽位的客户端侧提供，**不得为它们恢复 middleware 或新增槽位**」 | `docs/standards/architecture-contracts.md:118-122` |
| ARC-CAPABILITY-CLOSURE-001 | 关闭能力必须同时关闭 direct tools、deferred 索引、slash/ACP/TUI 投影、静态 prompt、subagent/workflow 继承与运行时授权；「只隐藏 catalog/UI 或只移除 middleware 实例不算关闭」；builtin 关闭三条路径之一是 MetaHarness 策略键 `"WorkspaceMiddleware": false` | `docs/standards/architecture-contracts.md:60-64` |
| ARC-MIDDLEWARE-CAPABILITY-001 | 「hook 只接收其阶段真实支持的能力组合，不得继承完整 `MiddlewareState`」；`BeforeInputState` 的输入替换按已有 MessageId 进行，不增删重排；「后续非空用户批次仅运行 `before_input`」 | `docs/standards/architecture-contracts.md:124-128` |
| ARC-FROZEN-001 | 会话创建时冻结日期/项目指引/skills 摘要/MetaHarness/system prompt；禁止中途重读改变 prompt 前缀 | `docs/standards/architecture-contracts.md:30-33` |
| STD-SIZE-001 | 「单个文件最多 1000 行……**新增或修改文件须满足限制**；既有未改动超限文件的治理另立任务」 | `docs/standards/index.md:38-40` |
| RUST-ERROR-001 / RUST-ASYNC-001 / RUST-TRACE-001 | 库 crate 用 `thiserror`；阻塞 I/O 不得堵塞 async runtime；诊断用 `tracing`（禁 `println!`/`dbg!`） | `docs/standards/rust.md:11-28` |
| MCP 资源语义 | 「`BlobResourceContents`：不给原始字节；给 `<N bytes>` 提示或本地文件路径」（**模型面**投影规则）；「MCP resources 只有 `list` / `read` / `templates/list`，**没有写方法**」，「不引入自定义写方法」 | `docs/reference/mcp-ecosystem.md:258-283` |

**声明**：`~/.peri/images` 的存量文件不在本设计治理范围（删除优于兼容 ⇒ 不设计迁移/双写；存量清理另立任务）。

---

## §1 现状：三条通路（源码证据）

### 1.1 通路 ① 外部 ACP 客户端 inline image（**保留不动**）

- 能力声明：`PromptCapabilities::new().image(true)` —— `peri-acp/src/dispatch/init.rs:28`。
- wire 归一：`prompt_blocks_to_content` 把 `{"type":"image","data","mimeType"}` 转 `ContentBlock::image_base64` —— `peri-acp/src/dispatch/prompt.rs:24-51`；顶层 `attachments` 经 `merge_attachments_into_content` 并入 content —— 同文件 `:53-73`、`:104-107`。
- 入口：`extract_prompt_params` —— 同文件 `:79-107`；`message.content` 分支直接 `serde_json::from_value` 还原 `MessageContent`（`:91-95`）。

**结论**：通路 ① 已是上传式，不依赖 `@image`，不在本次改造范围。

### 1.2 通路 ② TUI 默认：`Ctrl+V` 写盘 + `@image` 文本（**本次改造对象**）

TUI 侧现状（两条分支，均回落为「写文件 + 插文本」）：

| 分支 | 位置 | 行为 |
| --- | --- | --- |
| macOS 原生 PNG | `peri-tui/src/kit/input_area.rs:461-477` → `peri-tui/src/kit/input_area/image.rs:29-62` | 读 `NSPasteboardTypePNG` → 尺寸上限校验 → 只读 IHDR → `std::fs::create_dir_all` + `std::fs::write` 到 `managed_images_root()` |
| arboard RGBA 通用 | `peri-tui/src/kit/input_area.rs:483-523` | `get_image()` → 手算 hash + 时间戳 → 自建 `~/.peri/images` 目录（`:501-505`）→ `png_encode` 落盘（`:510-516`） |

- 落盘后统一调 `insert_image_reference` 插入 `@image <abs path>` 独占行 —— `peri-tui/src/kit/input_area/image.rs:64-87`。
- 受管理目录定义：`managed_images_root()` —— `peri-tui/src/kit/image_safety.rs:134-140`。

服务端消费现状（ImageMiddleware，链上 `ChainSlot::Image`）：

- 链序蓝本 `ChainSlot::Image` 位于 AtMention 之后、GitAttribution 之前 —— `peri-agent/src/session/factory.rs:91-105`；构造点 —— `peri-middlewares/src/assembly.rs:257-261`。
- `before_input` 扫描本批 Human 消息，正则 `@image\s+(\S+)` 提取路径 —— `peri-middlewares/src/middleware/image/mod.rs:71-98`（regex 在 `:90`）。
- 逐张 `tokio::task::spawn_blocking(load_image_file)` —— `:122-136`；`load_image_file` 内部直接 `std::fs`：`shellexpand::tilde`（`:173`）、`path.exists()/is_file()`（`:176-182`）、`std::fs::metadata`（`:185`）、`std::fs::read`（`:196`）、魔数嗅探 `image::guess_format`（`:199-202`、`:207-218`）。
- 软降级：失败结果统一包成文本块 `[{err}]` —— `:151-156`；错误文案（`load_image_file` 产出，共 4 类）：`Image not found: <ref>`（`:177`）、`Not a file: <ref>`（`:181`）、`Image too large: A MB > B MB limit`（`:189-192`）、`Not an image: <ref>`（`:201`），另有 `Cannot read file: <io error>`（`:185,196`）。
- 文本清理：移除标记后为空则丢弃该 text 块，保留非文本块 —— `:138-149`；替换经 `state.replace_message` 回写 —— `:158-164`。
- 上限常量：`max_size = 20 * 1024 * 1024` —— `:35`（与 TUI `MAX_IMAGE_BYTES` 相同 —— `peri-tui/src/kit/image_safety.rs:28-29`）。

**已知回归背景**：本条通路曾在「同一 run 后续批次只留 `@image` 文本、图片未转换」上出过缺陷并修复（`2026-09-20-micro-compact-user-image-unavailable.md` 已压缩至 `../history/2026-09.md` 2026-09-20 条目，对应 `before_input` 逐批能力）。**该缺陷类别在 P1 迁移后仍然存在**（见 §5.2 回归项 R1）。

### 1.3 通路 ③ TUI user input queue（steer）：**已具备上传式，但 `Ctrl+V` 未接入**

- 附件类型：`PendingAttachment { label, media_type, base64_data }` —— `peri-tui/src/kit/atoms.rs:213-218`（`#[derive(Debug, Clone, Default)]`，**无 `PartialEq/Eq`**）。
- 附件原子：`PENDING_ATTACHMENTS` —— 同文件 `:223`。**现状写入点只有一处**：take-back 恢复 —— `peri-tui/src/kit/input_area.rs:856-892`（`:883` 调 `attachments_from_content`）。
- base64 → block 转换：`content_for_draft` —— `peri-tui/src/kit/steer_state.rs:469-481`；**反向**转换（block → 附件）：`attachments_from_content` —— `:483-500`。
- 入队：`enqueue` 构造 `UserInput { input_id, content, original_draft }` —— `peri-tui/src/kit/steer_state.rs:395-419`；发送经 `STEER_TX`（`:461-467`）。
- wire 契约：`EnqueueUserInputRequest { session_id, generation, command_id, input_id, content: MessageContent, original_draft }` —— `peri-acp-types/src/session/user_input.rs:50-59`（`deny_unknown_fields`）；`UserInput` —— `:6-14`。
- 服务端受理：`session/input/enqueue` → `mailbox.enqueue(&request)` —— `peri-acp/src/host/requests/user_input.rs:49-53`（能力协商门在 `:27-32`）。
- mailbox 校验：`request.content.is_empty()` 拒绝 —— `peri-agent/src/session/user_input_mailbox.rs:169-171`；`MessageContent::is_empty()` 对 `Blocks` 只判块数为 0 —— `peri-acp-types/src/messages/content.rs:399-405` ⇒ **纯图片（空文本 + 1 张图）不算空，可入队**。
- 交接：`handoff_locked` 原样克隆 `record.input.content` 进 `BaseMessage::Human { id, content }`，经 `QueuedMessage::prompt(MessageSource::UserInput, …)` 入 `SessionInbox` —— `peri-agent/src/session/user_input_mailbox.rs:603-627`。

### 1.4 上传式端到端是否保留 image blocks（**结论：保留，逐跳已证**）

| 跳 | 位置 | 证据 |
| --- | --- | --- |
| 入队 → Human 消息 | `peri-agent/src/session/user_input_mailbox.rs:617-623` | `content` 原样克隆进 `BaseMessage::Human` |
| 主 prompt → Human 消息 | `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:361-365` | `BaseMessage::human(req.agent_input.content)` 推入 v2 queue（`MessageKind::Prompt` / `V2MessageSource::UserInput`） |
| TurnInput 载体 | `peri-acp/src/host/prompt.rs:573-585` | `TurnInput { content, .. }`（由 `extract_prompt_params` 产出，`:225`） |
| receive → before_input | `peri-agent/src/agent/stages/mod.rs:872`；`peri-agent/src/agent/stages/middleware_runner.rs:165-175` | 逐批 `before_input(&context, &receive_out.input_message_ids)` |
| compact 投影 | `peri-agent/src/agent/compact_v2/projection.rs:444-483` | `project_block` 逐块投影，保留 `Blocks` variant 与未命中 action 的块（`_ => block.clone()` 语义见 `:676-681` 同族注释） |
| provider 请求 | `peri-model/src/anthropic/request.rs:218-222`；`peri-model/src/openai_compatible/request.rs:217-224` | `ContentBlock::Image` → anthropic `source.base64` / openai `image_url: data:<mime>;base64,<data>` |

**结论**：`ContentBlock::Image`（base64）在 mailbox → dispatch → executor → provider 全程未被降级为文本。**上传式主提交路径的唯一缺口是 TUI 侧未把附件带进 `SubmitRequest`**（见 §3.1）。

### 1.5 MCP 侧现状（决定 P1 可行性的既有能力）

| 事实 | 位置 |
| --- | --- |
| builtin `workspace` 实例已启用 resources 能力并实现 `list_resources` / `read_resource`，当前**只有** `workspace://git/ref` 一条（text/plain） | `mcp-packages/workspace/src/workspace.rs:198-208`、`:218-229`、`:231-249`；URI 常量 `mcp-packages/workspace/src/git_watch.rs:25` |
| 未知 URI 按协议回 `-32602`（`invalid_params`） | `mcp-packages/workspace/src/workspace.rs:237-242` |
| 实例持构造期冻结的 `cwd`（`WorkspaceMcpServer::new(cwd, input)`），7 个工具共享 | `mcp-packages/workspace/src/workspace.rs:49-66`、`:81`；`list_tools` 不重算 cwd（`:210-216`） |
| 实例名 = `"workspace"`，策略键 = `"WorkspaceMiddleware"` | `peri-acp-types/src/builtin_mcp.rs:219-225` |
| 宿主向链装配注入 builtin 上下文（含 cwd） | `peri-acp/src/host/assemble.rs:406-417` |
| 宿主侧已有「按名字读资源」的工具：`mcp_read_resource(server_name, uri)` → `get_client_visible_to` → `peer` → `read_resource_cached`，`skill://` 额外做 digest 绑定校验 | `peri-middlewares/src/mcp/resource_tool.rs:39`、`:184-222`、`:258-309` |
| **该工具把 Blob 投影成占位文本**：`[blob/<mime>]` + `<N bytes of binary data>` | 同文件 `:328-336` |
| 池级读接口 `read_resource_cached` 为 `pub(crate)` | `peri-middlewares/src/mcp/client/cache.rs:72-83` |
| `McpClientPool` 具体类型可经 `get_client_visible_to(name, session_id)` + `is_visible_to_session` 做会话过滤 | `peri-middlewares/src/mcp/client.rs:69`、`:363-372`；`peri-middlewares/src/mcp/client/lifecycle.rs:142-147` |
| 读超时常量 | `peri-middlewares/src/mcp/resource_tool.rs:40`（120s） |

---

## §2 目标数据流（方案 A）

```
                 ┌──────────────────────── TUI ────────────────────────┐
 Ctrl+V ─────────┤ 剪贴板字节 → PendingAttachment{base64, mime}         │
                 │   ├─ steer 可用：PENDING_ATTACHMENTS → enqueue ──────┼──┐
                 │   └─ steer 不可用：SubmitRequest::AgentText{text,     │  │
                 │                     attachments} → session/prompt    │  │
                 └──────────────────────────────────────────────────────┘  │
                                                                           ▼
         (A) 上传式：content = Blocks[Text?, Image(Base64)…]  ──► 服务端全程保留
                                                                           │
 手输 `@image <path>` ──► 文本进 content ──────────────────────────────────┤
                                                                           ▼
                                                      before_input（逐批）
                                                                           │
                    ┌──────────────────────────────────────────────────────┤
                    │ ImageMiddleware                                       │
                    │  · 不再 std::fs/tokio::fs                              │
                    │  · 抽文本中的 @image 标记 → 收集路径                    │
                    │  · 调宿主注入的 Reader 端口（workspace 实例）  ────────┼──► MCP
                    │  · 成功 → 追加 ContentBlock::Image(Base64)             │   resources/read
                    │  · 失败 → 追加占位文本（软降级，不阻断）                │   workspace://image?…
                    └──────────────────────────────────────────────────────┘   │
                                                                               ▼
                        builtin `workspace` 实例：tilde/相对路径解析（实例 cwd）
                        → 可读根判定 → regular file → 大小上限 → 魔数嗅探
                        → 返回 BlobResourceContents(base64, mime)
```

### 2.1 三条通路的目标去留

| 通路 | 目标 | 依据 |
| --- | --- | --- |
| ① 外部 ACP inline image | **不动**。仍走 `extract_prompt_params` | 已符合 J2 上传式形态 |
| ② TUI `Ctrl+V` | **改为上传式**：不再落盘 `~/.peri/images`、不再插 `@image` 文本；base64 进消息 | J2 第一句 |
| ②′ 服务端 `@image` 文本解析 | **保留正则与占位符**，但读取改经 MCP；`std::fs` 全部退出 | J1 + J2 第二句 |
| ③ steer 队列 | **不新增协议**；`Ctrl+V` 改为写入 `PENDING_ATTACHMENTS` 后，与既有 `content_for_draft` 汇合 | 已具备，接线即可 |

### 2.2 通道选择：为什么是 `resources/read` 的 Blob，而不是新工具

| 候选 | 结论 | 理由（含证据） |
| --- | --- | --- |
| (a) `resources/read` → `BlobResourceContents`（base64） | **推荐（主方案）** | ① MCP 标准能力，rmcp 3.1.4 原生支持（`ResourceContents::BlobResourceContents { blob, .. }` 已在 `peri-middlewares/src/mcp/resource_tool.rs:289,328` 被匹配，`:431-436` 直接 base64 解码）；② workspace 实例**已实现** `read_resource` handler 与 resources 能力（`mcp-packages/workspace/src/workspace.rs:198-208,231-249`），增量最小；③ 宿主侧已有池级读 API 与 peer 解析路径（`resource_tool.rs:258-264`）；④ 与并行计划既有裁决「**正文一律经 `resources/read`**」同向（`spec/issues/2026-09-29-workspace-mcp-resources-plan.md:485`）。 |
| (b) 新增 workspace 工具（如 `ReadImage`）返回 image content | **不推荐** | ① `BaseTool::invoke` 返回 `Result<String, _>`（`peri-acp-types/src/tools.rs:549-553`），`invoke_output` 的 `ToolOutput` **只有 `text`**（`:119-123`）；② `invoke_tool_call` 把结果硬编码为 `ContentBlock::text`（`mcp-packages/common/src/helpers.rs:41-68`，成功路径 `:58-60`）；③ 要返回图像必须改共享工具抽象与 common 映射，blast radius 远大于 (a)，且会把二进制面引入模型可调用工具；④ 「写操作归 tools、resources 保持只读」（`docs/reference/mcp-ecosystem.md:265-280`）不构成走工具的理由——本任务是读。 |
| (c) 走 ACP `resources/read`（宿主 ↔ 客户端） | 不适用 | 那是 MCP-over-ACP 的传输切片（`peri-acp-types/src/ports.rs:118` 起），目标实例是本进程 builtin workspace，不经 ACP 客户端。 |

**补充（对 `docs/reference/mcp-ecosystem.md:261` 的边界说明）**：该条约束的是「资源以**模型上下文**形态投递时，blob 不给原始字节」。本方案的消费方是**宿主内部 middleware**（把图片还原为 `ContentBlock::Image` 供 provider 请求），与现状「服务端读盘 → base64 → Image block」的信息流向完全一致，**不新增**任何面向模型的二进制透传面；宿主侧 `mcp_read_resource` 对 blob 的占位投影（`resource_tool.rs:328-336`）保持不变（见 §4.3 安全边界）。

---

## §3 逐文件改动

标注：**[新]** 新增文件 · **[改]** 修改 · **[删]** 删除。所有改动均按「删除优于兼容」执行——不保留写入 `~/.peri/images` 的旧分支、不设计双写或静默回落磁盘。

### 3.1 TUI（`peri-tui`）— P0

| # | 文件 | 动作 | 内容 | 现状锚点 |
| --- | --- | --- | --- | --- |
| T1 | `peri-tui/src/kit/atoms.rs` | [改] | `PendingAttachment` 增加 `PartialEq, Eq`（供 `SubmitRequest` 派生 `PartialEq, Eq` 复用）；新增构造辅助（`PendingAttachment::png(base64)` 或等价）避免调用点各自硬编码 media_type | `:213-223` |
| T2 | `peri-tui/src/kit/input_area/paste.rs` | [新] | 从 `input_area.rs` 抽出的 `Ctrl+V` 处置：剪贴板 → `PendingAttachment`（macOS PNG 优先，失败回落 arboard RGBA → PNG 编码）+ 尺寸/字节上限校验 + 通知文案；**不含任何 `std::fs` 写** | 源块 `input_area.rs:441-552` |
| T3 | `peri-tui/src/kit/input_area.rs` | [改] | 删除 `Ctrl+V` 内联块与 `image::save_native_clipboard_png` / `insert_image_reference` 调用，改为调用 `paste::handle_paste_shortcut(...)`；**必须同时满足 STD-SIZE-001**（当前 1061 行 > 1000，见 §3.6） | `:441-552`、`:461-477`、`:483-523` |
| T4 | `peri-tui/src/kit/input_area/image.rs` | [改] | 删除 `save_native_clipboard_png` / `save_pasteboard_png` / `insert_image_reference`；保留并**下沉**「RGBA → PNG 字节（内存）」能力（现 `png_encode` 写盘，`:89-142`，需改为返回 `Vec<u8>`） | `:29-62`、`:64-87`、`:89-142` |
| T5 | `peri-tui/src/kit/submit_request.rs` | [改] | `SubmitRequest::AgentText(String)` → 携带附件：`AgentText { text: String, attachments: Vec<PendingAttachment> }`（或新增同形变体）；`parse_submit_request` 产出 `attachments: Vec::new()` | `:5-12`、`:44-134` |
| T6 | `peri-tui/src/kit/input_area/submit.rs` | [改] | 统一「取附件」：三条出口（空文本 + 附件、steer 可用、steer 不可用→`SUBMIT_TX`）都从 `PENDING_ATTACHMENTS` 取一次并随请求下发；**禁止**再出现 `enqueue(text, Vec::new())` 的丢弃点 | `:8-44`、`:58-80`（丢弃点 `:60`） |
| T7 | `peri-tui/src/kit/submit_consumer.rs` | [改] | `handle_agent_text_submit` 由 `MessageContent::text(trimmed)` 改为「有附件则 `MessageContent::blocks`，无附件保持 `text`」；复用 `steer_state::content_for_draft` 风格的构造（建议抽 `content_for_submit(text, attachments)` 单一实现，避免两处漂移） | `:195-199` |
| T8 | `peri-tui/src/kit/steer_state.rs` | [改] | `content_for_draft` 提升为 `pub(crate)` 复用（或与 T7 合并为同一构造函数）；保持不变的是「首个 text 块 + 追加 image 块」的顺序语义 | `:469-481` |
| T9 | `peri-tui/src/kit/input_area/image_test.rs`、`input_area_test.rs`、`submit_request_test.rs`、`submit_consumer_test.rs`、`steer_state_test.rs` | [改] | 断言从「生成 `@image <路径>` 行」改为「`PENDING_ATTACHMENTS` 收到 base64 附件」；新增「附件经 `session/prompt` 与 `session/input/enqueue` 两条路径抵达 content」的用例 | 现存断言 |

### 3.2 peri-acp / peri-acp-types — P0

| # | 文件 | 动作 | 内容 | 现状锚点 |
| --- | --- | --- | --- | --- |
| A1 | `peri-acp/src/dispatch/prompt.rs` | [改]（**建议零改动**） | 通路 ① 已支持 `message.content` 的 `Blocks` 直读与顶层 `attachments` 合并；P0 不新增 wire 字段。实施时若确认 TUI 走 `message.content`，本文件只需补一条回归用例（`:24-51`、`:79-107`） | `:24-107` |
| A2 | `peri-acp-types/src/session/user_input.rs` | [改]（**建议零改动**） | `EnqueueUserInputRequest.content: MessageContent` 已承载 image blocks；`deny_unknown_fields` 意味着**不得**为附件另加顶层字段（否则新旧端不兼容） | `:50-59` |
| A3 | `peri-acp/src/host/requests/user_input.rs` | [改]（**建议零改动**） | 已原样透传 `content` 到 mailbox | `:49-53` |

**结论**：P0 的协议面**已经就绪**，真正的缺口只在 TUI（§1.4 表末行）。任何「为图片新增 ACP 方法/字段」的动议都应被驳回，除非 §8 的运行时验证（V1/V2）证伪。

### 3.3 peri-middlewares — P1

| # | 文件 | 动作 | 内容 | 现状锚点 |
| --- | --- | --- | --- | --- |
| M1 | `peri-middlewares/src/middleware/image/mod.rs` | [改] | 删除 `load_image_file` / `detect_mime` / `spawn_blocking` 文件 I/O（`:170-205`、`:207-218`）；`prepare_image_input` 改为调用注入的 Reader 端口；保留正则（`:90`）、替换与占位符逻辑（`:138-164`）。**目标文件必须保持 < 1000 行**（当前 228 行，余量充足） | `:71-98`、`:101-167`、`:170-218` |
| M2 | `peri-middlewares/src/middleware/image/source.rs` | [新] | Reader 端口定义（trait）+ 生产实现（持 `Arc<McpClientPool>` 与 `session_id`）：按 `server_name = "workspace"` 取 client → `read_resource_cached` → 匹配 `BlobResourceContents` → base64 解码 → 返回 `ImageBytes { mime, bytes }`；**带显式超时**（对齐 `resource_tool.rs:40` 的 120s 或更短的图片专用值） | 参照 `resource_tool.rs:184-222,258-264,328-336` |
| M3 | `peri-middlewares/src/assembly.rs` | [改] | `ChainSlot::Image` 构造点把 `mcp_pool_concrete`（已在同一个 `assemble()` 内解析，`:122-129`）传给 `ImageMiddleware::new(...)`；**槽位顺序不变、不新增槽位**（ARC-MIDDLEWARE-001） | `:94-129`、`:257-261` |
| M4 | `peri-middlewares/src/assembly/preparation.rs` | [改] | 复用既有 `mcp_pool_concrete`（`:41-45`）；若需 session 可见性过滤，把 `session_id` 一并暴露给调用点（`AssemblyContext.session_id` 已存在，`peri-agent/src/session/factory.rs:288`） | `:25-45` |
| M5 | `peri-middlewares/src/middleware/image/mod_test.rs` | [改] | 现有 8 项回归需改为注入 fake Reader（含 `test_image_later_input_reaches_model_after_micro_compact`，`:17` 起）；新增「错配/超时/超限/非图片」四类降级用例 | `:1-100`（构造 `:94`） |
| M6 | `peri-middlewares/src/mcp/resource_tool.rs` | [改]（可选，见 §7-Q4） | 若要复用其 peer 解析路径，抽出 `pub(crate) async fn read_resource_blob(pool, server_name, session_id, uri) -> Result<Blob, ResourceError>` 供 M2 共用；**不改**其模型面 blob 占位语义（`:328-336`） | `:39-40`、`:184-222`、`:258-341` |

**关于「端口 vs 具体类型」**：`McpPoolPort` 目前只暴露 `as_any` / `begin_shutdown` / `shutdown` / `snapshot`（`peri-acp-types/src/ports.rs:79-96`），**不**包含资源读取。装配处已有 `downcast_arc::<McpClientPool>()` 的既有模式（`peri-middlewares/src/assembly/preparation.rs:41-45`），且 `McpMiddleware` 就是持具体池入链的先例（`peri-middlewares/src/assembly/mcp.rs:67-77`）。因此 **M3 采用构造期注入**，不改 `BeforeInputState`（遵守 ARC-MIDDLEWARE-CAPABILITY-001「hook 不得继承完整状态」），也不新增 `peri-acp-types` 端口。

**备选方案（评估后不推荐为主方案）**：把 `@image` 转换点整体移出 middleware（例如移到 MCP 层或 builtin bridge 的工具结果映射）。理由：转换需要发生在 `before_input`（逐批、按 MessageId 替换，`peri-agent/src/agent/stages/mod.rs:872`），而 MCP 层只在 `tools/call` 结果上做映射（`mcp-packages/common/src/helpers.rs:41-68`），**没有**「用户输入批」这一上下文；迁移会把图片语义绑到工具调用上，与 J1 保留 ImageMiddleware 的表述直接冲突。**与 J1 的关系**：J1 明确要求「ImageMiddleware 解除文件系统依赖」，即保留该 middleware、只换数据来源；本方案据此执行。

### 3.4 mcp-packages/workspace — P1

| # | 文件 | 动作 | 内容 |
| --- | --- | --- | --- |
| W1 | `mcp-packages/workspace/src/image.rs` | [新] | 图片资源实现：URI 解析（percent-decode + 路径还原）、tilde 展开、相对路径按**实例冻结 cwd** 解析（`workspace.rs:49-66`）、canonicalize、regular file 校验、大小上限、魔数嗅探（PNG/JPEG/GIF/WebP，对齐 `middleware/image/mod.rs:15-20`）、`ResourceContents::blob(...)` 返回 |
| W2 | `mcp-packages/workspace/src/workspace.rs` | [改] | `read_resource` 增加 `workspace://image?...` 分支（`workspace.rs:231-249`）；`list_resources` **不列举**该资源（无界/实例局部，避免把主机路径刷进 `resources/list`）；`ServerCapabilities` 不变（resources 已启用，`workspace.rs:198-208`） |
| W3 | `mcp-packages/workspace/src/workspace.rs` 或 `image.rs` | [改] | 可读根与 tmp 策略：**实例 cwd 为默认根**；提供显式配置/构造输入键以收紧或放宽（见 §7-Q1/Q2）。**P1 不落 tmp**——直接读成 blob；若未来需要暂存（转码/超限降采样），暂存目录由 workspace 实例决定（建议 `<实例 cwd>/.peri/tmp/images/`），宿主与 TUI 不得自行决定 |
| W4 | `mcp-packages/workspace/src/workspace.rs:1-18` 模块文档 | [改] | 更新「exposes exactly one resource」表述（现有文档与代码一致，新增资源后必须同步，DOC-UPDATE-001） |

**URI 形态建议**：`workspace://image?path=<percent-encoded 路径>`。
- 理由：路径含 `/`、可能含 `#`/`?`/非 ASCII，用 query 承载比 path 段更稳；`path=` 显式可读，便于错误文本与测试断言。
- 需与并行计划冻结的 scheme 对齐：`spec/issues/2026-09-29-workspace-mcp-resources-plan.md:170-183` 正在为 skills/agents/meta 定义 URI，本文件新增的 `workspace://image` 必须并入同一张表（§9）。**该点列入 §7-Q3 待裁决**。

### 3.5 删除清单（P0 / P1 各自的 `~/.peri/images` 面）

| 对象 | 位置 | 阶段 | 说明 |
| --- | --- | --- | --- |
| macOS `NSPasteboard` → 写盘 | `peri-tui/src/kit/input_area/image.rs:29-62` | P0 | 整段删除（保留 IHDR 头校验思路，改为内存字节校验） |
| arboard RGBA → 写盘 | `peri-tui/src/kit/input_area.rs:483-523` | P0 | 整段替换为「编码到内存 → `PendingAttachment`」 |
| `insert_image_reference` | `peri-tui/src/kit/input_area/image.rs:64-87` | P0 | 删除（`@image` 文本手输仍可用，但不再由程序生成） |
| `png_encode(... -> path)` | `peri-tui/src/kit/input_area/image.rs:89-142` | P0 | 签名改为返回 `Vec<u8>`（不落盘） |
| `managed_images_root()` | `peri-tui/src/kit/image_safety.rs:134-140` | P0（**择一**） | 若 TUI 预览面板（T7）仍需「受管理目录」判定，则保留；否则删除。**注意**：`grade_path`/`PathGrade::Managed` 被用户气泡 `@image` 行渲染消费（`peri-tui/src/kit/message_area/render/user.rs:235-260`），删除会连带影响手输路径的展示分级 —— 列为 §7-Q5 |
| `ImageMiddleware::load_image_file` / `detect_mime` | `peri-middlewares/src/middleware/image/mod.rs:170-218` | P1 | 删除（能力迁 W1） |
| `ImageMiddleware` 的 `spawn_blocking` 文件 I/O | `peri-middlewares/src/middleware/image/mod.rs:122-136` | P1 | 删除（MCP 客户端本身异步） |

**保留边界（明确不删）**：
- 正则 `@image\s+(\S+)`（`middleware/image/mod.rs:90`）与文本清理逻辑（`:138-149`）——手输路径引用仍需要（J2 第二句「保留」）。
- 软降级占位符文案家族（`Image not found:` 等）——用户可见文本，且被现有测试锁定；**迁移只换数据来源，不改文案**（避免无意义的行为漂移）。
- TUI `@image` 行的展示/预览子系统（`message_area/render/user.rs:182-290`、`image_action.rs`、`image_overlay.rs`、`hits.rs`、`vm_cache.rs:204`）——服务于**手输**路径引用；是否削减为 §7-Q5。

### 3.6 STD-SIZE-001 硬约束（必须进实施清单）

`peri-tui/src/kit/input_area.rs` 当前 **1061 行**，已超 1000 行上限；STD-SIZE-001 要求「新增或**修改**文件须满足限制」。T3 触达该文件 ⇒ **P0 必须同时完成拆分**（建议把 `Ctrl+V` 处置与 `Event::Paste` 处置整体移入 `input_area/paste.rs`，与 `input_area/{hooks,popup,render,submit}.rs` 的既有分层一致）。交付验收须含 `wc -l` 证明。

---

## §4 失败与降级语义、关闭矩阵、安全边界

### 4.1 失败与降级语义

| 场景 | 目标行为 | 依据/对齐 |
| --- | --- | --- |
| `@image` 路径不存在 | 追加占位文本 `[Image not found: <原始引用>]`，消息照常发送 | 现状文案 `middleware/image/mod.rs:176-177` + 软降级 `:151-156` |
| 目标存在但不是常规文件（目录/fifo/设备） | `[Not a file: <引用>]` | `:180-182` |
| 超过大小上限 | `[Image too large: A MB > B MB limit]` | `:186-192` |
| 魔数不是支持的图片 | `[Not an image: <引用>]` | `:199-202` |
| workspace 实例未连接 / 已关闭 | `[Image unavailable: workspace instance unavailable]`（**新增文案**，需同步 i18n 与测试） | §4.2 关闭矩阵 |
| 资源读取超时 | 同上 unavailable 家族（**不得**阻塞 turn） | 见 M2 超时 |
| 实例返回非 blob（异常/降级实现） | `[Image unavailable: unexpected resource payload]` | 防御分支 |
| base64 解码失败 | 同「非 blob」 | `resource_tool.rs:428-436` 的校验思路 |
| 单条消息含多个 `@image` | 逐个独立成败，成功者追加 block、失败者追加占位文本（**不整批失败**） | 现状逐张独立 `results` — `:122-136`、`:151-156` |

**方向性原则**：图片失败**永不阻断**用户消息（与 `peri-tui/src/kit/image_safety.rs:12-13` 既有安全降级声明一致）。

### 4.2 关闭矩阵（`WorkspaceMiddleware: false` / 池缺失）

关闭入口为 MetaHarness 策略键 `"WorkspaceMiddleware"`（`peri-acp-types/src/builtin_mcp.rs:223-225`；ARC-CAPABILITY-CLOSURE-001 `docs/standards/architecture-contracts.md:60-64`）。

| 场景 | P0（Ctrl+V 上传） | P1（`@image` 文本） |
| --- | --- | --- |
| `WorkspaceMiddleware: false` | **不受影响**（图片已在消息里，不需要读取） | 占位文本 `[Image unavailable: ...]`；**无磁盘兜底**（J1：读取归 MCP，关闭即不可用），不回落 `std::fs` |
| `mcp_pool = None`（print 模式等） | 不受影响 | 同上 unavailable |
| workspace 实例连接失败/未就绪 | 不受影响 | 同上 unavailable |
| 实例存在但未实现 image 资源（版本错配） | 不受影响 | `-32602` → unavailable |
| `ImageMiddleware: false`（槽位关闭） | 上传式仍可用（不经 middleware） | `@image` 文本**原样保留**在消息中（现状行为，不回归） |

**必须新增的闭包断言**：关闭 workspace 后，`@image` 路径**不得**出现任何磁盘读取痕迹（可用「路径存在但实例关闭 → 仍降级」的用例证明无旁路）。

### 4.3 安全边界

| 边界 | 处理 |
| --- | --- |
| 路径穿越 / 符号链接 | workspace 侧 `canonicalize` 后再做根判定；根判定失败拒绝。**注意**：`@image` 是用户显式输入，不是「拼接不可信片段」，主要风险是「把任意文件读成图片」——由魔数嗅探兜底（非图片一律拒绝，无法读出文本文件内容） |
| 可读根 | 默认 = 实例 cwd（+ 见 §7-Q1 的裁决项）；越根访问必须显式配置放开，不能默认放开 |
| 不可信数据 | 资源读取结果**不得**直接进 system prompt / 冻结面（ARC-FROZEN-001）；只作为用户消息的 image block |
| 大小限制 | 上限唯一来源建议下沉到 workspace 侧（20MB，对齐 `middleware/image/mod.rs:35` 与 `image_safety.rs:29`）；**TUI 侧仍需前置校验**（避免为会被拒绝的图片付 base64/编码代价）——尤其 arboard RGBA 分支当前**无**字节上限检查（`input_area.rs:483-523`），P0 必须补 |
| 模型可达性 | 该资源经 `mcp_read_resource` 对模型可见（任何 MCP resources/read 都可见）。残留风险仅为「存在性/错误文本预言机」，且 workspace 实例本就向模型暴露 `Read`/`Glob`/`Grep`（`mcp-packages/workspace/src/workspace.rs:1-18`），**不构成新增能力类别**；缓解措施 = 错误文本统一为无路径的通用文案（RUST-ERROR-001 风格，对齐 `mcp-packages/common/src/result_mapping.rs:6-25` 的 allowlist 投影）。 |
| 二进制外泄 | 宿主 `mcp_read_resource` 对 blob 的投影保持不变（`resource_tool.rs:328-336`），模型永远拿不到原图字节 |
| 秘密泄漏 | 错误文本、日志不得含主机绝对路径（`docs/standards/rust.md:11-21`）。**两个面口径不同，实施时不得混淆**：(i) 用户可见占位符可回显**用户自己输入的原始引用**（§4.1，等价于用户已在输入框写过的内容）；(ii) 面向**模型**的资源读取错误（`mcp_read_resource` 等）必须是 allowlist 通用文案，不得回显 workspace 解析出的主机绝对路径 |
| tmp | **P1 不产生 tmp**（无暂存）。若后续引入暂存，目录归属 workspace 实例、命名可预测、生命周期与实例绑定并显式清理（§3.4 W3） |

---

## §5 测试计划与验收命令

### 5.1 分层测试

| 层 | 覆盖点 | 位置 |
| --- | --- | --- |
| TUI 单元 | 剪贴板字节 → `PendingAttachment`（含超限拒绝、非图片拒绝）；两条 submit 出口都带走附件；take-back 恢复仍能还原附件 | `peri-tui/src/kit/input_area/image_test.rs`、`submit_request_test.rs`、`steer_state_test.rs` |
| TUI 装配/线路 | `SubmitRequest::AgentText{attachments}` → `session/prompt` 的 `message.content` 含 image block（json 断言） | `peri-tui/src/kit/submit_consumer_test.rs`、`peri-tui/src/acp_client/client_test.rs` 同族 |
| ACP 契约 | `extract_prompt_params` 对 `message.content` 的 `Blocks` 直读；顶层 `attachments` 合并；`EnqueueUserInputRequest` 的 `deny_unknown_fields` 不被破坏 | `peri-acp/src/dispatch/prompt_test.rs`、`peri-acp-types/src/session/user_input_test.rs` |
| mailbox | 纯图片（空文本 + 1 block）可入队；`is_empty` 判定不被误伤 | `peri-agent/src/session/user_input_mailbox_test.rs` |
| middleware（P1） | 注入 fake Reader：成功转换、四类降级、超时、多图独立成败、**逐批**（首轮 / 后续批次 / Micro 之后） | `peri-middlewares/src/middleware/image/mod_test.rs` |
| workspace 侧（P1） | URI 解析（percent-encoding、`~`、相对路径、`..`、symlink）、魔数四格式、超限、未知 URI `-32602`、blob 往返 | 新增 `mcp-packages/workspace/src/image_test.rs`（该 crate 既有模式为 `src/<mod>_test.rs` + `#[cfg(test)] #[path = ...] mod tests`）。**注意现状**：`workspace.rs` 本身**没有** crate 内测试模块（`:76`、`:282` 注释提到的 `workspace_test.rs` 在本工作树中不存在），其 handler 行为目前由宿主侧 wire 测试覆盖，例如 `peri-middlewares/src/mcp/builtin_subscription_workspace_wire_test.rs:156-163`。新资源的验收须显式选择一处事实源，不得依赖已失效的注释引用 |
| 端到端线路 | 真实 builtin duplex 的 `resources/read` → middleware → provider 请求含 image block | 参照 `peri-middlewares/src/mcp/builtin_subscription_workspace_wire_test.rs:156-163` 的 wire 手法 |

### 5.2 回归项（必须显式覆盖）

1. **逐批转换不回归**：`test_image_later_input_reaches_model_after_micro_compact`（`peri-middlewares/src/middleware/image/mod_test.rs:17`）必须在 P1 后仍然通过——「同一 run 后续批次输入仍向模型传图片字节」。这是 `2026-09-20-micro-compact-user-image-unavailable.md`（已压缩至 `../history/2026-09.md` 2026-09-20 条目）关闭过的缺陷。
2. **不上传内容不得被当成图片**：P0 后，若消息**只含文本**且无附件，`session/prompt` 的 content 必须与今天逐字等价（防止 content 形态整体改写）。
3. **关闭面**：`WorkspaceMiddleware: false` 时 `@image` 降级且无磁盘读取（§4.2）。
4. **手输兼容**：`@image <path>` 手输路径在 workspace 可用时仍能被模型看到图片（P1 的核心验收）。

### 5.3 验收命令

```bash
# TUI（P0）
cargo test -p peri-tui --lib -- kit::input_area kit::submit_consumer kit::submit_request kit::steer_state
cargo test -p peri-tui --lib
# ACP 契约（P0，零改动也须回归）
cargo test -p peri-acp --lib -- dispatch::prompt
cargo test -p peri-acp-types --lib -- session::user_input
cargo test -p peri-agent --lib -- user_input_mailbox
# middleware（P1）
cargo test -p peri-middlewares --lib -- middleware::image
cargo test -p peri-middlewares --lib -- mcp::resource_tool
# workspace 侧（P1）
cargo test -p peri-mcp-workspace
# 关闭矩阵（ARC-CAPABILITY-CLOSURE-001 指定项）
cargo test -p peri-middlewares --lib -- mcp::builtin_apply
cargo test -p peri-middlewares --lib -- mcp::builtin_runtime
cargo test -p peri-middlewares --lib -- assembly::tests
cargo test -p peri-acp --lib -- host::mcp_v4_builtin
# 装配期 doc 约束（ARC-MIDDLEWARE-CAPABILITY-001 指定项）
cargo test -p peri-agent --doc
# 规模与格式
bash scripts/check-file-size.sh          # 本次触达文件必须 < 1000 行（STD-SIZE-001）
rustfmt --check <changed-rust-paths>
git diff --check
```

> `scripts/check-file-size.sh` 已核实存在（`scripts/`）；本次**未运行**它，扫描口径与本次改动范围的关系见 §8-V5。

---

## §6 分阶段与依赖

### 6.1 P0 — 上传式接线（TUI only）

**目标**：`Ctrl+V` 不再落盘、不再插 `@image` 文本；图片经既有 `session/prompt` / `session/input/enqueue` 上传。

| 项 | 依赖 | 验收 |
| --- | --- | --- |
| T1–T9（§3.1） | 无（协议面已就绪） | §5.3 前三组命令 + `input_area.rs` < 1000 行 |
| 删除 `image.rs` 写盘面（§3.5） | T3/T4 完成 | `rg 'std::fs::write|create_dir_all' peri-tui/src/kit/input_area*` 无写盘命中 |

**风险**：① `SubmitRequest` 加字段会波及 `dispatch_submit_request` 的 `PartialEq` 使用点（如 `matches!(request, SubmitRequest::AgentText(_))`，`peri-tui/src/kit/input_area/submit.rs:26,59`），需改模式匹配形状；② `input_area.rs` 拆分必须与改动同批，否则违反 STD-SIZE-001。

### 6.2 P1 — `@image` 归 MCP（middleware + workspace）

| 项 | 依赖 | 验收 |
| --- | --- | --- |
| W1–W4（§3.4） | 无硬前置；URI scheme 建议先按 §7-Q3 冻结 | `cargo test -p peri-mcp-workspace` |
| M1–M5（§3.3） | W1–W4 可用（或先以 fake Reader 并行开发） | `cargo test -p peri-middlewares --lib -- middleware::image` |
| 删除 `load_image_file`（§3.5） | M1 完成 | `rg 'std::fs|tokio::fs|shellexpand' peri-middlewares/src/middleware/image/` 无命中 |

**依赖说明**：P1 与并行计划 `spec/issues/2026-09-29-workspace-mcp-resources-plan.md`（workspace resources provider，W1–W4 同名编号见其 §8）共享同一个 `read_resource` handler 与 URI 命名表。**建议顺序**：本计划 P1 的 URI 形态由其 W1 冻结一并处理，避免两处各自发明 scheme（§7-Q3）。P1 **不**依赖该计划的 skills/agents/meta 落地。

### 6.3 P0 与 P1 的独立性

- P0 不依赖 MCP，任何时刻可独立交付与回滚（回滚 = 恢复写盘分支，但按「删除优于兼容」不建议预置开关）。
- P1 完成后，`@image` 的手输路径才具备「无 FS 依赖 + 由 workspace 决定根」的能力。
- 二者可并行开发；**不建议**合并为一个 PR（P1 涉及 workspace 与 middleware 两个 crate 的行为变更面）。

### 6.4 与并行/R 相关工作的边界

| 相邻工作 | 关系 |
| --- | --- |
| `2026-09-29-workspace-mcp-resources-plan.md` | 共享 `read_resource` handler 与 URI 命名表；`resources/list` 的过滤/可见性裁决同样适用于 `workspace://image` |
| `2026-09-29-git-watch-workspace-sink-plan.md` | 同属 workspace 实例的 resources/订阅面，改动点相邻（`mcp-packages/workspace/src/workspace.rs`），实施时需串行以免冲突 |
| LSP 下沉 | 无交集 |

---

## §7 未决问题（逐条给推荐，需用户裁决）

| # | 问题 | 推荐选项 | 备选与影响 |
| --- | --- | --- | --- |
| **Q1** | `@image` 的**可读根**默认取什么？ | **推荐 R1**：根 = workspace 实例 cwd；**同时**允许「用户显式输入的绝对路径」经魔数+常规文件+大小上限后放行（保留今天「`@image ~/Desktop/x.png` 可用」的体验），越界不额外报错但记 `tracing` 审计 | R2（纯 cwd 根）：安全最强，**破坏**今天所有「`@image` 指向 cwd 之外」的用法；R3（白名单配置）：灵活但需新增配置键与文档。**裁决影响**：R1/R3 需要 W1 实现两层判定，R2 更简单 |
| **Q2** | tmp 归属是否现在就落地？ | **推荐**：P1 **不引入 tmp**（直读成 blob）；在 W3 只预留「若需暂存，目录由实例决定」的口径 | 若用户要求现在就固定目录（如 `<cwd>/.peri/tmp/images/`），需补生命周期与清理设计（谁删、何时删、崩溃残留） |
| **Q3** | 图片资源 URI 形态与命名 | **推荐 U1**：`workspace://image?path=<percent-encoded>`，并入并行计划的 URI 命名表 | U2：`workspace://image/<base64url(path)>`（无 query，但不可读、调试差）；U3：独立 scheme（如 `peri-image://`）——**新增 scheme 需全局裁决**，且与「workspace 实例决定」的归属表述不一致 |
| **Q4** | 是否复用/重构 `resource_tool.rs` 的 peer 解析 | **推荐**：抽出 `pub(crate)` 的 blob 读函数由 M2 复用（M6） | 复制一份解析逻辑（约 30 行）——可接受但会产生两处漂移 |
| **Q5** | TUI `@image` 展示/预览子系统是否削减 | **推荐**：P0 **不动**（保留手输路径的展示/预览），仅删除「程序生成 `@image` 文本」的写盘面 | 若一并削减 `image_safety`/`image_overlay`/`hits` 面，波及文件多（`message_area/render/user.rs`、`image_action.rs`、`image_overlay.rs`、`hits.rs`、`vm_cache.rs`、`atoms.rs:32`），且会削弱手输路径的安全分级 |
| **Q6** | 上传式图片在**用户气泡**里如何显示？ | **推荐**：P0 沿用现有 `@ N files` footer 计数（`peri-tui/src/kit/input_area.rs:904-909`），提交后气泡只显示文本；**P2 另立**「附件标识/缩略」任务 | 追加 P0 范围会在 P0 引入 TUI 渲染面改动（TUI 当前**不渲染** image block：全仓 `ContentBlock::Image` 的 TUI 侧引用仅 `steer_state.rs:490-491` 的 take-back 反解） |
| **Q7** | 上传式图片的**持久化/体积**策略 | **推荐**：P0 先按「图片块随消息持久化」事实推进，并把「体积与保留策略」记为独立待办；不在本计划内设计裁剪 | 若要求压缩/降采样，需要新增 `CompressorPipeline` 真实实现（当前为空切面，`middleware/image/mod.rs:26,29`）——属于行为增强，超出本次裁决范围 |
| **Q8** | 关闭 workspace 时是否要「提示用户」而非静默降级 | **推荐**：静默降级（占位文本），与既有软降级一致 | 若要求 UI 通知，需在 TUI 侧新增通知通路（middleware 无法直接写 TUI 通知原子） |

---

## §8 未验证假设与验证方法

| # | 假设（本次**未**取得运行时证据） | 验证方法 |
| --- | --- | --- |
| V1 | `session/prompt` 携带 image block 时，真实 provider 收到的请求体确含图片（静态链路已逐跳核对，§1.4） | 起真实 provider 或本地 mock provider，抓取请求体；或 `cargo test -p peri-model --lib -- runtime::request` 家族 + 端到端 e2e |
| V2 | 大图（接近 20MB）经 `MpscTransport` 的 `session/prompt`/`session/input/enqueue` 不触发消息体上限或明显卡顿 | 构造 20MB PNG，跑 TUI 到本地 server 的线路测试，记录耗时与内存 |
| V3 | workspace 实例在 `read_resource` 内做重 I/O（读 20MB 文件）不会阻塞 server 的其它请求 | 并发读取测试（同实例同时跑 `Bash`/`Read`）；确认 rmcp server 的并发模型（需查 rmcp 3.1.4 handler 调度方式） |
| V4 | 图片块进入 transcript 后，Micro/Full Compact 不会丢弃或降级图片 | 复用 `2026-09-20` 的回归手法 + 长会话压缩用例；断言压缩后模型请求仍含 image block |
| V5 | `scripts/check-file-size.sh` 是 STD-SIZE-001 的验收脚本（**已核实存在**：`scripts/check-file-size.sh`），但其输出口径与「只检查改动范围」的差别需复核 | 运行 `bash scripts/check-file-size.sh` 并核对其扫描范围 |
| V6 | `@image` 上传式与 steer 队列的 generation/epoch 语义在附件场景下不产生重复投递 | 跑 `steer_consumer_test` / take-back 恢复用例（附件版） |

**另需复核（行号漂移类）**：本文件所有 `文件:行号` 均在 `feat/mcp-adaptation-v4-part-3` 工作树的某一时刻读取；实施前按符号名复核（尤其 `peri-tui/src/kit/input_area.rs` 与 `peri-middlewares/src/assembly.rs`，两文件有并行改动）。

---

## §9 文档路由（DOC-UPDATE-001 影响面）

| 文档 | 影响 | 动作 |
| --- | --- | --- |
| `docs/reference/mcp-ecosystem.md:258-283` | 新增 `workspace://image` 资源与「blob 由宿主内部消费」的边界说明 | 在 §4.6/§4.7 补一段「宿主内部消费 vs 模型面投影」的区分 |
| `spec/issues/2026-09-29-workspace-mcp-resources-plan.md:170-183` | URI 命名表新增图片资源 | 由 Q3 裁决后并入（跨文件唯一事实源） |
| `mcp-packages/workspace/src/workspace.rs:1-18` 模块文档 | 「exposes exactly one resource」失效 | W4 同步 |
| `docs/standards/architecture-contracts.md:124-128`（ARC-MIDDLEWARE-CAPABILITY-001） | 若 M2/M3 改变了 `ImageMiddleware` 的构造契约，需在 Verify 段补充「注入 Reader 的构造」检查项 | 实施时判断是否需要（**不改链序**，预计无需改 ARC-MIDDLEWARE-001） |
| `2026-09-20-micro-compact-user-image-unavailable.md`（已压缩至 `../history/2026-09.md` 2026-09-20 条目） | 其回归用例在 P1 被重新绑定到 MCP 路径 | 用例改造后同步该 issue 的「涉及文件」 |
| 模块 CLAUDE 文件 | `peri-tui/src/kit/`、`peri-middlewares/src/middleware/image/`、`mcp-packages/workspace/` 若有模块级说明需同步 | 实施时按目录检查 |

---

## §10 一页摘要

- **裁决**：J1（ImageMiddleware 解 FS 依赖，读取/可读根/tmp 归 MCP）+ J2（方向 A：Ctrl+V 上传式；`@image` 文本保留但归 workspace）。
- **关键事实**：三条通路中，① 外部 ACP 上传式**已就绪**；③ steer 队列**已能携带 image blocks**（`EnqueueUserInputRequest.content: MessageContent`），瓶颈仅在 TUI 未把 `PENDING_ATTACHMENTS` 接入 `SubmitRequest`（TUI 主提交路径 `handle_agent_text_submit` 只构造 `MessageContent::text`，`peri-tui/src/kit/submit_consumer.rs:195`）；② 服务端 `ImageMiddleware` 直接 `std::fs` 读盘是 J1 的整改对象。
- **推荐通道**：MCP `resources/read` 的 **BlobResourceContents**。理由：标准能力、rmcp 3.1.4 已支持、workspace 实例**已实现** `read_resource` + resources 能力、宿主已有池级读路径；而工具通道被 `BaseTool::invoke -> String` 与 `invoke_tool_call -> ContentBlock::text` 双重限制（`peri-acp-types/src/tools.rs:549-553`、`mcp-packages/common/src/helpers.rs:58-60`），改造成本与二进制面暴露风险都更高。
- **Seam**：构造期注入（`assemble()` 已持 `Arc<McpClientPool>`，`peri-middlewares/src/assembly.rs:122-129`），**不改** `BeforeInputState`、**不新增**链槽位、**不改**链序（ARC-MIDDLEWARE-001 / ARC-MIDDLEWARE-CAPABILITY-001）。
- **分阶段**：P0（TUI 上传式接线 + 删除写盘面 + `input_area.rs` 按 STD-SIZE-001 拆分）→ P1（workspace `workspace://image?path=…` 资源 + ImageMiddleware 注入 Reader + 删除 FS 读）；P0/P1 可并行，不建议同 PR。
- **必须显式回归**：`test_image_later_input_reaches_model_after_micro_compact`（逐批转换）在 P1 后仍通过。
- **待裁决**：Q1 可读根默认策略、Q2 tmp 是否现在落地、Q3 URI 形态、Q4 是否抽公共 blob 读函数、Q5 TUI 预览面是否削减、Q6 用户气泡如何显示附件、Q7 持久化体积策略、Q8 关闭时是否提示。
