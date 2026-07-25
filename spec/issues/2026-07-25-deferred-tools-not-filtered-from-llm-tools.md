# Deferred Tools 未向 LLM 过滤——根因：缺少工具自声明的层级标记

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-25

## 问题描述

设计上，工具分两类：Direct（直接发给 LLM 的 Core/Meta，如 Read/Write/SearchExtraTools）和 Deferred（仅通过 SearchExtraTools 按需发现的 MCP/Cron/SubAgent/Workflow 等）。但当前实现中，`reason.rs` 从 `shared_tools` 读取工具时**没有任何过滤**，导致所有工具（含 Deferred）都直接出现在 LLM API 的 `tools` 参数中。

## 根因

`CORE_TOOLS`/`META_TOOLS` 白名单 + `is_deferred_tool()` 是集中式反模式：

1. **工具层级信息存储在工具外部**——`core_tools.rs` 维护一份白名单名字列表，新增工具必须同步更新此名单
2. **名字匹配脆弱**——`is_deferred_tool("artifact")` 返回 true，因为 `"artifact"` 不在 `META_TOOLS` 常量中（遗漏）
3. **过滤逻辑未接入 LLM 请求路径**——`is_deferred_tool()` 只在搜索索引和 prompt 文本生成中调用，从未在 `reason.rs` 的 `tool_refs` 构造中使用（因为 `peri-agent` 不能依赖 `peri-middlewares`）

## 症状详情

| 现象 | 期望 | 实际 |
|------|------|------|
| MCP 工具在 LLM tools 数组中 | Direct 工具才有 | 全部工具都有 |
| SearchExtraTools 的定位 | 唯一发现 Deferred 工具的入口 | 冗余入口——LLM 可直接调用 |

## 修复方向

在 `BaseTool` trait 新增 `fn is_direct(&self) -> bool`（默认 `false`），让每个工具**自声明**是否直接发送给 LLM：

```rust
pub trait BaseTool {
    // existing methods...
    /// 是否直接发送给 LLM（Core/Meta=true, Deferred=false）
    fn is_direct(&self) -> bool { false }
}
```

然后删除 `CORE_TOOLS`/`META_TOOLS`/`is_deferred_tool()` 全部集中式常量/函数。

## 涉及文件

- `peri-agent/src/tools/mod.rs` —— `BaseTool` trait 定义
- `peri-agent/src/agent/stages/reason.rs:109-114` —— 添加 `is_direct()` 过滤
- `peri-middlewares/src/tool_search/middleware.rs` —— 用 `is_direct()` 替代 `is_deferred_tool()` 构建索引
- `peri-middlewares/src/tool_search/core_tools.rs` —— **整文件删除**
- ~17 个 Core/Meta tool 实现文件 —— 各覆写 `is_direct() → true`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 创建 |
| 2026-07-25 | Open | Fixed | agent | 修复：BaseTool::is_direct() 替代集中式白名单 |

## 修复记录

### 修复 #1（2026-07-25）

- **操作人**：agent
- **用户原意**：让每个工具自声明层级（Direct/Deferred），过滤 LLM tools 数组
- **修复内容**：
  - `peri-agent/src/tools/mod.rs`：BaseTool trait 新增 `fn is_direct(&self) -> bool { false }`
  - 18 个 Direct 工具覆写 `is_direct() → true`（15 Core + 3 Meta）
  - `peri-agent/src/agent/stages/reason.rs`：tool_refs 添加 `.filter(\|t\| t.is_direct())`
  - `peri-middlewares/src/tool_search/middleware.rs`：用 `!t.is_direct()` 替代 `is_deferred_tool()`
  - `peri-middlewares/src/tool_search/core_tools.rs`：删除 CORE_TOOLS/META_TOOLS/is_deferred_tool
  - 清理 lib.rs/mod.rs 重导出
- **验证状态**：已验证（cargo build/test/clippy 全通过，2704 tests passed）
