## acp-hub UI 高保真改造计划

### 已确认范围

- 仅修改 `acp-hub/web`。
- 保留现有 WebSocket、Yjs、store、server 和协议行为。
- 不新增文件浏览、文档预览等后端能力。
- 高保真参考截图的 Codex 桌面工作台风格。
- 支持完整响应式布局。
- 不引入新的 UI / icon 依赖，优先使用现有 SolidJS、Tailwind CSS 与内联 SVG。
- 当前基线 `bun run build` 已通过。

---

## 一、当前实现判断

技术栈：

- SolidJS：`acp-hub/web/package.json:13`
- Tailwind CSS v4：`acp-hub/web/package.json:17`
- Vite：`acp-hub/web/vite.config.ts:10`
- Yjs：`acp-hub/web/package.json:14`
- 前端无测试框架，现有验证入口只有 `bun run build`。

现有页面已经具有三区所需的数据和组件：

| 参考图区域 | 现有组件 | 处理方式 |
|---|---|---|
| 左侧导航与会话 | `ConnectCard`、`WorkspaceList`、`InstanceList`、`ChatList` | 重排为全高 sidebar |
| 中间对话 | `ChatHeader`、`MessageList`、`Composer` | 改为独立全高聊天工作区 |
| 右侧工作区 | `StatusRail` | 改为状态与活动详情 rail |
| 全局反馈 | `Toasts` | 保留现有行为，只调整视觉位置 |

当前布局入口是简单的三列 grid：

- 页面标题：`acp-hub/web/src/panel/main.tsx:17`
- 三列布局：`acp-hub/web/src/panel/main.tsx:24`
- 左侧组件：`acp-hub/web/src/panel/main.tsx:25`
- 中间聊天：`acp-hub/web/src/panel/main.tsx:32`
- 右侧状态：`acp-hub/web/src/panel/main.tsx:36`

当前与参考图差距主要是：

1. 页面不是固定视口应用壳。
2. 三栏没有各自独立滚动。
3. 左右栏是卡片堆叠，而不是连续侧栏。
4. 消息区固定为 `52vh`：`MessageList.tsx:167`。
5. Composer 是普通流式卡片：`Composer.tsx:49`，而不是底部固定的大圆角输入区域。
6. assistant 消息仍是气泡形式：`MessageList.tsx:81`，参考图更接近无边框正文。
7. 连接 token 表单占据左栏顶部：`ConnectCard.tsx:7`，不符合参考图的低干扰连接入口。
8. 右栏只能展示现有状态，不能真实复刻文件树和文档预览。

---

# 二、目标布局

## 宽屏：≥ 1280px

采用固定视口三栏：

```text
┌──────────────┬───────────────────────────┬──────────────────┐
│ 左侧导航      │ 中间对话                    │ 右侧状态工作区     │
│ 280px         │ minmax(520px, 1fr)        │ 320px            │
│              │                           │                  │
│ 品牌/工具栏   │ 会话标题栏                  │ 状态标题栏         │
│ 新对话        ├───────────────────────────┤                  │
│ 工作区        │                           │ 连接信息           │
│ 会话列表      │ 独立滚动消息正文             │ 实例列表           │
│              │                           │ ack / 错误         │
│ 用户/连接入口 ├───────────────────────────┤                  │
│              │ 悬浮 Composer              │                  │
└──────────────┴───────────────────────────┴──────────────────┘
```

CSS 结构目标：

```css
height: 100dvh;
overflow: hidden;
grid-template-columns: 280px minmax(0, 1fr) 320px;
```

各栏之间使用细灰边界，不再给每个模块套明显卡片边框。

## 中等宽度：768–1279px

- 左栏保留。
- 中间区填满剩余空间。
- 右栏默认收起，通过 header 按钮打开 overlay drawer。
- drawer 不影响聊天区内部滚动位置。
- 点击遮罩或按 `Escape` 关闭。

## 窄屏：< 768px

- 只显示中间聊天区。
- 左栏和右栏都作为 drawer。
- header 左侧显示导航按钮，右侧显示状态按钮。
- drawer 宽度使用 `min(88vw, 320px)`。
- Composer 始终保持底部可见。
- 消息宽度和左右 padding 缩小。
- 不允许整个 `body` 横向滚动。

---

# 三、视觉设计规范

## 3.1 设计方向

视觉方向定义为 **Quiet Technical Workspace（克制的技术工作台）**：高信息密度、低装饰、清晰层级，依靠留白、细分隔线、浅灰选中态和少量蓝色状态提示建立秩序。目标是贴近参考图中的 macOS/Codex 桌面质感，而不是制作通用后台管理页。

必须遵守：

