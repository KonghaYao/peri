# 2026-10-13 peri-cf Chat 前端接入 assistant-ui

> **历史实现记录，已被 2026-10-26 重构取代。** 本文保留当时的接入过程与验收结论（结论不改写），
> 但下列文件/依赖**已删除**，不要按现行代码查找：`@assistant-ui/react`、
> `@assistant-ui/react-markdown`、`components.json`、`web/chat/adapter.ts`、`MessageThread`、
> `ConversationPanels`、`web/components/ui/`。`web/` 目录已不在仓库中（现为 `frontend/`，消息流由
> `frontend/chat/{timeline,composer,components}` 自绘）。现行设计见
> [peri-cf-chat-ui.md](../../docs/design/peri-cf-chat-ui.md)。

目标设计：[docs/design/peri-cf-chat-ui.md](../../docs/design/peri-cf-chat-ui.md)。
范围：仅 `npm-packages/peri-cf`（`web/` 与 `shared/`；SDK 不改动）。

## 分步

1. **依赖与基础设施**：安装 `@assistant-ui/react`、`@assistant-ui/react-markdown`；接入 Tailwind +
   shadcn 基础设施（`components.json`、`cn`、Vite 别名）；registry 组件源码拷入 `web/`。
2. **适配层**：`web/chat/adapter`——`convertMessage`（EntryDTO → ThreadMessageLike，映射表见设计 §4）、
   回调（`onNew` / `onCancel` / `isRunning`）、`ExternalStoreThreadListAdapter`（现有 `Chat[]`）；
   转换层单测。
3. **消息流替换**：Thread / Message / Composer 接管消息区与输入；工具卡片（含载荷展开）、推理折叠、
   错误与取消状态对齐现有行为（设计 §6）。
4. **会话列表替换**：sidebar 换 ThreadList，保持现有能力（选择 / 新建 / 刷新；不新增 rename /
   delete / archive，除非现有 runtime 已支持）。
5. **未决项收敛**：interaction（permission / elicitation）、tasks / plans 面板、inputQueue、
   turn 分组（设计 §7）——逐项裁决并记录。
6. **收尾**：`styles.css` 清理、组件测试迁移、README / CLAUDE.md / code-index 同步。

## 实施状态

> 以下按当时 `web/` 目录记录；`web/` 现为 `frontend/`，文中的 adapter、`MessageThread`、
> `ConversationPanels`、`*.aui.tsx` 与 assistant-ui runtime 均已删除。

分步（截至 2026-10-14）：

1. **依赖与基础设施：完成。** `@assistant-ui/react` 0.15.x + `@assistant-ui/react-markdown`；`components.json`（style
   `base-nova`）、Tailwind v4（`web/tailwind.css`，主题 token 取自官方 CLI 参照产物）、`@` 别名、registry 源码落盘
   `web/components/{assistant-ui,ui}`、`web/hooks`、`web/lib`。
2. **适配层：完成。** `web/chat/adapter.ts`（`convertMessage` / `promptText` / `threadListAdapter` / `usePeriRuntime`）
   + 14 用例；`onNew`/`onCancel` 接 `session.send`/`session.stop`；不提供编辑、重生成、队列、附件、语音 handler。
3. **消息流替换：完成。** `web/chat/MessageThread.tsx`（Thread 装配 + 工具卡片工厂 + unknown 块渲染器）、
   `web/chat/ToolCard.tsx`（`getExternalStoreMessages` 读原始 Entry 块，载荷仍走注入的 `ToolPayloadReader`）、
   `web/chat/Welcome.tsx`。
4. **会话列表替换：完成。** `web/App.tsx` 的会话区换成 `web/chat/ChatList.tsx` + registry `ThreadList`；数据与操作走
   `ExternalStoreThreadListAdapter`（选择 → `selectChat`，新建 → `selectChat(null)`）。刷新按钮、抽屉开合、焦点管理与
   `cancelling`/`syncing` 期间禁用切换保留在外壳。三项裁决（2026-10-14）：
   - **时间分组**：采用 registry 内建分法（今天 / 昨天 / 更早，按活动时间倒序），替换原手写的 4 组（今天 / 昨天 / 最近 7 天 / 更早）。
     分组读的是 `threadListItem.lastMessageAt`——该字段不在 `ExternalStoreThreadData` 公开类型里，但外部 store 运行期把传入对象
     原样展开进状态；适配层因此显式透传 `chat.updatedAt`（不可解析时省略，退化为运行时顺序且不分组）。
   - **搜索框与「更多」菜单（重命名 / 归档 / 删除）不启用**：runtime 无对应能力，registry 源码已按此收敛。
   - **空态与骨架屏**：首次加载且无数据用 registry 骨架屏，无数据且不在加载时保留原空态文案（产品语义，非 registry 自带）。
