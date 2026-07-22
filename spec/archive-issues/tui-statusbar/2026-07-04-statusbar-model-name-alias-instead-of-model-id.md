# 状态栏模型名称字段显示短别名且样式分离，不符合 spec/global/domains/tui/tui-rendering.md §4 设计规范


> 归档于 2026-07-20，原路径 spec/issues/2026-07-04-statusbar-model-name-alias-instead-of-model-id.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-04

## 问题描述

状态栏第一行的 provider/model 字段存在两个与 spec/global/domains/tui/tui-rendering.md §4 设计规范不符的问题：

1. **字段值**：使用短别名 `model_alias`（如 `sonnet`）而非完整模型标识，设计示例为 `claude-code-sonnet`
2. **样式**：`provider`、`/`、`model` 拆成三种不同颜色渲染（muted/dim/text），而规范中 `anthropic/claude-code-sonnet` 表现为一个整体

## 症状详情

| 维度 | spec/global/domains/tui/tui-rendering.md §4 设计规范 | 当前实际表现 |
|------|-------------------|--------------|
| 模型字段 | `anthropic/claude-code-sonnet`（完整模型名） | `anthropic/sonnet`（短别名） |
| 样式 | 整体文本 | provider 用 muted 色、`/` 用 dim 色、model 用 text 色，视觉上不是统一单元 |

### 设计规范引用

spec/global/domains/tui/tui-index.md 第 4 节 StatusBar 区域组件，设计图及能力说明：

> 第 1 行显示 permission mode、cwd basename、provider/model、CPU、MEM。

设计示例：`Auto · perihelion · anthropic/claude-code-sonnet · CPU 12% · MEM 430MB`

### 当前代码实现

`peri-tui/src/kit/status_bar.rs:55-71`：

- `provider_name`：muted 色（`statusbar().muted`）
- `/` 分隔符：dim 色（`statusbar().dim`）
- `model_alias`：text 色（`statusbar().text`，高亮时 BOLD + SLOW_BLINK）

数据来源 `peri-tui/src/kit/service_snapshot.rs:397-411`，`derive_provider_and_model` 返回 `(provider_type, active_alias)`，其中 `active_alias` 是配置中的别名（如 `sonnet`），而非 provider 的实际模型名称。

## 期望改进方向

1. provider/model 字段应统一为一个整体文本，使用单一样式渲染
2. 模型部分应展示可辨识的模型标识（如 provider.model_name() 或 model_id），而非配置层面短别名

## 涉及文件

- `peri-tui/src/kit/status_bar.rs:55-71` —— StatusBar 第一行 provider/model 渲染逻辑
- `peri-tui/src/kit/service_snapshot.rs:397-411` —— `derive_provider_and_model()`，返回 `(provider_type, active_alias)`
- `peri-tui/src/kit/atoms.rs:55-66` —— `ServiceSnapshot` 结构体，`model_alias` 字段

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-04 | — | Open | user | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
