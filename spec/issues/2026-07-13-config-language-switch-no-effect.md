# Config 面板语言切换无效，始终显示英文

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-13

## 问题描述

在 Config 面板中将语言从 `English` 切换为 `中文`，面板本身以及整个 TUI 界面都**没有任何变化**，所有文本仍显示英文。

## 症状详情

### 现象 1：切换语言后面板无反应

| 步骤 | 期望 | 实际 |
|------|------|------|
| 1. 打开 Config 面板，语言当前显示 `English` | — | — |
| 2. 按 Space/Enter 切换到 `中文` | 面板标签和所有 TUI 文本变为中文 | 面板显示 `中文` 但所有文本仍为英文 |

### 现象 2：切换失败完全无声

Config 面板中无任何错误提示。`i18n::switch()` 内部遇到不匹配的语言 code 后返回错误，但该错误被 `let _ = ...` 丢弃（`i18n/mod.rs:26`），`LANG_VERSION` 仍然递增，导致组件重渲染后结果与渲染前完全一致。

## 代码定位

**直接根因**：`peri-tui/src/kit/panels/config.rs:52`

```rust
const LANGUAGE_OPTS: &[&str] = &["en", "zh"];
```

用户可选的语言值是 `"en"` / `"zh"`，但 i18n bundle 注册的 key 是 `"en"` / `"zh-CN"`（`i18n/mod.rs:54-55`）：

```rust
bundles.insert("en".to_string(), Self::create_bundle("en", EN_FTL));
bundles.insert("zh-CN".to_string(), Self::create_bundle("zh-CN", ZH_CN_FTL));
```

当选择 `"zh"` 时：
1. `crate::i18n::switch("zh")` → `lc.borrow_mut().switch("zh")`
2. `switch()` 检查 `bundles.contains_key("zh")` → `false`
3. 返回 `Err("unsupported language: zh")`
4. **错误被丢弃**（`let _ = lc.borrow_mut().switch(lang)`，`i18n/mod.rs:26`）
5. `LANG_VERSION` 仍然递增，但 LC 仍在 `"en"`

### 影响范围

| 位置 | 当前值 | 影响 |
|------|--------|------|
| `config.rs:52` LANGUAGE_OPTS | `["en", "zh"]` | 选项 key 不匹配，导致 switch 失败 |
| `config.rs:455` cycle_display_label | `"zh" => ...` | display label 映射也不匹配 |
| `config.rs:381` activate_row ROW_LANGUAGE | `cfg.config.language = Some(new_val.to_string())` | 写入的值可能是 "zh"（无效 key） |
| `i18n/mod.rs:26` | `let _ = lc.borrow_mut().switch(lang)` | 切换失败被静默丢弃 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 打开 Config 面板（Ctrl+O 或对应快捷键）
  2. 移动到 Language 行
  3. 按 Space/Enter 从 `English` 切换到 `中文`
  4. 观察：所有 TUI 文本仍为英文
- **环境**：所有环境

## 涉及文件

| 文件 | 当前状态 | 说明 |
|------|----------|------|
| `peri-tui/src/kit/panels/config.rs:52` | ✅ 已修复 | `LANGUAGE_OPTS` 的 `"zh"` → `"zh-CN"` |
| `peri-tui/src/kit/panels/config.rs:455` | ✅ 已修复 | `cycle_display_label` 中 `"zh"` → `"zh-CN"` |
| `peri-tui/src/i18n/mod.rs:26` | ✅ 已修复 | `let _ = ...` → `tracing::warn!` 记录切换失败 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |
| 2026-07-13 | Open | Fixed | agent | 修复：`"zh"` → `"zh-CN"` 对齐 bundle key，switch 失败加 log |

## 修复记录

### 修复 #1（2026-07-13）

- **操作人**：agent
- **修复内容**：
  1. `config.rs:52` `LANGUAGE_OPTS`: `["en", "zh"]` → `["en", "zh-CN"]`（与 i18n bundle key 对齐）
  2. `config.rs:455` `cycle_display_label`: `"zh"` → `"zh-CN"`（display label 匹配同步修改）
  3. `i18n/mod.rs:26` `switch()`: `let _ = lc.borrow_mut().switch(lang)` → `if let Err(e) = ... { tracing::warn!(...) }`（防止同类问题再次无声）
  4. `config.rs:626` 测试断言同步更新：`Some("zh")` → `Some("zh-CN")`
- **涉及文件**：`config.rs`（3 处）、`i18n/mod.rs`（1 处）
- **测试**：peri-tui kit::panels 29/29 通过
- **验证状态**：待用户手动验证
