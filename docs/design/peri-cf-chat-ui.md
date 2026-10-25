# peri-cf Chat 前端（assistant-ui）

> 状态：**已批准目标设计**（2026-10-13 三项裁决；2026-10-25 更新为 Yjs 副本数据流）。
> 渲染层与接线已落地，真实浏览器验收仍以 [active issue](../../spec/issues/2026-10-13-peri-cf-chat-ui-assistant-ui.md)
> 的记录为准。
> Scope：`npm-packages/peri-cf` 浏览器应用的 Chat 前端——消息流、输入框、会话列表、交互卡片。
> 边界：`@peri-code/sdk` **保持独立**——不新增 UI 依赖、不感知 assistant-ui；对象转换与回调注入只
> 在 `peri-cf/frontend`。设置页、服务信息与应用外壳保留现有实现。
> 相邻设计：[服务端 Agent 执行](peri-cf-server-agent.md)、[聊天对象契约](peri-cf-chat-objects.md)、
> [SDK Chat](peri-sdk-chat.md)；代码定位以 [Peri CF 索引](../code-index/peri-cf.md) 为准。

## 1. 前提与已定基线

1. **assistant-ui 是渲染层，不是状态权威**。唯一数据源是 `ChatController` 持有的 `ChatProjection`
   （底层 SDK `ChatReplica` 的只读 Yjs 文档对）；assistant-ui 不持久化会话状态、不建立第二套对象规则。
2. **SDK 独立**。`@peri-code/sdk` 主入口与 `/view` 不引入 assistant-ui；适配层只存在于应用侧。
3. **全量替换范围**：消息流、输入框、会话列表。设置页、首次配置门禁、topbar 外壳保留。
4. **能力对齐现状**：消息编辑、重新生成、附件、语音等未实现能力不启用；UI 特性按 handler 是否提供
   自动开关。
5. **组件源码进项目**：registry 组件复制进仓库后属本项目代码，可定制；上游升级为手动同步。
6. 适配层只消费 assistant-ui 稳定 API；`unstable_` 前缀 API 不进适配层。
7. **视觉方向（2026-10-20 定稿）**：结构沿用 assistant-ui registry 源码；信息流排版与交互密度参考
   deepseek-harness（MIT，commit 687f282）的 chat 观感——统一消息列、收敛过程行密度、重做 composer
   与消息操作行。仅借鉴观感，不引入其架构或代码。

## 2. 当前数据流

```text
[服务端 SDK ChatService]
    ↓ ChatFactSource（绑定、历史、通知、队列、任务、交互、运行态）
[ChatView] → 一对 Y.Doc → WebSocket HELLO/SNAPSHOT/UPDATE/ACK
    ↓
[浏览器 ChatReplica]（只读；readiness / schema / gap repair）
    ↓
[core/agent ChatProjection]（Zod 校验的 ChatDisplayState，按 bindingRevision 缓存）
    ↓
[frontend/chat/runtime/useChat]（ChatController + TanStack Query 目录缓存）
    ↓
[assistant-ui 适配层 adapter] → Thread / Message / Composer / Tool UI / ThreadList
```

依赖方向单向：assistant-ui 组件 → 适配层 → `ChatController`/`ChatDisplayState` → SDK `/view`。
反向不存在；SDK 依赖中不出现 assistant-ui。

页面控制器 `frontend/remote/controller.ts` 负责：

- 句柄身份与两步打开历史：`POST /api/chats` → `change_session` 202 → 才连接 WS；
- 绑定代次与动作目标：Action 必须匹配副本当前的 `sessionId + bindingRevision`；
- 动作回执：`send` 的 `inputId`/`accepted` 与投递等待分开；`cancel` 只显示 requested；
- 连接生命周期：切换会话只释放本地连接，不 cancel、不 close 服务端句柄；
- 错误与恢复：`failed`/`unsupported`/`not-ready` 明确区分，不用空历史掩盖版本不支持。

## 3. 对象映射

