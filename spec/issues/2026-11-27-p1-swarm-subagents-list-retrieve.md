# P1：Peri Swarm 的 sessions.subagents list/retrieve 未实现

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：功能缺口（官方契约未实现）。
**创建日期**：2026-11-27。
**来源**：用户报告（`/fix-bug`）。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `3eecbdede`）。

## 确认证据

- 两条路由已注册但绑定统一兜底：
  `peri-swarm/src/http/mod.rs:120-127` → `handlers::unsupported_resource`。
- 错误分类器把资源名投影为 `sessions.subagents`：
  `peri-swarm/src/http/handlers.rs:195-217`。
- 集成测试钉住当前 404：
  `peri-swarm/tests/it/session_resources_test.rs:1655-1663`。
- 官方契约：`Subagent` 含 `id`、`object`、`session_id`、`parent_agent_id`、
  `name`、`instructions`、`opened_at`、`closed_at`、`status`；
  list 支持 `order`/`after`/`limit` 且包含 nested 与 closed；
  retrieve 需校验属于该 session。
  本地 SDK 声明：`agents.d.ts:4354-4397`、
  `sessions/subagents/subagents.d.ts:20-63`。
- 数据源已经存在：`SessionResources::list_session_tree` /
  `load_session_meta`（`peri-acp-types/src/session_resources/session_resources.rs:572,663,666`）；
  SQLite 递归 CTE 在 `peri-resources/src/sessions/sqlite_store/context.rs:70-86`，
  **包含 root 本身**、按 `created_at ASC`、不过滤 `hidden`。
- 子会话只有两个产生点：
  `peri-agent/src/session/subagent/factory/spawn.rs:299`（Task/Agent 派生）与
  `peri-agent/src/agent/workflow/agent/execution.rs:61`（workflow）。
  用户已决定：workflow 子会话也视为正规 subagent，二者都纳入 list，
  因此不需要新增 origin/kind 列。
- 两层身份不同：`SwarmSessionRow.logical_id` 是对外 id，
  `root_session_id` 是 Peri 根线程 id（`peri-swarm/src/store/mod.rs:57-80`）。
- 运行时未持有可复用句柄：`AcpChild` 字段没有 `SessionResources`
  （`peri-swarm/src/actor/runtime.rs:314-324`）；`build` 在
  `runtime.rs:443-444` 用 `into_parts()` 把句柄移交给 `HostAssemblyInput`。

## 根因链

- 触发：官方 SDK 调用 `GET .../subagents` 或 `GET .../subagents/{id}`。
- 机制：路由存在，但 handler 是 `unsupported_resource`；actor/service 层
  没有把 Peri 的 child thread 投影为官方 `Subagent` 对象。
- 用户可见后果：`404 unsupported_contract`，官方客户端无法发现或读取子 agent。
- 证据性质：路由、测试、持久化事实均为代码观察；旧文档「subagent 没有产生源」
  的说法与现状不符。

## 影响面

- 用户影响：官方 Agents SDK 的 `sessions.subagents.list/retrieve` 不可用。
- 受影响模块/调用点：`peri-swarm/src/http/`、`service/`、`actor/`、`gateway/`。
- 是否可规避：可绕过网关直接查询 Peri 会话库，但不是兼容面。
- 本轮不做：`subagents/{id}/items`、`subagents/{id}/turns`、
  `subagents/{id}/turns/{turn_id}`、`subagents/{id}/turns/{turn_id}/items`
  仍返回 `404 unsupported_contract`，因为官方 item 的 `turn_id` 是必需字段，
  而子 agent 的逐轮 turn 没有持久化。

## 修复方案

- 范围：
  - 实现 list 与 retrieve 两条路由及 actor action / gateway 转发。
  - 数据源用 `list_session_tree(root_session_id)` 剔除 root；
    `session_id` 返回 `logical_id`。
  - 字段映射：`id = thread.id`；`object = "agent.session.subagent"`；
    `parent_agent_id` 对直接子会话为 `"root"`、nested 为 `parent_thread_id`；
    `name = title`；`instructions = null`；`opened_at = created_at` 的 Unix 秒；
    `closed_at = 终态时 updated_at`，否则 `null`；
    `status = Active → active，其余 → closed`。
  - 分页：`order` 默认 `desc`，`limit` 默认 20、上限 500，`after` 为排他游标；
    非法 `after` 返回 `400 invalid_query`。
  - 活会话复用 `AcpChild` 打开的门面；冷会话/重启使用懒打开的只读
    `SubagentReader`，按既有 `SessionStoreDeployment` 配置打开。
- 明确不做：
  - 不新增数据库 schema，不加 `closed_at`/origin 列。
  - 不填充 `instructions`，不实现 items/turns。
  - 不做进程级 `SessionResources` 单例注入（记录为规模化后续项）。
- 回归测试：
  - 新增 `peri-swarm/tests/it/subagents_test.rs`：真实临时 SQLite fixture
    （`Resources::open_with` + `save_child`）→ `AcpSwarmService` → HTTP。
  - 覆盖直接子会话、nested、closed、root 不出现、hidden 仍出现、
    默认/升序排序、分页与非法 `after`、未知会话/跨会话 404、
    未鉴权 401、四条未实现路由仍 404。
  - 更新 `session_resources_test.rs:1655-1663`，移除 list/retrieve 的
    `unsupported_contract` 断言。

## 验收标准

1. `GET /v1/agents/sessions/{logical_id}/subagents` 返回官方页型
   `{object, data, has_more, nextCursor}`；`data` 含直接、nested、closed
   与 workflow 子会话，不含 root。
2. `GET .../subagents/{subagent_id}` 返回单个对象；跨 session 的 id 返回
   `404 unknown_agent`。
3. 四条 items/turns 路由仍返回 `404 unsupported_contract`。
4. 未鉴权请求返回 `401`，先于 404。
5. `bash scripts/check-changed.sh` 与
   `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` 的目标用例通过。

## 验证记录

基线 `3eecbdede`，worktree `fix/swarm-subagent-list-retrieve`；以下为实际执行结果。

- `bash scripts/check-changed.sh` → 通过（`cargo check --locked -p peri-swarm`，exit 0）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --test it subagents`
  → 4 passed / 0 failed / 128 filtered。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib subagent`
  → 3 passed / 0 failed / 451 filtered。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm`
  → lib 434 passed / 20 ignored；it 124 passed / 8 ignored；doc 0。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings`
  → 通过（exit 0）。
- `bash scripts/check-layer-imports.sh` → 22 条规则，违规边 0。
- `python3 scripts/check-feature-catalog.py` → PASS。
- `git diff --check` → 通过；`wc -l peri-swarm/src/actor/runtime.rs` → 988。

未覆盖：活 `AcpChild.session_resources()` 分支无独立集成用例（与冷路径共用
`records_from`）；远程 PostgreSQL 只读打开未验证；未用真实
`openai@7.60.0` SDK 探针调用 subagents；items/turns 四条按范围仍 404。