5. **未决项收敛：完成（裁决如下）。**
   - `interaction`：候选 B —— 映射到官方 elements `elements-approval-card` / `elements-elicitation-form`（registry 命名带
     `elements-` 前缀；`option-list` / `question-flow` 未采用）；置顶为可操作卡片，`onAnswer` 未注入时不提供操作。
   - `tasks` / `plans` / `inputQueue` / 会话信息 / 交互记录：收进 Thread 外折叠面板 `ConversationPanels.tsx`（位置沿用消息区上方）。
   - 输入队列：只做展示（计数 + 列表），不映射 assistant-ui message queue（现有 runtime 无队列发送能力）。
   - turn 分组：不合并——沿用 SDK 投影的 `entryOrder`，适配层不重排。
   - 工具渲染：走 registry Thread 的 `ToolFallback` 槽位（inline render override），未使用已废弃的 `makeAssistantToolUI`。
6. **收尾：完成。** 删除 `web/chat/{MessageList,Panels}.tsx` 及其测试；`web/styles.css` 231 → 162 行（移除 28 个旧 chat 专用类
   及关联媒体查询；`@keyframes pulse` 保留并注明由 Tailwind `animate-pulse` 消费）；
   全局重置移入 `@layer base`、外壳 token 前缀化（`--shell-muted`），消除对 Tailwind utilities 与主题 token 的覆盖；
   测试迁移（见下）。

测试基线（`bun test`，100 pass / 0 fail）：

- `web/chat/adapter.test.ts`：映射与回调（`convertMessage` 保序、状态映射、工具失败、unknown、threadList 的
  `lastMessageAt` 透传与 `isLoading` 收敛）。
- `web/chat/message-thread.test.tsx`：经真实 runtime 渲染 `MessageThread` 的接线回归（错误可见、hidden reasoning 不出现、
  工具调用进入官方分组、投影顺序、部分 Markdown 与复制动作保留）。
- `web/chat/chat-list.test.tsx`：会话列表接线回归（条目顺序、选中态 `aria-current`、标题兜底、骨架屏与空态互斥、
  禁用时 `inert` 门禁）。日期分组依赖客户端首帧（服务端渲染拿不到当日零点），由浏览器验收覆盖。
- `web/chat/conversation-panels.test.tsx`、`web/chat/payloadReader.test.ts`、`web/chat/interactionForm.test.ts`。
- 已删除：`messageStatus.test.tsx`（旧渲染断言由 `adapter.test.ts` + `message-thread.test.tsx` 替代，载荷读取用例迁至
  `payloadReader.test.ts`）、`interaction-panel.test.tsx`（面板断言由 `conversation-panels.test.tsx` 替代，表单校验用例迁至
  `interactionForm.test.ts`）、一次性探针 `probe.test.tsx`。

验证证据：`bun run typecheck` 0 错误；`bun test` 100 pass / 0 fail；`bunx vite build` 成功（client + worker，含 wasm 资产）；
`GET http://127.0.0.1:5174/` → 200（推进时用户本机 dev server 在运行）。

会话列表的真实浏览器核对（headless Chrome + CDP，一次性探针，dev server 5174 + sqld 8090）：

- 列表渲染出日期分组「昨天」「今天」，条目数随会话增减（1 → 新建后 2），无控制台异常。
- 点击条目 → `aria-current="true"` 落到该条目、topbar 标题跟随切换；`onSwitchToThread` 链路通。
- 点「开启新对话」→ `session/new` 成功、新会话进入列表并成为当前项，无错误横幅。
- 刷新按钮点击后列表不清空、无新增错误。
- 空会话首屏渲染欢迎页与 4 张建议卡，composer 占位符在；侧边栏外壳样式（背景 `rgb(243,244,239)`）未变。

待办 / 已知差异（交给浏览器验收）：

- 时间分组由 4 组（今天 / 昨天 / 最近 7 天 / 更早）变为 3 组（今天 / 昨天 / 更早，registry 内建口径）。
- 新建入口由自绘绿底卡片按钮改为 registry 的 ghost 行按钮（官方视觉），尺寸与旧按钮不同。
- 空回答与 `cancelled` 不再有专属文案（旧「这条回答没有文本内容」「已停止生成」随旧渲染器退役）；状态仍可在 metadata 读到。
- 工具卡片内容默认折叠（官方 ToolGroup 行为），展开后的载荷加载未做 DOM 级测试（项目无 DOM 测试基建）。
- unknown 块渲染器经 `useAssistantDataUI` 注册（effect 内注册），SSR 测试不可见，留浏览器验收。

## 视觉重做（2026-10-15，用户裁决；已被 2026-10-20 定稿取代）

> 以下为早期记录，保留用于追溯；实际实现与验收口径以「视觉定稿（2026-10-20）」为准。

背景：用户以 /workspace 旧会话截图反馈「排版与视觉重做」——留白太大、元素零散、卡片样式怪，整体不像聊天界面（非信息收纳、非内容错误、非加载性能问题）。裁决：向 ChatGPT 观感收敛。

改动（均在 `npm-packages/peri-cf/web/`）：

