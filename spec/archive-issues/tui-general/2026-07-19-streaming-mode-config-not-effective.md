# streaming_mode 配置切换无效——"block"和"none"模式未实现，渲染始终为流式


> 归档于 2026-07-20，原路径 spec/issues/2026-07-19-streaming-mode-config-not-effective.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-19

## 问题描述

Config 面板中提供了三种渲染模式选项——`streaming`、`block`、`none`——用户可通过面板切换并持久化到 `~/.peri/settings.json`。但实际消息区渲染逻辑完全不读取 `streaming_mode` 配置，始终以逐 token 流式方式渲染 AI 回复。切换为 `block` 后，LLM 回复仍然逐字蹦出，配置如同"假开关"。

## 症状详情

| 观察项 | 期望 | 实际（修复后） |
|--------|------|------|
| 切换到 `block` 模式后发 prompt | AI 回复以完整句子/段落为单位渲染 | ✅ Markdown 块边界（双换行/标题/代码块/水平线）触发推送，fallback ≥3 行防冻结 |
| 切换到 `none` 模式后发 prompt | 回复生成完成前不显示任何中间内容 | ✅ TextChunk/ReasoningChunk（主+子 agent）+ Bash 输出全部抑制，TurnDone 时一次性展示 |
| 重启后配置持久性 | `streaming_mode` 值正确保留 | ✅ 值正确保留在 settings.json 中 |

`streaming_mode` 字段的完整链路现状（修复后）：

```
~/.peri/settings.json  ← 持久化正常 ✅
    ↓
AppConfig.streaming_mode  ← 解析正常 ✅
    ↓
ConfigPanel 读写  ← UI 正常 ✅
    ↓
current_streaming_mode()（acp_events.rs:34-48）← 即地读取 PERI_CONFIG_HANDLE ✅
    ↓
TextChunk / ReasoningChunk handler 门控（acp_events.rs:235-247, 277-289）✅
    ├─ Streaming → 逐 token 推（原有行为）
    ├─ Block → has_md_block_boundary_since() 块边界推
    └─ None → 跳过，TurnDone 重置 len → 一次性展示
    ↓
Bash tool tick 门控（acp_bridge.rs:88-94）✅
    └─ None 模式跳过 push_view_models
```

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 打开 Config 面板（`Ctrl+F`），切换到第 3 行"渲染模式"
  2. 按 `→` 切换为 `block`
  3. 发送任意 prompt
  4. 观察消息区：AI 回复仍逐 token 流式渲染
- **环境**：所有环境（与模型/OS 无关）

## 涉及文件

- `peri-acp/src/provider/config.rs:152-154` —— `AppConfig.streaming_mode` 字段定义（配置层）
- `peri-tui/src/kit/panels/config.rs:51` —— `STREAMING_OPTS` 常量 + ConfigPanel 读写逻辑（UI 层）
- `peri-tui/src/kit/acp_events.rs` —— `dispatch_and_notify`（ACs事件分发，应在此处或上游做模式切换判断）
- `peri-tui/src/kit/message_area/` —— 消息区渲染管线（`append_text` → `push_view_models` → markdown 解析 → 视口裁剪，应在此处实现 block 模式的缓冲/节流）

## 预期行为

参考用户的反馈，三种模式的预期语义：

| 模式 | 预期行为 |
|------|----------|
| `streaming` | 逐 token 流式渲染（当前行为，默认） |
| `block` | LLM 输出先缓冲，以完整的句子/段落为单位批量推送到消息区渲染，减少闪烁和重绘次数 |
| `none` | LLM 回复完全生成后才一次性展示，过程中不显示任何中间内容 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-19 | — | Open | agent | 创建 |
| 2026-07-20 | Open | Fixed | agent | 实现完成，streaming_mode 全链路生效 |

## 修复记录

**修复日期**：2026-07-19（早于本 issue，实现在 issue 创建前已完成）

**修复方案**：

在 `peri-tui/src/kit/acp_events.rs` 新增：
- `StreamingMode` 枚举（`Streaming`/`Block`/`None`）
- `current_streaming_mode()` —— 从 `PERI_CONFIG_HANDLE` 即地读取，配置热切换即时生效
- `has_md_block_boundary_since()` —— Markdown 块边界检测（双换行/`#`标题/` ``` `代码块/`---`水平线），fallback ≥3 行防冻结
- `BridgeState` 新增 `last_pushed_text_len`、`last_pushed_reasoning_len` 追踪字段

在 `acp_events.rs` 的 `dispatch_and_notify` 中为 4 个流式 handler 加模式门控：
| Handler | Streaming | Block | None |
|---------|:---------:|:-----:|:----:|
| TextChunk（主 agent） | 逐 token 推 | 块边界推 | 不推 |
| TextChunk（子 agent） | 逐 token 推 | 逐 token 推 | 不推 |
| ReasoningChunk（主 agent） | 逐 token 推 | 块边界推 | 不推 |
| ReasoningChunk（子 agent） | 逐 token 推 | 逐 token 推 | 不推 |

在 `acp_bridge.rs` 中为 Bash 工具 tick 加 `None` 模式门控。

TurnDone / SessionReplay / TurnInterrupted 等多处 handler 重置 `last_pushed_text_len` / `last_pushed_reasoning_len` 为 0。

**涉及文件**：
- `peri-tui/src/kit/acp_events.rs`（核心实现 + 单元测试）
- `peri-tui/src/kit/acp_bridge.rs`（Bash tick 门控 + BridgeState 重置）

**历史设计**：原过程 spec 已删除；本归档 issue 与 Git 历史保留执行语境
**历史实现计划**：过程计划已删除；本归档 issue 与 Git 历史保留执行语境
