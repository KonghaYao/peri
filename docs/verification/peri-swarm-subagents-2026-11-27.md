# Peri Swarm `sessions.subagents` list/retrieve 验收（2026-11-27）

**基线**：`3eecbdede`（主 checkout）+ 本 worktree 未提交改动。
**范围**：实现官方 `GET /v1/agents/sessions/{logicalId}/subagents` 与
`GET .../subagents/{subagentId}`。`subagents/{id}` 下的 items/turns 四条路由
仍返回 `404 unsupported_contract`，不在本轮范围。

## 实现事实（本批）

- 数据源是 Peri 会话树的 `SessionResources::list_session_tree(root_session_id)`，
  剔除 root 后投影；SQLite 递归 CTE 仍按 `created_at ASC`、不过滤 `hidden`，
  排序在 service 层按官方 `(opened_at, id)` 重做。
- `session_id` 返回 Swarm 行对外的 `logical_id`；`id` 是 Peri thread id。
- 直接子会话的 `parent_agent_id` 为 `root`，nested 保留父 thread id。
- `name = title`；`instructions` 恒 `null`；`opened_at = created_at` 的 Unix 秒；
  `closed_at` 仅在终态时取 `updated_at`；`status = active|closed`。
- 活会话复用 `AcpChild` 已打开的 `SessionResources`；冷会话由
  `SubagentReader` 懒打开只读 store（本机只读 seam 不建目录/库/迁移）。
- 未新增数据库 schema、列或 store 表结构；repository 中 workflow 与
  Task/Agent 子会话都是同一种 child thread，因此没有 origin/kind 列。

## 命令与结果

在 `/Users/mino/code/peri/.worktrees/swarm-subagent-list-retrieve` 执行；
`cargo-rmcp-patched.sh` 会忽略传入的 `CARGO_TARGET_DIR` 并使用 worktree 内的
`./target`（wrapper 自身提示）。

| 命令 | 结果 |
| --- | --- |
| `CARGO_TARGET_DIR=/Users/mino/code/peri/target bash scripts/check-changed.sh` | 通过（`cargo check --locked -p peri-swarm`，exit 0） |
| `... ./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --test it subagents` | 4 passed；0 failed；128 filtered |
| `... test --locked -p peri-swarm --lib subagent` | 3 passed；0 failed；451 filtered |
| `... test --locked -p peri-swarm --lib subagents` | 3 passed；0 failed；451 filtered |
| `... test --locked -p peri-swarm` | lib 434 passed / 20 ignored；it 124 passed / 8 ignored；doc 0 |
| `... clippy --locked -p peri-swarm --all-targets -- -D warnings` | 通过（exit 0） |
| `git diff --check` | 通过（无 whitespace error） |

## 覆盖矩阵（`peri-swarm/tests/it/subagents_test.rs`）

真实临时 SQLite fixture：`Resources::open_with(Some(db))` →
`resolve_workspace` → `create_session` → `save_child`（direct + nested）→
`update_session_meta(status=Done)` → shutdown → `MemorySwarmStore` 写
`logical_id/root_session_id` → `AcpSwarmService` → `build_router` 走 HTTP。

- 直接子会话、nested（workflow 形态）、closed 子会话均出现在页中；
  root 不出现；`hidden=true` 的子会话仍出现。
- 默认 `desc` 与 `order=asc` 的稳定排序。
- `limit` 截断、`has_more`、`nextCursor` 与 `after` 排他游标。
- 非法 `after`（含 root id）、`limit=0`、`limit=501`、非法 `order` →
  `400 invalid_query`。
- 未知会话 list/retrieve → `404 unknown_agent`；跨会话 subagent id →
  `404 unknown_agent`；root id retrieve → `404 unknown_agent`。
- 已知无子会话会话 → 空页（`data=[]`、`has_more=false`、`nextCursor=null`）。
- 未鉴权 list/retrieve → `401`，先于 404。
- `subagents/{id}/items`、`turns`、`turns/{turnId}`、`turns/{turnId}/items`
  仍为 `404 unsupported_contract`，`details.resource` 分别为
  `sessions.subagents.items` / `sessions.subagents.turns` /
  `sessions.subagents.turns.items`。

## 收尾修正（同 worktree 第二轮）

主 agent 审阅后完成三项收尾：

1. 新增 `peri-swarm/src/actor/runtime/session_open.rs`，把 `open_resources`
   （共享打开规则）与 `open_session_resources`（`ChildConfig` 薄包装）移出
   `runtime.rs`。`SubagentReader::open_read_only` 改为调用 `open_resources`，
   不再复制 remote/local match 或半对配置错误文案。
2. `docs/reference/feature-catalog.md` 的 F-SWARM-012 追加 `subagents`；
   为满足 100 字符行宽门禁，采用压缩措辞
   `官方资源；list/update/delete/turns/subagents，默认字段/环境 none，空 update no-op，未映射资源显式 404`。
3. 本记录补充本轮重跑结果。

### 行数（实际 `wc -l`）

```text
     988 peri-swarm/src/actor/runtime.rs
      59 peri-swarm/src/actor/runtime/session_open.rs
    1047 total
```

`runtime.rs` 已回到 `STD-SIZE-001` 的 1000 行以内。

### 重跑命令与真实结果

| 命令 | 结果 |
| --- | --- |
| `bash scripts/check-changed.sh` | 通过（exit 0，`Finished dev profile`） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --test it subagents` | 4 passed；0 failed；128 filtered |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib subagent` | 3 passed；0 failed；451 filtered |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` | lib 434 passed / 20 ignored；it 124 passed / 8 ignored；doc 0 |
| `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings` | 通过（exit 0） |
| `bash scripts/check-layer-imports.sh` | 22 条规则，违规边 0 |
| `python3 scripts/check-feature-catalog.py` | 通过：F-SWARM-012 可见行 100 字符（上限 100），exit 0 |
| `python3 scripts/check-doc-links.py` | 仓库无此脚本（skip） |
| `wc -l peri-swarm/src/actor/runtime.rs peri-swarm/src/actor/runtime/session_open.rs` | 988 / 59（见上） |
| `git diff --check` | 通过（无 whitespace error） |

### 行宽处理

F-SWARM-012 的可见行按 100 字符上限压缩为
`官方资源；list/update/delete/turns/subagents，默认字段/环境 none，空 update no-op，未映射资源显式 404`，
`check-feature-catalog.py` 通过。

### 仍未覆盖

- 活 `AcpChild.session_resources()` 复用分支没有独立集成用例；冷
  `SubagentReader` 路径由 fixture 覆盖，两条分支共用 `records_from`。
- 远程 PostgreSQL 会话存储的只读打开未验证；本机 SQLite 只读路径已覆盖。
- 未用真实 `openai@7.60.0` SDK 探针调用 `subagents.list/retrieve`。
- `subagents/{id}/items`、`subagents/{id}/turns*` 按范围仍 404。
- 未做进程级 `SessionResources` 单例注入；每次冷读由 actor 懒打开并缓存。
- `instructions` 恒 `null`：网关没有把子 agent 的开启任务持久化为官方内容。
- `parent_thread_id` 缺失（不应出现在树中）时按直接子会话投影为 `root`。