- **Reasoning 去卡片化**：thread 内改用 ghost 变体（去描边与内边距）；折叠行去掉 Brain 图标，文案中文化（思考中 / 已思考 N 秒 / 思考过程）；根节点底距 mb-4 → mb-3。
- **工具组触发行中文化**：`N tool call(s)` → `N 次工具调用`。
- **会话面板幽灵化**：折叠行去掉 paper 描边卡片，降级为与 reasoning / 工具行一致的弱化内容行（`ConversationPanels.tsx`）。
- **排版收敛**：thread 宽度 44rem → 48rem（对齐 ChatGPT 正文列，ConversationPanels 同步）；composer 圆角 1rem → 1.5rem（user 气泡随动）；消息组底距 mb-14 → mb-8；footer 行距 gap-4 → gap-3。
- **定制记录**：`reasoning.tsx` / `tool-group.aui.tsx` 文件头注释（上游同步时逐项保留）；`thread.aui.tsx` 视觉基线注释指向本节。

验证：`bun run test` 100 pass / 0 fail；`bun run typecheck` 无新增错误（存量仅未跟踪探针 `scripts/probe-session-load.ts` 的 TS2339）；浏览器验收待用户执行（刷新 http://127.0.0.1:8791，核对旧会话消息流与新建会话入口）。

未做（2026-10-20 更新）：空会话欢迎页已完成（`Welcome.tsx` + `web/styles/chat-welcome.css`）；英文工具文案（composer tooltip 等）仍未扩展。

## 视觉定稿（2026-10-20）

> 本节记录当时的 `web/` 实现；`web/` 现为 `frontend/`，`tool-fallback.aui.tsx` / `thread.aui.tsx`
> 等 `*.aui.tsx` 与 assistant-ui runtime 均已删除。

方向：参考 deepseek-harness（MIT，commit 687f282）的 chat 信息流排版与交互密度，保留 assistant-ui registry 结构；只借鉴观感，不引入其架构或代码。

1. **基础层 token**：新增 `web/styles/chat-tokens.css`（Light/Dark、字号/行高、圆角、间距、列宽）；`web/main.tsx` 在 `tailwind.css` / `styles.css` 之后引入，文件内 `@import "./chat-stream.css"`。
2. **消息流**：`web/styles/chat-stream.css`——消息列 `clamp(680px, 64%, 920px)`；用户消息右对齐蓝气泡，Assistant 无气泡、无头像；过程行统一 24px；composer 最大列宽 +32px、圆角 28px；滚动跟随阈值 24px、采样 500ms、仅用户自己发送时 smooth；返回底部按钮 34×34、位于 composer 上方；`prefers-reduced-motion` 覆盖全部新增动效。
3. **Markdown**：`web/styles/chat-markdown.css` + `markdown-text.tsx`——正文 14/24；代码块 sticky header + Copy；行内代码、表格与图片样式；未引入 shiki 或增量解析。
4. **过程行与工具**：`web/styles/chat-tools.css` + `reasoning.tsx` / `tool-group.aui.tsx` / `tool-fallback.aui.tsx` / `surfaces.tsx`——reasoning 默认折叠，运行中取最新完成段落首行；工具组 24px 行；工具 IN/OUT 卡；输出上限 150/224/260px；错误与中断着色。
5. **欢迎页**：`web/chat/Welcome.tsx` + `web/styles/chat-welcome.css`——Peri 品牌欢迎页，全部改用 `chat-welcome-*` 类名。
6. **旧样式清理**：`web/styles.css` 删除旧欢迎页与建议卡死代码，文件 156 → 121 行；旧 `.welcome-*` / `.suggestion-*` 已无运行时代码引用（仅历史文档提及）。
7. **CSS 加载顺序**：修复 `web/main.tsx`，CSS import 移到 `App` 之前；构建后层顺序为 `theme < base < components < utilities`，避免 `@layer components` 被 preflight 反向覆盖。

验证（2026-10-20）：

- `bun run test`：165 pass / 0 fail / 26 files / 545 expects。
- `bun run typecheck`：仅既有 `scripts/probe-session-load.ts(341,39) TS2339`。
- `MISE_OFFLINE=1 MISE_ENV=wasm mise exec -- bun run build` 成功；产物 `index-Bx8cGmvA.js` / `index-CTMXGaUs.css`（CSS 121.1 kB）。
- 预览 `http://127.0.0.1:8791/` 返回 200；headless Chrome 截图 `/tmp/peri-chat-ui-desktop.png`、`/tmp/peri-chat-ui-mobile.png`；首屏被首次配置门禁挡在 `settings-shell`，0 条未捕获 JS 错误。

已知限制：

- headless 新 profile 无法进入欢迎页（首次配置门禁）；欢迎页视觉、真实消息流、滚动、hover 与暗色主题留给用户在浏览器验收。
- `chat-markdown.css` 给 `.aui-thread-viewport` 增加 `container-type`，属跨层耦合；当前功能验证无冲突，后续可评估移入基础层。
- 英文工具文案（composer tooltip 等）未扩展。

## 验收（用户执行）

- 浏览器完成一轮真实对话：发送、流式、停止、工具卡片（载荷展开）、推理折叠正常。
- 会话切换与新建正常；刷新后历史由 `session/load` 重放恢复。
- 视觉符合 assistant-ui 官方规范；`@peri-code/sdk` 依赖中不出现 assistant-ui。
