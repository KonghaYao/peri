# Workspace MCP 统一任务归属（Task Owner）改造

- 状态：已确认，待实施
- 日期：2026-10-22
- 范围：`mcp-packages/workspace`、`peri-middlewares`、`peri-mcp-core`、workspace CLI 与相关测试/文档
- 目标：workspace MCP 默认是通用 MCP 服务；Peri 的 session 级后台任务隔离收敛为一个显式 owner 概念

## 1. 背景

当前 workspace MCP 通过 `TaskScopeAuthority` + `_meta["peri/taskScope"]` 做 session 级任务隔离。该实现存在以下问题：

1. 独立 CLI 默认可能要求 Peri 专有元数据，外部 harness 无法直接使用。
2. `trusted_connection` 的 token 是 `v1.<base64url(session_id)>`，只是编码，不是秘密；真正的信任来自连接边界。
3. `validate_current_invocation` 同时校验 `peri/taskScope` 与 `peri.invocation.initiatorSessionId`。两个值都由客户端提供，无法防御恶意客户端，只能发现字段不一致。
4. `workspace/taskOpen|taskClose|taskSnapshot|taskChanges` 为自定义生命周期，带来 epoch、barrier、连接重建、丢失任务归属等额外复杂度。
5. 会话恢复能力与 Peri “不持久化 Agent 执行恢复状态”的架构目标冲突。
6. 为了让外部 harness 使用，曾需要注入 `peri/taskScope` 的代理；这是协议设计问题的症状。

## 2. 已确认决策

1. 并发后台任务是多数场景，父子 Agent 并发后台任务是 P0。
2. 任务归属 = 直接发起会话；父会话结束/取消不得误清理仍存活的子会话任务。
3. 会话开始/结束只是 per-session 生命周期事件，不能使用全局 open/close。
4. 移除跨进程会话恢复；不持久化任务归属、恢复 locator、terminal delivery ACK。
5. 删除 `validate_current_invocation`。
6. 删除 per-session token；远程场景依赖部署级认证或 loopback 信任。
7. 事件订阅使用 MCP 2026-07-28 标准 `subscriptions/listen`；通用 server 可不实现。
8. 稳定连接下删除客户端 reconnect/subscription 去重；仅保留必要的本地幂等。
9. 删除 `workspace/taskOpen|taskClose|taskSnapshot|taskChanges`；保留标准 `tasks/get|cancel`。
10. 文件 rewind 使用很少；本轮不迁移计算，继续由 workspace 服务端计算；是否纳入通用工具另行决定。
11. 统一为一个 authority：`owner = 直接发起会话的 session id`，由宿主在请求中显式注入。

## 3. 目标模型

### 3.1 唯一身份

- 请求元数据键：`_meta["peri/taskOwner"]`，值为字符串 session id。
- 缺省允许：没有 owner 时，7 个基础工具照常工作；后台任务不参与 session 隔离。
- owner 只存在于当前进程；不持久化、不跨重启恢复。
- 模型不能通过工具参数伪造 owner；宿主在工具桥接层注入。

### 3.2 服务端

- `WorkspaceMcpServer` 不再持有 `TaskScopeAuthority`。
- 每次请求从 `_meta["peri/taskOwner"]` 解析 owner。
- 后台 Bash、`tasks/get|cancel`、任务事件过滤统一使用该 owner。
- 删除 `workspace/taskOpen|taskClose|taskSnapshot|taskChanges`；调用返回标准 method-not-found。
- `workspace/rewindFiles` 保留；owner 可选，存在时按 owner 准入。
- `subscriptions/listen` 保留标准 0728 语义；有 owner 时只投递该 owner 的任务事件。

### 3.3 客户端

- `McpClientPool` 不再维护 token 表或 authority。
- `task_owner_meta(source, session_id)`：对 builtin workspace 与 `WorkspaceRemote` 返回 `_meta["peri/taskOwner"] = session_id`；其他 server 不注入。
- `tool_bridge`、`rewind`、标准 `tasks/get` 使用同一 helper。
- 删除 `open_workspace_task_scope`、`close_workspace_task_scope`、`reconcile_closing_workspace_scope`、`resolve_execution_evidence`。
- 会话释放时只取消 `TaskManager` 已知 external task，并等待 settled；不再做 snapshot/close barrier。

