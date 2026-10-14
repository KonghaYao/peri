# TUI 消息区：过程区形态（修订版 · 基于候选甲）

- 日期：2026-10-14
- 状态：**已定案（第 6 版）**——连接定 1 行台阶（斜线方案弃：单格斜线两端接不齐）；转角字形定**圆角**（`╰╮╭╯`，见下节）。已按本形态落地实现。
- 累计修订：
  1. 连接：进入 / 退出各 1 行台阶，接点落在竖线中心线上。
  2. thought 行左侧只留一条线。
  3. read 行左侧 = icon，不放竖线。
  4. 注记行 = col3 `│` + 正文（你的改法）。
  5. 转角字形定圆角：`╰╮╭╯` 笔画端点在格心，与 `│` 的格心笔画严格对齐；直角（`└┐┌┘`）小样已撤。
- 相关：`spec/issues/2026-10-14-tui-transcript-process-zone-indent.md`（已按本形态改写为 v2 实施事实源）。

列号（0 起；图示 100 列终端，行右缘 = 99）：

- 文本区：col1 竖线，col4 正文。
- 过程区：col1 留空；col3 = 左缘标记（工具行 icon，其余行 `│`）；col6 正文。
- 台阶：进入 `╰─╮`（col1 ╰ / col2 ─ / col3 ╮）→ 块行；退出 `╭─╯` 镜像。

```text
 │  我先确认目标形态再动手。
 ╰─╮
   ✓  Read peri-tui/src/kit/message_area/render/helpers.rs — 113 lines                         0.2s
   │  use crate::kit::message_area::grid::{…};
   │  Thought for 501s · 38 lines
   │  Micro compaction completed (7 messages, ~1974 tokens saved, 0 files, 0 skills)
 ╭─╯
 │  结论如下……
```

## 转角字形（定案：圆角）

```text
 │  我先确认目标形态再动手。
 ╰─╮
   ✓  Read … — 113 lines
 ╭─╯
 │  结论如下……
```

圆角 `╰─╮`（`U+2570 U+2500 U+256E`）/ 退出 `╭─╯`（`U+256D U+2500 U+256F`）由
`render/helpers.rs` 的 `STEP_ENTER` / `STEP_EXIT` 常量承载，改字形只动这两个常量。

## 落定项

1. 转角字形：圆角（上节）。
2. 注记行：col3 `│` + 正文，已写入图。
3. <60 列（Compact/Narrow）：回退现状，无缩进与台阶，逐列与改动前一致。