- 主色调为中性白和暖灰，不使用渐变、玻璃拟态或大面积品牌色。
- 主界面不使用一组组悬浮 dashboard 卡片；三栏本身就是视觉容器。
- 阴影只用于 Composer、popover、drawer 和 toast 等真正浮起的层级。
- 蓝色仅用于焦点、未读点和关键可操作状态，不把普通主按钮全部染蓝。
- 图标统一使用 1.75px 描边的内联 SVG；不得混用 emoji、Unicode 图标和不同风格 icon。
- 保留 `acp-hub` 名称，不复制 Codex 商标、头像或专有图标。

## 3.2 颜色系统

所有颜色在 `src/styles.css` 的 `:root` 中定义，组件不得散落新的近似色值。

```css
:root {
  color-scheme: light;

  /* 基础层 */
  --ui-canvas: #ffffff;
  --ui-sidebar: #f7f7f6;
  --ui-rail: #fbfbfa;
  --ui-surface: #ffffff;
  --ui-surface-subtle: #f4f4f3;
  --ui-surface-hover: #eeeeed;
  --ui-surface-selected: #e9e9e7;
  --ui-scrim: rgb(20 20 19 / 22%);

  /* 文本 */
  --ui-text: #20201f;
  --ui-text-secondary: #60605d;
  --ui-text-muted: #92928e;
  --ui-text-faint: #b4b4b0;
  --ui-text-inverse: #ffffff;

  /* 线条 */
  --ui-border: #e4e4e1;
  --ui-border-strong: #d5d5d1;
  --ui-divider: #ececea;

  /* 强调与语义 */
  --ui-accent: #3488f6;
  --ui-accent-hover: #2378e6;
  --ui-accent-soft: #eaf3ff;
  --ui-success: #26895c;
  --ui-success-soft: #eaf6ef;
  --ui-warning: #a36516;
  --ui-warning-soft: #fff5df;
  --ui-danger: #c74444;
  --ui-danger-soft: #fff0ef;

  /* 交互 */
  --ui-focus-ring: rgb(52 136 246 / 32%);
  --ui-shadow-float: 0 12px 32px rgb(25 25 24 / 10%), 0 2px 8px rgb(25 25 24 / 6%);
  --ui-shadow-popover: 0 16px 42px rgb(25 25 24 / 14%), 0 2px 8px rgb(25 25 24 / 8%);
}
```

语义使用规则：

| 场景 | 背景 | 前景/边框 |
|---|---|---|
| 默认页面 | `--ui-canvas` | `--ui-text` |
| 左侧栏 | `--ui-sidebar` | 右侧 `--ui-border` 分隔线 |
| 右侧栏 | `--ui-rail` | 左侧 `--ui-border` 分隔线 |
| hover | `--ui-surface-hover` | 不额外加边框 |
| selected | `--ui-surface-selected` | `--ui-text`，不使用蓝底 |
| focus | 原背景 | 2px `--ui-focus-ring` 外环 |
| streaming / unread | 透明 | 6px `--ui-accent` 圆点 |
| success | `--ui-success-soft` | `--ui-success` |
| permission | `--ui-warning-soft` | `--ui-warning` |
| error | `--ui-danger-soft` | `--ui-danger` |

不实现暗色主题；本轮高保真目标只覆盖参考图的浅色主题，避免在未提供暗色参考的情况下自行扩展设计。

## 3.3 字体与文字层级

参考图依赖 macOS 原生排版，因此不下载 Web Font，使用系统字体栈以保证中文与英文统一：

```css
--ui-font-sans: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC",
  "Hiragino Sans GB", "Microsoft YaHei", sans-serif;
--ui-font-mono: "SFMono-Regular", "SF Mono", Menlo, Monaco, Consolas,
  "Liberation Mono", monospace;
```

| Token | 字号/行高 | 字重 | 使用位置 |
|---|---:|---:|---|
| `display-sm` | 20px / 28px | 600 | 空状态或重要页标题，谨慎使用 |
| `title` | 16px / 22px | 600 | 会话标题、品牌名称 |
| `body` | 15px / 24px | 400 | 消息正文、主要文本 |
| `body-compact` | 14px / 20px | 400 | 导航项、表单、状态内容 |
| `label` | 13px / 18px | 500 | 分组标题、按钮文字 |
| `caption` | 12px / 17px | 400 | cwd、时间、状态辅助信息 |
| `mono` | 13px / 20px | 400 | ID、ack、错误、工具调用 |

排版约束：

- 消息正文中文每行目标 32–42 个汉字，正文容器最大宽度 `820px`。
- 标题和导航项单行截断，完整值通过 `title` 或 tooltip 查看。
- 消息正文允许换行，不能使用全局 truncate。
- ID、路径和错误使用 mono 字体并允许 `overflow-wrap: anywhere`。
- 不通过全大写制造层级；英文状态值保持数据原样。

