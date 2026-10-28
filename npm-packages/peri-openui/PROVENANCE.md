# Vendored canvas provenance

本文件记录 `peri-openui` 包内 vendored canvas（`src/canvas/`）的来源与许可链。canvas 原先位于
`peri-cf/frontend/chat/openui/canvas/`，随 OpenUI 组件库抽取迁入本包；组件库其余部分为本仓库第一方代码。

## 来源

- 上游包：`peri-canvas@0.1.0`
- 上游路径：`/Users/mino/code/remote/canvas/packages/canvas/src`
- 复制日期：2026-10-27
- 复制方式：人工复制后改写；不是 npm 依赖，也不发布

## 许可与授权链

- `packages/canvas/package.json` 声明 `"license": "MIT"`。
- 上游仓库根目录**没有 LICENSE 文件**（只有包级声明）。
- 上游 README 自述为 “Standalone React implementation of the Cursor Canvas SDK”
  （Cursor Canvas SDK 的移植/重实现）。
- 因此**授权链不明**：MIT 声明是否覆盖全部代码、与 Cursor Canvas SDK 原始许可的关系
  均未核实。此风险已由用户明确知晓并接受，仅用于 Peri 内部使用（`peri-openui` 当前
  `private: true`，仅由 `peri-cf` 以 `file:` 依赖消费）。
- 对外分发、发布或商业化前必须完成法律复核；不得在未复核的情况下再分发。

## 保留文件

`hooks.ts`（已精简）、`tokens.ts`、`theme.ts`、`merge-style.ts`、`highlight.ts`、
`callout-tone-icons.tsx`、`ui-primitives.tsx`、`chart-primitives.tsx`、
`form-primitives.tsx`、`diff-view.tsx`、`todo-list.tsx`、`usage-bar.tsx`、`index.ts`。

未复制：`SKILL.md`、`dag-layout.ts`、`host.ts`、`swatch.tsx`、`collapsible-section.tsx`。

## 修改清单

1. `hooks.ts` 精简为进程内 theme store：
   - 删除 `window.__periCanvas`、`PeriCanvasBridge`、`installCanvasBridge`；
   - 删除 canvas 状态持久化、`/canvas/*` fetch、`useCanvasState`、`useCanvasAction`；
   - 只保留 `setHostTheme` / `getHostTheme` / `useHostTheme`。
2. 删除未使用组件与类型：`Swatch`、`TodoListCard`、`IconButton`、`CollapsibleSection`、
   `computeDAGLayout`（文件本身也未复制）。
3. `Pill` 删除无效的 `tone` prop 与 `PillTone` 类型。
4. `DiffView` / `DiffStats` 的硬编码 `#3FA266` / `#FC6B83` 改为
   `useHostTheme().category.green/red`。
5. 图表 `useMeasuredWidth` 增加 `ResizeObserver` 存在性守卫，方便 happy-dom 等环境。
6. 浅色 palette 对比度调整：
   - `foregroundTertiary` `#8A8A8A` -> `#6B6F76`；
   - `foregroundQuaternary` `#B3B3B3` -> `#8A8D93`；
   - 深色保持 `#C7C7C7` / `#A8A8A8`。
7. 标题字号对齐 peri-cf Markdown：h1 21/30、h2 19/28、h3 18/26（正文 14/20 不变）。
8. 每个复制文件顶部加入来源与许可注释。

`ui-primitives.tsx`、`chart-primitives.tsx` 按上游近似原样保留（仅上述定点修改），
因此文件规模高于第一方新文件的常规目标；这是 vendoring 的明确取舍。
