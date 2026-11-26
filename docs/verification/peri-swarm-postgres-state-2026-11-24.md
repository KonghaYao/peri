# peri-swarm 状态库迁移到 PostgreSQL（验收记录 2026-11-24）

- 分支：`feat/swarm-pg-state`，基线 `origin/main` @ `9332fae19`
- 范围：`peri-swarm` 状态库从 Loom/Turso 换成独立 PostgreSQL 库；`SwarmStore` 端口与
  公开 Sessions API、Rivet 集成不变。
- 结论：只部署 PostgreSQL（会话库 + 状态库两个数据库）即可完成 create → turn →
  items/SSE 续读 → 重启 → 同一会话再输入；无 Loom 服务、无 Loom/token 环境变量。

## 1. 环境

| 项 | 值 |
| --- | --- |
| 主机 | macOS 26.5.1（arm64），Docker（OrbStack） |
| 构建 | `scripts/cargo-rmcp-patched.sh build -p peri-swarm`（debug） |
| PostgreSQL | 一次性容器 `postgres:16-alpine`，`127.0.0.1:55433`（55432 已被本机其它项目占用） |
| 数据库 | `peri_session`（会话库）、`peri_swarm_state`（状态库），同一实例 |
| 模型 | `peri-swarm/scripts/local_model_stub.mjs`（`127.0.0.1:9496`，确定性 SSE） |
| Rivet engine | 3.1.1；`@rivetkit/engine-cli-darwin-arm64@3.1.1`（npm registry）解包后经 `RIVET_ENGINE_BINARY_PATH` 指定，由网关按需拉起 |
| 网关 | `target/debug/peri-swarm`，`127.0.0.1:18090`（18080/18081 已被本机其它进程占用） |

环境变量（摘录，凭据为一次性测试值）：

```bash
export PERI_SWARM_SESSION_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_session'
export PERI_SWARM_STATE_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_swarm_state'
export PERI_SWARM_WORKSPACE_ROOT=/private/tmp/peri-pg-accept/ws   # macOS：必须是 /private/tmp
env | grep -Ei 'loom|hrana|turso' || echo 'no loom/hrana/turso env'
# -> no loom/hrana/turso env
```

`PERI_SWARM_STORE_TOKEN_ENV` 未设置，也没有任何 Loom 服务在参与本次链路。

## 2. 网关启动

```bash
RUST_LOG=peri_swarm=info ./target/debug/peri-swarm
```

关键日志（去掉 ANSI）：

```text
INFO rivet engine configuration endpoint=http://127.0.0.1:6420 namespace=default pool=peri-swarm engine_spawn="always"
INFO opening the remote Peri session store bind=127.0.0.1:18090 engine=postgres machine_id=0192f0c4-0000-7000-8000-00000000c001
INFO opening the independent swarm state store state_store=postgres://127.0.0.1:55433/peri_swarm_state
INFO peri-swarm gateway is listening addr=127.0.0.1:18090 ...
INFO spawned engine process pid=24713 ... version=3.1.1
```

- 状态库日志只有 `postgres://host:port/db`，不含 DSN 凭据。
- `GET /healthz` → `{"status":"ok"}`。

## 3. create → 首轮 turn → items / SSE

```bash
curl -X POST http://127.0.0.1:18090/v1/agents/sessions \
  -H 'Authorization: Bearer local-bearer-pg' -H 'content-type: application/json' \
  -d '{"logicalId":"agent-pg-1","model":"sonnet","input":"hello pg"}'
```

- HTTP `201`，`id=agent-pg-1`，`rootSessionId=01a21788-ee3e-7c63-8c8c-0fd79380269b`，
  `state=idle`。

`GET /items?order=asc&limit=100`：`items=6`（逐帧原始行）、`data=2`（两条 item）、
`lastItemSeq=6`。