## 3.4 尺寸、栅格与间距

使用 4px 基础网格：`4 / 8 / 12 / 16 / 20 / 24 / 32 / 40`。除 1px 边框、图标 optical adjustment 外，不出现任意间距值。

```css
:root {
  --ui-sidebar-width: 280px;
  --ui-rail-width: 320px;
  --ui-header-height: 64px;
  --ui-content-max: 820px;
  --ui-composer-max: 864px;
  --ui-radius-sm: 6px;
  --ui-radius-md: 10px;
  --ui-radius-lg: 16px;
  --ui-radius-composer: 24px;
}
```

| 区域 | 规格 |
|---|---|
| 应用壳 | `100dvh × 100vw`，最小可用宽度 320px，禁止 body 滚动 |
| 左栏 | 280px，内部左右 padding 16px |
| 中间栏 | `minmax(0, 1fr)`，最小桌面宽度 520px |
| 右栏 | 320px，内部左右 padding 16px |
| 顶部栏 | 64px，左右 padding 20px，底部 1px divider |
| 消息滚动区 | 上 24px、下 156px；内容列最大 820px |
| Composer | 最大 864px，距左右至少 20px，距底 20px |
| 导航项 | 最小高度 40px，圆角 8px，水平 padding 10px |
| 紧凑 icon button | 32×32px；触控场景命中区至少 40×40px |
| drawer | `min(88vw, 320px)`，覆盖式，不压缩中间栏 |

宽屏列宽允许通过 CSS custom property 后续调整，但本轮不实现拖拽 resize，避免引入超出参考图的交互和持久化状态。

## 3.5 边框、圆角与阴影

- 栏间分隔：1px `--ui-border`。
- 普通导航项无边框；hover/selected 只改变背景。
- 输入框：1px `--ui-border-strong`，focus 后边框变 `--ui-accent` 并增加 focus ring。
- 小型 surface：8–10px 圆角。
- popover / permission panel：12–16px 圆角。
- Composer：24px 圆角，是中间区唯一显著的大圆角元素。
- 消息正文不加阴影。
- 用户消息只使用 `--ui-surface-subtle` 背景，不加边框和阴影。
- `--ui-shadow-float` 仅用于 Composer；`--ui-shadow-popover` 仅用于 popover、drawer 和 toast。

## 3.6 图标规范

- 统一为 20×20px viewBox、1.75px stroke、`stroke-linecap="round"`、`stroke-linejoin="round"`。
- 默认颜色 `currentColor`，普通状态使用 `--ui-text-secondary`。
- sidebar 顶层操作可用 18–20px；行内辅助操作用 16px。
- icon-only button 必须有 `aria-label` 和 tooltip。
- 发送按钮使用向上箭头，不使用纸飞机；尺寸 40×40px，启用时深灰底白色箭头，禁用时浅灰底灰色箭头。
- 新对话、文件夹、会话、实例、刷新、折叠、关闭、状态等图标均使用项目内内联 SVG；不添加 icon package。

## 3.7 应用壳样式

### 左侧栏

- 背景 `--ui-sidebar`，右侧 1px divider。
- 顶部品牌行高 64px；品牌文字 16/22、600，不使用大 logo。
- “新对话”是 ghost 导航项，而不是蓝色实心 CTA：hover 灰底，左侧 compose icon。
- section label 使用 12px、500、`--ui-text-muted`，上下分别留 20px/8px。
- 中间列表 `min-height: 0; overflow-y: auto`，滚动条默认隐藏、滚动或 hover 时显示 6px 浅灰 thumb。
- 底部连接区使用顶部 divider，背景与 sidebar 相同，不做悬浮卡片。

### 中间工作区

- 背景纯白。
- header 保持低噪声，只展示当前对话标题、必要 meta 和面板开关。
- 消息正文和 Composer 共用水平中心线。
- 长对话滚动时 header 与 Composer 不移动。
- Composer 下方不再出现额外 footer，保留 20px breathing room。

### 右侧栏

- 背景 `--ui-rail`，左侧 1px divider。
- header 与中间栏等高。
- tab 使用文字 + 2px bottom indicator，不使用 segmented control 胶囊。
- 状态分组以 divider 分隔，不使用多张白色卡片。
- ack/error 日志采用紧凑 mono 列表，单项 padding 8px 0。

## 3.8 组件视觉与状态

### 导航项 / 会话项

