# E2E: Glob/Grep 工具头行缺少匹配数后缀

**状态**：Open
**优先级**：中
**类型**：缺陷
**创建日期**：2026-08-06
**来源**：E2E 全量运行（2026-08-06，`e2e/e2e-results-2026-08-06.md` 问题 4）

## 问题描述

`e2e/tests/tool-cards/header-suffix-and-error.test.ts` 失败（104s，未重试）。测试覆盖 Read/Glob/Grep/Write/Edit 头行后缀与错误态无后缀。

- 位置：`tests/tool-cards/header-suffix-and-error.test.ts:171`，`expect(r2.pass).toBe(true)`（Judge `glob-grep`）
- Judge 反馈：Glob/Grep 痕迹存在（check 1 通过），但**工具头行无匹配数后缀**——"Glob 工具头行显示为 'Glob 结果：peri-tui/src/**/*.rs'，未包含匹配数后缀格式 'Glob (pattern: ...) — N matches'"（Grep 同理）

## 现状

Glob/Grep 工具卡头行当前显示 `Glob 结果：<pattern>`，缺少 `— N matches` 匹配数后缀。测试期望格式：

- `Glob (pattern: ...) — N matches`
- `Grep (pattern: ...) — N matches`

其中 N 为至少 1 的正整数。

## 期望改进方向

- Glob/Grep 工具头行包含匹配数后缀，格式与测试/Judge criterion 一致。
- 错误态（工具失败）头行不显示该后缀。

## 验收标准

- [ ] `npm test -- tests/tool-cards/header-suffix-and-error.test.ts` 通过（从 `e2e/` 执行）。
- [ ] 匹配数 N 为实际结果数（至少 1 的正整数）。

## 涉及文件

- `peri-tui/src/kit/acp_events/`、`peri-tui/src/kit/atoms.rs` —— 工具卡头行渲染（待修复时定位）
- `peri-middlewares/src/tools/filesystem/glob.rs`、`grep.rs`、`grep_format.rs` —— 工具结果与元数据
- `e2e/tests/tool-cards/header-suffix-and-error.test.ts` —— 场景测试

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-06 | — | Open | agent | E2E 全量运行失败，创建 issue |

## 修复记录

（由修复 agent 修复阶段追加，创建时留空）

### 2026-08-06 修复（agent）

**根因**：非产品代码缺陷，而是 e2e 测试鲁棒性问题。渲染链路本身正确——`peri-tui/src/kit/message_area/render.rs` 的 `render_generic_tool_card_lines` 对 Glob/Grep 完成态（非 running、非 error、output_summary 非空）生成 `— N matches` 后缀（N = 实际输出行数），且已被 2026-08-02 通过快照（`Glob (pattern: "src/**/*.rs") — 1 matches`）与本次新增单测证实。失败快照（`e2e/recordings/header-suffix-glob-grep.txt`）中 Glob 卡**整体不在屏幕可见区域**：该次运行 agent 回复超长（约 47 行 markdown 表格），turn 完成后消息区吸底，位于 Grep 卡上方的 Glob 卡被挤出 60 行终端视口，Judge 看不到头行遂误判"缺少匹配数后缀"（Grep 卡因紧邻回复仍在屏上，故 check 3/4 通过）。

**修复内容**：

1. `e2e/tests/tool-cards/header-suffix-and-error.test.ts`：阶段 2（Glob/Grep）截图前发送 `Ctrl+Home`（消息区 Global/High 键盘滚动快捷键，见 `focus_router::message_accepts_key`）滚动到消息区顶部，并 `waitFor` `● Glob (` 出现后再截图，确保两张工具卡进入可见区域。后续阶段不受影响（submit 时 LOADING_EPOCH 递增强制滚底）。
2. `peri-tui/src/kit/message_area/render_test.rs`：新增 `test_glob_grep_header_match_suffix` 回归单测——Glob/Grep 完成态头行含 `— N matches`（163 行输出）且含 `Tool (pattern:` 参数，错误态头行无 `matches` 后缀。

**验证结果**：

- `cargo test -p peri-tui --lib`：870 passed, 0 failed（含新增单测）。
- `cargo clippy -p peri-tui --all-targets -- -D warnings`：通过。
- `git diff --check`：通过。
- e2e：`npm test -- tests/tool-cards/header-suffix-and-error.test.ts`（e2e/ 目录）**通过**，耗时 114.6s（4 个 Judge 全部 pass；本次 Glob 卡显示 `● Glob (pattern: "peri-tui/src/**/*.rs") — 206 matches`）。

**修改文件**：

- `e2e/tests/tool-cards/header-suffix-and-error.test.ts`
- `peri-tui/src/kit/message_area/render_test.rs`
- 本 issue 文件