| seq | kind | 说明 |
| --- | --- | --- |
| 1 | message | 用户输入 `hello pg` |
| 2–5 | message | assistant 的 4 个 delta，共享同一 `itemId` |
| 6 | message | `complete:true`，文本 `stub-echo:be704992fc61#3`，同一 `itemId` |

`GET /turns?order=asc`：1 个 turn，`status=completed`，
`created_at/started_at/completed_at` 均为 Unix 秒。

SSE 续读：

- `GET /events?after=0`：6 个 `item` 帧（`id: 1`…`id: 6`）+ 官方
  `agent.session.*` 事件（`turn.created`/`in_progress`/`item.added`×2/`item.done`/
  `content_part.*`/`output_text.done`/`turn.completed`/`session.in_progress`/`idle`）。
- `GET /events?after=2`：从 `seq=3` 开始回放，`id` 从 3 递增。

## 4. 重启网关 → 同一 session 再输入

`SIGINT` 关停（日志：`remote Peri session store closed`、`swarm state store closed`），
同一环境重新启动：

```text
INFO swarm actor restored a read-only session view logical_id="agent-pg-1" root_session_id=01a21788-ee3e-7c63-8c8c-0fd79380269b last_item_seq=6 state=Idle
INFO swarm reattached the persisted session logical_id="agent-pg-1" root_session_id=01a21788-ee3e-7c63-8c8c-0fd79380269b
```

- `GET /sessions/agent-pg-1` → 同一 `rootSessionId`，`state=idle`，`lastItemSeq=6`。
- `GET /items` → 6 行，内容与重启前一致。

新输入（新 `Idempotency-Key`）：

```bash
POST /v1/agents/sessions/agent-pg-1/events  -H 'Idempotency-Key: 154af35d-204a-4b08-9e3f-ca1578b72424'
{"text":"after restart"}
# -> 202 {"inputId":"154af35d-...","accepted":true,"disposition":"inserted","mode":"turn","lastItemSeq":7}
```

- items 继续增长：`lastItemSeq` 6 → 12（seq 7 用户输入；8–11 delta；12 complete）。
- `GET /turns` → 2 个 turn，均 `completed`。
- 用同一 `Idempotency-Key` 重放：`202`，`disposition="existing"`，
  `swarm_inputs` 行数保持 1，items 与 `lastItemSeq` 不变（12）。

## 5. 库侧核对

```bash
docker exec peri-swarm-pg psql -U peri -d peri_swarm_state -Atc \
  "select version, engine, contract from swarm_meta;"
# 1|postgres|peri.swarm.postgres/v1
docker exec peri-swarm-pg psql -U peri -d peri_swarm_state -Atc \
  "select count(*) from swarm_sessions; select count(*) from swarm_items; \
   select count(*) from swarm_inputs; select count(*) from swarm_turns; \
   select count(*) from swarm_bindings;"
# 1 / 12 / 1 / 2 / 0
docker exec peri-swarm-pg psql -U peri -d peri_swarm_state -Atc \
  "select logical_id, count(*), min(seq), max(seq), count(distinct seq) from swarm_items group by 1;"
# agent-pg-1|12|1|12|12      -- seq 从 1 起、连续、无重复
docker exec peri-swarm-pg psql -U peri -d peri_swarm_state -Atc \
  "select s.logical_id, s.last_item_seq, coalesce(max(i.seq),0) from swarm_sessions s \
     left join swarm_items i using (logical_id) group by 1,2;"
# agent-pg-1|12|12          -- 缓存投影 = MAX(seq)
docker exec peri-swarm-pg psql -U peri -d peri_swarm_state -Atc \
  "select relname from pg_class c join pg_namespace n on n.oid=c.relnamespace \
    where n.nspname='public' and c.relkind in ('r','p') order by 1;"
# swarm_bindings, swarm_inputs, swarm_items, swarm_meta, swarm_sessions, swarm_turns
docker exec peri-swarm-pg psql -U peri -d peri_session -Atc \
  "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace \
    where n.nspname='public' and c.relname like 'swarm%';"
# 0                          -- 会话库没有任何 swarm_* 对象
docker exec peri-swarm-pg psql -U peri -d peri_session -Atc \
  "select schema_version, contract from peri_store_meta;"
# 1|peri.session.postgres/v1
```