| 状态 | 样式 |
|---|---|
| Default | 透明背景，主文字 `--ui-text`，副文字 `--ui-text-muted` |
| Hover | `--ui-surface-hover`，120ms 背景过渡 |
| Selected | `--ui-surface-selected`，主文字 500；不加左侧蓝条 |
| Streaming | 行尾 6px 蓝点；selected 时仍保留 |
| Terminal | 主文字 `--ui-text-secondary`，状态副文案显示“已结束” |
| Focus | 2px focus ring，不能只依赖 hover |

每项主行 20px、副行 17px；总高度 52–58px。删除/关闭按钮默认透明，父项 hover 或 `:focus-within` 时出现。

### ChatHeader

- 标题 16/22、600，最大宽度按可用空间截断。
- meta 12/17、`--ui-text-muted`，与标题垂直间距 2px。
- header action 使用 32px ghost icon button。
- ACP session popover 宽 336px，最大高 420px，12px padding，12px 圆角。
- popover 当前项使用 selected 灰底和“当前”小标签，不使用重色边框。

### 消息

**assistant**：

- 左右不使用气泡容器。
- 15px / 25px 正文，文字 `--ui-text`。
- 不重复显示 `assistant` 标签；时间与状态仅在 hover/focus 或消息异常时显示。
- 段落间距 12px，代码/工具块间距 10px。

**user**：

- 右对齐，最大宽度 72%，背景 `--ui-surface-subtle`。
- 16px 圆角，padding `12px 16px`。
- 文字仍为深色，不使用蓝底白字。

**system**：

- 居中，12px caption，背景 `--ui-surface-subtle`，pill 圆角。
- 最大宽度 70%，不与 assistant 正文争夺视觉层级。

**reasoning**：

- 默认折叠；summary 13px、`--ui-text-secondary`。
- 展开后使用左侧 2px `--ui-divider` 引导线，而不是完整边框卡片。
- 内容 mono 12/19，背景透明。

**tool call / resource**：

- `--ui-surface-subtle` 背景，10px 圆角，padding 10px 12px。
- 顶行展示名称和状态，详情使用 mono。
- success/warn/error 只给状态文字着色，不给整块强色背景。

**错误消息**：

- `--ui-danger-soft` 背景，左侧 3px `--ui-danger`，12px 圆角。
- 错误原文可复制，不能只给笼统提示。

### PermissionBar

- 位于消息滚动区域顶部，宽度与消息正文一致，`position: sticky; top: 12px`。
- `--ui-warning-soft` 背景，1px 淡化 warning 边框，16px 圆角。
- 标题 14/20、600；描述 13/19。
- “允许”为深灰实心按钮，“拒绝”为普通 outline/ghost 按钮，避免把安全决策设计成绿色诱导操作。
- 两个按钮同等可见，顺序保持现有“允许 / 拒绝”。

### Composer

结构分为输入区和底部工具行：

```text
┌─────────────────────────────────────────────────────┐
│ 输入消息…                                            │
│                                                     │
│ ＋  [模型] [effort]                   [上下文]  [↑]   │
└─────────────────────────────────────────────────────┘
```

- 背景 `--ui-surface`，1px `--ui-border-strong`。
- 默认 `--ui-shadow-float`；`:focus-within` 时边框转 accent，但阴影不增强。
- textarea 最小高 52px、最大高 180px，15/23，padding `16px 18px 8px`。
- textarea 无自身边框、无 resize handle、背景透明。
- 工具行高 44px，padding `4px 10px 8px 14px`。
- 模型、effort、上下文使用 12px muted 文本；空间不足时依次隐藏“上下文”、effort 文案，但发送按钮始终保留。
- 发送按钮 40px 圆形；可发送时 `#252524`，hover `#111110`。
- disabled Composer 降低到 72% 对比度，但 placeholder 仍需满足可读性。

### 表单与按钮

- 普通输入高度 36px，圆角 8px，padding 0 10px。
- 主按钮高度 36px，背景 `#252524`、白字；hover `#111110`。
- 次按钮透明或白底，边框 `--ui-border-strong`。
- destructive action 默认 ghost，hover 才出现 danger soft 背景。
- disabled 使用 `opacity: .45; cursor: not-allowed`，不能移除按钮导致布局跳动。

### Badge

- 高度 20px，padding 0 7px，圆角 999px。
- 字号 11px，字重 500。
- neutral badge 采用灰底；语义 badge 采用对应 soft 背景与深色文字。
- badge 不使用全彩实心底。

### Toast

- 宽度 `min(360px, calc(100vw - 32px))`。
- 白底、1px border、12px 圆角、popover shadow。
- 左侧可使用 3px 语义色标识，不使用整块红/绿背景。
- 进入/离开只做 `opacity + translateY(4px)`，持续 160ms。

