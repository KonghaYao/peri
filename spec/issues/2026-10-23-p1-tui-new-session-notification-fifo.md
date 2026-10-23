# P1：新建会话通知乱序导致启动后 `/` 列表丢失 skill

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：回归 / 错误。
**创建日期**：2026-10-23。
**来源**：用户报告。
**确认方式**：主 agent 运行时复现 + explorer 只读确认 + 主 agent 复核（基线 `07f5e982a`）。

## 确认证据

- 运行时复现（`RUST_LOG=debug`，修复前，启动后立即输入 `/`）：
  - `02:17:56.196` 首次 MCP discovery projection boundary（session/new 的 prewarm）。
  - `02:17:56.222` `core 域技能命令投影已刷新 ... added=27`。
  - `02:17:56.223` notifier：`updated AVAILABLE_SLASH_COMMANDS (46)`。
  - `02:17:56.235` TUI：`initial session created`。
  - `02:17:56.235` notifier：`updated AVAILABLE_SLASH_COMMANDS (19)`。
  - 最终列表为 19 条旧快照，`/fix` 搜不到 `fix-bug`；直到外部 MCP server 连接触发刷新才恢复。
- 修复后同一复现流程：`updated AVAILABLE_SLASH_COMMANDS (19)` → `(34)`，列表立即出现 skill，
  `/fix` 命中 `/fix-bug`，无需外部事件。
- 代码路径：
  - `peri-acp/src/host/requests/session_lifecycle.rs:559-573`：先发首帧，再 prewarm。
  - `peri-tui/src/acp_client/interaction_lifecycle.rs:399-425`：`route_ordinary` 的缓冲/转发决策。
  - `peri-tui/src/acp_client/client/session.rs:225-240`：`commit_stable` 后仍 await 初始化，再 flush。

## 根因链

1. `session/new` 期间首帧 `available_commands_update`（19 条，skill 尚未投影）进入 `new_buffer`
   （观察 + 代码：`route_ordinary` 的 `Transitioning(New)` 分支）。
2. 响应到达后 `commit_stable` 把路由改为 `Stable`，同时把缓存取出交调用方暂存。
3. `finish_session_initialization(...).await` 可能等待 `session/input/snapshot` RPC；该窗口内
   第二帧（46 条，含 skill）因路由已是 `Stable` 被立即 Forward。
4. 初始化结束后才 `flush_buffered` 投递首帧，wire FIFO 被破坏；`AVAILABLE_SLASH_COMMANDS`
   “最后一次写入获胜”，旧快照覆盖含 skill 的快照。
5. 触发条件：启动后会话创建完成到初始化 RPC 返回之间出现注册表变更（启动预热首次
   投影 skill 即满足）；用户可见后果为 `/` 列表与搜索均无 skill。

## 影响面

- 用户影响：启动后斜杠命令列表缺少 skill；可稳定复现，发送任意消息或等待外部 MCP
  连接事件可临时规避。
- 受影响模块/调用点：`interaction_lifecycle.rs` 的普通通知缓冲与转发、
  `pump.rs::flush_buffered`、`session.rs` 的 new/load 路径；同一顺序问题也会影响
  config/session_info/plan/history_committed 等 `session/update`。
- 是否可规避：是（触发后续注册表刷新），但启动首屏不可靠。

## 修复方案

- 范围：`commit_stable` 保留缓存并置 flush pending；pending 期间 `route_ordinary`
  继续入队；新增原子批量 flush（持锁取批、空队列同临界区清 pending）；new/load
  初始化结束后调用；transition 开始/失败时重置 pending。
- 明确不做：不改 ACP wire，不改服务端首发/预热契约，不引入命令修订号，不重构
  其它生命周期逻辑。
- 回归测试：`interaction_lifecycle_test.rs`（顺序、pending、fail 清理）、
  `client_reverse_test.rs`（挂起 `session/input/snapshot` 期间的第二帧）。

## 验收标准

1. 启动后立即输入 `/` 能搜到 skill，无需等待外部 MCP 连接事件。
2. 定向测试与 `peri-tui` lib 全量测试通过；`git diff --check` 无输出。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- acp_client::interaction_lifecycle::tests`
  → 19 passed, 0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- reverse_tests`
  → 23 passed, 0 failed（含新增回归）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib`
  → 2313 passed, 0 failed, 9 ignored。
- `./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui` → 成功。
- `cargo fmt -p peri-tui -- --check` → 无差异。
- 真实 TUI 复现（修复后）：`AVAILABLE_SLASH_COMMANDS (19)` → `(34)`，`/fix` 命中
  `/fix-bug`。
