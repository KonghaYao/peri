# peri-cf Chat 前端（assistant-ui）

> 状态：**已批准目标设计**（2026-10-13 用户裁决三项）：(1) 组件来源 = assistant-ui 官方 shadcn registry 组件（源码进项目），视觉以官方规范为底；(2) 替换范围 = 全量 chat 区域（消息流 / 输入 / 会话列表）；(3) 流程 = 设计文档先行。
> Scope：`npm-packages/peri-cf` 浏览器应用的 Chat 前端——消息流、输入框、会话列表。
> 边界：设置页（SettingsView）、首次运行门禁与应用外壳保留现有实现；`@peri-code/sdk` **保持独立**——不新增 UI 依赖、不感知 assistant-ui，对接层只存在于应用侧。
> 相邻设计：[peri-cf 浏览器宿主](peri-cf-browser-agent.md)（数据流、生命周期与凭据）、[peri-cf 聊天对象契约](peri-cf-chat-objects.md)（对象契约与展示消费面）。
> 实施进度、未决项裁决与验收只写 [active issue](../../spec/issues/2026-10-13-peri-cf-chat-ui-assistant-ui.md)；代码定位以 [Peri CF 索引](../code-index/peri-cf.md) 为准。

## 1. 前提与已定基线

1. **assistant-ui 是渲染层，不是状态权威**。唯一数据源仍是 SDK 投影（`SyncState` 展示 DTO）；assistant-ui 不持久化会话状态、不建立第二套对象规则。
2. **SDK 独立**。`@peri-code/sdk/{view,portable,wasm}` 不新增依赖、不引入 assistant-ui；对象转换与回调注入（适配层）只在 `peri-cf/frontend`。
3. **全量替换范围**：消息流、输入框、会话列表。设置页、首次配置门禁、topbar 外壳保留。
4. **能力对齐现状**：不为换框架新增产品能力——消息编辑、重新生成、附件、语音等当前未实现的能力不启用（assistant-ui 的 UI 特性按对应 handler 是否提供自动开关）。
5. **组件源码进项目**：registry 组件（Thread / ThreadList / Reasoning / ToolFallback 等）复制进仓库后属本项目代码，可定制；上游升级为手动同步，非黑盒依赖。
6. 适配层只消费 assistant-ui 稳定 API；`unstable_` 前缀 API（按其 stability policy 可随时变更）不进适配层。
7. **视觉方向（2026-10-20 定稿）**：组件结构沿用 assistant-ui registry 源码；信息流排版与交互密度参考 deepseek-harness（MIT，commit 687f282）的 chat 观感——统一消息列、收敛过程行密度、重做 composer 与消息操作行。仅借鉴观感，不引入其架构或代码；细目、验证与已知限制见实施 issue 的「视觉定稿（2026-10-20）」。

## 2. 现状基线（2026-10-13 核查）

| 项 | 现状 | 位置 |
| --- | --- | --- |
| 消息渲染 | 手写 `MessageList`：Entry / EntryBlock → JSX（text / reasoning 折叠 / tool_card） | `frontend/chat/MessageList.tsx` |
| 输入 | 内联 textarea + 发送/停止按钮 + busy 门禁 | `frontend/app/App.tsx` |
| 会话列表 | 手写 sidebar：时间分组、选择、新建、刷新 | `frontend/app/App.tsx` |
| 状态来源 | `SyncState`（Zod 校验的展示 DTO），由 `AgentManager` 状态机维护 | `frontend/shared/sync.ts`、`frontend/agent/manager.ts` |
| 样式 | 手写 `styles.css` 全局类 | `frontend/styles.css` |
| 工具渲染 | `ToolBlock` + 注入式载荷 reader（`PayloadCommandSource` 窄接口） | `frontend/chat/MessageList.tsx`、`frontend/chat/runtime/payloadReader.ts` |
| 测试 | 组件测试直接渲染手写组件 | `frontend/chat/{runtime,components}/*.test.{ts,tsx}` |

## 3. 目标架构

```text
[SDK 内核]（不变；不感知 assistant-ui）
    ↓ /view 只读 DTO
[peri-cf frontend/agent/*]（平台无关：AgentManager、命令通道、投影读取）
    ↓ SyncState（Zod 校验后的展示态）
[新增 适配层  frontend/chat/runtime/adapter]
    convertMessage: EntryDTO[] → ThreadMessageLike[]
    onNew → session.send()     onCancel → session.stop()     isRunning ← phase
    threadList ← Chat[]（ExternalStoreThreadListAdapter，同步内联）
    ↓ useExternalStoreRuntime()
[assistant-ui 组件层]（registry 源码进项目）
    Thread / Message / Composer / Reasoning / Tool UI / ThreadList
```