## 3.9 空状态与异常状态

必须设计以下非 happy path：

| 状态 | 表现 |
|---|---|
| 未连接 | 中间区保留布局；显示“连接 acp-hub 后开始”及打开连接设置按钮 |
| 已连接但无 workspace | 左栏 workspace 区显示一行 muted 提示和“新建工作区” |
| workspace 无会话 | 消息区居中显示“开始一个新对话”，提供新对话按钮 |
| 未选择对话 | header 显示“未选择对话”，Composer disabled |
| 历史终态对话 | header 标注“已结束”，Composer disabled，不隐藏历史消息 |
| ack 为空 | 右栏显示“暂无活动”，不保留空白卡片 |
| error 为空 | 显示轻量“无最近错误”状态 |
| 超长 ID/路径 | 单行区域截断；日志和详情区域 anywhere 换行 |
| 连接中 | 连接状态旁 12px spinner；按钮保持原宽度防止跳动 |

空状态图形只用简单线性 icon，不制作插画，保持工作台的克制感。

## 3.10 响应式细则

| 断点 | 布局 | 细节 |
|---|---|---|
| `≥1440px` | 280 / fluid / 320 三栏 | 消息最大 820px，Composer 864px |
| `1280–1439px` | 260 / fluid / 300 三栏 | 左右栏 padding 降为 12px |
| `768–1279px` | 260 / fluid 双栏 | 右栏变 overlay drawer |
| `<768px` | 单栏 | 左右均为 drawer；header 56px |
| `<480px` | 单栏紧凑 | 消息 padding 16px，Composer 距边 10px、底 10px，圆角 20px |

移动端补充：

- 使用 `100dvh` 并给 Composer 容器增加 `padding-bottom: env(safe-area-inset-bottom)`。
- drawer 打开时锁定中间消息区滚动。
- scrim 点击关闭；drawer 初始 focus 落在关闭按钮，关闭后 focus 返回触发按钮。
- 右栏 drawer 从右进入，左栏 drawer 从左进入。
- 不用横向滚动承载三栏。

## 3.11 动效与反馈

整体动效必须近乎无感：

- hover / focus 颜色：120ms `ease-out`。
- drawer：180ms `cubic-bezier(.2,.8,.2,1)`。
- popover：140ms opacity + 4px translate。
- 消息 streaming 不做整块闪烁，只保留现有 LoadingDots。
- 新消息不做大幅滑入动画，以免流式更新造成视觉抖动。
- `prefers-reduced-motion: reduce` 时取消位移与循环动画，只保留即时状态变化。

## 3.12 滚动条

桌面端三处独立滚动：左侧列表、消息区、右侧内容。统一为：

```css
.ui-scrollbar {
  scrollbar-width: thin;
  scrollbar-color: transparent transparent;
}
.ui-scrollbar:hover {
  scrollbar-color: #cececa transparent;
}
```

WebKit 使用 6px thumb、透明 track、999px 圆角；不要永久显示粗滚动条。Composer textarea 自身滚动条沿用同一规则。

## 3.13 高保真验收基准

视觉验收不能只判断“像不像”，需按以下可观察标准检查：

1. 首屏最强视觉元素是中间对话内容和 Composer，而不是彩色按钮或卡片边框。
2. 左右栏能被识别为连续工作区，不能呈现为 dashboard 卡片集合。
3. 除用户消息、工具块和异常状态外，assistant 内容直接排在白色正文面上。
4. 页面同时出现的高饱和蓝色面积不超过可视区域的 1%。
5. 常态下只存在 Composer 一处明显 shadow。
6. 三栏 header 的底边和垂直节奏对齐。
7. 左侧选中项与参考图一致使用浅灰底，而非蓝色描边卡片。
8. 1440px 宽度下正文列不能铺满中间栏；两侧保留明显留白。
9. 390px 宽度下 Composer、drawer、权限按钮均无截断或横向溢出。
10. 所有 hover-only 操作也能通过键盘 focus 显示和触发。

---

# 四、具体实施步骤

## 1. 建立应用壳与响应式状态

### 修改

`acp-hub/web/src/panel/main.tsx`

### 内容

- 删除当前页面级标题和普通 grid。
- 引入两个纯 UI signal：
  - `leftPanelOpen`
  - `rightPanelOpen`
- 建立：
  - `AppShell`
  - `NavigationSidebar`
  - `ConversationWorkspace`
  - `StatusSidebar`
- 增加移动端 drawer 遮罩。
- 支持 `Escape` 关闭 drawer。
- 使用正确的语义结构和 `aria-label`。
- drawer 状态只属于布局，不进入 `store.ts`。

### 验证

