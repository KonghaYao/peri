# WASM 分支与主线收敛状态

状态：`refactor/wasm-on-pre-release` 基于本地 `pre-release/main` 的 `fa10756d`。该基点已融合旧 WASM 分支的 `01c0b26d` 和 `834a2982`：本机按 locator 选择 SQLite 或 Turso，Emscripten 的 Resources 依赖图只保留 Turso；本机 SQLite 开库和只读降级在原生专属模块，共用会话规则在 SQLite adapter 外。新分支不重放原始两笔提交。

## 已进入基点

| 能力 | 主线提交 |
| --- | --- |
| Resources 共用会话规则、远端草稿初始化准入、虚拟执行与 SQLite adapter 隔离 | `fa10756d` |
| 后台任务投递、回执、发起者路由及 Workflow MCP scope | `59e69a2b` |
| Workspace task scope 共用类型 | `018459f7` |
| Tokio feature 边界 | `02e97e52`、`3a999a1e` |
| 时间库与 Emscripten 计时器修复 | `252bce89`、`7f3ebff7` |
| Resources 存储 v2 迁移规划 | `8481595c` |

## 本分支剩余提交

按依赖顺序：MCP 宿主能力裁剪与 OAuth 凭证共用 bootstrap MCP → ACP WASM 装配 → Emscripten 工具链补丁 → WASM ACP 入口。旧分支的两笔 MCP 提交已合并；新提交不再包含曾经的凭证直连适配。

这些提交提供目标平台的编译和部署接线。`peri-wasm/`、Emscripten/Hyper 补丁、Workers 示例及 `@peri-sdk` 仍随 WASM 功能整体验收；不作为单独的通用 Resources 提交。宿主配置写入的持久性与跨实例 CAS、缺席的本地进程和 builtin 能力仍需按部署契约裁决，不能把 WASM 空实现解释为执行成功。

## 验收与限制

- 新分支上的原生 workspace `cargo check --locked --workspace` 通过。
- Emscripten 目标的 `peri-wasm` check 通过；依赖图包含 `peri-mcp-credentials`，不包含 SQLx、`peri-process` 或 `peri-mcp-workspace`。
- WASM 链接构建通过。Node ACP smoke 在打开本地 sqld 的远端会话库时返回 `remote session store server_error`，尚未进入会话创建与宿主重启恢复；旧分支上同一 smoke 在同一阶段失败。远端 MCP 与真实 OAuth 授权仍需重新验证；本地 `workerd` 验收不能代表托管 Workers 部署。

虚拟工作区格式使用主线 `peri.remote.workspace.v2` 域、UUIDv8 及 `kind: virtual-v1` 快照。旧 WASM 身份和仅含 `root` 的快照可按 ID 读取历史，但不能取得执行资格。约束见 [ARC-REMOTE-ENV-001](../../docs/standards/architecture-contracts.md) 和 [Resources 代码索引](../../docs/code-index/peri-resources.md)。