依赖方向单向：assistant-ui 组件 → 适配层 → `frontend/agent/*` 与 `SyncState`；反向不存在，SDK 依赖中不出现 assistant-ui。

## 4. 对象映射

| 现有对象 | assistant-ui 表示 | 说明 |
| --- | --- | --- |
| `EntryDTO`(role=user) | `ThreadMessageLike { role: "user" }` | 文本块拼 content |
| `EntryDTO`(role=assistant) | `ThreadMessageLike { role: "assistant" }` | blocks 保序转 content parts |
| `block.type="text"` | `{ type: "text", text }` | |
| `block.type="reasoning"` | `{ type: "reasoning", text }` | Reasoning 折叠组件；`visibility="hidden"` 不输出 |
| `block.type="tool_call"` | `{ type: "tool-call", toolCallId, toolName, args, result }` | 工具 UI 自定义渲染；`unavailable` 映射为失败/不可读态 |
| `block.type="unknown"` | 自定义 data part | 保留「不可读原因」展示 |
| `phase` ∈ {streaming, cancelling, blocked} | `isRunning` | |
| `Chat[]` 会话列表 | `ExternalStoreThreadListAdapter.threads` | 选择 → 现有 `selectChat`；新建 → 现有 ACP `session/new` 流程；日期分组读 `lastMessageAt`（随 thread 数据透传，见实施 issue） |
| 工具载荷（`argumentsRef` / `resultRef`） | 工具渲染组件内经现有 `ToolPayloadReader` 读取 | 保持 `PayloadCommandSource` 注入，不新增通道 |
| `interaction`（permission / elicitation） | 未决（§7） | ACP 反向请求无 assistant-ui 对等物 |
| `tasks` / `plans` | 未决（§7） | 现「会话面板」内容 |

消息 id 使用 `EntryView.id` 等稳定标识，保证增量更新不重建消息、滚动位置不跳动。

## 5. 依赖与基础设施

- `@assistant-ui/react`（runtime / primitives / hooks）、`@assistant-ui/react-markdown`（Markdown 渲染）。
- Tailwind CSS + shadcn 基础设施：`components.json`、`cn` 工具、cva / tailwind-merge / clsx。
- registry 组件源码拷入 `frontend/`（Thread / ThreadList / Reasoning / ToolFallback 等及依赖的 shadcn 基础组件）；Vite 增加路径别名以匹配 registry 导入约定。
- 现有 `styles.css`：chat 相关规则随组件退役；设置页与外壳样式保留；Tailwind preflight 与现有全局重置的合并策略在实施中处理。
- React 19 官方支持（无需 `forwardRef` 调整）；版本以实施 issue 记录为准。

## 6. 行为对齐（现有 → 目标）

1. **发送**：`onNew` 经现有 `session.send()`；运行中禁用发送（`isSendDisabled` 与 busy 门禁对齐）。
2. **停止**：`onCancel` → 现有 `session.stop()`；未确认停止仍阻断重跑（沿用现有纪律）。
3. **会话切换 / 新建**：线程列表选择 → 现有 `selectChat()`；新建 → 现有一次性宿主 `session/new` + `session/rename` 流程。
4. **历史复原**：仍按「先建 runtime / 订阅，再触发 `session/load`」纪律；assistant-ui 不参与复原。
5. **刷新与关闭标签页**语义不变：展示态由下一次 `session/load` 重放重建，不恢复旧执行。
6. **多标签页**：各标签页独立，不互相观察。

## 7. 未决项（实施中收敛，裁决记录入 issue）

1. `interaction`（permission / elicitation）承载：候选 A = 保留现有交互表单作为 Thread 外自定义区域；候选 B = 映射为 assistant-ui approval / interrupt UI（需评估可行性后裁决）。
2. `tasks` / `plans` 会话面板：候选 A = 保留现面板；候选 B = 按 assistant-ui 元素概念重做。
3. 用户输入队列（`inputQueue`）与 assistant-ui message queue 能力的映射。
4. 消息分组与 turn 边界（user / assistant turn 的展示合并策略）。
5. 现有组件测试（`interaction-panel.test.tsx`、`messageStatus.test.tsx`）的迁移基线。

## 8. 验证

- 行为验证以替换后真实对话为准：发送、流式、停止、工具卡片（含载荷展开）、推理折叠、会话切换与新建、刷新复原、错误与取消状态。
- 测试基线：现有组件测试迁移或替换；`convertMessage` 转换层为新增单测重点。
- 用户验收：浏览器完成一轮真实对话与一次会话切换，视觉符合官方规范。