- 不改动任何 store action 的调用关系。
- 宽屏三栏稳定。
- 中窄屏 drawer 能打开和关闭。
- 切换面板不会重新创建聊天状态。

---

## 2. 建立统一视觉 token

### 修改

`acp-hub/web/src/styles.css`

### 内容

使用 CSS custom properties 定义有限的视觉 token：

- 背景：
  - `--app-bg`
  - `--sidebar-bg`
  - `--surface`
  - `--surface-muted`
- 边框：
  - `--border-subtle`
  - `--border-strong`
- 文本：
  - `--text-primary`
  - `--text-secondary`
  - `--text-muted`
- 交互：
  - `--accent`
  - `--hover`
  - `--selected`
- 尺寸：
  - sidebar width
  - rail width
  - header height
  - composer radius

同时设置：

```css
html,
body,
#app {
  height: 100%;
}

body {
  margin: 0;
  overflow: hidden;
}
```

其他规则：

- 继续使用系统字体，贴近参考图。
- 正文字号以 14–16px 为主。
- 删除全局 `body` 的 slate 大背景感。
- 降低全局 `code` 背景对比度。
- 添加统一 focus-visible ring。
- 尊重 `prefers-reduced-motion`。
- 不新增 Tailwind config。

### 验证

- 无整页滚动条。
- 所有按钮键盘聚焦状态清晰。
- 中文和等宽代码字体显示正常。

---

## 3. 重构左侧导航

### 修改

- `acp-hub/web/src/panel/components/Lists.tsx`
- `acp-hub/web/src/panel/components/ConnectCard.tsx`
- `acp-hub/web/src/panel/main.tsx`

### 左栏信息顺序

1. 顶部品牌栏：`acp-hub`
2. “新对话”主操作
3. 工作区导航
4. 在线实例
5. 当前工作区会话列表
6. 底部连接状态与设置入口

### `WorkspaceList`

- 去除外层白色卡片。
- “全部”作为普通导航项。
- 工作区使用文件夹图标、名称和 cwd 副标题。
- 当前项使用低对比灰色 selected 背景。
- 删除动作只在 hover/focus 时显示。
- 新建工作区改为紧凑展开表单。
- 保持以下行为完全不变：
  - `setSelectedWsId`
  - `createWorkspace`
  - `removeWorkspace`

### `InstanceList`

- 从独立卡片改为可折叠的导航分组。
- 在线状态用小圆点表示。
- 保留 hostname 和状态数据。
- 数量放到 section header 尾部。

### `ChatList`

- 成为左栏主要可滚动列表。
- 对话标题作为主文本。
- cwd / status / 短 ID 作为副文本。
- 当前选中项使用参考图中的浅灰圆角背景。
- 蓝点用于 active/streaming 状态提示。
- 将新建、new session、cancel、close 操作改为图标按钮，但保持原 handler。
- 危险操作保留 tooltip 和明确的 accessible name。

### `ConnectCard`

参考图没有大型 token 卡片，因此调整为左栏底部连接 popover：

- 默认只显示连接状态和设置按钮。
- 点击后展开 token 输入及连接/断开动作。
- token 仍然是 password 输入。
- 不显示 token 内容，不改变 sessionStorage 行为。
- CLI 签发说明放在展开区域内。
- 不修改 `connect`、`disconnect` 和 `tokenInput`。

### 边界

不得修改：

- `store.ts` 中连接逻辑。
- token 保存策略。
- workspace / chat 过滤逻辑。
- 任何 server API。

---

## 4. 重构中间聊天工作区

### 修改

`acp-hub/web/src/panel/components/ChatView.tsx`

### 内容

让 `ChatView` 成为真正的三段式 flex column：

```text
ChatHeader     固定
MessageList   flex: 1; min-height: 0
Composer      固定在底部
```

建议结构：

```tsx
<section class="flex h-full min-h-0 flex-col">
  <ChatHeader />
  <MessageList />
  <Composer />
</section>
```

`MessageList` 自己承担滚动，ChatView 与 body 不滚动。

---

## 5. 改造聊天标题栏

### 修改

`acp-hub/web/src/panel/components/ChatHeader.tsx`

### 内容

- 从卡片变成固定高度顶部 toolbar。
- 左侧显示会话标题和 workspace/cwd 元数据。
- 标题旁保留 ACP session 切换入口。
- 右侧加入：
  - 左栏开关（中窄屏）
  - session 历史入口
  - 右栏开关
- session tooltip 改为贴近参考图的圆角浮层。
- 当前 session、历史 session 和刷新状态更清晰。
- 保留：
  - 点击外部关闭
  - `Escape` 关闭
  - `refreshSessions`
  - `openAcpSession`
