# peri-swarm

本地单机 Agent 网关：HTTP 客户端 → Rivet actor → 进程内 Peri ACP Host。Hosted 工具经
Bun 托管的 workspace MCP 执行。Peri 会话与 Swarm 状态分别保存在两个 PostgreSQL 数据库
（同一实例、不同数据库即可），只部署 PostgreSQL 就能同时支撑「会话库 + 状态库」。

- **接入**：[启动顺序](#启动顺序)、[Node SDK 示例](#node-sdk-同会话追问与续读)。
- **实现契约**：[Peri Swarm 设计](../docs/design/peri-swarm.md)；[生产拓扑](#生产部署拓扑目标)仅为目标。
- **验收**：[本地环境](../docs/verification/peri-swarm-local-dev.md)、
  [追问、空闲回收与宿主中断](../docs/verification/peri-swarm-local-followup-2026-11-23.md)、
  [状态库 PostgreSQL 迁移](../docs/verification/peri-swarm-postgres-state-2026-11-24.md)。

这是从源码构建的本地开发路径。Agent CLI 的 `agent-v*` 发布资产不分发 Swarm 服务；
Swarm 的 OpenAI 兼容客户端使用 npm `openai`，不是 `@peri-code/sdk`。本文是操作手册，
字段和错误码以设计、源码与契约测试为准。

## 前置

- macOS（SRT 走 `sandbox-exec`）或带 `systemd-run --user` 的 Linux；非 root。
- PostgreSQL 数据库及可建表、读写的账号；**两个**数据库（会话库 + 状态库）。
- Rust（仓库补丁脚本）、Bun >= 1.4、Node >= 22.12。
- 沙箱内运行的 Peri 二进制：`./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri`（产物 `<repo>/target/debug/peri`）。
- 本机 8080 若被占用，同时改 `PERI_SWARM_BIND` 和 SDK 的 `baseURL`；不要只改一处。

## 构建

Rust 二进制必须经补丁脚本构建；直接 `cargo run -p peri-swarm` 会在依赖解析阶段失败。

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm

# openai_hosted 会话还需要
./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri
```

## 环境变量（网关）

`src/main.rs` 只走 `BootstrapConfig::from_env`（`src/bootstrap.rs`）。下表是该入口实际
读取的变量；缺必填项直接启动失败，不回退本地 SQLite。

| 变量 | 必填 | 模块（读取 → 使用） | 说明 |
| --- | --- | --- | --- |
| `PERI_SWARM_API_KEY` | 是 | `src/bootstrap.rs` → `src/config.rs::DeploymentApiKey`；`src/http/auth.rs` 校验 | 部署 Bearer；空值拒绝；不打印 |
| `PERI_SWARM_BIND` | 否 | `src/bootstrap.rs` → `src/config.rs::parse_bind` | 默认 `127.0.0.1:8080`；非 loopback 需同时开 `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` |
| `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` | 否 | `src/config.rs::parse_bind_allow_non_loopback` → `parse_bind`（`BootstrapConfig` 与 `SwarmConfig` 共用）；启动日志 | 默认关；`1/true/yes/on` / `0/false/no/off`，其他值启动失败；开启后 WARN：Bearer 是唯一鉴权、TLS 由入口终止 |
| `PERI_SWARM_SESSION_STORE` | 是 | `src/bootstrap.rs` → `src/actor/runtime.rs::SessionStoreDeployment` | PostgreSQL DSN，可包含用户名、密码和 TLS 参数 |
| `PERI_SWARM_STATE_STORE` | 是 | `src/bootstrap.rs` → `src/store/postgres/mod.rs::SwarmStateStore` | Swarm 状态库 PostgreSQL DSN；必须与会话库指向**不同数据库**（同实例允许）；库须为空或已是本 build 的 `swarm_meta` schema，否则启动失败 |
| `PERI_SWARM_STORE_ENGINE` | 否 | `src/bootstrap.rs` → `src/actor/runtime.rs::SessionStoreDeployment` | 默认 `postgres`；只影响会话库 |
| `PERI_SWARM_RIVET_ENDPOINT` | 否 | `src/bootstrap.rs::rivet_config` → `rivetkit::ServeConfig.endpoint` | 默认 `http://127.0.0.1:6420`；只接受明文 `http`、显式端口（写出的默认端口如 `:80` 也算），无 userinfo/query/fragment，path 为空或 `/`；字面 IP 只接受环回/RFC1918/ULA/link-local，域名不解析；`https` 拒绝 |
| `PERI_SWARM_RIVET_TOKEN_ENV` | 视 endpoint | `src/bootstrap.rs::rivet_config` 校验变量名；`RivetConfig::resolve_token`（`run()` 一次取值）→ `ServeConfig.token` | **变量名**，不是 token 值；非 loopback endpoint 必填 |
| `PERI_SWARM_RIVET_NAMESPACE` | 否 | `src/bootstrap.rs::rivet_config` → `ServeConfig.namespace` | 默认 `default`；非空、无空白、≤128 |
| `PERI_SWARM_RIVET_POOL` | 否 | `src/bootstrap.rs::rivet_config` → `ServeConfig.pool_name` | 默认 `peri-swarm`；规则同上 |
| `PERI_SWARM_RIVET_ENGINE_SPAWN` | 否 | `src/bootstrap.rs::RivetEngineSpawn::rivetkit_mode` → `ServeConfig.engine_spawn`；`preflight_external_engine` | 默认 `always`；`always`/`auto` 只允许 loopback；非 loopback 必须 `never`，启动前 5s TCP 预检 |
| `PERI_SWARM_MACHINE_ID` | 是 | `src/bootstrap.rs` → `src/actor/service.rs::ActorServiceConfig`；`RemoteWorkspaceEnvironment` | 稳定机器 UUID |
| `PERI_SWARM_WORKSPACE_ROOT` | 是 | `src/bootstrap.rs`；`src/domain/workspace.rs::resolve_cwd`（`src/http/handlers.rs` 调用）；`src/actor/runtime.rs` | 绝对、规范化目录；macOS 用 `/private/tmp` |
| `PERI_SWARM_MODEL` | 否 | `src/bootstrap.rs` → `src/actor/service.rs::ActorServiceConfig.model` | 部署级默认模型 |
| `PERI_SWARM_SETTINGS_JSON` | 否 | `src/bootstrap.rs` 校验 JSON → `src/actor/service.rs::ActorServiceConfig.settings_json`；`src/actor/runtime.rs` | providers/profiles 与 `config.active_alias` |
| `PERI_SWARM_BUN_BASE_URL` | 否 | `src/bootstrap.rs` → `SandboxClientConfig`；`src/sandbox/client.rs` | Bun 桥正式地址 |
| `PERI_SWARM_BUN_URL` | 否 | `src/bootstrap.rs` → `SandboxClientConfig`；`src/sandbox/client.rs` | 别名；与上一项同时设置时上一项优先 |
| `PERI_SWARM_BUN_TOKEN` | 否 | `src/bootstrap.rs` → `SandboxClientConfig.token`；`src/sandbox/client.rs` | Bun 外层 Bearer 值 |
| `PERI_SWARM_BUN_BEARER_ENV` | 否 | `src/bootstrap.rs` 校验变量名并取值 → `SandboxClientConfig.token`；`src/sandbox/client.rs` 使用 | 变量名；与 `PERI_SWARM_BUN_TOKEN` 互斥 |
| `PERI_SWARM_SSE_HEARTBEAT_MS` | 否 | `src/config.rs::parse_sse_heartbeat`（bootstrap 调用）→ `src/http/handlers.rs` / `src/http/sse.rs` | `1..=600000`，默认 `15000`；非法值启动失败 |

- `PERI_SWARM_STORE_TOKEN_ENV`（Loom 时代的状态库 token 变量）已删除；旧部署残留的
  同名变量不会被读取，可原样保留以便回退。
- 未配置 `PERI_SWARM_BUN_BASE_URL`/`PERI_SWARM_BUN_URL` 时，
  `environment.type=openai_hosted` 在创建会话时显式失败；`type=none` 不受影响。
- `src/config.rs` 另有 `PERI_SWARM_STORE_LOCATOR`、`PERI_SWARM_STORE_CREDENTIAL_ENV`、
  `PERI_SWARM_MODEL_CREDENTIAL_ENV`，只用于 `SwarmConfig::from_env` 的独立调用/测试；
  网关进程入口不读它们。
- `peri-swarm/tests/it/state_store_test.rs` 使用同样的两个 DSN 变量（`PERI_SWARM_SESSION_STORE`
  / `PERI_SWARM_STATE_STORE`），`#[ignore]`，未设置时干净跳过。
- 状态库的 PostgreSQL 契约测试用 `PERI_TEST_POSTGRES_URL`（先例见
  `peri-resources` 的 PostgreSQL 夹具），同样 `#[ignore]`，每个用例建独立 schema。
- Rivet 与 bind 的六个变量不设置时，endpoint/token/namespace/pool/spawn 与旧常量逐字节
  一致；token 值只在 `run()` 解析一次，不进入 `Debug`、启动日志或错误消息。
- Bun 侧完整变量清单见
  [peri-sandbox README](../npm-packages/peri-sandbox/README.md#本地-swarm-服务devswarm)。

相邻进程变量（不是 `peri-swarm` 读取）：

| 变量 | 读取方 | 用途 |
| --- | --- | --- |
| `RIVETKIT_ENGINE_AUTO_DOWNLOAD` | `rivetkit` engine spawn | 首次按需拉起 6420 |
| `PERI_PATH` | `peri-sandbox` | 沙箱内 Peri 二进制 |
| `PERI_SWARM_IDLE_MS` | `peri-sandbox` | 空闲回收，`0` 关闭 |

## 启动顺序

```mermaid
flowchart LR
  BUILD["构建网关"] --> DB["PostgreSQL<br/>会话库 + 状态库"]
  DB --> GW["启动网关<br/>:8080"]
  BUILD --> BUN["Bun 沙箱<br/>:8081"]
  GW -.->|"hosted"| BUN
```

### 1. PostgreSQL（会话库 + 状态库）

两个独立数据库，可以在同一实例上。会话库由 `peri-resources` 的 Peri schema 管理，
状态库由网关自己建表并记录 `swarm_meta`；两者互不探测对方的 schema。

```bash
# 示例：本地 PostgreSQL 16（端口按你的实例改）
docker run -d --rm --name peri-swarm-pg \
  -e POSTGRES_USER=peri -e POSTGRES_PASSWORD=peri -e POSTGRES_DB=postgres \
  -p 127.0.0.1:55432:5432 postgres:16-alpine
docker exec peri-swarm-pg createdb -U peri peri_session
docker exec peri-swarm-pg createdb -U peri peri_swarm_state

export PERI_SWARM_SESSION_STORE='postgresql://peri:peri@127.0.0.1:55432/peri_session'
export PERI_SWARM_STATE_STORE='postgresql://peri:peri@127.0.0.1:55432/peri_swarm_state'
```

状态库的要求：

- **空库**：首次启动由网关建 6 张表（`swarm_meta`、`swarm_sessions`、`swarm_items`、
  `swarm_inputs`、`swarm_bindings`、`swarm_turns`）与 2 个索引，并写入
  `swarm_meta(version=1, engine='postgres', contract='peri.swarm.postgres/v1')`。
- **非空但没有 `swarm_meta`**：拒绝初始化（fail closed）。误把会话库填进
  `PERI_SWARM_STATE_STORE` 会得到「这是 Peri 会话库」的专门错误。
- **版本/引擎/契约不匹配**：启动失败，不自动升级或重写身份行。已知版本只重建派生对象（索引）；
  **缺少权威表或列一律 fail closed**（重建空表会悄悄丢掉 items、幂等收据、turn 与绑定），
  提示改用备份恢复或换空库重新导入。
- DSN 的 `search_path` 必须解析到已存在的 schema（默认 `public`）；表建在 `current_schema()`。

账号权限：会话库需要可建表/读写；状态库需要 `CREATE`（首次）以及表的读写权限。
两个库的 DSN 都不要写进日志或工单。

### 2. Rivet

本地 engine 为 Rivet 3.1.1，默认 endpoint `http://127.0.0.1:6420`
（`PERI_SWARM_RIVET_ENDPOINT` 可改）。首次运行设置 `RIVETKIT_ENGINE_AUTO_DOWNLOAD=1`，
网关会按需拉起（`~/.rivetkit/engine/v3`）；预装 engine 时无需额外步骤。外置 engine 时
设 `PERI_SWARM_RIVET_ENGINE_SPAWN=never`，并给出 endpoint、token 变量与 namespace/pool。
engine 是 Rivet 官方组件，不归 Peri 代码所有。
**6420 是共享 engine 进程，不归网关所有**：网关停止后它可能继续运行。确认占用用
`lsof -nP -iTCP:6420 -sTCP:LISTEN`，再用 `ps -p <pid> -o pid,command` 核对；不要为
释放端口杀无关进程。

### 3. 网关

```bash
export PERI_SWARM_BIND=127.0.0.1:8080
export PERI_SWARM_API_KEY='<gateway-bearer-key>'
export PERI_SWARM_SESSION_STORE="${DATABASE_URL:?请设置 PostgreSQL 会话库 DSN}"
export PERI_SWARM_STATE_STORE="${SWARM_STATE_DATABASE_URL:?请设置 PostgreSQL 状态库 DSN}"
export PERI_SWARM_STORE_ENGINE=postgres
export PERI_SWARM_MACHINE_ID='<持久化 UUID>'

mkdir -p /private/tmp/peri-swarm-workspace
export PERI_SWARM_WORKSPACE_ROOT=/private/tmp/peri-swarm-workspace

# 真实模型才需要
# export PERI_SWARM_MODEL='<model>'
# export PERI_SWARM_SETTINGS_JSON='<providers/profiles JSON>'

export RIVETKIT_ENGINE_AUTO_DOWNLOAD=1
./target/debug/peri-swarm
```

`PERI_SWARM_MACHINE_ID` 只生成一次并持久化；不要每次重启变化。健康检查：

```bash
curl -fsS http://127.0.0.1:8080/healthz
# {"status":"ok"}
```

### 4. Bun 沙箱（仅 hosted）

`openai_hosted` 会话必需；`type=none` 不需要。

```bash
cd /Users/mino/code/peri/npm-packages/peri-sandbox
bun install

PERI_PATH=/Users/mino/code/peri/target/debug/peri \
PERI_SWARM_BUN_TOKEN='<bun-token>' \
bun run dev:swarm
```

网关启动时追加：

```bash
export PERI_SWARM_BUN_URL=http://127.0.0.1:8081
export PERI_SWARM_BUN_TOKEN='<与 Bun 相同的 token>'
```

默认监听 `127.0.0.1:8081`，沙箱根默认 `~/.peri-swarm/sandboxes`，空闲回收默认 30 分钟
（`PERI_SWARM_IDLE_MS=0` 关闭）。需要固定 settings 时用
`bun run src/cli.ts init --settings ...`，详见
[peri-sandbox README](../npm-packages/peri-sandbox/README.md#本地-swarm-服务devswarm)。
网关可以先于 Bun 启动；Bun 就绪前创建 hosted 会话会得到 `502 upstream_unavailable`。

## 升级与回退（状态库）

### 从 Loom 割接

本版本**不做自动迁移**：网关不连接旧库，也没有 Loom/Hrana 运行时依赖。割接有两条路，
按是否需要保留旧 Swarm 状态选择。

**A. 不要旧状态（空库启动）**

1. 等客户端空闲后停掉旧网关；旧状态库不再被写入。
2. 准备**专用** PostgreSQL 数据库（不要与会话库同库）：
   `createdb peri_swarm_state`，或 `psql "$PG_ADMIN_DSN" -c 'CREATE DATABASE peri_swarm_state'`。
3. 用新镜像 + `PERI_SWARM_STATE_STORE=<新库 DSN>` 启动。启动日志出现
   `opening the independent swarm state store` / `postgres://host:port/peri_swarm_state`；
   库内出现 `swarm_meta(version=1, engine=postgres, contract=peri.swarm.postgres/v1)`。
4. 旧 Loom 库原样保留（只读，供事后核对）。新 build 从不连接它。

影响：旧 Loom 会话对新部署不可见（`sessions.list` 从空开始，旧 `logicalId` 的 `GET`
返回 404）。Peri 会话历史在会话库里不受影响，丢的是 Swarm 侧的投影行（会话 / items /
turns / bindings 游标）。

**B. 保留旧状态（离线导入，一次性）**

[`scripts/import_loom_state.py`](scripts/import_loom_state.py) 是 Python 3 标准库脚本：
读取旧库的**只读 SQLite 快照**，生成写 PostgreSQL 的 SQL（由 `psql` 执行），再对账。它不运行网关，
也不给网关增加任何 Loom/Turso 依赖。

1. **停写并取快照**。停掉旧网关后，用存储方提供的只读备份/导出得到 SQLite 格式文件；
   若拿到的是 `.dump` 文本，先还原成普通 SQLite 文件：
   `sqlite3 snapshot.db < dump.sql`。热拷贝要连 `-wal`/`-shm` 一起复制（或先 checkpoint），
   否则可能丢最后若干提交；工具在同一 SQLite 事务里读取，若读取期间主文件或 WAL 变化
   会拒绝导入。manifest 分别记录主文件与非空 WAL 的 sha256。`--immutable` 只用于已
   checkpoint（没有非空 `-wal`）的副本——带未 checkpoint 的 WAL 时会直接拒绝，避免
   `immutable=1` 静默忽略最后的提交。
2. **检查快照**（只读打开，不写源文件）：
   ```bash
   python3 peri-swarm/scripts/import_loom_state.py inspect --snapshot snapshot.db
   ```
   输出五表行数与不变式；无法被新 build 读取的形状（未知枚举、非法 JSON、非整数
   `seq`/时间、`truncated` 非 0/1、重复主键、非 UUID `inputId`、`swarm_meta.version != 1`）
   直接拒绝并给出原因。
3. **生成 SQL**：
   ```bash
   python3 peri-swarm/scripts/import_loom_state.py export \
     --snapshot snapshot.db --out-dir out/
   ```
   产出 `01-schema.sql`（新 schema + `swarm_meta` 身份行，镜像 `schema.rs` 的 DDL）、
   `02-data.sql`（按主键顺序的 INSERT，单事务）、`03-verify.sql`（对账查询）、
   `expected.json`（期望值 + manifest）。目标 schema 非空时，生成的 SQL 在执行阶段
   直接 `RAISE EXCEPTION`（不会覆盖已有数据）。
4. **导入空库并核对**：
   ```bash
   psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -f out/01-schema.sql
   psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -f out/02-data.sql
   psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -qAt -f out/03-verify.sql > out/observed.txt
   python3 peri-swarm/scripts/import_loom_state.py check \
     --expected out/expected.json --observed out/observed.txt
   ```
   对账项：五表行数、`swarm_meta` 身份、`last_item_seq = MAX(seq)`、`seq` 从 1 起且无洞、
   重复 `(logical_id, seq)` / 重复 `inputId` / 重复 `root_session_id`、悬挂行、枚举与
   JSON 可解码、JSON/TEXT 字节总量。
5. **启动新 build**。首次打开会走「已初始化」分支：只校验身份与列形状，不重写数据；
   旧 `rootSessionId`、items、`lastItemSeq`、`inputId` 收据和 SSE 游标都按原值恢复。

列映射由旧 DDL（`9332fae19:peri-swarm/src/store/turso.rs`）推导，工具内置同一份 DDL
并有单测比对 `schema.rs`：`truncated` `INTEGER 0/1 → BOOLEAN`，`seq`/`last_item_seq`/
turn 时间 `INTEGER → BIGINT`，JSON 列保持 TEXT 原样字节；旧 build 用 `ALTER TABLE`
追加的 `metadata_json` / `prompt_cache_options_json` 缺失时按 `NULL` 导入。

已知修复与限制：

- 旧删除不是单事务，快照可能有悬挂行。工具默认修复并写进 manifest：陈旧
  `last_item_seq` 改成 `MAX(seq)`、丢弃无会话的 items/inputs/turns/bindings；
  `--strict` 改为直接失败。
- 已用脚本自建的 legacy SQLite fixture 做过端到端演练（导入 → 启动网关 → 校验
  `rootSessionId`、旧 `inputId` 幂等、SSE 游标续读 → 重启），并单独验证过未 checkpoint
  的 WAL（复制主文件 + 非空 `-wal`）：WAL 内提交可导入，`--immutable` 被拒绝。命令与输出见
  [验收记录](../docs/verification/peri-swarm-postgres-state-2026-11-24.md) 的「离线导入演练」。
  **未对真实 Loom 生产快照验证**：现场快照的 `-wal` 状态、存储方导出格式与数据规模
  仍需在割接窗口内按上面第 1–4 步实测。
- 导入只恢复 Swarm 侧投影。会话能否继续对话取决于 Peri 会话库里对应的
  `root_session_id` 历史；历史不在时导入的会话是只读的（`GET`/items/SSE 可用，新输入
  需要能重新 attach 的宿主）。

### 校验 SQL

导入工具已内置这些检查（`03-verify.sql`）。需要人工复核时在状态库执行同样的语义：

```sql
SELECT version, engine, contract FROM swarm_meta;                  -- 1 | postgres | peri.swarm.postgres/v1
SELECT COUNT(*) FROM swarm_sessions;
SELECT COUNT(*) FROM swarm_items;
SELECT COUNT(*) FROM swarm_inputs;
SELECT COUNT(*) FROM swarm_turns;
SELECT COUNT(*) FROM swarm_bindings;
-- (logical_id, seq) 是主键，不存在重复 seq
SELECT COUNT(*) - COUNT(DISTINCT (logical_id, seq)) FROM swarm_items;
-- 缓存投影 = MAX(seq)：结果应为空
SELECT s.logical_id, s.last_item_seq, COALESCE(MAX(i.seq), 0) AS max_seq
  FROM swarm_sessions s LEFT JOIN swarm_items i USING (logical_id)
 GROUP BY s.logical_id, s.last_item_seq
HAVING s.last_item_seq <> COALESCE(MAX(i.seq), 0);
-- item 内最大 seq 用于分页游标；seq 从 1 起、按会话连续
SELECT logical_id, MIN(seq), MAX(seq), COUNT(*) FROM swarm_items GROUP BY logical_id ORDER BY 1;
```

需要手工导入（例如快照形状超出工具支持）时按列显式写入：`truncated` 是 `BOOLEAN`、
`seq`/`last_item_seq`/turn 时间是 `BIGINT`、JSON 列是 `TEXT`（原样字节）；导入前后用上面
的计数与 `MAX(seq)` 对账。手工导入仍需先满足 `swarm_meta` 身份行与列形状，否则新 build
会 fail closed。

### 回退

- **回退到 Loom 版本**：停新网关 → 换回上一版镜像 → 恢复旧的
  `PERI_SWARM_STATE_STORE`（Loom locator）与 `PERI_SWARM_STORE_TOKEN_ENV`/token。
  新 build 从未写过 Loom，旧库内容与割接前一致（离线导入只读快照）；割接窗口内产生的
  新会话只存在于 PostgreSQL，不会回灌。
- **回退 PostgreSQL 内的一次升级**（未来）：升级前 `pg_dump -Fc peri_swarm_state >
  peri_swarm_state.dump`；回退 = 旧镜像 + `pg_restore`。`swarm_meta.version` 比当前
  build 高时进程会 fail closed，不会静默降级。
- **不确定时**：停进程即可，两个库都只读不写；重新指向正确的 DSN 再启动。实现层面
  不会把有数据的库当空库初始化（非空且无 `swarm_meta` 直接拒绝）。

## 官方 SDK 差异

普通 SDK 初始化按官方文档；把 `baseURL` 指到 `http://127.0.0.1:8080/v1`，`apiKey` 用
`PERI_SWARM_API_KEY`。

- **官方**：Node 包 `openai` 的 `beta.agents.sessions` 客户端。下表的契约对照以
  `7.60.0` 为基线；本地追问和续读还记录了 `7.62.0`，不代表所有官方资源均兼容。
- **我们**：Peri Swarm 网关的实际行为。
- 下表只列两者不一致的地方；一致的官方字段和流程不再重复。

定级判据：`P0` 是官方 SDK 默认调用失败或返回错误结果（当前无此类差异）；`P1` 是官方必需字段或文档能力缺失，主流程可走但兼容性不完整；`P2` 是可选能力、扩展或边缘语义差异，不影响官方默认调用。

| 优先级 | 项目 | 官方 | 我们 | 不能同步的难点 |
| --- | --- | --- | --- | --- |
| P1 | 批量事件 | `events.create({events:[...]})` 接受多条事件，或取消、工具结果、审批响应等消息以外的输入事件 | 多条事件或消息以外的输入事件（取消、工具结果等）→ `400 invalid_input`；单条 `agent.session.input.mcp_approval_request_result` 已受理（`202`，结算一次待办） | 受理侧按单条 message 建 turn 与幂等键；批量需要多事件原子受理、跨事件排队与部分失败语义，现有 service/actor 边界没有这个事务单位 |
| P1 | 会话状态 | `requires_action`（等待一个或多个必需动作；四类：`mcp_approval_request`、`computer_use_approval_request`、`function_call`、`environment_connection`） | 只有声明了 `require_approval` 的 MCP 绑定产生 `requires_action`：`required_actions` 每条为官方嵌套形状 `{type:"mcp_approval_request",request_id,turn_id,request:{type:"mcp_tool_call",server_label,name,arguments,message}}`（`name`/`arguments` 缺失时为 `null`，host 给空工具名也投影 `null`；来源无法归属时 `server_label` 为 `""`，`message` 说明来源未知）；答复接受官方 `{request_id,response:{type:"mcp_tool_call",decision}}`（保留顶层 `decision`/`requestId` 扩展别名），回到原挂起 `session/prompt`、不新建 turn；待办结算/过期/turn 结束/actor 丢失后快照清空，硬崩溃遗留的 pending 行在恢复时结算为 `actor_lost`（turn 已结束则 `turn_ended`）；ACP `subscribe` 与生产 actor `GET /events` 共用同一 backlog 装配（先结算过期、再回放、最后追加当前快照，同一 `event_id` 幂等）；结算行落库失败返回 `503 required_action_settlement_failed` 且待办保持可答复；子 agent 继承父会话声明的审批范围，来源无法归属且无审批通道时 `PolicyDenied`，范围存在但子链无 broker 时同样 `PolicyDenied`，不静默执行。`AskUserQuestion` 仍走 ACP `elicitation/create`，不映射官方动作 | `require_approval` 只有网关扩展接受，纯官方 SDK 客户端无法声明审批，因此该状态不会被纯官方入参触发；其余三类动作（`computer_use`/`function_call`/`environment_connection`）本地没有事实；`AskUserQuestion` 没有官方类型 |
| P1 | agent 子字段 | `agent` 是嵌套对象，必需字段含 `id`、`name`、`tools`、`multi_agent`、`reasoning`、`service_tier`、`text`（`compaction` / `experimental` 可选）；`model` 是 string | REST `agent` 只有 `model`（可空）/ `instructions`，没有 `id`；SSE `agent.session.*` 的 agent 有固定 `id: "root"` / `model`（可空）/ `instructions`，缺 `name` / `tools` / `multi_agent` / `reasoning` / `service_tier` / `text` | REST 与 SSE 都不能保证 `model` 是非空 string；REST 还缺 `id`；`name` / `tools` / `text` 可用默认值或投影补齐（schema 可新增）；subagent 调度、多 agent 配置、推理与 service tier 仍缺运行时事实 |
| P1 | `sessions.update` 未支持字段 | `agent.reasoning`、`agent.service_tier`、`review`，以及 `agent.tools` 中的函数/内置工具 | 这些字段 → `400 invalid_request`；函数/内置工具 → `422 unsupported_tool`；`agent.model:null` → `400` | 本地没有 reasoning、service tier 与 review 的执行语义；`agent.tools` 只有 MCP 注入通道，没有函数或内置工具运行时 |
| P1 | items 形状 | `AgentSessionItem` 联合：message、reasoning、function_call、mcp_call、web_search_call、command_execution、subagent 调用等 | 每项是 Swarm `ItemView`（`id` / `itemId` / `seq` / `kind` / `payload`）；`items` 保留逐帧原始行（含 delta） | 官方联合依赖工具调用、推理、审批等高层事实；本地持久化的是 message 与原始 ACP 帧，缺少可映射的数据源 |
| P1 | 官方事件覆盖 | `AgentSessionEvent` 共 33 种；`turn.item.done` 是官方 SDK 读取最终文本的来源 | 发 15 种：`turn.created` / `turn.in_progress` / `turn.item.added` / `turn.item.done` / `turn.completed` / `turn.failed` / `content_part.added` / `content_part.done` / `output_text.done` / `session.created`（仅 create 流）/ `session.in_progress` / `session.idle` / `session.failed` / `error` / `requires_action`（仅当前 MCP 待办）；取消在 live 折叠为 `turn.completed(status="cancelled")`，回放固定为 `completed`；`turn.subagent_id` 恒 `null` | 仍缺 18 种：8 种环境生命周期、工具输出增量、subagent 3 种、`turn.cancelled`、文本 delta、reasoning 4 种；文本 delta 需把 `item.added` 前移到首 chunk 并保留回放同一性；环境与工具增量没有运行时事实 |
| P1 | 环境投影 | `Environment.EnvironmentResourceOpenAIHosted` 必需字段含 `capability_directories`、`files`、`network`、`packages`、`plugins`、`skills`（`container_size`、`desktop` 可选）；`AgentSession.environment` 必填 | hosted 只有 `{type,id}`（`id` 是 logicalEnvId）。顶层 `sandbox` 只有 `logicalEnvId` / `sandboxId` / `runId` / `state` / `workspacePath`：`state` 恒 `null`，`runId` 来自绑定记录的 `last_run_id`（可能是上次预留值，与当前 state 不一致）；SSE `agent.session.*` 的 `OfficialSession` 携带同一 `{type,id}` 投影（未知环境为 `{"type":"none"}`，不造 id） | 网关只持久化绑定，不跟踪 Bun SRT 实时状态；hosted 的 `capability_directories` / `files` / `network` / `packages` / `plugins` / `skills` 没有同步通道 |
| P1 | turn 用量与错误 | `Turn.usage` 是 `TokenUsage` 或 `null`；`error` 持久化，`turns.retrieve` 与事件回放一致 | `usage` 恒 `null`；失败 turn 实时发 `error`；回放只在最后一 turn 且会话 `failed` 时发 `error`（`message` 为空），其余已关闭 turn 回放为 `completed`；`turns.retrieve` 读持久 turn 行 | 网关不做 token 记账；回放层不读 turn 行，只能重建 `code` / `type`；失败文本只在实时帧与持久 turn 行 |
| P1 | 未映射资源 | `turns/{id}/items`、`subagents/*`、`traces`、`artifacts` 都可用 | 这些路由返回 `404 unsupported_contract`（`details.resource` 指资源名） | 持久化表可以新增；真正的缺口是 traces/artifacts/subagent 没有产生源，turns items 也没有官方结构映射 |
| P2 | MCP 工具形状 | `agent.tools[].transport`：`{type:"http", server_url, headers?, authorization?}` 或 `{type:"stdio", command, cwd, ...}`；`allowed_tools` 在调用侧过滤工具 | 接受官方 `transport` 形状：`transport.type="http"` 映射为内部 `server_url`/`headers`，`transport.authorization` 在缺少同名 `Authorization` 头时补成 `Bearer`；平铺 `server_url`/`headers` 保留为 deprecated alias（两种形状的 URL 不一致 → `400 invalid_input`，`details.field` 指向 `transport`；一致则接受）。`transport.type="stdio"` 与未知类型 → `422 unsupported_tool`（`details.field` 指向 `transport.type`，`details.value` 是收到的类型）。`require_approval` 经宿主审批通道强制执行（产生 `requires_action`）；`allowed_tools` 只记录在 ACP `_meta`，未在调用侧强制过滤 | 本部署没有 stdio 进程运行时；`allowed_tools` 没有消费方，不能当作安全边界 |
| P2 | `sessions.list` 的 `agent_id` | 可按任意根 agent id 过滤 | 只认 `root`；其他值返回空页 | 网关只有 root agent，子 agent 维度不存在，没有可过滤的 agent 标识 |
| P2 | turns 字段 | `agent_id` 可为子 agent、`subagent_id` 可非空、`usage` 有值；`status` 还含 `queued` / `waiting` | `agent_id` 恒 `root`、`subagent_id` 恒 `null`、`usage` 见「turn 用量与错误」行；不产生 `queued` / `waiting` | 没有子 agent 调度与 `queued` / `waiting` 排队状态；turn 只由单会话串行执行产生 |
| P2 | 删除会话 | 物理清理可异步 | 按会话同步删除，共享绑定在最后一个引用释放时删除；无 live runtime 但持久 `running` → `502 session_resume_required` | 先用新的用户输入触发交互恢复，再执行删除或更新 |
| P2 | prompt cache | 服务端按 `prompt_cache_options` 的 TTL 影响后续 turn 的缓存 | 只持久化并回显；本地模型链是否使用该 TTL 未验证 | TTL 只是官方兼容字段，本地模型链尚未消费，无法影响后续 turn 的缓存 |
| P2 | SSE 续读 | query 类型没有 `after`；从订阅时刻起只送新事件 | 额外支持 `after` / `afterSeq` / `Last-Event-ID` 回放；追平末尾后连接保持，只发心跳 | 官方 SDK 的 query 类型没有 `after`；回放只能作为网关扩展存在，SDK 不会为它提供原生查询字段，去掉回放就失去续读能力 |

调用方式上还有四点不是契约差异，但容易踩：

- `sessions.update` 官方 SDK 用 `POST`；网关同样只接受 `POST`（`PATCH` → `405`）。
- 空 body、整个 body 为 JSON `null` 或 `{}` 都是 no-op；官方 SDK 省略 body 时会发 `{}`。
- 网关严格拒绝未知 query 字段（`400 invalid_query`）与未知 body 字段（`400 unknown_field`）；
  列表分页用 `after` + `limit`，没有 `cursor` 参数。
- `prompt_cache_options: {}` 等于官方默认 `{"ttl":"30m"}`；只有 `null` 清空，省略表示保持。

### Node SDK 同会话追问与续读

下面使用已创建的会话 `id`（沿用 create 返回的 id），不为追问新建会话。用收到的最后
事件游标续读；没有游标只订阅未来事件，因此要观察本轮必须先开流再 POST。断线后可重试
同一条输入，但应保留原幂等键；新输入使用新键。HTTP 202 是受理回执，不代表已经运行或得到终态。

```js
// 实时：先开流再 POST；tail 是上次流收到的最后事件游标，没有游标时省略（Tail 只收实时帧）。
// 必须三个实参；两实参时 { after } 会被当作 RequestOptions。tail 为 0 时保留 after，回放全部。
const streamOptions = tail == null ? {} : { after: tail };
const live = await client.beta.agents.sessions.events.stream(id, streamOptions, {});

// 使用官方 SDK 在同一会话追问。重试同一条输入时复用 inputId。
const inputId = crypto.randomUUID();
const response = await client.beta.agents.sessions.events.create(id, {
  'Idempotency-Key': inputId,
  events: [{
    type: 'agent.session.input.message',
    input: [{ role: 'user', content: [{ type: 'input_text', text: 'follow up' }] }],
  }],
}).asResponse();
const ack = await response.json(); // HTTP 202，含 inputId 和 lastItemSeq

// items：官方 CursorPage 自动翻页可用；直接读 body 时 TS 中 body 是 protected
const page = await client.beta.agents.sessions.items.list(id, { order: 'asc', after, limit: 50 });
const body = /** @type {any} */ (page).body;
```

### sessions / turns

| 方法 | 路径 | 语义 |
| --- | --- | --- |
| `GET` | `/v1/agents/sessions` | `sessions.list`：`after`（上一页末项 `logicalId`，排他）、`order`（默认 `desc`）、`limit`（1..=500，默认 20）、`agent_id`（仅 `root`）、`metadata[key]=value`（≤16 对，精确匹配）；响应 `{object:"list",data,has_more,hasMore,nextCursor}`，`nextCursor` 仅在 `has_more` 时给出；`cursor` → `400 invalid_query` |
| `POST` | `/v1/agents/sessions/{logicalId}` | `sessions.update`（官方 SDK 用 POST；`PATCH` → 405）：`agent.model`、`agent.tools`（仅 MCP）、`metadata`、`prompt_cache_options`；`metadata` 省略保持、`null`/`{}` 清空；`prompt_cache_options` 省略保持、`null` 清空、`{}` 等于 `{"ttl":"30m"}`；值整体替换；无 body 或 JSON `null` 是 no-op；`agent.model:null` 与未映射字段 → `400` |
| `DELETE` | `/v1/agents/sessions/{logicalId}` | `sessions.delete` → `{id,object:"agent.session.deleted",deleted:true}`；级联 items/inputs/turns/bindings |
| `GET` | `/v1/agents/sessions/{logicalId}/turns` | `turns.list`：`order`（默认 `desc`）、`limit`（1..=500，默认 20）、`after`（必须是本会话的 turn id，排他；无效 → `400 invalid_query`）；响应同上 |
| `GET` | `/v1/agents/sessions/{logicalId}/turns/{turnId}` | `turns.retrieve`：官方 Turn JSON；不属于该会话 → 404 |

- 准入：`update`/`delete` 在 live 运行中或有未关闭 turn 时 `409 turn_in_progress`；无 live
  runtime 但持久行 `running` 或仍有开放 turn 行时 `502 session_resume_required`。用新用户输入执行交互恢复后，可再次进行管理操作。
- create attach：同一 `logicalId` 已存在时，省略 metadata/prompt cache 保持原值；显式不同
  值 → `409 identity_conflict`。
- turns 在 `begin_turn` 写入 `in_progress`，终态在 `finish_turn` 更新；pending 输入不落 turn
  行；取消（`stop_reason="cancelled"`）持久为 `cancelled`，实时官方帧是 `turn.completed`
  + `status="cancelled"`（不发送 `agent.session.turn.cancelled`）。
- 未映射的官方资源一律显式 `404` + `unsupported_contract`（`details.resource`）：
  `traces`、`artifacts`（含 `content`）、`subagents`、`subagents/{id}/items`、
  `subagents/{id}/turns*`、`turns/{id}/items`。Bearer 校验先于 404。

### SSE 语义

| 场景 | 帧 | 终止 |
| --- | --- | --- |
| 实时（先开流，再 POST） | 自定义 `item`（用户输入先于 `turn.started`）+ 自定义 `turn`（`started` → `completed` \| `failed`）+ 官方 `agent.session.turn.created` → `turn.item.added` → `turn.completed` \| `turn.failed` → `agent.session.idle` \| `failed`；可能有 `gap` | 原生客户端看 `turn.state`；官方 SDK 看 `agent.session.idle` / `agent.session.failed` |
| 回放（显式 `after`/`afterSeq`/`Last-Event-ID`） | 自定义 `item` + 同一套官方帧，没有自定义 `turn` 帧；可能有 `gap` | 官方 SDK 在 turn 终态后的 `agent.session.idle` 结束；原生客户端看 `payload.complete === true` |
| 无游标订阅 | 不读历史，只收订阅之后的实时帧 | 同上；要补历史必须显式给游标（`afterSeq=0` 回放全部） |
| `after` 等于/超过末尾 | 只有心跳 | 不会自行关闭；用客户端超时、`AbortController` 或 `items.list` |
| 心跳 | `{ "lastItemSeq": N }` | 无 `seq`、无 `state`；不是本流已投递的位置 |

- 实时帧序不是「先 `turn.started`」：用户输入的自定义 `item` 先到，官方 `turn.created`
  随后；不要收到 `turn.started`/`turn.created` 就清空已到达的帧。
- `POST /events` 是接收语义：202 只表示 receipt、用户 item 与 turn 预留已持久化，turn 在
  后台执行；失败在流上以 `turn.failed`/`session.failed` 出现。要观察本轮先开流再 POST；
  `ack.lastItemSeq` 已包含用户输入，不能用它开流补看本轮。
- `POST /sessions?stream=true` 先建立订阅并返回 SSE 响应，再执行首轮；响应不等待首轮，
  但缓冲区仍有界，读取过慢同样按 `gap` 处理。
- 无游标订阅是 `Tail`：只收实时帧，不读历史；`after=0` 才回放全部。`Last-Event-ID` 与
  `after`/`afterSeq` 等价，同时给出必须一致，否则 `400 conflicting_cursor`。
- 直接 HTTP 必须按流读取 `response.body`，不要 `res.json()`/`res.text()`；只有自定义
  `item` 帧的 `id:` 是 seq。自定义 `turn`、官方 `agent.session.*` 与 `gap` 帧不带 `id:`，
  不推进 `Last-Event-ID`。
- 订阅落后超过通道容量时收到一帧 `gap` 后流即结束（可能丢掉 turn 终态）；按最后渲染的
  item `seq` 重连补齐。
- 心跳间隔由 `PERI_SWARM_SSE_HEARTBEAT_MS` 决定（默认 15s，最短 1ms）；空流是否收到
  心跳取决于网关进程配置。

### items 帧

| 帧 | payload | 聚合与终止 |
| --- | --- | --- |
| 用户输入 | `{ role: 'user', text, inputId? }` | 单帧，无 `delta`/`complete`；不参与 assistant 聚合 |
| assistant delta | `{ role: 'assistant', messageId, text, delta: true }` | 按 `messageId` 聚合 |
| assistant complete | `{ role: 'assistant', messageId, text, complete: true }` | 最终文本以该帧为准 |
| `tool_call` / `tool_update` | `{ toolCallId, title, status, input?/body? }` | 按 `toolCallId` 聚合；可能先到无 input 的帧 |
| `tool_result` | `{ toolCallId, status, title?, output? }` | 终态；文件内容不在 `output` |

- `items.list` 返回 item 级游标页：`data` 每个 item 一行（该 item 的最后一行），`items`
  是该页全部 item 的逐帧原始行；`object` 为 `"list"`，`has_more` 与 `hasMore` 同值。
- item 的 `id` 与 `itemId` 同值；同一 assistant 消息的 delta/complete 共享 id，不能按 id
  去重。
- `limit` 计 item 数（1..=500，默认 200），不按行数截断：单个 item 超过行预算时整条返回，
  一页行数可能大于 `limit`。
- `nextCursor` 是页尾 item 的最大 seq，下一页传给 `after`/`afterSeq`。`after` 也接受
  `itim_...` item id（官方 `CursorPage` 的翻页方式），请求时解析为该 item 的当前最大 seq，
  之后追加的帧不会再由该游标返回；要在增长中续读用 `afterSeq=<seq>`。传 `cursor` 会得到
  `400 invalid_query`；`order` 默认 `desc`，升序需显式 `order=asc`；`order=desc` 时
  `after` 表示「更旧于该 seq」。
- `POST /events` 的用户输入 item 是 `kind=message`，没有 `kind=user`；同一
  `Idempotency-Key`/`inputId` 重试返回 `existing`，不重复追加。

### hosted 投影

| 字段/行为 | 网关投影 | 判断方式 |
| --- | --- | --- |
| `environment.type=none` | `environment` 只有 `{"type":"none"}`；顶层 `sandbox` 为 `null` | 用 session id / logicalId 识别会话，不要从 `environment` 取 id |
| `environment.type=openai_hosted` | `environment.id` 等于 `sandbox.logicalEnvId` | 用它复用逻辑环境 |
| `sandbox.state` | 恒 `null` | 查 Bun `GET /v1/sandboxes/{logicalEnvId}` |
| `sandbox.runId` | 预留时 Bun 返回值；后续运行不回填 | 查 Bun 的 `runId`/`state` |
| `cwd` | 网关会话工作目录 | 工具在 `sandbox.workspacePath` 下执行 |
| `tool_result.output` | 只有退出摘要，没有文件内容 | 读 workspace 或 Bun 状态接口 |

共享环境的绑定在创建时与会话行一起提交。删除会话时由其余引用者保留绑定，最后一个会话删除后才移除绑定。

`swarm_bindings` 是绑定权威，Bun 侧 `bindings.json` 只是派生缓存；恢复时先读状态库
绑定，缺失即失败，`sandboxId`/`workspacePath` 变化按 `sandbox_binding_changed` 拒绝。

### 创建中断与恢复

```mermaid
stateDiagram-v2
  [*] --> creating: 写完整会话行
  creating --> idle: session/new 成功
  creating --> creating: 重试同一 logicalId
  idle --> running: 接受 turn
  running --> idle: turn completed/failed
```

- `state=creating` 不是终态；用同一 `logicalId` 重试 create 完成核对。
- 恢复先 `session/load`；只有 Peri 会话确实不存在时才用**同一个** root id 建会话；
  `session/new` 返回 `-32010` 时再 attach。绝不重新生成 root id。
- 恢复成功返回 `created=false`；初始输入至多执行一次。两条恢复路径都失败返回
  `502 upstream_unavailable`（`details.operation=create_recovery_failed`），行保留
  `creating`。
- `GET`/`items` 只读投影，不驱动恢复；`POST /events` 的首次输入只走 `ensure_live`，
  不会补做创建恢复。

## 已知限制

- 模型侧没有内置真实供应商；本地验收使用确定性 stub。真实模型需要
  `PERI_SWARM_SETTINGS_JSON` 中的 provider/profile 配置。
- 宿主在 turn 中退出后，历史查询返回已存状态。下一条新用户输入触发原会话恢复；同步中断的旧 turn 记录为失败，Peri 按持久 MQ 恢复待处理工作。
- 真实两库 PostgreSQL 已验证 Swarm 共享绑定转交和最后引用删除；这不等于清除 Bun 的
  工作目录或预留。Loom 存量自动迁移（本版本不支持）、双副本并发首启与删除事务的故障
  边界尚待验收。
- `turns.usage`/`subagent_id` 恒 `null`；`prompt_cache_options` 只持久化/回显，本地模型链
  是否使用未验证。官方 SDK 的删除已通过本地 Rivet engine，后台 `ctx.destroy` 的完成时机仍待独立观测。
- 沙箱空闲回收关闭运行实例但保留 MCP URL、sandboxId 和工作区；下一次工具调用
  唤醒新 run。已跟踪的后台任务在同一 Bun 进程中保活，终态后恢复空闲计时。显式
  停止的环境须重新预留；不承诺 Bun 重启后恢复在途任务。
- SDK 探针中的取消在完成边界收到 `completed/end_turn`；真实模型生成中断、
  跨故障域恢复与无 fencing 条件下的双宿主接管没有验收。
- `sandbox.state` 在会话投影中固定为 `null`；`sandbox.runId` 只反映预留时 Bun 返回的值，
  不随后续运行更新；实时状态以 Bun 为准。
- `peri-swarm` 默认只绑定 loopback，非 loopback 需显式开关；只有单 Bearer，自身不做
  TLS，入口依赖外部 Nginx/Gateway。外部 engine 配置面已实现，但未在活体 engine 上验收；
  Bun bridge 仍只接受 loopback；多副本部署、跨机调度、多租户、1000 Agent
  规模、Linux SRT 与真实第三方 MCP 未验收。

## 生产部署拓扑（目标）

下图用于说明目标部署的组件边界，不是部署步骤。Peri Swarm Server 即上文的网关；
仓库目前没有对应的生产编排或验收记录。跨机调度、多租户和跨实例接管缺少实现或验收，
不能套用本地配置推断上线能力；差距见下表。

```mermaid
flowchart TB
  subgraph EDGE["边缘"]
    SDK["远程客户端<br/>OpenAI SDK"]
    OPS["运维 / 控制面"]
  end

  subgraph ING["入口层（部署时提供）"]
    IG["Nginx / API Gateway<br/>TLS 终止 · 统一鉴权 · 配额 · 审计<br/>SSE 不缓冲"]
  end

  subgraph APP["应用层：Peri Swarm Server 副本组"]
    subgraph SP1["Server 进程 #1"]
      subgraph SL1["无状态接口"]
        P1["axum /v1 · Bearer"]
      end
      subgraph AL1["有状态 actor"]
        A1["SwarmActor<br/>agent/&lt;logicalId&gt;"]
        H1["ACP Host<br/>Peri 库"]
        A1 --> H1
      end
    end
    subgraph SPN["Server 进程 #N"]
      subgraph SLN["无状态接口"]
        PN["axum /v1 · Bearer"]
      end
      subgraph ALN["有状态 actor"]
        AN["SwarmActor<br/>agent/&lt;logicalId&gt;"]
        HN["ACP Host<br/>Peri 库"]
        AN --> HN
      end
    end
  end

  subgraph RIVET["Rivet engine 集群（3+ 实例）"]
    ELB["入口 LB<br/>6420 · TLS · 1h idle"]
    E1["engine 实例 A"]
    E2["engine 实例 B"]
    ELB --> E1
    ELB --> E2
    E1 <--> E2
  end

  subgraph SANDBOX["沙箱主机池（需放开 loopback 校验）"]
    BUN["Bun 桥（peri-sandbox）"]
    subgraph SRT["SRT 沙箱"]
      PX["peri mcp-start<br/>Peri 可执行进程"]
    end
    BUN --> PX
  end

  S1[("PostgreSQL 会话库")]
  subgraph STATE["PostgreSQL 状态库（可同实例）"]
    S2[("swarm_meta + swarm_* 表")]
  end

  MODEL["模型供应商<br/>HTTPS"]

  SDK -->|"HTTPS"| IG
  OPS --> IG
  IG -->|"HTTP"| P1
  IG --> PN
  P1 <-->|"typed action"| ELB
  PN <--> ELB
  ELB <-->|"route"| A1
  ELB <--> AN
  P1 -.->|"前置读"| S2
  PN -.-> S2
  A1 -->|"写"| S2
  AN --> S2
  H1 -->|"PostgreSQL"| S1
  HN --> S1
  H1 -->|"HTTPS"| MODEL
  HN --> MODEL
  H1 -.->|"MCP"| BUN
  HN -.-> BUN

  classDef stateless fill:#e8f1ff,stroke:#4a7fd8,color:#0b2545
  classDef stateful fill:#fff0e2,stroke:#d1843a,color:#4a2400
  class P1,PN stateless
  class A1,H1,AN,HN stateful
  style SL1 fill:#e8f1ff,stroke:#4a7fd8
  style SLN fill:#e8f1ff,stroke:#4a7fd8
  style AL1 fill:#fff0e2,stroke:#d1843a
  style ALN fill:#fff0e2,stroke:#d1843a
```

### 网络分段

- 公网只暴露入口层；Peri Swarm Server、Rivet engine 与两个 PostgreSQL 库全部在私网，
  客户端不直连 `8080`。
- 入口层只做南北向 HTTP；`agent/<logicalId>` 的 actor 分配由 Rivet engine 负责，
  Ingress 不需要粘滞会话。
- 会话库与状态库由两个 DSN 分别指向（可以是同一实例的两个数据库），Server 分别直连，
  两库都需要写权限；状态库 DSN 不得指向会话库的数据库。

### 进程内分层

同一份 `peri-swarm` 二进制里逻辑上分成两部分；当前没有 API-only / actor-only 的 role
开关，每个副本同时承担两者。

| 维度 | 无状态接口 | 有状态 actor |
| --- | --- | --- |
| 入口 | axum `/v1`、Bearer、body limit | `SwarmActor` action、`GET /events` |
| 职责 | 校验、公开契约投影、SSE 包装 | 会话执行、进程内 ACP Host、事件序列 |
| 内存状态 | 无会话状态；只有 client 与 store 句柄 | live ACP Host、事件广播、会话视图 |
| 持久状态 | 少量前置读（`rootSessionId`、404） | 以状态库为准写行、items、绑定 |
| 多副本 | 任意副本处理任意请求 | 一个 `agent/<logicalId>` 同一时刻只在一个副本 |
| 转发 | 经 Rivet client 发 typed action | 由 engine 定位；副本间不直连 |
| 失败/重建 | 副本可随时替换 | 只读恢复 + `session/load`；在途 turn 502 |
| 扩缩容 | 加副本即可 | 由 engine 放置；保留 spare capacity |

请求路径：无状态层解析 → typed action → engine → 承载 actor 的副本；actor 的 `on_start`
只恢复只读视图。沙箱侧的 `peri mcp-start` 是独立 Peri 进程，属 workspace MCP，不算
这两层。

### 组件分离条件

| 组件 | 生产位置 | 落地前置（当前差距） |
| --- | --- | --- |
| 传统 Gateway（Nginx 等） | 独立边缘层 | 不需要自研；`PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` 未开启时 Server 只能 loopback，需同机代理 |
| Peri Swarm Server（`peri-swarm`） | 可替换副本组（每副本同时是 Rivet node） | 仓库内无多副本编排；副本间共享 engine、namespace、pool 与部署身份；在途 turn 的跨实例接管是设计非目标 |
| Rivet engine | 3+ 实例 + LB（独立主机组） | endpoint/token/namespace/pool 与 spawn 已可配置；非 loopback 必须 `never`；客户端只接受明文 `http`，TLS/CA 未支持 |
| `peri-sandbox` | 沙箱主机池 | `validate_bun_url` 只接受 loopback `http`；远程需 HTTPS 与主机认证 |
| Swarm 状态库 | 同一 PostgreSQL 实例或独立实例 | 与会话库不同数据库即可；DSN 支持 TLS 参数（SQLx rustls），跨机需网络隔离 |
| 模型供应商 | 公网 | 无 |

### Rivet engine 怎么部署？

这里的 engine 是 Rivet 的 control plane（本地 3.1.1，`:6420`），不是 Peri 进程或 ACP
Host。Peri 侧的运行单元是 actor 实例：`peri-swarm` 的 `SwarmActor`（key
`agent/<logicalId>`）。早期试验性 `@peri-code/rivet` 的 `PeriChatActor` 已随适配层移除；
Peri Swarm 是唯一 actor 主线。control plane 选择承载
actor 的 node、按需唤醒，并把 HTTP/WebSocket 请求转发给该 node；actor 逻辑跑在连到
同一 control plane 的 Server 进程里。node 不需要公网地址或 actor 入站端口。

官方生产形态不是「一台 engine」，而是 3+ 个 engine 实例 + 前置 LB（6420/TLS/1h idle），
每个实例独立块盘并共享启用 versioning 的 bucket；单机只适合开发。所有 Server 副本必须
使用同一 endpoint、namespace 与 pool，并先创建 pool。endpoint 填**集群入口 LB** 地址，
不填单个实例；客户端不做集群发现。依据：
[Architecture](https://rivet.dev/docs/architecture)、[Nodes](https://rivet.dev/docs/nodes)、
[Control plane production checklist](https://rivet.dev/docs/deploy/self-host/control-plane/production-checklist)。

control plane 是 Rivet 官方组件，Peri 不实现也不设计其内部：本地由 `rivetkit` 下载到
`~/.rivetkit/engine/v3` 并按需拉起，生产直接部署官方 `rivetdev/engine`。Peri 只负责
配置面与部署编排（LB、TLS、namespace/pool、凭据、bucket 与磁盘）。

配置面已可外置：endpoint、token、namespace、pool 由环境变量提供，token 只按变量名引用；
`PERI_SWARM_RIVET_ENGINE_SPAWN=never` 关闭本地拉起，非 loopback endpoint 启动前做 5s
TCP 预检，不可达直接失败。仍未支持连接安全：只接受明文 `http`，官方生产要求 6420
前置 TLS 与 1h idle。endpoint 的字面地址必须是环回、RFC1918、ULA 或 link-local；
域名不做 DNS 解析，由部署保证指向私网。

仅把 engine 搬到另一台机器，不会自动获得生产调度能力。Rivet 已负责 node 放置、唤醒与
15s 重调度；Peri 侧仍不承诺在途 turn 接管：设计把跨机调度、租约/fencing 与跨实例接管
列为非目标，`running` 行在重建 actor 上返回 502。扩副本前先固定部署边界。

### 入口层（Ingress）

入口层用现成的传统 Gateway（Nginx、Envoy、云 API Gateway）即可，不需要 Peri 自研：
它只做 TLS 终止、统一鉴权、配额与审计。Peri Swarm Server 是 Agents API 的应用层
服务（`/v1`、Bearer、SSE），本身没有这些能力。要求：

- 入口只做 HTTP 负载；副本之间没有点对点 actor 连接（node 不开放 actor 入站端口），
  action 由 engine 转发到承载 actor 的副本，按 `logicalId` 路由，不依赖粘滞会话。
- 关闭响应缓冲，SSE 逐帧透传；读超时大于 `PERI_SWARM_SSE_HEARTBEAT_MS` 的心跳间隔。
- 入口完成 TLS 与统一鉴权；Server 的 Bearer 作为第二道，两处凭据独立轮换。
- 入口到副本走私网；健康检查用 `GET /healthz`（无鉴权）。

未开 `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` 时，入口只能与 Server 同机或共享网络命名空间；
打开后 Server 可监听任意非 loopback 地址（含 `0.0.0.0`），但 Bearer 仍是唯一鉴权，
TLS 必须由入口终止。
