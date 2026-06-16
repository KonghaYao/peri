# Windows 环境下表单输入框 overlay textarea 水平偏移，与静态文字重叠

**状态**：Verified
**优先级**：中
**创建日期**：2026-06-16

## 问题描述

在 Windows 环境下（Windows Terminal），Setup Wizard 和 Config Panel 中的表单输入框使用两层渲染：底层 `Paragraph` 绘制静态文字，上层 `FieldTextarea`（基于 `tui_textarea`）overlay 覆盖编辑。Windows 上 overlay textarea 的水平位置与底层静态文字不重合——textarea 渲染在偏移的位置，导致静态文字和 textarea 文字同时可见，形成双重影。macOS/Linux 无此问题。

## 症状详情

| 维度 | 现象 |
|------|------|
| **触发条件** | Setup Wizard 编辑模式或 Config Panel 中任何文本输入字段聚焦时 |
| **实际行为** | overlay textarea 渲染位置与底层 Paragraph 静态文字水平偏移，两个文字同时可见 |
| **期望行为** | textarea 精确覆盖静态文字，用户只看到可编辑的 textarea 内容 |
| **复现频率** | 必现 |
| **环境** | Windows + Windows Terminal |
| **语言** | 中英文 locale 均受影响 |
| **macOS/Linux** | 无此问题 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 Windows Terminal 中启动 peri-tui
  2. 打开 Setup Wizard（`/setup` 或无配置自动触发），进入 Form → Edit 模式，聚焦任意文本字段（如 Base URL、API Key）
  3. 或打开 Config Panel（`Ctrl+O`），聚焦 text 输入行（如 Threshold、Persona、Tone）
  4. 观察：底层 Paragraph 的静态文字和上层 textarea 文字同时可见，位置有水平偏移
- **环境**：Windows + Windows Terminal

## 涉及文件

- `peri-tui/src/ui/main_ui/popups/setup_wizard.rs:457-480` —— Setup Wizard 表单的 overlay textarea 定位逻辑：通过 `inner.y + line_idx` 确定 Y，`inner.x + x_offset` 确定 X，宽度为 `inner.width - x_offset`
- `peri-tui/src/ui/main_ui/panels/config.rs:305-321` —— Config Panel 的 overlay textarea 定位逻辑：`inner.x + label_width` 确定 X，`inner.y + line_idx` 确定 Y
- `peri-tui/src/app/field_textarea.rs` —— `FieldTextarea` 定义：封装 `tui_textarea::TextArea`，`configure_style()` 设置 POPUP_BG 纯黑背景

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-16 | — | Open | agent | 创建 |
| 2026-06-16 | Open | Fixed | agent | 在 `FieldTextarea::render()` 中先 `Clear` overlay 区域再渲染 textarea，双重保险防止底层静态文字穿透 |
| 2026-06-16 | Fixed | Verified | agent | 用户验证通过。Login Panel 也统一为 overlay textarea 方案，消除内联 `█` 光标 hack |

## 修复记录

| 日期 | 文件 | 变更 | 说明 |
|------|------|------|------|
| 2026-06-16 | `peri-tui/src/ui/main_ui/popups/setup_wizard.rs` | 活跃字段（ProviderId/BaseUrl/ApiKey/alias models）向 `render_field_line` 传空字符串 | 聚焦字段不再渲染静态值文字，仅由 overlay textarea 显示编辑内容。消除静态文字与 textarea 的重叠源。 |
| 2026-06-16 | `peri-tui/src/ui/main_ui/panels/config.rs` | `ROW_THRESHOLD/PERSONA/TONE` 行活跃时 `value_display` 为空字符串 | 同上，Config Panel 中文本输入行聚焦时不再渲染静态值，由 overlay textarea 独占显示。 |
| 2026-06-16 | `peri-tui/src/ui/main_ui/panels/login.rs` | Login Panel Edit/New 模式改为 overlay textarea 方案 | 6 个文本字段（Name/BaseUrl/ApiKey/Opus/Sonnet/Haiku）活跃时 `value_display` 为空，`active_overlay` 记录行索引，Paragraph 渲染后 overlay 调用 `field.render()`；Type 保持内联 toggle。去掉手动 `█` 光标 hack，由 tui_textarea 内置光标接管。与 config.rs / setup_wizard.rs 方案统一。 |

### 验证 #1（2026-06-16）—— 通过

用户确认 Login Panel provider 编辑改用统一 overlay textarea 组件后修复成功，不再有内联文字与光标的不一致问题。