- 不把当前 UI signal 移进全局 store。

### 必要接口调整

为避免组件读取全局布局状态，可以给 `ChatHeader` 增加少量 UI props：

```ts
type ChatHeaderProps = {
  onOpenNavigation?: () => void;
  onOpenStatus?: () => void;
};
```

这是纯 UI seam，不涉及协议。

---

## 6. 改造消息呈现

### 修改

`acp-hub/web/src/panel/components/MessageList.tsx`

### 内容

删除固定高度：

```tsx
h-[52vh]
```

替换成：

```tsx
min-h-0 flex-1 overflow-y-auto
```

消息内容采用居中正文列：

- 最大宽度约 `760–840px`
- 宽屏水平居中
- 小屏使用 16px padding
- assistant：
  - 去掉白色边框气泡
  - 使用普通正文布局
- user：
  - 保留右对齐浅灰圆角气泡
  - 不再使用大面积蓝底
- system：
  - 使用紧凑居中提示条
- reasoning：
  - 使用灰色折叠面板
- tool call / resource：
  - 使用独立浅灰信息块
- error：
  - 保留醒目的红色语义，但降低整体饱和度
- streaming：
  - 保留现有 LoadingDots 和自动吸底机制

权限请求：

- 放在消息区顶部 sticky 区域。
- 保留 allow / deny handler。
- 增加明显但不刺眼的 amber 状态。
- 窄屏按钮保持足够触控尺寸。

### 不改变

- 消息顺序。
- reasoning / text / toolCalls / resources 数据模型。
- 自动吸底算法。
- permission decision 值。
- Yjs 数据读取。

---

## 7. 改造底部 Composer

### 修改

`acp-hub/web/src/panel/components/Composer.tsx`

### 目标外观

参考截图中的大圆角悬浮输入框：

- 居中，最大宽度与消息正文一致。
- 约 20–24px 圆角。
- 低对比 border。
- 柔和 shadow。
- 上方可输入内容。
- 下方 toolbar 显示模型、effort、上下文和发送按钮。
- 发送按钮为圆形箭头。
- disabled 状态清晰。

### 输入行为

当前使用单行 `<input>`，高保真目标应换为 `<textarea>`：

- `Enter` 发送。
- `Shift+Enter` 换行。
- 自动扩展到有限高度。
- 超过最大高度后内部滚动。
- disabled 和 placeholder 逻辑不变。
- `sendMessage(text)` 不变。

需要注意：从 input 改为 textarea 属于 UI 交互调整，但不涉及数据能力。

### 数据保留

- `model()`：`Composer.tsx:30`
- `effort()`：`Composer.tsx:31`
- `ctxText()`：`Composer.tsx:34`
- `sendMessage()`：`Composer.tsx:45`

---

## 8. 将右侧改造成状态工作区

### 修改

`acp-hub/web/src/panel/components/StatusRail.tsx`

### 内容

由于本次明确不新增功能，右侧不伪造截图中的文件树和文档预览，而是高保真复用其视觉结构：

1. 顶部 tabs：
   - “状态”
   - “活动”
2. 状态摘要：
   - 连接
   - registry
   - heartbeat
   - subscriptions
3. 实例信息可继续留在左侧；右侧不重复。
4. 最近 ack。
5. 最近错误。
6. 空状态文案。
7. 各 section 独立折叠。
8. 右栏整体独立滚动。

参考图右侧的“文件树层级视觉”可用于表现状态分组，但内容必须来自现有数据。

### 保留

- `connState`
- `globalStatus`
- `heartbeatCount`
- `subscribedDocs`
- `ackLog`
- `errorLog`

不改变任何状态含义。

---

## 9. 统一辅助组件视觉

### 可能修改

- `acp-hub/web/src/panel/components/Badge.tsx`
- `acp-hub/web/src/panel/components/Toasts.tsx`

### `Badge`

- 降低高饱和状态色。
- 统一圆角、字号、行高。
- 保持 status → class 的逻辑映射不变。

### `Toasts`

- 宽屏放在右下角。
- 小屏放在顶部居中或底部 composer 上方。
- 避免遮挡 Composer 和 drawer。
- 增加 `pointer-events` 与 stacking context 约束。
- 保留 toast 生命周期。

---

# 五、建议的组件边界

为保持改动小而清晰，建议只新增一个布局组件文件：

```text
src/panel/
├── main.tsx
└── components/
    ├── AppShell.tsx       # 新增：响应式三栏和 drawer
    ├── ChatView.tsx
    ├── ChatHeader.tsx
    ├── MessageList.tsx
    ├── Composer.tsx
    ├── Lists.tsx
    ├── ConnectCard.tsx
    ├── StatusRail.tsx
    ├── Badge.tsx
    └── Toasts.tsx
```

