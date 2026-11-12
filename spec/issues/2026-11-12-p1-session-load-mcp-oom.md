# P1：会话切换未要求关闭导致 MCP 进程线性增长

**状态**：已修复，待 PR / CI 与生产 512MB cgroup + 真实 Hindsight 验收。
**优先级**：P1。
**类型**：错误 / 性能（资源生命周期）。
**创建日期**：2026-11-12。
**来源**：用户报告、GitHub #394、cgroup OOM 日志与代码审计。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `40d1a65f93e4`）。

## 确认证据

1. 用户复现：512MB cgroup 内依次打开 14 个不同历史会话（26 次 `session/load`）后，常驻约 12 组 `npm exec hindsight` + `node`，最终由 cgroup OOM killer 终止 `peri`；旧会话未被 `session/close`。
2. 每次冷 load 经 `SessionEnvironment::assemble_prepared` → `assemble_server_config_with_mcp_profile(session_scoped=true)` 创建独立 `McpClientPool` 并初始化 stdio MCP；环境随后由 `SessionState.environment` 长期持有（`peri-acp/src/host/workspace.rs:230-266,402-409`、`peri-acp/src/host/assemble.rs:406-417,509-520,696-720`、`peri-acp/src/host/mod.rs:185-206`）。
3. `prepare_shared` 只对同 ID 的 live session 复用；不同 ID 走 cold restore 并发布新的 `SessionState`（`peri-acp/src/host/requests/session_restore/load_shared.rs:116-170`）。
4. 当前 wire 路由允许同一连接连续成功执行 `session/new|load|resume|fork`，`ConnectionContext` 没有当前会话/切换状态（`peri-acp/src/host/connection.rs:11-35`、`peri-acp/src/host/server_loop.rs:477-617`）。
5. `session/close` / `session/delete` 成功后才完成会话资源排空和 map 移除；失败路径保留旧会话（`peri-acp/src/host/requests/session_close.rs:115-160`、`peri-acp/src/host/requests_close_failure_test.rs:369-450`）。
6. 初始 load 与只读历史请求已有明确分流：`HistoryPage` / `HistoryWindow` 在执行生命周期之前返回（`peri-acp/src/host/server_loop.rs:523-598`）。
7. 恢复协调即使没有 `working` child，也无条件读取整棵 `session_tree`（`peri-acp/src/host/recovery/coordinator.rs:143-164`），放大历史切换时的 SQLite 工作量。
8. 本机未运行带真实 Hindsight 与 512MB cgroup 的完整复现；运行时结论来自用户日志，生命周期与查询机制由当前代码及基线测试确认。

## 根因链

1. 客户端在同一 ACP 连接上 load 会话 A，Host 为 A 建立独立执行环境、MCP pool 与 stdio 子进程。
2. 客户端未先调用 `session/close(A)`，又 load 不同会话 B；服务端没有连接级切换约束，因此接受 B 并保留 A。
3. 每个新 ID 重复上述过程，live `SessionEnvironment` 与 stdio MCP 进程数按已打开会话数增长，而不是按当前会话数保持有界。
4. 在 512MB cgroup 中，Hindsight 进程树成为主要内存消费者并最终触发 OOM；无条件 `session_tree` 查询在 swap 压力下进一步拉长 load。

## 影响面

- 用户影响：错误客户端或缺少关闭逻辑的集成只需顺序打开多个历史会话，即可耗尽容器内存并使 Agent 不可用。
- 受影响模块/调用点：ACP connection 状态、server loop 的四个 setup 方法、`session/close` 成功提交、恢复树读取；所有 per-session stdio MCP 都会放大影响。
- 是否可规避：客户端严格执行 `session/close(old) → setup(new)` 可规避；Peri TUI 已这样实现，但服务端当前未强制，第三方客户端可绕过。
- 定级依据：核心会话加载路径可稳定触发进程级资源耗尽，但存在明确关闭规避，且不涉及数据损坏或安全边界，定为 P1。

## 已批准契约

用户于 2026-11-12 裁决：同一 ACP 连接默认表达**会话切换关系**；`session/close` 是切换前的稳定关闭方式，客户端必须先成功关闭当前会话，服务端负责强制该顺序。

具体不变量：

1. 一个开放连接至多绑定一个客户端 setup 会话；`session/new|fork` 在已有当前会话时拒绝。
2. `session/load|resume` 目标不同于当前会话时拒绝；同 ID 重载仍允许且不得创建第二套环境。
3. setup 并发必须先取得连接级 reservation；失败或响应未成功发送时回滚，成功响应发送后才提交当前会话 ID。
4. 只有匹配当前 ID 的 `session/close` / `session/delete` 成功响应发送后才释放连接绑定；关闭失败继续阻止切换。
5. cursor/history/window/child history 是只读浏览，不建立或改变当前会话，不要求关闭。
6. 内部 recovery child 不经过 wire setup gate，不占用或改写连接的当前会话身份。
7. 责任边界：外部集成必须在切换前调用 Peri 的 `session/close`（或 `session/delete`）并等待成功响应；Peri 负责在未关闭时拒绝新的 setup，不自动关闭、LRU 淘汰或池化旧会话。未按该约定使用导致的旧会话资源驻留或泄露不由 Peri 承担。

