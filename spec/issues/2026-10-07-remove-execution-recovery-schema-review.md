# 执行恢复机制剥离：改动面与 SQLite 表结构审核

- 状态：**结构基线与 schema18 实现对照；剥离已在独立 worktree 实施，真实用户库操作不在授权内**。
- 日期：2026-10-07。
- 裁决：删除新增执行恢复机制，不删除 history 面板的历史会话加载和正常续聊。
- 基线：本地 `main=d7ee444e`（2026-09-29），对照本轮当前工作树；未 fetch。
- 初始审核仅只读源码；后续实现与验证见剥离计划。未访问或修改真实数据库。
- 后续（2026-10-08）：本文只读快照里的 `legacy_execution_registrations` 与 `session_bindings` 外键已被
  [移除计划](2026-10-08-remove-legacy-execution-registrations-plan.md) 删除（schema 19，绑定收敛到会话归属行）；下文对该表的形状描述读作 schema 18 时点，不再是现行结构。
- 本文表数指代码管理的新库结构，不是用户数据库的实际对象数量；旧库扩展对象另行保护。

## 1. 删除与保留的能力边界

本次不是关闭一个 recovery 开关，而是从正常执行链中剥离持久执行账本。

| 删除 | 保留 |
| --- | --- |
| WorkState、阶段检查点、持久模型请求、执行恢复投影 | Receive → Compact → Reason → Act 的当前进程执行 |
| Work 命令 journal、事件及回执、跨重启执行对账 | 消息追加、历史顺序、压缩和消息 flags/projection |
| 新增持久控制状态与控制回执 | 当前进程的暂停、取消、输入队列和执行预算，按所需现有行为重接 |
| 旧批次领取、旧工具调用恢复、cold child 自动续跑 | 当前任务的结果、进度、取消和普通父子会话关系 |
| Work 内的 owner、child resume metadata、终态交付恢复账本 | 环境身份、历史会话配置与 frozen/inherited context |
| 加载历史时的旧执行隔离/对账门禁 | history 列表、加载、事件重放、会话切换和手动续聊 |

进程退出后不自动续跑旧执行，不从未结束的历史工具消息推导当前 running。
未完成动作不能被改写为成功；外部任务也不会因为删除本地账本而自动停止。
新输入不得再因已撤销的旧 Work/Unknown 账本被冻结。
尚未提交的内存输入和执行预算不再获得本机制的跨重启保证。

“删除恢复机制”不等于删除存储事务、错误报告、只读准入或普通网络重试。
流中断重试、compact 的当前执行预算恢复等须按职责判定，不按 `recover` 名称机械删除。

此前 2026-10-06 的运行状态恢复 issue 保留了可靠交付和持久绑定等能力；
本次裁决更宽，批准实施后须同步原 active spec 与相关架构契约，不能宣称二者范围相同。

## 2. 改动面估计

### 2.1 已观察的直接依赖

在五个 crate 中，用 `work::`、`WorkState/Command/Snapshot/Mutation/Query`、
`recover_work`、`reconcile_finish`、`resolve_work_mutation` 搜索 Rust 文件：

| Crate | 生产文件命中 | 测试/夹具文件命中 | 生产命中文件在 main 中不存在 |
| --- | ---: | ---: | ---: |
| peri-acp-types | 16 | 8 | 12 |
| peri-agent | 22 | 31 | 16 |
| peri-acp | 14 | 20 | 12 |
| peri-resources | 18 | 13 | 11 |
| peri-middlewares | 7 | 9 | 6 |
| 合计 | 77 | 81 | 57 |

这是本轮源码快照的直接引用命中，不是最终必改文件数或可删除行数。
控制类型、schema/migration、入口装配、MCP task discovery 和文档等还会扩大范围。
盘点期间其他线程继续修改 Work 存储；以上数字不代表最终集成工作树的文件总数。
结论：**跨至少五个 crate 的较大剥离，不能作为单独删表或删除一两个恢复函数交付**。
本地 main 后有大量无关改动，不能用整个分支 diff 数量充当本任务规模。

