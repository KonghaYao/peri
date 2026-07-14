# AskUserQuestion 面板：多选交互缺失 + 文本超长不换行 + 缺少用户自定义输入

**状态**：Open
**优先级**：中
**创建日期**：2026-07-14

## 问题描述

`AskUserQuestion` 面板有三个问题：

1. **多选交互缺失**：工具的 JSON Schema 和 ACP Broker 均已支持 `multiSelect`，但 TUI 面板交互只实现了单选——Space 键只能切换单个选项，答案也只传单个 label。
2. **文本超长不换行**：问题文本、选项 label、选项 description 使用 `Line::from()` 直接渲染，超长文本会被截断而不是自动换行。
3. **缺少用户自定义输入**：工具定义文档声明 "users can always input custom content"，i18n 中已预留 `ask-user-placeholder` 文本，但面板仅支持从预设选项中选取，不支持用户在选项外自由输入文本。

## 症状详情

### 现象 1：面板多选交互缺失

| 项目 | 详情 |
|------|------|
| 触发方式 | LLM 调用 AskUserQuestion 工具，`multiSelect: true` |
| 期望行为 | 用户可以勾选多个选项（☑/☐ toggle），提交后答案包含多个选中的 label |
| 实际行为 | Space 键只做单选切换（☑→☐），同一问题只能选一个选项；提交后答案只含单个 label |
| UI 渲染 | 已经正确显示 ☑/☐ 符号（`ask_user.rs:272-276`），外观层面无误 |

### 现象 2：文本超长不换行

| 项目 | 详情 |
|------|------|
| 触发方式 | 问题的 question 文本、选项 label 或 description 超出面板可用宽度 |
| 期望行为 | 长文本自动折行显示，所有内容可见 |
| 实际行为 | 超出宽度的文本被截断，用户看不到完整内容 |
| 影响位置 | `ask_user.rs:260`（问题文本）、`:287`（选项 label）、`:290`（选项 description）——均使用 `Line::from()` 直接渲染，无换行处理 |

### 现象 3：缺少用户自定义输入

| 项目 | 详情 |
|------|------|
| 触发方式 | 用户想在预设选项之外输入自定义内容 |
| 期望行为 | 选项列表末尾提供一个「自定义输入」入口，选中后可打开文本输入框输入任意文本 |
| 实际行为 | 只能从预设选项中选择，无法输入自定义文本 |
| 已有基础 | i18n 已预留 `ask-user-placeholder = 输入自定义内容...`（`zh-CN/main.ftl:262`），工具定义描述也声明了 "users can always input custom content" |
| 涉及改动 | TUI 面板需支持内嵌文本输入框（或弹出式输入）；ACP Broker 需支持 `StringPropertySchema`（无 oneOf 约束）以传输自由文本 |
| 当前状态 | ❌ 未实现，作为后续独立 issue 跟踪 |

### 支持现状矩阵

| 层级 | multiSelect 支持 | 文件 |
|------|:--:|------|
| JSON Schema（LLM 端） | ✅ | `peri-middlewares/src/ask_user/mod.rs:91` |
| camelCase 解析（`multiSelect`） | ✅ | `peri-middlewares/src/tools/ask_user_tool.rs:44` |
| ACP Broker（MultiSelectPropertySchema） | ✅ | `peri-acp/src/broker/transport_broker.rs:111-119` |
| TUI 面板交互（Space toggle） | ❌ 只做单选 | `peri-tui/src/kit/panels/ask_user.rs:96` |
| TUI 答案回传（多值） | ❌ 只传单值 | `peri-tui/src/kit/panels/ask_user.rs:363` |

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/panels/ask_user.rs:42` | `answers: Vec<Option<usize>>`——状态结构，多选时需改为 `Vec<Vec<usize>>` |
| `peri-tui/src/kit/panels/ask_user.rs:88-108` | Space 键切换逻辑——当前是单选 toggle，需改为多选 toggle |
| `peri-tui/src/kit/panels/ask_user.rs:142-189` | Enter 提交逻辑——`all_answered` 判定、跳转下一个未答问题、答案提交 |
| `peri-tui/src/kit/panels/ask_user.rs:256-293` | 渲染逻辑——文本超长不换行（question、label、description） |
| `peri-tui/src/kit/panels/ask_user.rs:358-375` | `build_answers_map()`——答案序列化，多选时应传 label 数组 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-14 | — | Open | agent | 创建 |

## 修复记录

### 修复 #1（2026-07-14）— 多选交互 + 文本折行

- **操作人**：agent
- **用户原意**：面板支持 `multiSelect` 多选 toggle，超长文本自动换行显示
- **修复内容**：
  - `answers` 状态从 `Vec<Option<usize>>` 改为 `Vec<Vec<usize>>`，支持多选存储
  - Space 键：多选时 toggle 选项入/出 vec，单选时保持原有替换行为
  - Enter 提交逻辑：`all_answered` 从 `is_some()` 改为 `!is_empty()`
  - `build_answers_map`：多选时返回 label 数组 `[label1, label2]`，单选返回单个 label
  - 新增 `wrap_text()` 函数（CJK 安全，unicode-width），对 question/label/description 超长文本按 80 列折行
  - 新增 8 个 i18n key（en/zh-CN x 4），根据当前问题 `multiSelect` 属性动态切换提示文本
  - 涉及文件：`ask_user.rs`、`locales/en/main.ftl`、`locales/zh-CN/main.ftl`
- **涉及 commit**：待提交
- **验证状态**：待验证

### 待办：用户自定义输入（现象 3）

作为后续独立 issue 跟踪，需改造 TUI 面板支持内嵌文本输入 + ACP Broker 自由文本 Schema。
