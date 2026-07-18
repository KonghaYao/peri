# tools 数组顺序随 HashMap 迭代顺序，新增工具触发 rehash 后 prompt cache 前缀全断

**状态**：Done
**优先级**：中
**创建日期**：2026-07-18

## 问题描述

Reason 阶段构建 LLM 请求时，tools 数组来自 `HashMap` 的 `.values().collect()`，顺序即 HashMap 迭代顺序（Rust `RandomState`，per-instance 随机）。Anthropic prompt cache 是严格前缀匹配（system → tools → messages），tools 数组任何一个位置变化都会导致 tools 及其后全部内容的缓存失效。当前稳定会话内不出问题，但运行期向 tools map 新增 key（MCP reconnect 重注册、plugin 动态加载、deferred 工具注册）可能触发 rehash 导致全序变化；跨进程 resume 时新 HashMap 实例顺序随机，必然 miss。

## 症状详情

| 场景 | 表现 |
|------|------|
| 稳定会话内、工具集不变 | 正常：tools 顺序稳定（app 级单例 + 同 key `insert` 覆盖不改序），缓存命中 |
| 运行期新增工具 key（MCP reconnect / plugin 加载 / deferred 注册） | HashMap 可能 rehash，迭代顺序全变 → 该轮起 prompt cache 从 tools 处整体 miss，input token 成本与首 token 延迟突增 |
| 跨进程 resume session（TTL 5min 内） | 新进程 HashMap 实例顺序随机 → 首轮必然全量 cache miss |

该问题无可观察的功能性错误，仅表现为 `cache_read_input_tokens` 掉零、成本/延迟上升，需通过 Langfuse 或 usage 统计才能发现。

## 复现条件

- **复现频率**：仅在特定条件下（运行期工具集变化、或跨进程 resume）
- **触发步骤**（以 MCP reconnect 为例）：
  1. 开启一个会话，进行若干轮对话（缓存正常命中）
  2. 触发 MCP server reconnect，MCP 工具重新注册进 shared_tools
  3. 下一轮请求的 tools 数组顺序变化，观察 usage 中 cache_read 归零
- **环境**：所有 Anthropic provider 会话（OpenAI provider 无前缀缓存语义，不受影响）

## 涉及文件

- `peri-agent/src/agent/stages/reason.rs:92` —— `ctx.runtime.tools.read().values().cloned().collect()`，tools 数组顺序来源
- `peri-agent/src/agent/stages/mod.rs:80` —— `SharedToolMap = Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>` 类型定义
- `peri-acp/src/agent/builder_v2.rs:123` —— 每 turn `tools.insert()` 填充（同 key 覆盖不改序，是当前不出问题的原因）
- `peri-tui/src/launch.rs:197` —— shared_tools app 级单例创建点

## 期望改进方向

让 tools 数组顺序具备确定性：将 `SharedToolMap` 改为 `BTreeMap<String, Arc<dyn BaseTool>>`（按名排序天然确定），或在 reason.rs collect 后按工具名排序。消除运行期 rehash 与跨进程 resume 的缓存全断风险。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建（prompt cache 稳定性审计发现） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
