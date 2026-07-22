# E2E 测试更新：工具调用头行后缀显示格式变更

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-20

## 问题描述

commit `e5239171` 重构了 Read/Edit/Write/Glob/Grep 五个工具的 TUI 显示格式——将输出摘要从独立输出行改为头行后缀。现在工具完成后的显示从：

```
● Read (src/main.rs)            ← 旧格式
  ⎿ 47 lines
```

变为：

```
● Read (src/main.rs) — 47 lines  ← 新格式
```

同时 Edit/Write 从 `FORCE_EXPAND_ON_COMPLETE` 移入 `COLLAPSED_BY_DEFAULT`（完成后不再自动展开）。现有 e2e 测试的 judge criteria 和注释仍基于旧格式，需要更新，并新增覆盖新行为的测试。

## 症状详情

### 变更摘要

| 工具 | 旧行为 | 新行为 |
|------|--------|--------|
| Read | 输出行显示行数 | 头行后缀 `— N lines` |
| Edit | 自动展开 + 输出行显示 diff | 默认折叠 + 头行后缀 `— N lines changed · +N · -N` |
| Write | 自动展开 + 输出行显示摘要 | 默认折叠 + 头行后缀 `— N lines changed · +N · -N` |
| Glob | 输出行显示匹配数 | 头行后缀 `— N matches` |
| Grep | 输出行显示匹配数 | 头行后缀 `— N matches` |
| 错误态 | 强制展开 + 输出行 | 不受影响（与旧行为一致） |

### 涉及的现有测试

| 测试文件 | 受影响的断言/criteria | 影响说明 |
|----------|----------------------|----------|
| `edit-diff-display.test.ts` | 注释 "Edit 成功后工具卡片自动展开" | 不再自动展开，注释过时 |
| `edit-diff-display.test.ts` | judge: "Edit 工具的变更应可见" | 变更为头行后缀，非输出行 |
| `tool-output-truncation.test.ts` | Read 输出截断验证 | 输出截断行为本身未变，但摘要显示方式变了，judge 需确认覆盖 |

## 任务范围

### 1. 更新现有测试

- [ ] **edit-diff-display.test.ts**：更新注释（移除 "自动展开" 描述），调整 judge criteria 匹配头行后缀格式
- [ ] **tool-output-truncation.test.ts**：确认 judge criteria 仍能匹配新的 Read 头行后缀显示

### 2. 新增测试

- [ ] **Read 显示行数**：验证 `— N lines` 后缀中的 N 与实际文件非空行数一致
- [ ] **Edit/Write diff 摘要**：验证 `— N lines changed · +N · -N` 格式正确，diff 增减统计准确
- [ ] **Glob/Grep 匹配数**：验证 `— N matches` 后缀中的 N 与实际匹配数一致
- [ ] **错误态不受影响**：验证 Read/Edit/Write/Glob/Grep 错误时头行无后缀，仍然强制展开显示错误信息

## 涉及文件

- `e2e/tests/tool-cards/edit-diff-display.test.ts` —— 现有，需更新
- `e2e/tests/tool-cards/tool-output-truncation.test.ts` —— 现有，需确认
- `e2e/tests/tool-cards/` —— 新增 4 个测试文件
- `peri-tui/src/kit/message_area/render.rs` —— 变更源（参考，不改）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