### 2.2 按模块处理

| 模块 | 删除/调整 | 必须保住的行为 |
| --- | --- | --- |
| peri-acp-types | work reducer/types、control journal 类型、执行恢复协议的消费接口 | 会话、消息、身份、普通取消/父子关系类型 |
| peri-agent stages | work_boundary/ledger/receive/reason/dispatch/recovery/pipeline；解除 stages 和 TurnContext 的耦合 | 内存 MQ → Transcript → 模型 → 工具结果 → MQ；消息正常入库 |
| peri-agent mailbox | 移除 durable 输入发布/领取及 SDK work ticket 对账依赖，重接普通内存队列 | 发送、排队、steer、takeback、取消当前 turn，不借删机制取消正常交互 |
| peri-acp | work query/resolve、finish/reconcile、cold execution、持久 control；历史 restore 的 owner/work 装配 | session/list/load/resume/fork、历史 replay、frozen 上下文与当前执行装配 |
| peri-resources | 共用 work/control SQL、两种 adapter、SessionDataPort/SessionResources 方法、gate 中对应门禁 | 历史/绑定/压缩/普通关闭/OAuth，以及真实存储错误可见 |
| peri-middlewares MCP | 持久 invocation binding、owner recovery、历史 task discovery 恢复路径 | 当前进程工具派发、活跃 task subscription、进度/结果/取消 |
| schema/docs/tests | 新版本移除六表，撤销对应恢复契约，保留普通行为回归 | 旧历史、rowid、扩展对象、环境身份和只读加载 |

盘点结束时发现并行 WIP 正将 WorkState 改成 head/分记录文档，复用原来的
`session_work_state` / `session_work_events`，未新增表，也仍是持久执行恢复。
这些 storage/repository、专用维护命令及其测试同样属于本次候选剥离面，
不能保留为已撤销机制的第二种实现；实现前须冻结/核对集成基线。

SDK 暂不修改。其现有 work query/admission/resolve 协议消费会受影响，
必须在上层实施前明确支持的客户端模式；不能承诺现有 alpha SDK 原样可用。
不恢复 main 的本地 owner lease、dirty reset、sidecar 或 `execution_runs`。

### 2.3 History 的实际解耦点

剥离前 `peri-acp/src/host/requests/session_restore.rs` 不仅加载 payload/frozen，
还读取 WorkState 的 resource owner，再调用 `requests/resource_owners.rs::load_for_restore`。
后者缺 owner 时会写入 Work quarantine，因此 history 的上层装配仍有依赖，必须删除该耦合。

底层 snapshot/history 的读取本身不需要六张 Work/control 表：
`sqlite_store/session_data.rs::load_snapshot`、`sqlite_store/context.rs`、
`remote/session_read.rs` 和 `remote/session_sql.rs` 读取 meta、binding、frozen、messages 与继承快照。
应保留这条历史数据链，按原 main 的语义重新装配新的 live runtime，而不是恢复旧 runtime。
MCP 连接配置重新装配不等于恢复旧 MCP 调用或接管旧外部任务。

## 3. schema18 目标表清单

### 3.1 保留：共享业务表 9 张

| 表 | 保存内容 | main 对照/保留理由 |
| --- | --- | --- |
| machines | Machine 身份 | 当前环境归属架构，不是恢复账本 |
| workspaces | Machine 下的路径身份 | 保留当前结构，不退回 main 的旧 workspace 含义 |
| projects | 项目身份 | main 已有，绑定依赖 |
| legacy_execution_registrations | 项目/根目录/发现快照的旧登记身份 | 承接 main 的旧 workspaces，当前 binding 外键仍依赖；不是 attempt journal |
| threads | 会话元数据、配置、frozen、继承上下文和父子关系 | history 与正常续聊核心 |
| messages | canonical 消息/提醒、flags/projection | history 内容和顺序核心 |
| session_bindings | 会话不可变环境绑定及发现证据 | history/执行环境准入，不是恢复检查点 |
| mcp_oauth_credentials | Workspace 级 OAuth 凭证 | 普通 MCP 接入能力 |
| session_close_intents | 显式关闭意图 | 当前普通关闭/结清也使用；本方案不取消关闭语义 |

