# Peri Swarm 官方会话时间字段对齐验收

- 状态：代码与契约测试证据记录；未运行真实 Loom/Rivet/Bun engine、真实模型链或
  真实 SDK 客户端探针
- 日期：2026-11-21
- 基线：`146a826f4`（分支 `feat/swarm-time-fields`）
- 范围：仅把 `SessionView` 的会话时间字段对齐官方 `AgentSession`：`created_at` /
  `last_active_at`，snake_case，Unix 秒 number。持久层继续保存 RFC3339 字符串，
  由投影层转换；`ItemView.created_at`、`TurnView.created_at` 等其它结构不变。
- 历史证据：[会话资源验收](peri-swarm-session-resources-2026-11-21.md) 与
  [官方默认值对齐验收](peri-swarm-official-defaults-2026-11-21.md) 保留原样，
  本记录不覆盖其结论。

## 1. 官方依据

| 事实 | 源码位置 |
| --- | --- |
| `AgentSession.created_at` 为 Unix 秒 number | `openai` SDK `resources/beta/agents/agents.d.ts` `AgentSession` |
| `AgentSession.last_active_at` 为 Unix 秒 number | 同上 |
| Swarm 的扩展字段（`logicalId`、`lastItemSeq`、`turn` 等）不在官方类型内 | 同上 |

本仓 `peri-swarm/src/domain/official.rs` 的 `OfficialSession` 已经按官方字段
（`created_at`/`last_active_at`，`i64`）投影 `agent.session.*` 事件；本记录把同一
名字与类型收敛到 HTTP `SessionView`。

## 2. 行为落地

| 项 | 实现 |
| --- | --- |
| 字段名与类型 | `SessionView.created_at: i64`、`SessionView.last_active_at: i64`，分别显式 `#[serde(rename = "created_at")]` / `#[serde(rename = "last_active_at")]`；`rename_all = "camelCase"` 仍服务其它 Swarm 扩展字段。未保留 `createdAt`/`lastActivityAt` serde alias。 |
| 投影 | `project_session` 用 `crate::domain::official::unix_from_rfc3339(&row.created_at)` 与 `...(&row.last_activity_at)` 把持久行转换到响应。 |
| 存储 | `swarm_sessions.created_at` / `last_activity_at` 仍为 `TEXT` RFC3339；schema 与写入路径未改。 |
| 内部引用 | Rust 字段从 `last_activity_at` 改名为 `last_active_at`；store 行与 patch 类型仍叫 `last_activity_at`（存储事实不变）。`actor/reader.rs`、`service/in_memory.rs` 的 `OfficialSession` 转换保持原样。 |
| e2e 脚本 | `scripts/sdk_sessions_e2e.mjs` 的摘要读取改为 `fetched.created_at` / `fetched.last_active_at`；本轮未执行该脚本。 |

## 3. 契约测试

| 行为 | 测试与关键断言 |
| --- | --- |
| POST 与 GET 的时间字段 | `session_resources_test::session_times_use_official_snake_case_unix_seconds`：create 后 GET；对两个响应都断言 `created_at`、`last_active_at` 是 JSON 整数（`as_i64()` 成功）、等于持久行 RFC3339 经 `unix_from_rfc3339` 的转换值、大于 0，且不存在 `createdAt` / `lastActivityAt`；create 与 retrieve 的两个值相等。 |
| 投影单元行为 | `service::tests::project_session_normalizes_legacy_none_environment_and_official_defaults`：固定行 `2026-01-01T00:00:00Z` 投影为 `created_at=1767225600`、`last_active_at=1767225600`，并断言 camelCase 时间字段不存在。 |

## 4. 执行命令与结果

所有命令在 `/Users/mino/code/peri/.worktrees/swarm-time-fields` 执行：

```sh
export PATH="$HOME/.local/share/uv/python/cpython-3.12.14-macos-aarch64-none/bin:/Users/mino/.cargo/bin:$PATH"
export CARGO_HOME=/tmp/cargo-home-rsproxy

cargo test -p peri-swarm
cargo clippy -p peri-swarm --all-targets -- -D warnings
cargo fmt -p peri-swarm -- --check
MISE_DISABLE_TOOLS=rust npx -y lefthook@1.13.6 run pre-commit --all-files
```

结果：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-swarm` | **334 passed, 0 failed, 6 ignored** |
| `cargo clippy -p peri-swarm --all-targets -- -D warnings` | 通过（exit 0，0 error） |
| `cargo fmt -p peri-swarm -- --check` | 通过（exit 0） |
| `lefthook run pre-commit --all-files`（npx 1.13.6，`MISE_DISABLE_TOOLS=rust`） | 首次因本环境无法解析 `static.crates.io`（`check`/`clippy` 的 `curl` 失败）而退出 1；从主 worktree 的 `target/` 复制同一 SHA 的 rmcp/hyper-util/turso patch 缓存（不进入提交）后重跑，**全部 6 个命令通过**：`check`、`clippy`、`feature-catalog`、`fmt`、`layer-imports`、`tui-components` |

分项计数：

| 目标 | passed | failed | ignored |
| --- | --- | --- | --- |
| lib 单测 | 237 | 0 | 0 |
| `actor_runtime_test` | 16 | 0 | 1 |
| `http_contract_test` | 25 | 0 | 0 |
| `items_pagination_test` | 13 | 0 | 0 |
| `loom_store_test` | 0 | 0 | 3 |
| `mcp_injection_test` | 4 | 0 | 0 |
| `openai_hosted_test` | 7 | 0 | 0 |
| `rivet_actor_test` | 0 | 0 | 2 |
| `session_resources_test` | 17 | 0 | 0 |
| `streaming_test` | 15 | 0 | 0 |

基线 `146a826f4` 的既有记录为 333 passed、6 ignored；本轮新增 1 个
`session_resources_test` 契约测试，既有用例无跳过或删改。

## 5. 限制与未验证项

- 未运行真实 Loom 库、Rivet engine、Bun/SRT 沙箱、真实模型链或真实官方 SDK
  客户端探针；上述测试均走内存 store 与真实 axum router。
- 未执行 `scripts/sdk_sessions_e2e.mjs`（需要本地 Rivet/Bun/SDK 环境），只修正了
  其读取的字段名。
- `peri-swarm/README.md` 的官方差异表仍写着旧结论；按协调要求本轮不修改该文件，
  由协调者在合并后处理。
- `docs/reference/feature-catalog.md` F-SWARM-012 描述“官方会话资源面、默认字段与
  environment 投影”，未枚举时间字段，故无需同步；本轮不改功能分级或验收映射。
- 其它 crate 未发现依赖 `SessionView.createdAt` / `lastActivityAt` 的 Rust 代码；
  `npm-packages/@peri-sdk` 的 `createdAt` 属于 Peri SDK 自身视图，不是 Swarm HTTP
  契约，未改。
- 未验证新旧 actor/gateway 混跑的滚动升级：本仓 actor 与网关同 crate 同版本编译，
  且当前无生产数据；跨版本兼容不在本轮范围。
