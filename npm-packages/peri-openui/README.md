# peri-openui

Peri 的 OpenUI 组件库、懒加载 Renderer 与服务端提示词产物。当前 `private: true`，由
`npm-packages/peri-cf` 通过 `file:` 依赖消费。

## 安装与消费

```bash
cd npm-packages/peri-cf
bun install            # 解析 "peri-openui": "file:../peri-openui"
```

消费端按用途选择出口：

| 出口 | 内容 | 说明 |
| --- | --- | --- |
| `peri-openui` | `splitOpenUI`、`looksLikeOpenUIProgram`、围栏类型、`installOpenUITheme`、`resolveOpenUIThemeKind`、`OpenUIActionEvent` | 轻量；不加载 React Renderer、`@openuidev/react-lang` 或图表 |
| `peri-openui/renderer` | `OpenUIProgram`、`OpenUIErrorBoundary` | 重出口；只应由 `lazy(() => import("peri-openui/renderer"))` 引入 |
| `peri-openui/error-boundary` | `OpenUIErrorBoundary` | 轻量出口；聊天侧需要同步包裹 `Suspense` 时使用，不牵入 Renderer |
| `peri-openui/prompt` | `generateOpenUISystemPrompt()`、`OPENUI_ARTIFACT` | 服务端出口；只读 `spec.json`，不 import React |
| `peri-openui/spec.json` | `{ spec, promptOptions }` 生成产物 | 构建期校验与工具消费 |

```tsx
import { splitOpenUI } from "peri-openui";

const LazyProgram = lazy(() =>
  import("peri-openui/renderer").then((module) => ({ default: module.OpenUIProgram })),
);
```

```ts
import { generateOpenUISystemPrompt } from "peri-openui/prompt";
const prompt = generateOpenUISystemPrompt();
```

`OpenUIProgram` 的文本回退通过 `renderText?: (text: string) => ReactNode` 注入；宿主可传
Markdown 渲染器，缺省按纯文本输出。

## 生成 spec

`src/library.tsx` 是组件、zod props、分组与提示词选项的唯一事实源；`spec.json` 由它生成：

```bash
bun run spec        # 写入 spec.json
bun run check:spec  # 校验 spec.json 是否过期（不通过即失败）
bun test            # library / fence / theme / renderer
bun run typecheck
```

修改 `src/library.tsx` 后必须运行 `bun run spec` 并提交新的 `spec.json`。消费端
`peri-cf` 的 `bun run build` 会先执行 `bun ../peri-openui/scripts/generate.ts --check`。

## 组件来源

`Gauge`、`ChangedFiles` 与 `Sources` 的形态借鉴自 t3code 的 `ContextWindowMeter`、
`ChangedFilesTree`/`DiffStatLabel` 与 citation source 展示。这里只复用信息结构与展示方式，
未复制 t3code 代码，也没有新增第三方运行时依赖；三个组件均为纯数据展示，不包含宿主交互。

## 许可风险

`src/canvas/` 是从 `peri-canvas@0.1.0` 复制改写的 vendored 代码。上游包声明 MIT，但仓库根
没有 LICENSE 文件，且上游自述为 Cursor Canvas SDK 的移植；授权链未核实，仅限 Peri 内部
使用。对外分发、发布或商业化前必须完成法律复核。详见 [PROVENANCE.md](./PROVENANCE.md)。
