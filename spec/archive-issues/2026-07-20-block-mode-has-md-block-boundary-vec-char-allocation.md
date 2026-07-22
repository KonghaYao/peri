# block 模式下 has_md_block_boundary_since 每 token 分配 Vec\<char\>

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-20

## 问题描述

在 `streaming_mode=block` 配置下，`has_md_block_boundary_since` 函数每次 TextChunk 事件将已累积的全文 `.chars().collect()` 为 `Vec<char>`。1000 token 回复中每次调用分配 O(text_len) 的 Vec（4 bytes/char），累积分配约 O(N²) ≈ 2MB。原始设计文档中的 spec 版本使用零分配的 `&str` 字节切片方案，当前实现偏离了原始设计。

## 症状详情

| 维度 | 描述 |
|------|------|
| **触发条件** | `streaming_mode=block`（非默认，需显式配置） |
| **触发频率** | 每个主 agent TextChunk/ReasoningChunk 调用一次，1000 token 回复约 1000 次 |
| **每次分配** | `full_text.len() * 4` bytes 的 `Vec<char>` |
| **累积分配** | 1000 token → ~2MB；5000+ chars 文本时更显著 |
| **spec 版本** | 使用 `&full_text[start_byte..]` + `contains()`/`lines().count()`，零分配 |
| **streaming 模式** | 不触发此函数（`should_push` 恒为 true，直接 `push_view_models`） |
| **sub-agent 分支** | 已跳过块边界检测，不受影响 |

## 涉及文件

- `peri-tui/src/kit/acp_events.rs:59-121` —— `has_md_block_boundary_since` 函数，第 65 行为 `Vec<char>` 分配点
- `docs/superpowers/specs/2026-07-19-streaming-mode-config-design.md:86-105` —— 原始 spec 中的零分配版本

## 技术背景

当前实现中使用 `Vec<char>` 的原因：
1. 需要 char 级随机访问来检测跨字符边界的 `is_line_start`（判断当前位置是否行首）
2. 后续所有被检查的字符（`#`、`` ` ``、`-`、`*`、`_`、`\n`）都是 ASCII，字节访问安全
3. 跨边界 `is_line_start` 检测可用 `full_text.as_bytes()[start_byte - 1] == b'\n'` 替代

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | perf-scan 发现 |

## 修复记录

### 修复 #1（2026-07-20）
- **操作人**：agent
- **commit**：`b9015ab9 perf(tui): 消除 has_md_block_boundary_since 中 Vec<char> 全量分配`
- **修复内容**：替换 `Vec<char>` 为 `&str` 字节切片方案，零分配