## 修复方案

- 范围：
  1. 在 `ConnectionContext` 下增加连接级 session switch 状态机与 RAII setup reservation，按 generation 防止失败请求回滚其他并发 setup。
  2. 在 `ServerLoop::dispatch_request` 完成历史只读分流后、任何声明校验和环境装配前执行 setup 准入。
  3. setup 成功响应发送后提交 session ID；错误或发送失败由 reservation 回滚。
  4. 匹配当前 ID 的 close/delete 成功响应发送后释放绑定；失败或关闭其他 ID不释放。
  5. `server_loop.rs` 已超过 1000 行，先把 `dispatch_notification` 按职责拆到 `host/server_loop/notification.rs`，使修改后满足 `STD-SIZE-001`。
  6. `HostSessionRecovery::prepare` 仅在存在 `working` child 时读取 `list_session_tree`。
  7. 同步 ACP 协议设计、架构契约与 code index，明确 close-before-switch。
- 明确不做：不自动关闭或 LRU 淘汰旧会话；不共享通用 MCP pool；不增加固定会话上限；不修改数据库 schema/索引；不处理 OOM 后重启或 relay 恢复；本轮不改 `list_session_tree` SQL。
- 外部验证依赖：`KonghaYao/peri-e2e` 的 `helpers/llm-replay-tui.ts::prepareReplaySessions` 仍在同一连接上连续 `session/new`；需改为每次创建后立即 close（或每个会话使用独立连接），并更新 `.github/peri-e2e.rev`，否则 E2E replay 会按新契约正确拒绝第二个会话。
- 回归测试：
  - `peri-acp/src/host/connection_test.rs`：reservation、不同目标拒绝、同 ID复用、失败回滚、旧 generation 不覆盖新状态、close 匹配释放。
  - `peri-acp/src/host/requests_close_failure_test.rs` 或独立 server-loop 测试：关闭失败仍拒绝切换，关闭成功后放行，setup 响应失败不提交。
  - history wire 测试：当前会话存在时 cursor/window/child history 仍可读取。
  - recovery 测试：内部 child 不经过连接 gate；无 `working` child 时不读取整树。

## 验收标准

1. 同一连接成功 setup A 后，未 close 时 setup B 在环境装配/MCP spawn 前以 `-32010` 和稳定 reason 拒绝；A 保持可用。
2. `session/close(A)` 失败时仍拒绝 B；成功响应发送后 B 可建立。
3. 并发 setup 不能同时创建两套客户端根会话环境。
4. 同 ID load/resume 保持兼容且不新增环境。
5. history page/window/body/child 只读请求不受切换 gate 影响。
6. recovery child 仍可按内部路径装配，且不改变连接当前会话。
7. idle/root-only load 不调用 `list_session_tree`。
8. `server_loop.rs` 与所有修改源码/测试文件不超过 1000 行；不包含 schema 变更。
9. 目标测试、`scripts/check-changed.sh`、全量 async check/clippy、`git diff --check` 通过。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::workspace_cases`（修复前基线）→ 15 passed，0 failed；存在 1 条 macOS linker compact-unwind warning。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- session_switch` → 10 passed，0 failed；覆盖状态机、真实 close 失败/成功、history bypass 与响应 write failure 的 reservation 回滚边界。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- recovery_wire` → 1 passed，0 failed；过滤词已从修复前 0 tests 假绿变为非零。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- close_failure` → 10 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- recovery_child` → 20 passed，0 failed，1 ignored（既有产品裁决用例）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- connection` → 40 passed，0 failed；旧 fork wire 用例已同步为先成功 close 当前会话再 fork。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page_wire` → 79 passed，0 failed，1 ignored（既有 16 MiB 显式性能夹具）。
- CodeRabbit 后续 review 的四项 restore ownership finding 已收口：cold replay 失败清理、generation 精确回滚、Incomplete 保留 retry owner、fork cold source 成功/失败清理且 preexisting source 保留。authority 已下沉到 `load_shared::prepare_shared/rollback_restore_publication`，server loop 不再维护第二份 publication。响应 write failure 按既有 transport terminal + host shutdown 契约不复制清理。
- `cold_replay_failure`、`restore_publication`、`restore_cleanup_incomplete`、`fork_cold_source`、`fork_preexisting_source`、`fork_wire_uses_the_shared_preparation_path` 六个过滤词各命中 1 条并通过；Incomplete 用例同时验证 typed `reason/sessionId`、reload 拒绝与显式 close 重试。
- `bash ./scripts/check-changed.sh` → exit 0；实际执行 `cargo check --locked -p peri-acp -p peri-tui -p peri-wasm`。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-acp --all-targets -- -D warnings` → exit 0。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib` → 1260 passed，0 failed，2 ignored。
- `cargo fmt --all -- --check` → exit 0；`git diff --check` → exit 0。修改 Rust 文件均 ≤1000 行，`server_loop.rs` 为 955 行。
- 权威文档分层复核：稳定不变量在 `ARC-ACP-SESSION-SWITCH-001`，wire 语义在 `docs/design/peri-acp-protocol.md`，代码落点在 `docs/code-index/peri-acp.md`，本文件只保留在办验收。
