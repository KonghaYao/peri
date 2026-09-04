# Peri Meta 数据访问

> 状态：现行设计
>
> 事实源：持久化契约为 `peri-acp-types::store::ThreadStore`，生产数据源为
> `peri-resources::sessions::SqliteThreadStore`，CLI 启动接口位于 `peri-tui`。

## 1. 定义

`peri meta` 是面向 Peri 自身持久化数据的独立 CLI 命令域。它从数据库读取数据并输出
稳定投影，不依赖 Agent、ACP host、TUI 或其他 Peri 进程是否正在运行。

v1 提供 session 元数据查询：

```bash
peri meta session <SESSION_ID>
peri meta session <SESSION_ID> --json
```

这里的 session 对应持久化 `ThreadId`。调用者必须显式提供 ID；命令不根据 cwd、最近
更新时间、进程、环境变量或运行状态推断“当前 session”。Agent 若要查询自身数据，应
把自己已知的 session ID 显式传给命令。

“Meta”表示 Peri 自有数据的管理与查询命令域，不表示运行时控制面，也不表示对当前
进程进行 introspection。

## 2. 简单输入、简单输出

`meta session` 遵循一个最小原则：

> 输入一个明确的 session ID，读取一个明确的数据库，输出一份有限、稳定的 session
> metadata。

命令只接受三类输入：

```bash
# 必填：session ID
peri meta session <SESSION_ID>

# 可选：机器可读输出
peri meta session <SESSION_ID> --json

# 可选：显式选择唯一数据库
peri --db-path /path/to/threads.db meta session <SESSION_ID> --json
```

v1 不加入 `--include`、tree、messages、snapshot、compaction、filter、分页或字段选择。
调用者不需要理解内部 schema，也不能通过参数把命令扩展成任意查询。

human 输出与 JSON 输出表达同一组字段。JSON v1 示例：

```json
{
  "schemaVersion": 1,
  "id": "01...",
  "title": "Example session",
  "cwd": "/workspace/project",
  "createdAt": "2026-09-04T00:00:00Z",
  "updatedAt": "2026-09-04T00:10:00Z",
  "messageCount": 12,
  "parentThreadId": null,
  "persistedAgentStatus": "done"
}
```

输出只回答“数据库里这条 session 的基本信息是什么”。一次调用只返回一个 object；
不存在隐式列表、关联展开或内容读取。错误也保持确定，只区分参数、数据库、未找到、
schema/数据损坏和内部失败。

## 3. 目标

1. 在任意普通 shell 中按显式 session ID 查询同一份持久化数据。
2. 以 `ThreadStore` 为数据契约，不从 `SessionManager`、`AcpSession` 或其他内存对象读取。
3. CLI 只负责参数、输出和退出码；数据库访问与 schema 兼容由 Resources 层承担。
4. 输出专用 DTO，避免把存储内部字段或大字段意外变成 CLI interface。
5. 为未来 `peri meta <resource> ...` 数据能力保留统一但克制的命令结构。

## 4. 核心裁决

### 4.1 数据库是唯一事实源

`peri meta session` 只读取 SQLite 中已持久化的 thread metadata。即使对应 Agent 正在
运行，命令也不查询、合并或校正进程内状态。

因此：

- 输出反映最后一次成功持久化的值；
- `agentStatus=active` 只表示数据库记录值，不证明进程仍在运行；
- 尚未落库的消息、状态或标题不可见；
- 数据库与运行中对象短暂不一致是该命令的预期语义，不做隐式协调。

命令不得在数据库查询失败时启动或连接 Agent，也不得通过 ACP、socket、daemon、共享
内存或进程环境补充结果。

### 4.2 session ID 必须显式提供

稳定 interface 为：

```text
peri meta session <SESSION_ID> [--json]
```

缺少 ID 是参数错误。v1 不提供以下隐式选择：

- 当前进程的 session；
- 当前 cwd 最新 session；
- 全局最近 session；
- 数据库中的 current pointer；
- 唯一 active session。

显式选择保证并发 Agent、多终端、多项目和历史 session 下仍为确定性查询。

