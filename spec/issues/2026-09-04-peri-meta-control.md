# Peri Meta 数据访问实施

**状态**：已完成（归档）
**优先级**：中
**类型**：功能 / CLI / 持久化数据访问
**创建日期**：2026-09-04
**权威设计**：`docs/design/meta-control.md`

## 完成摘要

实现已按权威设计交付：`peri-tui` 提供 `peri meta session <SESSION_ID> [--json]`，在配置初始化前完成受限 Meta 路由、参数校验、稳定输出与退出码映射；`peri-resources` 提供 existing-file-only SQLite read-only seam，并经 `ThreadStore::load_meta` 返回持久化 metadata。CLI 使用九字段 DTO allowlist，不进入 ACP、Agent、Runtime、Workflow、IPC 或运行中 session 状态。

WP-007 的授权单次有界重试已通过完整验证矩阵，包括格式、`peri-resources` 与 `peri-tui` 测试、真实 binary contract tests、doc tests、layer-import gate、workspace clippy 和 diff check。此前观察到的无关 `peri-tui` library test timing/order-sensitive flaky 风险继续保留；本次通过不证明该风险不会复现。

本 issue 作为已完成实施记录归档保留；现行行为以 `docs/design/meta-control.md`、对应 code-index 与代码/契约测试为准。

## Problem Statement

Peri 缺少独立、与运行进程无关的自身数据查询命令。目标不是读取运行中 Agent 的内存
状态，而是让普通 shell 或 Agent 都能按显式 session ID 从 Peri 数据库读取同一份
持久化 session metadata：

```bash
peri meta session <SESSION_ID>
peri meta session <SESSION_ID> --json
peri --db-path /path/to/threads.db meta session <SESSION_ID> --json
```

调用者必须提供 ID。除可选 `--json` 与顶层 `--db-path` 外，v1 不增加其他输入；命令
不推断当前、最近或 active session。

## 已确认现状

- `peri` 二进制与 `clap` parser 位于 `peri-tui`。
- 持久化契约是 `peri-acp-types::store::ThreadStore`，`load_meta` 可按 `ThreadId` 获取
  `ThreadMeta`。
- 生产实现是 `peri-resources::sessions::SqliteThreadStore`，默认路径为
  `~/.peri/threads/threads.db`。
- 顶层已经提供 `--db-path`，可复用为 Meta 数据源覆盖。
- `ThreadMeta` 包含公开 metadata，也包含 `config`、`cached_context` 等不应直接输出的
  字段，因此需要独立 DTO。
- 现有 `SqliteThreadStore::new` 会创建数据库并初始化/迁移 schema，不适合只读查询
  command 直接使用。

## 已裁决方案

`peri meta session <SESSION_ID>` 直接经 Resources 层的 read-only store adapter 查询
数据库，不经过 ACP、Agent、Runtime、TUI session owner 或任何 IPC。

上一版 endpoint/capability/进程上下文方案已否决，不属于本 issue。

## 实施切片

### 1. 只读数据打开路径

- 在 `peri-resources` 增加只读打开现有 thread database 的 adapter。
- 默认路径和显式 `--db-path` 都要求文件已存在；不得创建数据库、运行 migration 或
  fallback 到临时路径。
- 使用 SQLite read-only 模式并设置有界 busy timeout，兼容 WAL writer。
- schema 不兼容和损坏数据必须类型化失败。

### 2. Session 查询与 DTO

- 复用 `ThreadStore::load_meta(ThreadId)`，不在 CLI 中写 SQL。
- 定义最小 `SessionMetaDtoV1`，仅投影 `schemaVersion`、`id`、`title`、`cwd`、
  `createdAt`、`updatedAt`、`messageCount`、`parentThreadId` 和
  `persistedAgentStatus`。
- 排除 `config`、`cached_context`、frozen snapshot、snapshot message ID、content size、
  hidden、cancel policy 和消息内容。
- `persistedAgentStatus` 的命名明确表示数据库记录值，不是实时状态。

### 3. CLI

- 在 `peri-tui` 增加 `meta` 顶层 command 和 `session <SESSION_ID>` 子命令。
- 支持 human 输出和可选 `--json`；复用顶层 `--db-path`。
- 不加入 `--include`、tree、messages、snapshot、compaction、filter、分页或字段选择。
- 缺失或非法 ID 在访问数据库前失败。
- 成功只写 stdout，错误只写 stderr；冻结 JSON error 与退出码分组。

### 4. 验证与文档归位

- 覆盖默认路径、显式路径、不存在数据库、不存在 session、损坏 schema 和 WAL 并发
  writer。
- 证明命令不创建数据库、不执行 migration、不访问进程状态。
- 更新 `peri-resources` 与 `peri-tui` code-index；实现完成后把权威设计改为现行设计并
  归档本 issue。

## 验收标准

- [x] `peri meta session <ID>` 能在没有其他 Peri 进程时读取已持久化 metadata。
- [x] 相同数据库与 ID 在 Agent 运行和不运行时使用相同查询路径。
- [x] 缺少 ID 时 parser 拒绝；不选择 cwd 最新、全局最新或 active session。
- [x] `--db-path` 精确选择单个数据库，不扫描或 fallback 到其他路径。
- [x] 不存在的数据库不会因查询而创建。
- [x] 查询不会更新 metadata、运行 schema migration 或产生业务写入。
- [x] `SessionMetaDtoV1` 不包含 config、cached context、frozen data、messages 或 secret。
- [x] 非法 `agent_status` / `cancel_policy`、损坏或不兼容 schema 不静默 fallback。
- [x] WAL writer 并发时读取已提交数据，busy 等待有界。
- [x] JSON stdout、错误 stderr 和退出码通过 contract tests。

## 非目标

- 自动识别 Agent 自身 session；
- IPC、capability、endpoint 或 daemon；
- 运行时状态查询；
- session list、tree、messages、snapshot、compaction、export 或通用 SQL；
- workflow 数据或操作；workflow 使用其已有独立二进制；
- session mutation、cancel、resume 或 prompt。

## 建议验证命令

```bash
cargo test -p peri-resources --lib
cargo test -p peri-tui --bin peri
cargo test --workspace --doc
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```