本次实现保留上述九表，不代表用户数据库已迁移；真实库结构仍须单独核对。
`session_close_intents` 虽非 main 已有，也不能只因为新增就归为执行恢复表。
若进一步要求取消跨重启关闭意图，需单独裁决其生命周期语义，再评估第九张表。

### 3.2 删除：新增恢复账本 6 张

| 表 | 删除内容 |
| --- | --- |
| session_work_state | 累积 WorkState JSON |
| session_work_events | Work 事件内容及投递去重证据 |
| session_work_receipts | Work mutation 持久回执 |
| session_work_commands | 原命令、digest、ACK journal |
| session_control_state | 持久 lifecycle/control generation/attempt |
| session_control_receipts | 控制命令持久回执 |

`childResumeMetadata`、resource owner、terminal obligations/ACK 等是 WorkState JSON 内的字段，
不是独立表；随机制删除，不为这些恢复字段另建替代表。

### 3.3 其他数据库/历史对象

- Turso 另保留 `peri_store_meta`、`peri_op_ledger` 两张 main 已有机制表；不在本地 SQLite 新库创建。
- `peri_op_ledger` 服务普通远端数据 mutation 的幂等与结果确认，不是 Agent 阶段恢复账本。
- SDK 的 execution registry 属于独立存储边界，不纳入本次删表。
- 旧库已有的 `thread_goals`、普通扩展表、view/trigger/index 原样保护；不保证新库存在这些对象。
- 当前 schema 不再创建 `execution_runs`、`session_environments`，本方案不重建它们。
- SQLite 自带的 `sqlite_*` 系统对象不计业务表数，也不操作。

## 4. 完整目标 DDL

这是供审核的**新库目标结构**，不是可直接对用户旧库执行的迁移脚本。
复用当前非恢复表的字段/默认值/约束，不顺带做 nullable、命名或字段清理。
权威来源：`sessions/canonical.rs::{CREATE_V2_TABLES, CREATE_V2_INDEXES}`，
以及 `remote/schema.rs`、`remote/ledger.rs`。

```sql
CREATE TABLE machines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    identity_kind TEXT NOT NULL
        CHECK(identity_kind IN ('known', 'legacy_unknown'))
);

CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL
        CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    UNIQUE(machine_id, path)
);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    locator TEXT NOT NULL,
    object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity)
);

CREATE TABLE legacy_execution_registrations (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL,
    root_identity TEXT NOT NULL,
    discovery TEXT NOT NULL,
    UNIQUE(root, root_identity),
    UNIQUE(id, project_id)
);

CREATE TABLE threads (
    id TEXT PRIMARY KEY,
    title TEXT,
    cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT,
    snapshot_at_message_id TEXT,
    hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade',
    config TEXT,
    frozen_context TEXT,
    inherited_context TEXT,
    agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))
);

CREATE TABLE messages (
    message_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0,
    excluded BOOLEAN NOT NULL DEFAULT 0,
    projection TEXT
);

CREATE TABLE session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT,
    evidence_origin TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id)
        REFERENCES legacy_execution_registrations(id, project_id)
);

CREATE TABLE mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(principal_id, workspace_id, server_key)
);

CREATE TABLE session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL
);

CREATE INDEX idx_messages_thread_id ON messages(thread_id);
CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id);
CREATE INDEX idx_bindings_workspace
    ON session_bindings(workspace_id, relative_cwd, thread_id);
CREATE INDEX idx_threads_updated ON threads(updated_at DESC, id DESC)
    WHERE hidden = 0 AND message_count > 0;
CREATE INDEX idx_threads_workspace_archived
    ON threads(workspace_id, archived, updated_at DESC, id DESC)
    WHERE parent_thread_id IS NULL AND message_count > 0;
```