| 展示对象 | assistant-ui 表示 | 说明 |
| --- | --- | --- |
| `EntryView`(role=user) | `ThreadMessageLike { role: "user" }` | 文本块拼 content |
| `EntryView`(role=assistant) | `ThreadMessageLike { role: "assistant" }` | blocks 保序转 content parts |
| `block.type="text"` | `{ type: "text", text }` | |
| `block.type="reasoning"` | `{ type: "reasoning", text }` | Reasoning 折叠；`visibility="hidden"` 不输出 |
| `block.type="tool_call"` | `{ type: "tool-call", toolCallId, toolName, args, result }` | 工具 UI 自定义渲染；`unavailable` 映射为不可读/失败态 |
| `block.type="unknown"` | 自定义 data part | 保留不可读原因 |
| `running`/`executionBlocked` | `isRunning` / 阻断态 | 运行事实来自副本，不由列表推断 |
| `ChatSummary[]` 目录 | `ExternalStoreThreadListAdapter.threads` | 选择 → `openChat`；新建 → `create_new_session`；分组读 `lastMessageAt` |
| 工具载荷（`argumentsRef`/`resultRef`） | 工具组件经 `ToolPayloadReader` 读取 | HTTP 端点按引用与字节数读取；不新增通道 |
| `interaction`（permission / elicitation / AskUserQuestion） | approval / elicitation 卡片 | 操作回 `answerInteraction`；无表单时明确不可答 |
| `tasks` / `plans` / `inputQueue` | Thread 外折叠面板 | 只读展示，不映射 message queue |

消息 id 使用 `EntryView.id` 等稳定标识，保证增量更新不重建消息、滚动位置不跳动。

## 4. 依赖与基础设施

- `@assistant-ui/react`（runtime / primitives / hooks）、`@assistant-ui/react-markdown`。
- Tailwind CSS + shadcn 基础设施：`components.json`、`cn`、cva / tailwind-merge / clsx。
- registry 组件源码位于 `frontend/components/assistant-ui/` 与 `frontend/components/ui/`；Vite 路径别名
  匹配 registry 导入约定。
- 样式拆分在 `frontend/styles/` 与 `tailwind.css`；chat 样式随组件迁移，外壳与设置页保留。
- React 19 官方支持；版本以实施 issue 记录为准。

## 5. 行为对齐

1. **发送**：`ChatComposer` → `controller.send`；`SendReceipt` 的 accepted 与投递等待分别驱动不同 UI。
2. **停止**：`controller.stop` 只显示 requested；服务端确认停止前不宣称已停止。
3. **会话切换/新建**：线程列表选择 → `openChat`；新建 → `create_new_session`；历史加载两步打开。
4. **历史与刷新**：展示态由 Yjs 副本重建；刷新/重连不恢复旧执行，服务端仍持有执行所有权。
5. **交互**：权限/问题卡片只在副本报告待决交互时出现；回答失败保留原因，不自动重试未知结果。
6. **多标签页**：各标签页独立连接；不跨标签页协调执行。

## 6. 非目标

- 不在浏览器启动 WASM 宿主，不在前端持有模型/Store 凭据或建立 relay。
- 不从列表伪造运行态；非选中会话不显示全局 running 汇总。
- 不新增编辑、重生成、附件、语音、队列发送等未实现能力。
- 不把 SDK 对象契约复制成第二套应用状态机。

## 7. 验证

- 行为验证：发送、流式、停止、工具卡片（含载荷展开）、推理折叠、交互回答、会话切换与新建、
  刷新/断线恢复、版本不支持与错误态。
- 自动化基线：`frontend/remote/*.test.ts`（Action/WS/两步打开/A-B 隔离）、`core/agent/view/*.test.ts`
  （投影/问答）、`frontend/chat/**/*.test.tsx`（组件与 DOM）。
- 用户验收：浏览器完成一轮真实对话与一次会话切换；真实模型/Store 未配置时只能标记 blocked。
