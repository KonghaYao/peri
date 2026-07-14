# Model 面板 Enter 切换后状态栏延迟 ~1-2s 才更新

**状态**：Open
**优先级**：中
**创建日期**：2026-07-04

## 问题描述

Model 面板按 Enter 选择 alias（如 Opus → Sonnet）后，面板立即关闭，但状态栏第一行的 provider/model 字段仍然显示旧值，约 1-2 秒后才更新为新值。用户在此期间无视觉反馈，不知道切换是否生效。

## 症状详情

| 时机 | Model 面板 | 状态栏 model_alias |
|------|-----------|-------------------|
| Enter 按下瞬间 | 立即关闭（`close_active_panel`） | **不变**——仍显示旧 alias |
| ~1-2 秒后 | 已关闭 | **更新**为新 alias |

## 复现条件

- **复现频率**：必现（每次通过 Model 面板切换 alias 都会出现）
- **触发步骤**：
  1. 打开 TUI → Ctrl+3 或相应快捷键打开 Model 面板
  2. ↑/↓ 移动光标选择一个与当前不同的 alias
  3. 按 Enter
  4. 面板立即关闭，视线移到状态栏
  5. 状态栏第一行仍显示旧 model_alias
  6. 等待 ~1-2s，状态栏才刷新为新 alias
- **环境**：macOS, TUI 模式

## 涉及文件

- `peri-tui/src/kit/panels/model.rs:95-113` —— Enter 处理：写 `PERI_CONFIG_HANDLE.active_alias`，但未同步写 `SERVICE_SNAPSHOT`
- `peri-tui/src/kit/service_snapshot.rs:68-93` —— 后台轮询 task，间隔 2s；`derive_provider_and_model()` 从 `PERI_CONFIG_HANDLE` 派生
- `peri-tui/src/kit/status_bar.rs:22-98` —— `StatusBarRow1` 订阅 `SERVICE_SNAPSHOT` atom 读取 `model_alias` 显示
- `peri-tui/src/kit/atoms.rs:178-179` —— `SERVICE_SNAPSHOT` atom 定义；`model_alias` 字段

## 数据流分析（现象层面）

```
Enter → PERI_CONFIG_HANDLE.active_alias（即时写入）
     ↘ StatusBar ← SERVICE_SNAPSHOT.model_alias（每 2s 轮询更新）
                          ↑
     spawn_service_snapshot 后台 task（2s 间隔 poll）
```

`model.rs:97-98` 注释已承认此设计：
> "service_snapshot 2s 内派生到 SERVICE_SNAPSHOT.model_alias 让 status bar 同步刷新"

StatusBar 已有 `MODEL_HIGHLIGHT_UNTIL` atom（`atoms.rs:171`）可在切换后提供闪烁高亮反馈，但目前 Model 面板的 Enter 逻辑**未设置该 atom**。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-04 | — | Open | agent | 创建 issue |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
