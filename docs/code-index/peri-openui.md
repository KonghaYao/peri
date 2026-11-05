# OpenUI 代码索引（@peri-code/sdk）

状态：现行实现。OpenUI 以 `@peri-code/sdk` 的源码 subpath 发布
（`npm-packages/@peri-sdk/src/openui`），不单独构建；由 `npm-packages/peri-cf` 以
`file:../@peri-sdk` 消费。组件库、Renderer 与服务端提示词从 CF 前端抽取而来；CF 只保留聊天适配层。
使用说明见 [SDK README](../../npm-packages/@peri-sdk/README.md#openui)，
消费侧见 [peri-cf](peri-cf.md)。

## 架构速览

`src/openui/library.tsx` 是组件名、zod props、分组、提示词选项与渲染器的唯一事实源；
`scripts/generate-openui-spec.ts` 把它序列化为 `src/openui/spec.json`（`{spec, promptOptions}`）。
服务端经 `src/openui/prompt.ts` 只读该 JSON，调用 `@openuidev/lang-core` 生成系统提示词，不 import React。
浏览器侧 `src/openui/renderer.tsx` 组合 parser、library 与 canvas 组件，是唯一的重量级出口；
`src/openui/index.ts` 只导出围栏切分、主题桥与共享类型，供聊天门面 eager 引入。
`src/openui/canvas/` 是从 `peri-canvas@0.1.0` 复制改写的画布组件。

```text
library.tsx ──generate-openui-spec──> spec.json ──prompt.ts──> 服务端系统提示词
     │
     └──renderer.tsx──> OpenUIProgram / OpenUIErrorBoundary（懒加载 chunk）
index.ts ──> splitOpenUI / theme / OpenUIActionEvent（轻量）
```

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 增删组件或改 props | `src/openui/library.tsx` | `openuiLibrary`、`openuiComponentGroups`、`openuiPromptOptions` | 32 组件、四分组；改后必须重新生成 `spec.json` |
| 生成/校验 spec | `scripts/generate-openui-spec.ts` | `writeOpenUISpec`、`checkOpenUISpec`、`serializeOpenUIArtifact` | 输出 `src/openui/spec.json`；`--check` 不一致即失败 |
| 服务端提示词 | `src/openui/prompt.ts` | `generateOpenUISystemPrompt`、`OPENUI_ARTIFACT` | 不传 `cloud`，不 import React；失败直接抛错 |
| 渲染 OpenUI 程序 | `src/openui/renderer.tsx` | `OpenUIProgram`、`OpenUIProgramProps` | parser 缓存、zod 递归校验、失败回退 `renderText` 原文 |
| 渲染异常兜底 | `src/openui/openui-error-boundary.tsx` | `OpenUIErrorBoundary` | `resetKey` 变化重置；输出 `data-openui-fallback="render-error"` |
| 组合组件与画布 | `src/openui/components/`、`src/openui/canvas/` | `components/shared.tsx` 的 `OpenUIActionEnabledContext` | 流式/聊天忙时禁用表单；图表数据不完整显式告警 |
| 围栏切分 | `src/openui/fence.ts` | `splitOpenUI`、`looksLikeOpenUIProgram` | 只认顶层完整围栏；多围栏回退；裸 DSL 仅流结束后识别 |
| 主题桥 | `src/openui/theme.ts` | `installOpenUITheme`、`resolveOpenUIThemeKind` | 读取 `--chat-code-bg`/`--chat-fg`/`--chat-link`，值须为 hex |
| 关闭 devtools | `src/openui/devtools-off.ts` | 副作用模块 | 由 `library.tsx` 在 react-lang 求值前引入 |

## 类型与消费边界

| 功能 | 出口 | 说明 |
| --- | --- | --- |
| 轻量门面 | `@peri-code/sdk/openui`（`src/openui/index.ts`） | `splitOpenUI`/类型、`installOpenUITheme`、`OpenUIActionEvent`；不加载 library/canvas |
| Renderer | `@peri-code/sdk/openui/renderer` | `OpenUIProgram`、`OpenUIErrorBoundary`；只由懒加载引入 |
| 错误边界 | `@peri-code/sdk/openui/error-boundary` | 聊天侧同步包裹 `Suspense`，不牵入 Renderer |
| 服务端 | `@peri-code/sdk/openui/prompt` | `generateOpenUISystemPrompt`、`OPENUI_ARTIFACT`；无 React |
| 生成产物 | `@peri-code/sdk/openui/spec.json` | `{spec, promptOptions}`；构建期校验输入 |

所有 OpenUI subpath 直接发布 TS/TSX 源码，不进入 `dist` 打包；消费端需要能编译源码的打包器
（Vite/Bun），Node 原生 ESM 直接 import 不在支持范围内。
依赖方向：CF → `@peri-code/sdk/openui*`；OpenUI 源码不得 import `peri-cf`。Renderer 依赖
`@openuidev/react-lang` 与 `react`（peer）；prompt 只依赖 `@openuidev/lang-core` 与 `spec.json`。
`OpenUIProgram` 的文本回退通过 `renderText?: (text: string) => ReactNode` 注入，缺省纯文本；
聊天侧传 `StreamdownText`。

## 回归入口

| 场景 | 测试文件 | 关键用例 |
| --- | --- | --- |
| spec 与 library 同步、32 组件、四分组 | `test/openui/library.test.ts` | 生成产物深度相等；提示词示例可解析；无 Cloud/密钥标记 |
| 围栏边界 | `test/openui/fence.test.ts` | 未闭合、多围栏、嵌套 text 代码块、缩进、CRLF、裸 DSL |
| 主题安装与 token | `test/openui/theme.test.ts` | `.light`/`.dark` 优先、CSS 变量回退、canvas 使用 host token |
| Renderer 行为 | `test/openui/renderer.dom.test.tsx` | 校验失败回退、orphaned、图表告警、表单 action、禁用态、XSS |
| Canvas/容器组件 | `test/openui/diff-view.dom.test.tsx`、`test/openui/containers.dom.test.tsx` | DiffStats 布局与格式化；容器 action 与图表数据 |
| DOM 引导 | `test/openui/dom.ts` | happy-dom 全局与 `ResizeObserver` 等；与 CF 测试各自独立 |

## 跨模块契约

- 生成产物是跨进程契约：CF 的 `openui:check` 调用 SDK 生成器；篡改 `spec.json` 必须导致失败。
- 服务端只读 `spec.json`，不得 import `src/openui/renderer.tsx` 或 React。
- 浏览器构建通过 CF `vite.config.ts` 的 `resolve.dedupe` 共用一份 React；Bun 测试通过 CF
  `bunfig.toml` 的 test preload（`frontend/test/react-dedupe.ts`）做同一去重。
- 验证命令（在 `npm-packages/@peri-sdk`）：`bun install`、`bun run openui:check`、
  `bun test test/openui`、`bun run typecheck`。