`AppShell.tsx` 只负责：

- 布局。
- drawer 开关。
- overlay。
- 响应式可见性。
- 键盘关闭。

它不得导入 WebSocket/Yjs 逻辑，也不得成为新的业务 store。

如果实施中 `AppShell` 不足约 80 行，则可以直接保留在 `main.tsx`，避免为单次使用过度抽象。

---

# 六、明确不改的文件和契约

原则上不修改：

- `acp-hub/web/src/panel/store.ts`
- `acp-hub/web/src/panel/lib/yjs.ts`
- `acp-hub/web/vite.config.ts`
- `acp-hub/server/**`
- `acp-hub/proto/**`
- 其他 `peri-*` crate
- workspace Rust 配置
- token 安全策略

需要保持的行为：

1. URL token 一次性注入后清理。
2. token 只进入 `sessionStorage`。
3. workspace 选择继续过滤 chats。
4. chat 终态判断不变。
5. ACP session 切换不新建 chat。
6. allow / deny 的值不变。
7. 新建、取消、关闭 action 不变。
8. 消息自动吸底时用户上滚暂停。
9. server 的静态文件 URL 契约仍为 `/`。

---

# 七、实施顺序

## 阶段 1：应用壳

修改：

- `main.tsx`
- `styles.css`
- 可选 `AppShell.tsx`

验证：

- 宽屏三栏。
- 中窄屏 drawer。
- 页面无整体滚动。

## 阶段 2：左栏

修改：

- `Lists.tsx`
- `ConnectCard.tsx`

验证：

- workspace / chat 选择仍正确。
- 所有原有 action 可触发。
- token 不明文显示。

## 阶段 3：聊天区

修改：

- `ChatView.tsx`
- `ChatHeader.tsx`
- `MessageList.tsx`
- `Composer.tsx`

验证：

- 消息区独立滚动。
- 自动吸底。
- session tooltip。
- Enter / Shift+Enter。
- permission 操作。

## 阶段 4：右栏与反馈

修改：

- `StatusRail.tsx`
- `Badge.tsx`
- `Toasts.tsx`

验证：

- 状态数据实时更新。
- ack / error 长内容不撑破布局。
- toast 不遮挡主要操作。

## 阶段 5：视觉收敛

- 对照参考图检查宽度、间距、边框、字体、圆角、阴影。
- 检查 1440px、1024px、768px、390px 四个宽度。
- 检查空状态、未连接、选中会话、streaming、permission、terminal chat。
- 只修正视觉偏差，不扩展功能。

---

# 八、验收标准

## 布局

- [ ] 1280px 以上稳定显示三栏。
- [ ] 中等宽度右栏收进 drawer。
- [ ] 手机宽度左右栏均为 drawer。
- [ ] `body` 不产生非预期滚动。
- [ ] 每个长内容区域独立滚动。
- [ ] Composer 始终可访问。

## 视觉

- [ ] 主体为白色与浅灰层级，而非大量独立边框卡片。
- [ ] 左侧列表密度、选中背景、状态圆点贴近截图。
- [ ] 中间正文列宽和底部 Composer 贴近截图。
- [ ] header、sidebar 和 rail 使用细分隔线。
- [ ] 控件圆角、字体和 shadow 保持一致。
- [ ] 不直接复制 Codex 名称或品牌资源。

## 功能回归

- [ ] 连接和断开正常。
- [ ] token 继续使用 password 输入且不被输出。
- [ ] workspace 新建、选择、删除正常。
- [ ] chat 新建、选择、取消、关闭正常。
- [ ] session 查询、刷新、切换正常。
- [ ] 消息发送与 streaming 正常。
- [ ] permission allow / deny 正常。
- [ ] 状态、ack、错误继续更新。

## 可访问性

- [ ] icon-only button 均有 `aria-label` 或明确文本。
- [ ] drawer 支持 `Escape`。
- [ ] 所有交互控件有 `focus-visible`。
- [ ] 触控按钮尺寸合理。
- [ ] 状态不能只依赖颜色表达。
- [ ] reduced motion 下不使用非必要动画。

---

# 九、验证命令

```bash
cd acp-hub/web
bun run build
```

当前没有现成的前端测试、lint 或视觉回归脚本，因此实现完成后还需人工验证：

```bash
cd acp-hub/web
bun run dev
```

浏览器检查尺寸：

- `1440 × 900`
- `1280 × 800`
- `1024 × 768`
- `768 × 1024`
- `390 × 844`

最终再运行：

```bash
git diff --check
git status --short
```

计划已收敛，可以按上述五个阶段进入实现模式。