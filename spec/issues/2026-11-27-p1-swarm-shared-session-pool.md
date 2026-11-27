# P1：Swarm 每个会话独立打开会话存储，1000 并发建会话耗尽 PostgreSQL 连接

**状态**：已修复，PR [#620](https://github.com/KonghaYao/peri/pull/620)。
**优先级**：P1。
**类型**：性能 / 可用性。
**创建日期**：2026-11-27。
**来源**：用户压测（1000 个空 session）+ PostgreSQL 与网关日志。
**确认方式**：explorer 只读确认 + 主 agent 复核。代码基线 `3eecbdede`（`origin/main`）；运行证据来自相关文件完全相同的 `1bbb7dec6` 构建。

## 确认证据

- 测法：`python3 /tmp/peri-loadtest/run_test.py --count 1000 --concurrency 50 --tag 20261127-2345`，目标为 `deploy/docker-compose.yml`（2 个 `peri-swarm` 副本 + `postgres:16-alpine` + `rivet-engine` + `nginx`，入口 `127.0.0.1:8080`）。
- 产物：`/tmp/peri-loadtest/run-20261127-2345/`（`summary.json`、`requests.jsonl`、`samples.jsonl`、`logs/*.log`、`meta_before.json`、`probe_summary.json`）。
- 主测：**57 成功（201）/ 943 失败（全部 502 `upstream_unavailable`，`operation=acp_child`）**，wall 400.99s。
- 网关日志：943 条 `swarm ACP host assembly failed … error=无法打开远程会话存储: store operation timed out (NotApplied)`，每条等待约 20s。
- PostgreSQL 日志：**12,613 次 `FATAL: sorry, too many clients already`**；结束时连接 100/100（`peri_session` idle 74、`peri_swarm_state` active 1 + idle 24 等）。
- 空闲观测：`/tmp/peri-loadtest/idle-watch.log` 中 `session_conns` 74 → 68 → 64 → 16 → 12（约 2 分钟），与 sqlx 默认 `idle_timeout=10min` 的回收行为一致；空闲 session 仍持有连接。
- 库内：`swarm_sessions`=1013（idle 68 / **creating 943** / running 2）。

## 根因链

1. **每个 child 打开一份独立会话存储。** `peri-swarm/src/actor/runtime.rs:151-174` 的 `open_session_resources` 按 remote 环境调用 `Resources::open_deployment_in_remote_environment`；`runtime.rs:443-444` 在每次 host 装配时执行。`spawn` / `attach` / `attach_interrupted` 全部收敛到 `build`，`peri-swarm` 内没有第二条打开旁路。
2. **每份 facade 建一个独立 sqlx 池，并在打开时立即建连。** `peri-resources/src/sessions/remote/native_postgres.rs:43-48` 使用 `max_connections(2) / min_connections(0) / acquire_timeout(20s)`，随后 `pool.connect_with(options)`（`:60-63`）。sqlx 0.9.0 的 `connect_with` 在创建池时至少建立 `max(1, min_connections)` 条连接；归还连接只进入 idle 队列，默认 `idle_timeout=10min` 才回收。因此**每个空 session 在存活期占 1 条连接**，不是用完即还。
3. **bootstrap 已有部署级句柄但丢弃了它。** `peri-swarm/src/bootstrap.rs:635-640` 打开部署级 `Resources`，`:653` 与 `:666` 的 `into_parts()` 都把 `_session_resources` 丢弃，只保留 `store_shutdown`；`:672` 安装 `StaticDeployment` 时没有携带业务句柄。`peri-swarm/src/actor/service.rs:435` 与 `:1421` 因此只能让每个 child 自己打开。
4. **容量耗尽。** 2 个副本各承载多个 child，每个 child 1 条连接；PostgreSQL 默认 `max_connections=100`（`reserved_connections=3`）。57 个成功 child + 43 条基线连接占满 100 个槽位（`57 = 100 − 43` 是数字吻合，标为**推断**），后续 child 在 20s 预算内反复收到 `too many clients already`，最终超时。
5. **错误映射。** 池超时被映射为 `Timeout`，经 ACP 资源错误码后由网关输出 502 `upstream_unavailable`；因此用户看到的是连接失败，而不是可重试的容量错误。
6. **放大环节。** `service.rs:1524-1571` 的 `live_for_creating_row` 在创建中断恢复时最多打开 2 次（先 `ensure_live`，失败再 `spawn_runtime`）；本次压测未触发，但重试场景会成倍放大连接压力。每次打开还包含身份读与一条带**全局 advisory 写锁**的 machine 行写入（`native_postgres.rs:168-180`、`connection.rs:33`），1000 次打开即 1000 次跨副本串行写。

## 影响面

- 用户影响：1000 空 session 的目标场景下约 94% 创建失败（502）；即使成功，空闲 session 也长期占用连接，部署无法扩到目标规模。单会话与低并发可用，属有临时规避（降低并发/数量）。
- 受影响模块：`peri-swarm` 的 actor host 装配与部署启动；`peri-resources` 的远程会话池参数。
- 未发现数据丢失、panic 或 OOM；失败会话行停留在 `creating`，需重试恢复。

## 修复方案

### 1. 共享部署级会话资源句柄（根因）

- `peri-swarm/src/actor/deployment.rs`
  - `ActorDeployment` 增加带默认实现的方法 `session_resources() -> Option<Arc<dyn SessionResources>>`（默认 `None`）。
  - `StaticDeployment` 保存该句柄；新增 `new_with_session_resources(store, config, resources)`，保留 `new(store, config)` 委托为 `None`，避免改既有测试构造点。
- `peri-swarm/src/actor/service.rs`
  - `AcpSwarmService` 保存 `Option<Arc<dyn SessionResources>>`；新增 `new_with_session_resources(store, config, resources)`，保留 `new` 委托为 `None`。
  - `:435` 与 `:1421` 两处 `ChildConfig` 构造统一走一个 `child_config(...)` 辅助函数，避免其中一条忘记传播。
- `peri-swarm/src/actor/swarm.rs::create` 用 `deployment.session_resources()` 构造 service。
- `peri-swarm/src/actor/runtime.rs` / `runtime/session_open.rs`
  - `ChildConfig` 增加 `session_resources: Option<Arc<dyn SessionResources>>`。
  - 新增 `resolve_session_resources(config)`：**先**做现有 `machine_id/workspace_root` 半配置校验（保持 fail-closed），**再**判断注入句柄；注入时直接返回该句柄且关闭权为 `None`，未注入时经 `session_open::open_resources` 打开并把关闭权包成 `Some`。合并 #618 后，打开规则已收敛到 `session_open.rs`，`runtime.rs` 只消费解析结果。
  - `build` 使用解析结果；`HostAssemblyInput.session_store_shutdown` 在注入路径传 `None`。**这是硬约束**：若仍传 `Some`，第一个 child 退出会关闭全进程共享池（`peri-resources/src/sessions/resources/deployment.rs` 的 `close()` 会使后续写入返回 `Unavailable`）。
  - `AcpChild` 保留同一 facade 的 clone，供 `sessions.subagents` 读取；`AcpSwarmService::subagent_records` 依次复用 live child → 部署级 facade，只有未安装共享句柄的独立/本地部署才走 `SubagentReader` 惰性只读打开。
- `peri-swarm/src/bootstrap.rs`
  - `:666` 保留 `session_resources`，连同 `store_shutdown` 一起进入部署；安装 `StaticDeployment` 时传入 `Arc::clone`。
  - 状态库打开失败分支（`:653`）改为直接调用保留的 `store_shutdown.shutdown()`。
  - 更新模块文档与 `:664-665` 的“actors open their own session store”注释。

### 2. 共享池容量（32）与默认值隔离

- 新增 typed 策略 `SessionStorePoolPolicy`（建议放在 `peri-resources/src/sessions/remote/` 并重导出）：
  - `Default` = `max_connections(2)`，精确保持 TUI/CLI/stdio 现有行为。
  - `new(n)` 拒绝 0（const assert 或显式错误）。
- 参数沿打开链传递：`Resources::open_deployment_in_remote_environment_with_pool` → `sessions::open_remote_in_environment` → `composition` → `RemoteSessionData::open_with_machine_and_policy` → `RemoteConnectionFactory` → `connection::connect` → `native_postgres::connect`。
- 保留现有 `open_deployment_in_remote_environment` / `open_with_machine` 签名，委托默认策略，避免无谓测试改动。
- Swarm bootstrap 调用新入口并传 `SessionStorePoolPolicy::new(32)`。
- 连接预算：`2 × (32 + 8) + 3 = 83 ≤ 100`；其中 8 是状态库池上限（`peri-swarm/src/store/postgres/mod.rs:134-143`），3 是 PostgreSQL 保留槽。32 是**上限不是预留**，`min_connections(0)` 使空闲池可回落到 0。
- `REQUEST_BUDGET` 保持 20s，不改 `acquire_timeout`，不引入新的错误码映射。

### 3. 明确不做

- 不引入 PgBouncer 或其它连接池中间件。
- 不修改 PostgreSQL `max_connections` 或部署拓扑。
- 不拆全局 advisory 写锁，不改 `swarm_sessions` / schema。
- 不改本地 SQLite 的 per-child 打开语义；不注入时行为不变。
- 不把 503 映射、`/metrics`、创建闸门纳入本修复（另开任务）。
- 不让 child 取得共享池的关闭权。

### 4. 已知遗留风险（写入验收与 PR）

- **故障隔离粒度变化**：共享 facade 的连接代际失效是每 facade 一份（`remote/generation.rs` + `session_data/connection_lifecycle.rs`），一个 session 的 20s 超时会退役全进程连接，其它 session 可能短暂收到 `Unavailable("... retired before use")`。本次复测 0 次该错误，但只是“没有发生超时”的观察，不是对故障隔离的验证。
- **进程退出顺序**：bootstrap 在 `drain` 后关闭部署级池（`:760-790`）；10s grace 超时后 `abort` 是否已释放全部 child 未验证。共享后若仍有 child 存活，会拿到 `Unavailable` 而非继续工作；进程退出即停是可接受语义，但需在验收记录中写明。
- **1000 个 live Agent 的内存/吞吐**：修复前按 57 个 live session 折算约 4.24 MiB/会话；本次 1000 个空 session 复测的网关峰值为 727.7 MiB/副本（约 510 host/副本，≈1.4 MiB/host）。两组样本的 host 状态不同，不能直接外推；1000 个同时执行 turn 的 Agent 吞吐与长时内存仍是独立缺口。

## 验收标准

1. 目标测试通过：`./scripts/check-changed.sh`（至少覆盖 `peri-resources` 与 `peri-swarm`）。
2. 回归测试：
   - 注入句柄时 `resolve_session_resources` 不打开任何存储，且关闭权为 `None`；`Arc::ptr_eq` 命中。
   - 半配置的 remote 身份即使注入句柄也保持 fail-closed。
   - 未注入时本地模式仍创建 per-child 存储。
   - `SessionStorePoolPolicy::default()` 仍为 2；swarm 传入 32。
   - 新增/更新 6 处 `ChildConfig` 字面量后既有测试全绿。
3. 运行时复测（重建镜像后）：
   - 用新 tag 重跑 `python3 /tmp/peri-loadtest/run_test.py --count 1000 --concurrency 50`。
   - PostgreSQL 日志无新增 `too many clients already`；`peri_session` 服务端连接峰值 ≤ 64（2 × 32）。
   - 1000 个新 session 最终无 `creating`，请求无批量 `store operation timed out` 导致的 502。
   - 记录 gateway/postgres/rivet 的 CPU、内存峰值与磁盘增量。
4. 文档同步：
   - `docs/design/peri-swarm.md` §2 记录“每进程一份会话存储句柄/池，actor 共享业务句柄，部署持有唯一关闭权；本地 SQLite 仍 per-child”。
   - `docs/design/peri-swarm.md` §6 PostgreSQL 段记录池上限与连接预算公式。
   - `docs/design/peri-swarm.md` §8 把“没有吞吐/并发压力证据”更新为本次实测与修复状态，保留未复测边界。
   - `docs/verification/peri-swarm-1000-concurrent-sessions-2026-11-27.md` 记录环境、测法、原始产物路径、修复前后对比与证据局限。

## 验证记录

- 2026-11-27，worktree `/Users/mino/code/peri/.worktrees/swarm-session-store-pool`，分支
  `fix/swarm-session-store-pool`，基线 `3eecbdede`。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib`：**435 passed / 0 failed /
  20 ignored**。新增的 `actor::runtime::tests` 3 条（注入句柄不打开存储且无关闭权、半配置
  fail-closed、未注入仍 per-child）与 `service_test::child_config_propagates_the_shared_handle_to_both_child_paths`
  均通过。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib`：**627 passed / 0 failed /
  122 ignored**。本机未设置 `PERI_TEST_POSTGRES_URL`，真实 PostgreSQL 用例均为
  `#[ignore = "requires PERI_TEST_POSTGRES_URL"]`；新增 `pool_policy` 3 条（默认 2、
  `new(32)=32`、`new(0)` panic）通过。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --test it actor_runtime`：
  **20 passed / 0 failed / 1 ignored**（ignored 为既有的本地 ACP 测量）。
- `./scripts/check-changed.sh`：通过（`cargo check --locked` 覆盖 15 个改动 crate 及其反向
  依赖，`Finished dev profile`）。
- `cargo fmt --check`、`git diff --check`：通过。
- 运行时 1000 并发空 session 复测：镜像 `sha256:2adb9281dc49333e02ab0e15087872c1ce4f8da46c3d2f5e140f4eb301e6226d`
  由本分支构建。`python3 /tmp/peri-loadtest/run_test.py --count 1000 --concurrency 50 --tag 20261128-0039`
  → **1000/1000 成功（201）**，wall **7.08s**，吞吐 141.2/s；**0 次** `too many clients already`，
  **0 条** `ACP host assembly failed`，**0 条** `store operation timed out`；网关会话库连接
  **32+32**（池上限）、状态库 **8+8**；新 session 1000 全部 `idle`。主机侧 `dbx` 客户端另占
  11 条会话库 + 7 条状态库连接，不属部署。详见
  [验收记录](../../docs/verification/peri-swarm-1000-concurrent-sessions-2026-11-27.md)。
- 2026-11-27 合并 `origin/main`（`cc76e2ad9`，#618 `sessions.subagents`）并解决
  `runtime.rs` / `service.rs` 冲突后：
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib`：**439 passed /
    0 failed / 20 ignored**；新增
    `subagent_reads_use_the_shared_facade_instead_of_opening_per_actor`。
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --test it`：**124 passed /
    0 failed / 8 ignored**。
  - `./scripts/check-changed.sh`：通过。
  - 合并新增的 subagent 读取路径未再次进入 1000 并发运行时样本；证据边界见验收记录 §6。