### 3.4 独立 CLI

- 默认 generic：不要求任何 `peri/*` 元数据。
- 删除 `--trusted-task-scope`。
- 显式 owner 仅通过请求 `_meta` 传入；如需固定 owner，可由部署层代理注入，不在 CLI 增加双模式。

## 4. 改造计划

1. 核心类型
   - 将 `peri-mcp-core/src/task_scope.rs` 替换为 `task_owner.rs`：
     - 保留 `TASK_OWNER_META_KEY` 与 `task_owner_from_meta`；
     - 删除 `TaskScopeAuthority`、`TaskScopeCapability`、`issue`、`resolve_capability`、token 编解码。
   - 更新 `Cargo.toml` 依赖（移除仅 token 使用的 base64）。
2. Workspace 服务端
   - `workspace.rs`：删除 authority 字段/构造器；owner 从 meta 读取；删除 invocation 校验；删除四个自定义方法。
   - `shell_tasks.rs`：`scope` 统一改名 `owner`；删除 snapshot/changes/close/open；保留 admit/get/cancel/update/belongs_to。
   - `task_takeover_process.rs`：删除（恢复/接管场景已移除）。
   - `workspace_tasks_wire_test.rs`：删除生命周期用例，新增 generic 无 owner 用例。
3. 独立 CLI
   - 删除 `--trusted-task-scope` 与 authority 构造。
4. Peri 客户端
   - `task_scope_metadata.rs` 改为 `task_owner_metadata.rs`；删除 token 表/authority。
   - `tool_bridge`、`rewind`、`subscription_task_binding` 改用 owner meta。
   - 删除 `subscription_tasks.rs` 的自定义生命周期与 reconciliation。
   - `release_agent_session` 仅取消已知 external task。
   - `builtin/dispatch.rs` 不再注入 authority。
5. 文档
   - 更新 `docs/design/session-async-tasks.md`：删除 token、snapshot/changes、open/close、恢复语义；写明 owner 与并发 P0。
   - 更新 `docs/code-index/mcp-packages.md`：改为 owner 契约。
6. 验证
   - `cargo check` / `cargo test`：`peri-mcp-workspace`、`peri-middlewares`。
   - 新增 wire test：无 `_meta` 的 generic 客户端可 Write/Bash。
   - 新增 owner 隔离测试：A/B 两个 owner 的后台任务互不可见、不可取消。
   - 用 `peri -p` + boxlite sandbox 回归一次真实链路。
7. 提交
   - 单独分支、单独 commit；PR 描述引用本 issue。

## 5. 验收标准

1. 通用 MCP 客户端不发送任何 `peri/*` 元数据时，7 个基础工具可用。
2. Peri 客户端发送 `peri/taskOwner` 时，后台任务按直接发起会话隔离。
3. 父子 Agent 并发后台任务：父会话结束不误清理子会话任务。
4. 不再存在 `TaskScopeAuthority`、per-session token、invocation 一致性校验、taskOpen/taskClose/snapshot/changes。
5. `tasks/get|cancel` 与 `subscriptions/listen` 只使用标准 MCP 方法。
6. 相关 crate 的 check/test 通过。

## 6. 风险与回滚

1. 删除自定义生命周期后，依赖 `workspace/taskSnapshot` 的旧客户端会收到 method-not-found；Peri 客户端同 PR 更新，不保留兼容层。
2. 失去跨连接任务恢复；后台任务在连接重建后不可见，符合“无执行恢复”的架构目标。
3. 远程 workspace 若暴露到 loopback 之外，需要部署级认证；本轮不新增认证层，文档明确边界。
4. 回滚方式：整 PR revert；不保留双写/兼容分支。

## 7. 不在本轮范围

1. rewind 计算位置迁移。
2. 新的传输认证/多租户隔离。
3. TUI 任务展示改版。
4. boxlite sandbox 实验代码（独立 side-project，不进本 PR）。
