# peri-openui 代码索引

状态：现行实现。`npm-packages/peri-openui` 是私有包（`private: true`），由
`npm-packages/peri-cf` 以 `file:../peri-openui` 消费。组件库、Renderer 与服务端提示词从 CF 前端
抽取而来；CF 只保留聊天适配层。使用说明见[包 README](../../npm-packages/peri-openui/README.md)，
消费侧见 [peri-cf](peri-cf.md)。

## 架构速览

`src/library.tsx` 是组件名、zod props、分组、提示词选项与渲染器的唯一事实源；
`scripts/generate.ts` 把它序列化为包根 `spec.json`（`{spec, promptOptions}`）。
服务端经 `src/prompt.ts` 只读该 JSON，调用 `@openuidev/lang-core` 生成系统提示词，不 import React。
浏览器侧 `src/renderer.tsx` 组合 parser、library 与 canvas 组件，是唯一的重量级出口；
`src/index.ts` 只导出围栏切分、主题桥与共享类型，供聊天门面 eager 引入。
`src/canvas/` 是从 `peri-canvas@0.1.0` 复制改写的 vendored 代码，仅限内部使用。

```text
library.tsx ──generate──> spec.json ──prompt.ts──> 服务端系统提示词
     │
     └──renderer.tsx──> OpenUIProgram / OpenUIErrorBoundary（懒加载 chunk）
index.ts ──> splitOpenUI / theme / OpenUIActionEvent（轻量）
```

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 增删组件或改 props | `src/library.tsx` | `openuiLibrary`、`openuiComponentGroups`、`openuiPromptOptions` | 29 组件、四分组；改后必须重新生成 `spec.json` |
| 生成/校验 spec | `scripts/generate.ts` | `writeOpenUISpec`、`checkOpenUISpec`、`serializeOpenUIArtifact` | 输出包根 `spec.json`；`--check` 不一致即失败 |
| 服务端提示词 | `src/prompt.ts` | `generateOpenUISystemPrompt`、`OPENUI_ARTIFACT` | 不传 `cloud`，不 import React；失败直接抛错 |
| 渲染 OpenUI 程序 | `src/renderer.tsx` | `OpenUIProgram`、`OpenUIProgramProps` | parser 缓存、zod 递归校验、失败回退 `renderText` 原文 |
| 渲染异常兜底 | `src/openui-error-boundary.tsx` | `OpenUIErrorBoundary` | `resetKey` 变化重置；输出 `data-openui-fallback="render-error"` |
| 组合组件与画布 | `src/components/`、`src/canvas/` | `components/shared.tsx` 的 `OpenUIActionEnabledContext` | 流式/聊天忙时禁用表单；图表数据不完整显式告警 |
| 围栏切分 | `src/fence.ts` | `splitOpenUI`、`looksLikeOpenUIProgram` | 只认顶层完整围栏；多围栏回退；裸 DSL 仅流结束后识别 |
| 主题桥 | `src/theme.ts` | `installOpenUITheme`、`resolveOpenUIThemeKind` | 读取 `--chat-code-bg`/`--chat-fg`/`--chat-link`，值须为 hex |
| 关闭 devtools | `src/devtools-off.ts` | 副作用模块 | 由 `library.tsx` 在 react-lang 求值前引入 |

## 类型与消费边界

| 功能 | 出口 | 说明 |
| --- | --- | --- |
| 轻量门面 | `peri-openui`（`src/index.ts`） | `splitOpenUI`/类型、`installOpenUITheme`、`OpenUIActionEvent`；不加载 library/canvas |
| Renderer | `peri-openui/renderer` | `OpenUIProgram`、`OpenUIErrorBoundary`；只由懒加载引入 |
| 错误边界 | `peri-openui/error-boundary` | 聊天侧同步包裹 `Suspense`，不牵入 Renderer |
| 服务端 | `peri-openui/prompt` | `generateOpenUISystemPrompt`、`OPENUI_ARTIFACT`；无 React |
| 生成产物 | `peri-openui/spec.json` | `{spec, promptOptions}`；构建期校验输入 |

依赖方向：CF → peri-openui；`peri-openui` 源码不得 import `peri-cf`。Renderer 依赖
`@openuidev/react-lang` 与 `react`（peer）；prompt 只依赖 `@openuidev/lang-core` 与 `spec.json`。
`OpenUIProgram` 的文本回退通过 `renderText?: (text: string) => ReactNode` 注入，缺省纯文本；
聊天侧传 `StreamdownText`。

## 回归入口

| 场景 | 测试文件 | 关键用例 |
| --- | --- | --- |
| spec 与 library 同步、29 组件、四分组 | `src/library.test.ts` | 生成产物深度相等；提示词示例可解析；无 Cloud/密钥标记 |
| 围栏边界 | `src/fence.test.ts` | 未闭合、多围栏、嵌套 text 代码块、缩进、CRLF、裸 DSL |
| 主题安装与 token | `src/theme.test.ts` | `.light`/`.dark` 优先、CSS 变量回退、canvas 使用 host token |
| Renderer 行为 | `src/renderer.dom.test.tsx` | 校验失败回退、orphaned、图表告警、表单 action、禁用态、XSS |
| DOM 引导 | `tests/dom.ts` | happy-dom 全局与 `ResizeObserver` 等；与 CF 测试各自独立 |

## 跨模块契约

- 生成产物是跨进程契约：CF 的 `openui:check` 调用本包生成器；篡改 `spec.json` 必须导致失败。
- 服务端只读 `spec.json`，不得 import `src/renderer.tsx` 或 React。
- 浏览器构建通过 CF `vite.config.ts` 的 `resolve.dedupe` 共用一份 React；Bun 测试通过 CF
  `bunfig.toml` 的 test preload（`frontend/test/react-dedupe.ts`）做同一去重。
- 验证命令：`bun install`、`bun run check:spec`、`bun test`、`bun run typecheck`（在本包目录）。
