# peri-cf Chat 前端

> 状态：**已批准目标设计**（2026-10-13 三项裁决；2026-10-26 更新：渲染走自有
> `frontend/chat/{timeline,composer,components}`，`frontend/components/assistant-ui/` 只保留 vendored
> 审批/问答卡片，无 `@assistant-ui/*` 运行时依赖）。
> Scope：`npm-packages/peri-cf` 浏览器应用的 Chat 前端——消息流、输入框、会话列表、交互卡片。
> 边界：`@peri-code/sdk` **保持独立**——不新增 UI 依赖、不感知 assistant-ui；对象转换与回调注入只
> 在 `peri-cf/frontend`。设置面只保留只读服务端监控与外壳。
> 相邻设计：[服务端 Agent 执行](peri-cf-server-agent.md)、[聊天对象契约](peri-cf-chat-objects.md)、
> [SDK Chat](peri-sdk-chat.md)；代码定位以 [Peri CF 索引](../code-index/peri-cf.md) 为准。

## 1. 前提与已定基线

1. **渲染层不持有状态权威**。唯一数据源是 `ChatController` 持有的 `ChatProjection`
   （底层 SDK `ChatReplica` 的只读 Yjs 文档对）；展示组件不持久化会话状态、不建立第二套对象规则。
2. **SDK 独立**。`@peri-code/sdk` 主入口与 `/view` 不引入 assistant-ui；展示适配只存在于应用侧。
3. **范围**：消息流、输入框、会话列表替换为自有组件；只读监控入口与外壳保留。凭据在服务端，
   前端没有配置门禁或 localStorage 凭据。
4. **能力对齐现状**：消息编辑、重新生成、附件、语音等未实现能力不启用；UI 特性按 handler 是否提供
   自动开关。
5. **组件源码进项目**：vendored 组件复制进仓库后属本项目代码，可定制；上游升级为手动同步。
6. vendored 卡片只消费 React、`lucide-react` 图标与本地样式常量（`elements/surfaces.tsx`）；上游 `unstable_` 前缀 API 不进入应用代码。
7. **视觉方向（2026-10-20 定稿）**：可交互卡片沿用上游 registry 源码（vendored）；信息流排版与交互密度
   参考 deepseek-harness（MIT，commit 687f282）的 chat 观感——统一消息列、收敛过程行密度、重做 composer
   与消息操作行。仅借鉴观感，不引入其架构或代码。

## 2. 当前数据流

```text
[服务端 SDK ChatService]
    ↓ ChatFactSource（绑定、历史、通知、队列、任务、交互、运行态）
[ChatView] → 一对 Y.Doc → WebSocket HELLO/SNAPSHOT/UPDATE/ACK
    ↓
[浏览器 ChatReplica]（只读；readiness / schema / gap repair）
    ↓
[frontend/remote ChatProjection]（Zod 校验的 ChatDisplayState，缓存随副本更新失效）
    ↓
[frontend/chat/runtime/useChat]（ChatController + TanStack Query 目录缓存）
    ↓
[frontend/chat/{timeline,composer,components}] → Timeline / Composer / 交互卡片 / Sidebar
```

依赖方向单向：展示组件 → `ChatController`/`ChatDisplayState` → SDK `/view`。
反向不存在；SDK 依赖中不出现 assistant-ui。

页面控制器 `frontend/remote/controller.ts` 负责：

- 句柄身份与两步打开历史：`POST /api/chats` → `change_session` 202 → 才连接 WS；
- 绑定代次与动作目标：Action 必须匹配副本当前的 `sessionId + bindingRevision`；
- 动作回执：`send` 的 `inputId`/`accepted` 与投递等待分开；`cancel` 只显示 requested；
- 连接生命周期：切换会话只释放本地连接，不 cancel、不 close 服务端句柄；
- 错误与恢复：`failed`/`unsupported`/`not-ready` 明确区分，不用空历史掩盖版本不支持。

## 3. 依赖与基础设施

- 运行时：React 19、Streamdown（`streamdown`，GFM 内建）渲染 Markdown，`@tanstack/react-query` 缓存目录，
  `@legendapp/list` 虚拟列表，`lucide-react` 图标，`zod` 校验共享 DTO；版本见 `package.json`。
- 样式：Tailwind CSS 4（`@tailwindcss/vite`），`cn` = `clsx` + `tailwind-merge`，`tw-shimmer` 提供运行态 shimmer。
- 主题：`frontend/app/theme.ts` 把 `prefers-color-scheme` 同步为 `<html>` 的 `.dark` / `.light`，
  `index.html` 内联脚本负责首帧；Tailwind 与 chat token（`:root.dark`）共用同一个 class 源。
- `frontend/components/assistant-ui/` 只表示 vendored 来源，目录内只剩 `elements/`（审批卡、问答表单、
  样式常量）；不引用 `@assistant-ui/*`、`@base-ui/react`、`zustand` 等已退出的运行时依赖。
- `frontend/components/ui/` 已删除，需要新组件时按需引入，不要假设该路径存在。
- 样式拆分在 `frontend/styles/`（chat token、消息、Markdown、欢迎页）与 `tailwind.css`；
  应用外壳由组件内的 Tailwind 工具类承担，`styles.css` 只保留设置/监控外壳与全局基础样式。

## 4. 行为对齐

1. **发送**：`ChatComposer` 经 `onSend` → 控制器 `send`；`SendReceipt` 的 accepted 与投递等待分别驱动不同 UI。
2. **停止**：`controller.stop` 只显示 requested；服务端确认停止前不宣称已停止。
3. **会话切换/新建**：线程列表选择 → `openChat`；新建 → `create_new_session`；历史加载两步打开。
4. **历史与刷新**：展示态由 Yjs 副本重建；刷新/重连不恢复旧执行，服务端仍持有执行所有权。
5. **交互**：权限/问题卡片只在副本报告待决交互时出现；回答失败保留原因，不自动重试未知结果。
6. **多标签页**：各标签页独立连接；不跨标签页协调执行。

## 5. 非目标

- 不在浏览器启动 WASM 运行后端，不在前端持有模型/Store 凭据或建立 relay。
- 不从列表伪造运行态；非选中会话不显示全局 running 汇总。
- 不新增编辑、重生成、附件、语音、队列发送等未实现能力。
- 不把 SDK 对象契约复制成第二套应用状态机。

## 6. 验证

- 行为验证：发送、流式、停止、工具卡片（含载荷展开）、推理折叠、交互回答、会话切换与新建、
  刷新/断线恢复、版本不支持与错误态。
- 自动化基线：`frontend/remote/*.test.ts`（Action/WS/两步打开/A-B 隔离）、`frontend/remote/projection.test.ts`
  与 `frontend/chat/runtime/askUserQuestion.test.ts`（投影/问答）、`frontend/chat/**/*.test.tsx`（组件与 DOM）。
- 用户验收：浏览器完成一轮真实对话与一次会话切换；真实模型/Store 未配置时只能标记 blocked。