### 4.3 使用 storage seam，不把 SQLite 泄漏给 CLI

调用链：

```text
peri-tui binary
  └─ meta CLI adapter
       └─ peri-resources read-only open
            └─ Arc<dyn ThreadStore>
                 └─ load_meta(ThreadId)
                      └─ SessionMetaDto
```

模块职责：

- `peri-tui`：定义 `clap` 命令、校验 ID、调用数据能力、渲染 human/JSON 输出并映射退出码。
- `peri-resources`：定位和打开 SQLite，处理 schema 兼容，提供 `ThreadStore` adapter。
- `peri-acp-types`：保留 `ThreadStore`、`ThreadMeta`、`ThreadId` 等存储契约。
- `peri-acp`、`peri-agent`、`peri-runtime`：不参与该命令调用链。

CLI 不直接执行 SQL，不依赖 SQLite 表名，也不实例化 session owner。新增资源类型时，先
在契约与 Resources 层建立对应数据 seam，不在 `main.rs` 累积数据库实现细节。

### 4.4 查询模式不得产生业务写入

`peri meta session` 是只读命令：

- 不创建 session；
- 不更新 `updated_at`、状态或访问计数；
- 不执行 schema migration；
- 显式 `--db-path` 不存在时不得创建空数据库；
- 不存在默认数据库时返回 `database_not_found`，不得用新空库伪装成 session 不存在。

现有 `SqliteThreadStore::new` 会创建文件并初始化 schema，因此不能直接作为该只读命令
的打开路径。现行实现通过 Resources 层的 read-only open adapter 复用
`ThreadStore::load_meta` 的读取语义。该 adapter 验证数据库是已存在的普通文件，并按查询
所需 schema shape fail closed；未来 schema 不兼容时返回类型化错误。

SQLite 为并发读取提供的必要内部机制不算业务写入，但 read-only 连接不得依赖创建
WAL、migration 或其他持久副作用才能成功。

## 5. 数据库选择

默认读取：

```text
~/.peri/threads/threads.db
```

复用现有顶层 `--db-path` 覆盖数据源：

```bash
peri --db-path /path/to/threads.db meta session <SESSION_ID> --json
```

规则：

1. 显式路径优先于默认路径。
2. 路径必须指向已存在、可读、schema 兼容的 SQLite 数据库。
3. 不在多个数据库间搜索 session ID。
4. 不因默认库查询不到记录而扫描项目目录、临时目录或旧路径。
5. 错误可以显示用户传入的数据库路径，但不得输出数据库内容或连接配置中的 secret。

## 6. Session 输出契约

### 6.1 JSON DTO v1

```json
{
  "schemaVersion": 1,
  "id": "01...",
  "title": "Example session",
  "cwd": "/workspace/project",
  "createdAt": "2026-09-04T00:00:00Z",
  "updatedAt": "2026-09-04T00:10:00Z",
  "messageCount": 12,
  "parentThreadId": null,
  "persistedAgentStatus": "done"
}
```

字段全部来自已持久化 `ThreadMeta`：

- `id`：thread/session ID；
- `title`：可空标题；
- `cwd`：创建 session 时记录的工作目录；
- `createdAt`、`updatedAt`：UTC RFC 3339；
- `messageCount`：持久化消息计数；
- `parentThreadId`：父 thread ID，可空；
- `persistedAgentStatus`：数据库中记录的 Agent 状态。

`persistedAgentStatus` 这个名称刻意表明它不是实时进程状态。

### 6.2 禁止字段

v1 不返回：

- `config`；
- `cached_context`；
- frozen snapshot；
- `snapshot_at_message_id`；
- `content_size`、`hidden`、`cancel_policy` 等非基础诊断字段；
- messages 或消息摘要；
- prompt、system prompt、tool 参数或 tool output；
- provider credential、token、environment 或其他 secret。

`SessionMetaDto` 必须从 `ThreadMeta` 显式逐字段构造，不能直接把 `ThreadMeta` 序列化为
CLI JSON。存储类型新增字段不得自动扩大 CLI interface。

### 6.3 Human 与 JSON 输出