重要结构说明：

- `threads.workspace_id` 指向新 Machine/path workspace。
- `session_bindings.workspace_id` 指向旧登记身份 `legacy_execution_registrations.id`，
  两个同名字段不是同一个外键目标；当前结构如此，本轮不顺带迁移/重命名。
  （2026-10-08 后续：schema 19 删除登记表并把该列收敛为 `threads.workspace_id`，迁移逐行校验根一致，不一致拒绝升级。）
- `parent_thread_id`、`snapshot_at_message_id` 当前没有声明 FK，不新增未有的约束。
- `messages` 保留隐式 `rowid`，历史读取以其排序，不改成 `WITHOUT ROWID` 或重建排序。
- 上面是五条显式业务索引；PK/UNIQUE 还会生成 SQLite 自动索引。
- 远端不能依赖本地 FK cascade；普通会话删除仍要显式清理相关行。

Turso 专用两表：

```sql
CREATE TABLE peri_store_meta (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 0),
    schema_version INTEGER NOT NULL,
    store_id TEXT NOT NULL,
    contract TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE peri_op_ledger (
    operation_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    digest TEXT NOT NULL,
    state TEXT NOT NULL,
    receipt TEXT,
    updated_at TEXT NOT NULL
);
```

这两表没有额外显式索引或外键。

## 5. 迁移与验收边界

1. 使用向前的新 schema 版本，不降级到 main 的 schema 10。
   隔离基线为17，本次实现推进到18；原工作树的并行改动未混入。
2. 先停止相关写入者并解除上层依赖，再删除六表；不能先删表让当前执行路径报错。
3. 17→目标版本无需重建九张保留表，保留 messages.rowid 和既有字段/数据。
4. 旧迁移清理器删除 `thread_goals` 的行为已修正；旧版本直升保留该表及普通扩展，
   不再创建 Work/control 表；11→12 保留扩展列、索引与 trigger。
5. 校验待删除对象的形状与外部引用；发现未知 FK/view/trigger 依赖时明确失败，
   不扩大删表范围。不自动 VACUUM，不操作其他数据库，不删除未知普通对象。
6. 本地删除/版本推进同事务；远端保留 store_id/contract，使用受守卫的托管事务。
   提交结果未知时通过身份/版本/结构读回确认，不宣称已成功。
7. 验证 history 列表、加载、切换、重放、frozen/继承上下文、fork 和正常续聊。
   验证当前工具/子任务、取消、消息/模型/工具结果持久化；重启后不恢复旧执行。
8. 现有恢复契约不再是验收要求，但要用新的可观察行为测试替换，不能仅删失败测试。
9. 实施时同步 architecture、RCRA active design/spec、模块指引和 code-index；
   当前源码与定向验证已同步，真实远端及完整端到端保证不作为已验证事实。

## 6. 证据与局限

Astra 在 SQLite 3.51.0 的纯内存夹具中验证了候选 DDL 和删六表隔离：
历史 rowid、flags/projection、父子字段、frozen/inherited、关闭意图、
两种既有 goal 表结构、扩展表及历史索引保留，foreign_key_check 无违规。
这是合成数据库结构验证，不是实际 adapter、Turso 网络或完整上层行为验收。

未测真实数据库对象数量、磁盘空间回收、实施工时或性能收益。
初始盘点时原工作区存在并行 WIP；实现从固定提交建立独立 worktree，未包含这些改动。
SQLite 与 SQLite-backed RemoteTransport adapter 的迁移、守卫和历史回归已补齐；
实际验证命令、结果及限制见剥离计划和 `peri-resources/execution-recovery-removal-report.md`。

实施状态与分工以 `2026-10-07-remove-execution-recovery-plan.md` 为准。
