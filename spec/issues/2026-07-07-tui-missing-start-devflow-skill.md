# TUI 未显示新创建的 start-devflow skill

**状态**：Open
**优先级**：中
**创建日期**：2026-07-07

## 问题描述

新创建的 skill 文件 `.claude/skills/start-devflow/SKILL.md` 没有出现在 TUI 的 skill 列表或可见加载结果中。用户期望在创建该 skill 后，TUI 能识别并展示 `/start-devflow`；实际在 TUI 中没有看到该 skill，说明 skill 加载或展示逻辑可能存在不完善之处。

## 症状详情

| 项目 | 观察到的现象 |
|------|--------------|
| 新增文件 | `/Users/konghayao/code/ai/perihelion/.claude/skills/start-devflow/SKILL.md` |
| 期望表现 | TUI 中可见新 skill，并可通过 `/start-devflow` 触发或被列入 skill 摘要 |
| 实际表现 | TUI 中没有看到该 skill |
| 用户判断 | “所以肯定有问题” |

## 复现条件

- **复现频率**：当前已观察到一次，是否必现待验证
- **触发步骤**：
  1. 创建 `.claude/skills/start-devflow/SKILL.md`
  2. 打开或刷新 Peri TUI
  3. 查看 TUI 中可见的 skills / slash skill 列表
  4. 未看到 `start-devflow`
- **环境**：Peri TUI，本地仓库 `/Users/konghayao/code/ai/perihelion`

## 涉及文件

- `.claude/skills/start-devflow/SKILL.md` —— 用户刚创建但未在 TUI 中显示的 skill 文件

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
