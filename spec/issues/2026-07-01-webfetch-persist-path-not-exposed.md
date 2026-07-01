# WebFetch 落盘文件路径未暴露给 LLM，Agent 无法读取完整内容

**状态**：Open
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-01

## 问题描述

WebFetch 工具在内容超过 2000 行时会将完整内容落盘到临时文件，但 LLM 收到的输出中没有暴露临时文件路径。Agent 只能看到截断后的前 ~2000 字符，无法通过 Read 工具获取完整内容。已有的落盘机制 `persist_truncated_output` 生成了文件路径提示，但该提示被后续的字符级截断逻辑吃掉。

## 症状详情

| 维度 | 当前行为 | 期望行为 |
|------|----------|----------|
| 截断提示 | 仅显示 `[Output truncated at 2000 chars]` | 显示文件路径，Agent 可自行 Read 完整内容 |
| 完整内容 | 已落盘到 `/tmp/peri-tool-output-{uuid}.txt` | 同上（落盘本身正常） |
| Agent 可见信息 | 前 2000 字符 + 截断标记 | 前 2000 字符 + 截断标记 + 文件路径 |
| 其他工具对比 | Bash/Read/Grep 等工具的截断输出正常包含文件路径 | — |

**实际 LLM 输出示例**：
```
[Content truncated: original had 5000 lines]
[Full output saved to /tmp/peri-tool-output-xxx.txt — use Read tool to view complete content]
```

**LLM 实际收到的内容**（被二次截断后）：
```
[Content truncated: original had 5000 li...[Output truncated at 2000 chars]
```

## 复现条件

- **复现频率**：必现（任何超过 2000 字符的 WebFetch 响应都会触发）
- **触发步骤**：
  1. 让 Agent 用 WebFetch 抓取一个长网页（内容超过 2000 字符）
  2. 观察 Agent 收到的工具输出——不包含文件路径
  3. Agent 表示"拿到了截断内容但没有文件路径，无法读取完整内容"

## 涉及文件

- `peri-middlewares/src/middleware/web_fetch.rs:198` — `output_char_limit()` 返回 `Some(2000)`，与内置截断冲突
- `peri-agent/src/agent/stages/tool_dispatch.rs:404-412` — `output_char_limit` 截断逻辑，在工具自身截断+落盘之后再次按字符截断
- `peri-middlewares/src/tools/output_persist.rs:13-15` — `persist_truncated_output()`，生成文件路径提示（正常工作，但提示被二次截断吃掉）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-01 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