## 6. 负向用例（真实进程）

| 用例 | 命令（同一进程环境） | 结果 |
| --- | --- | --- |
| 状态库 = 会话库（换一种写法 + query） | `PERI_SWARM_STATE_STORE='postgres://peri:peri@127.0.0.1:55433/peri_session?sslmode=disable'` | exit 1：`must not resolve to the same database as the session store (postgres://127.0.0.1:55433/peri_session); provision a dedicated PostgreSQL database for the Swarm state store` |
| 旧 Loom locator | `PERI_SWARM_STATE_STORE='http://127.0.0.1:9497/db/local_default/swarm/main/'` | exit 1：`expected a postgres:// or postgresql:// DSN; ... a Loom/Hrana locator is no longer supported` |
| 非空且无 `swarm_meta` 的库 | `createdb peri_foreign` + `create table foreign_table(id int)` | exit 1：`the target schema is not empty (relations: foreign_table) but has no swarm_meta; refusing to initialize over an existing schema ...` |
| 会话库被误指为状态库（非空且有 `peri_store_meta`） | 由契约测试覆盖：`the_peri_session_schema_is_never_adopted` | `Schema` 错误，文案指向「这是 Peri 会话库」 |

## 7. 自动化测试

```bash
# 纯单测（无外部服务）
scripts/cargo-rmcp-patched.sh test -p peri-swarm --lib
# test result: ok. 352 passed; 0 failed; 14 ignored

# 真实 PostgreSQL 契约测试（每用例一个私有 schema，结束 DROP SCHEMA）
PERI_TEST_POSTGRES_URL='postgresql://peri:peri@127.0.0.1:55433/postgres' \
  scripts/cargo-rmcp-patched.sh test -p peri-swarm --lib -- --ignored store::postgres
# test result: ok. 14 passed; 0 failed

# 两个真实 PG 库的部署级测试（真实 turn + 两库隔离 + 重启续写）
PERI_SWARM_SESSION_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_session' \
PERI_SWARM_STATE_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_swarm_state' \
  scripts/cargo-rmcp-patched.sh test -p peri-swarm --test it -- --ignored state_store_test
# test result: ok. 3 passed; 0 failed
```

未设置 `PERI_TEST_POSTGRES_URL` / 两个 DSN 时上述 ignored 用例打印一行 skip 后返回，
默认 `cargo test`（无 `--ignored`）完全跳过，无 PostgreSQL 的 CI 仍为绿。

## 8. 未跑通 / 未覆盖

- **Loom → PostgreSQL 的自动数据迁移**：本版本不提供（无 Hrana 导出客户端）；割接与
  人工导入步骤见 [peri-swarm README §升级与回退](../../peri-swarm/README.md#升级与回退状态库)。
- **CI 未接入 ignored 的真实 PG 用例**：本次只在本地 docker 容器执行；接入方式可复用
  `.github/workflows/ci.yml` 的 `PERI_TEST_POSTGRES_URL` 模式，另立任务。
- **跨机 / TLS / 多副本并发首启**：未验收。advisory-lock 初始化与 `swarm_meta` 唯一行
  的设计目标覆盖并发首启，但只有单进程实测。
- **首次尝试的失败与原因**：把 `PERI_SWARM_WORKSPACE_ROOT` 设为 `/tmp/...`（符号链接）
  时，`resolve_cwd` 规范化后得到 `/private/tmp/...`，落在根前缀之外，create 返回
  `409 session_conflict` 并在日志里给出 `workspace location is unavailable (code -32010)`。
  这是 macOS 上 README 已记录的 `/private/tmp` 约束，与会话库/状态库迁移无关；换成
  `/private/tmp/...` 后同一链路通过。