- 默认 human 输出用于交互阅读。
- `--json` 是机器可读 interface，成功时 stdout 只输出一个 JSON object。
- 成功数据只写 stdout；诊断和错误只写 stderr。
- human 排版可以演进，JSON 字段名称、类型与语义按 `schemaVersion` 管理。
- 时间和数字不受 locale 影响。

## 7. 错误语义

稳定错误 kind 至少包括：

- `invalid_session_id`：ID 不是可接受的 session ID；
- `database_not_found`：目标数据库不存在；
- `database_unreadable`：数据库不可读或无法打开；
- `schema_incompatible`：schema 缺失、损坏或版本不受支持；
- `session_not_found`：数据库中不存在该 ID；
- `corrupt_session_data`：记录字段无法按强类型契约解析；
- `internal_error`：脱敏后的其他失败。

命令成功退出码为 `0`。参数错误、未找到、数据库错误和内部错误使用不同非零类别；精确
数值由实现 contract tests 冻结。`--json` 下错误也应提供稳定 JSON error DTO，且只写
stderr。

错误不得回退到其他数据库、最近 session 或进程内状态。非法
`agent_status` 等数据库值必须上抛，不能静默使用默认值。

## 8. 并发与一致性

- 查询是一次数据库读取，不获取 Agent 进程锁。
- 必须兼容主 Peri 进程使用 WAL 时的并发读取。
- 单次输出来自一次一致读取，不在字段之间分别重查。
- writer 正在事务中时，读取遵循 SQLite 已提交数据语义；不等待“最新运行态”。
- 读取不得改变 session 生命周期，也不得阻塞 Agent 超出数据库正常锁等待边界。
- busy/locked 情况应有明确的有限等待策略，耗尽后返回可诊断错误，不无限挂起。

## 9. 非目标

v1 不支持：

- 自动识别调用它的 Agent 或 session；
- 读取运行时、内存或进程状态；
- IPC、endpoint、capability、daemon、UDS 或 Named Pipe；
- session mutation、cancel、resume、prompt 或 permission 操作；
- transcript、frozen data 或配置导出；
- tree、messages、snapshot、compaction、filter 或分页扩展；
- workflow 数据或操作；workflow 由其独立二进制和 CLI 负责；
- 无 ID 时选择最近、当前或 active session；
- 跨多个数据库发现数据；
- 把 `peri meta` 作为远程服务或公开 automation server。

未来新增写能力时，必须使用显式动词、独立授权和单独设计；不能把 v1 查询命令悄然
扩展为进程控制面。

## 10. 扩展规则

`peri meta` 是资源导向命令域：

```text
peri meta <resource> <explicit-key> [read options]
```

新增资源应满足：

1. 有明确的持久化事实源和稳定 key；
2. 不需要运行中进程参与；
3. 通过 Resources 层 adapter 访问；
4. 使用专用 versioned DTO；
5. 默认只读，敏感大字段不因“数据库里存在”就自动公开。

v1 不提前增加 `list`、`messages`、`tree`、`snapshot`、`compaction`、`export` 或通用
SQL/query command。Workflow 已有独立二进制，不进入 `peri meta`。其他能力在真实需求
出现后分别设计，避免形成与数据库 schema 等宽的浅 interface。

## 11. 验证覆盖

现行实现的 contract tests 与完整验证矩阵覆盖：

- 同一数据库和 session ID 在 Agent 运行与不运行时返回相同持久化投影；
- 命令调用链不依赖 `peri-acp` host、`SessionManager`、IPC 或环境中的 session context；
- 缺少 ID 时由 CLI parser 拒绝，不推断最近 session；
- 默认与显式数据库路径选择确定，不跨库 fallback；
- 不存在的数据库不会被创建，查询不执行 migration 或业务写入；
- 不存在 session、损坏枚举和不兼容 schema 均类型化失败；
- JSON DTO 不含禁止字段，`ThreadMeta` 新增字段不会自动进入输出；
- 并发 writer 下读取已提交快照，busy 等待有界；
- human/JSON 的 stdout、stderr 和退出码符合契约。
