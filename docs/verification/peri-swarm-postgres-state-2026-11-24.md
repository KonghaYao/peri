# peri-swarm 状态库迁移到 PostgreSQL（验收记录 2026-11-24）

- 分支：`feat/swarm-pg-state`，基线 `origin/main` @ `9332fae19`
- 本轮验收的代码：`34bc0f4b6`（含复核修复 `adcb9fe4d` / `6d67059e7` / `34bc0f4b6`）
- 范围：`peri-swarm` 状态库从 Loom/Turso 换成独立 PostgreSQL 库；`SwarmStore` 端口与
  公开 Sessions API、Rivet 集成不变；新增一次性离线导入脚本
  `peri-swarm/scripts/import_loom_state.py`（Loom SQLite 快照 → PostgreSQL）。
- 结论：只部署 PostgreSQL（会话库 + 状态库两个数据库）即可完成 create → turn →
  items/SSE 续读 → 重启 → 同一会话再输入；无 Loom 服务、无 Loom/token 环境变量。存量
  割接走离线导入，演练见 §7。

## 1. 环境

| 项 | 值 |
| --- | --- |
| 主机 | macOS 26.5.1（arm64），Docker（OrbStack） |
| 构建 | `scripts/cargo-rmcp-patched.sh build -p peri-swarm`（debug） |
| PostgreSQL | 一次性容器 `postgres:16-alpine`，`127.0.0.1:55433`（55432 已被本机其它项目占用） |
| 数据库 | `peri_session`（会话库）、`peri_swarm_state`（状态库），同一实例；为排除旧数据，本轮删除并重建了 `peri_swarm_state`，会话库保留 |
| 模型 | `peri-swarm/scripts/local_model_stub.mjs`（`127.0.0.1:9496`，确定性 SSE） |
| Rivet engine | 3.1.1；`@rivetkit/engine-cli-darwin-arm64@3.1.1`（npm registry）解包后经 `RIVET_ENGINE_BINARY_PATH` 指定；本轮 engine 已在 `127.0.0.1:6420` 运行（由上一轮验收拉起，version 与二进制无关） |
| 网关 | `target/debug/peri-swarm`，`127.0.0.1:18090`（18080/18081 已被本机其它进程占用） |

环境变量（摘录，凭据为一次性测试值）：

```bash
export PERI_SWARM_SESSION_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_session'
export PERI_SWARM_STATE_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_swarm_state'
export PERI_SWARM_WORKSPACE_ROOT=/private/tmp/peri-import-drill/ws   # macOS：必须是 /private/tmp
env | grep -Ei 'loom|hrana|turso' || echo 'no loom/hrana/turso env'
# -> no loom/hrana/turso env
```

`PERI_SWARM_STORE_TOKEN_ENV` 未设置，也没有任何 Loom 服务参与本次链路。

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
```

- 状态库日志只有 `postgres://host:port/db`，不含 DSN 凭据。
- `GET /healthz` → `{"status":"ok"}`。

## 3. create → 首轮 turn → items / SSE

```bash
curl -X POST http://127.0.0.1:18090/v1/agents/sessions \
  -H 'Authorization: Bearer local-bearer-pg' -H 'content-type: application/json' \
  -d '{"logicalId":"agent-pg-1","model":"sonnet","input":"hello pg"}'
```

- HTTP `201`，`id=agent-pg-1`，`rootSessionId=01a217a5-c741-72e2-ade3-b297472f8c1a`，
  `state=idle`，`created=true`。

`GET /items?order=asc&limit=100`：`items=6`（逐帧原始行）、`data=2`（两条 item）、
`lastItemSeq=6`。

| seq | kind | 说明 |
| --- | --- | --- |
| 1 | message | 用户输入 `hello pg` |
| 2–5 | message | assistant 的 4 个 delta，共享同一 `itemId` |
| 6 | message | `complete:true`，文本 `stub-echo:be704992fc61#4`，同一 `itemId` |

`GET /turns?order=asc`：1 个 turn，`status=completed`，
`created_at/started_at/completed_at` 均为 Unix 秒。

SSE 续读：

- `GET /events?after=0`：6 个 `item` 帧（`id: 1`…`id: 6`）+ 官方
  `agent.session.*` 事件（`turn.created`/`in_progress`/`item.added`×2/`item.done`/
  `content_part.added`/`content_part.done`/`output_text.done`/`turn.completed`/
  `session.in_progress`/`idle`）。
- `GET /events?after=2`：4 个 `item` 帧，`id` 从 3 递增到 6。

## 4. 重启网关 → 同一 session 再输入

`SIGINT` 关停，同一环境重新启动：

```text
INFO swarm actor restored a read-only session view logical_id="agent-pg-1" root_session_id=01a217a5-c741-72e2-ade3-b297472f8c1a last_item_seq=6 state=Idle
INFO swarm reattached the persisted session logical_id="agent-pg-1" root_session_id=01a217a5-c741-72e2-ade3-b297472f8c1a
```

- `GET /sessions/agent-pg-1` → 同一 `rootSessionId`，`state=idle`，`lastItemSeq=6`。
- `GET /items` → 6 行，内容与重启前一致。

新输入（新 `Idempotency-Key`）：

```bash
POST /v1/agents/sessions/agent-pg-1/events  -H 'Idempotency-Key: 154af35d-204a-4b08-9e3f-ca1578b72424'
{"text":"after restart"}
# -> 202 {"inputId":"154af35d-...","accepted":true,"disposition":"inserted","mode":"turn","lastItemSeq":7}
```

- items 继续增长：`lastItemSeq` 6 → 12（seq 7 用户输入；8–11 delta；12 complete，
  文本 `stub-echo:3ecb7a233376#5`）。
- `GET /turns` → 2 个 turn，均 `completed`。
- 用同一 `Idempotency-Key` 重放：`202`，`disposition="existing"`，
  `swarm_inputs` 行数保持 1，items 与 `lastItemSeq` 不变（12）。
- 注：新输入后立刻查询可能看到第二个 turn 仍为 `in_progress`（turn 由 ACP 宿主异步
  完成）；等待数秒后为 `completed`。这是正常时序，不是持久化延迟。

## 5. 库侧核对

```bash
docker exec peri-swarm-pg psql -U peri -d peri_swarm_state -Atc \
  "select version, engine, contract from swarm_meta;"
# 1|postgres|peri.swarm.postgres/v1
docker exec peri-swarm-pg psql -U peri -d peri_swarm_state -Atc \
  "select (select count(*) from swarm_sessions)||'/'||(select count(*) from swarm_items)||'/'||
          (select count(*) from swarm_inputs)||'/'||(select count(*) from swarm_turns)||'/'||
          (select count(*) from swarm_bindings);"
# 1/12/1/2/0
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
| 已初始化但缺权威表（`adcb9fe4d`） | `pg_dump peri_swarm_import \| psql peri_swarm_damaged`，再 `DROP TABLE swarm_inputs` 后启动 | exit 1：`swarm_meta says this database is initialized, but the authoritative table(s) swarm_inputs are missing; this build never re-creates them empty ...`；再次查询 `swarm_inputs` 仍不存在（未被悄悄建回） |
| 会话库被误指为状态库（非空且有 `peri_store_meta`） | 由契约测试覆盖：`the_peri_session_schema_is_never_adopted` | `Schema` 错误，文案指向「这是 Peri 会话库」 |

### 6.1 `type=none` 会话删除（复核新增回归）

`type=none` 是 WeBots 秘书的默认形态，`environment_json` 没有 `id`；binding 锁查询因此
返回**一行 SQL NULL**。原实现把 `fetch_optional` 的行按非空 `String` 解码，会在删除任何
行之前失败。修复把两层可空显式解码（`Option<Option<String>>` + `flatten`），并补两条
真实 PG 回归：

```bash
PERI_TEST_POSTGRES_URL='postgresql://peri:peri@127.0.0.1:55433/postgres' \
  scripts/cargo-rmcp-patched.sh test -p peri-swarm --lib -- \
  --ignored a_none_environment_session_deletes_and_cascades
# ok（同一用例在未修复实现上 FAILED：delete_session 返回列解码错误）

PERI_SWARM_SESSION_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_session' \
PERI_SWARM_STATE_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_swarm_state' \
  scripts/cargo-rmcp-patched.sh test -p peri-swarm --test it -- --ignored state_store_none_delete
# ok：真实 axum router + 真实 PG，DELETE 返回 200/deleted=true，随后 GET 返回 404
```

既有的 hosted 环境并发删除用例（25 轮 + 外部锁串行）保持通过。

## 7. 离线导入演练（Loom SQLite 快照 → PostgreSQL）

工具：`peri-swarm/scripts/import_loom_state.py`（Python 3 标准库；不运行网关、无
Loom/Turso 依赖）。legacy fixture 由
`peri-swarm/scripts/test-import-loom-state.py --write-fixture` 生成，形状与
`git show 9332fae19:peri-swarm/src/store/turso.rs` 的 DDL 一致（`swarm_meta(version)`、
`truncated INTEGER`、`seq`/时间为 `INTEGER`）。

演练目录：`/private/tmp/peri-import-drill`；fixture 含 2 个会话（1 个 `none`、1 个
`openai_hosted` + binding）、8 条 items（同一 `itemId` 的 delta 与 complete）、1 条
`inputId` 收据、1 个 completed turn。

```bash
python3 peri-swarm/scripts/test-import-loom-state.py --write-fixture old-loom.db
# -> old-loom.db, sha256 43745d9314488d26f0e55ca5c4d0e48cf50137b203d9aa5ca419f0842208f6de

python3 peri-swarm/scripts/import_loom_state.py inspect --snapshot old-loom.db
# swarm_sessions 2 / swarm_items 8 / swarm_inputs 1 / swarm_bindings 1 / swarm_turns 1
# last_item_seq_mismatch=0 seq_starts_at_one=0 seq_gap_sessions=0 duplicate_inputs=0
# duplicate_root_sessions=0 shared_input_ids=0 orphan_*=0 -> status: importable

python3 peri-swarm/scripts/import_loom_state.py export \
  --snapshot old-loom.db --out-dir out/ --created-at 2026-11-24T00:00:00Z
# -> 01-schema.sql 02-data.sql 03-verify.sql expected.json（manifest 含源 sha256）

# 目标：全新空库 peri_swarm_import（psql 经 docker exec 执行）
psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -f out/01-schema.sql
psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -f out/02-data.sql
psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -qAt -f out/03-verify.sql > out/observed.txt
python3 peri-swarm/scripts/import_loom_state.py check \
  --expected out/expected.json --observed out/observed.txt
# -> reconciliation OK: 35 checks match
```

对账输出（节选，`observed.txt`）：`meta_version=1`、`meta_engine=postgres`、
`meta_contract=peri.swarm.postgres/v1`、`sessions=2`、`items=8`、`inputs=1`、
`bindings=1`、`turns=1`、`items_payload_bytes=329`、`sessions_environment_bytes=56`，
其余不变式均为 0（含 `duplicate_inputs`、`duplicate_root_sessions`、`orphan_*`、
`invalid_*`）。

目标非空时两道守卫都拒绝（实测 exit 3）：

```text
01-schema: ERROR: refusing to import: schema public already has 6 relation(s); import into a dedicated empty database
02-data  : ERROR: swarm_sessions is not empty (2 row(s)); import needs an empty state database
```

启动网关（`PERI_SWARM_STATE_STORE=.../peri_swarm_import`）：日志出现
`opening the independent swarm state store state_store=postgres://127.0.0.1:55433/peri_swarm_import`，
无 schema 错误（导入库通过「已初始化」分支校验）。

| 检查 | 观察结果 |
| --- | --- |
| `GET /sessions/agent-old-1` | `rootSessionId=root-old-1`、`state=idle`、`lastItemSeq=6`、`metadata={"tenant":"alpha"}`、`environment={"type":"none"}` |
| `GET /items?order=asc` | `items=6`、`data=2`（`itim-old-1` 的 5 行按 item 折叠 + `itim-old-2` 收据），`lastItemSeq=6` |
| 旧 `inputId` 同文本重放（`8d6e2c43-3f7a-4c9e-9a15-1f0a2b3c4d5e` + `hello loom`） | `202`，`disposition="existing"`，`lastItemSeq=6` |
| 同 key 不同文本 | `409 input_conflict`（收据指纹生效） |
| SSE `GET /events?after=3` | 回放 `id: 4,5,6` 三个 `item` 帧，payload/`createdAt` 与导入值一致 |
| 重启网关 | `rootSessionId`/`lastItemSeq`/items 不变；再次重放旧 key → `202 existing` |
| 导入库行数（重放与重启后） | `2/8/1/1/1`，无新行、无重写 |

首次尝试的失败与原因（保留作为回归证据）：

- fixture 最初用 `old-input-1` 作 `inputId`，网关返回 `400 invalid_input: inputId must be a UUID`；旧库的行都由 HTTP 层写入，`inputId` 必然是 UUID，工具因此新增了 UUID 校验（`inspect`/`export` 直接拒绝非 UUID 的收据）。
- fixture 的 `inputId` 指纹最初按字母序（`BTreeMap`）计算，重放得到 `409 input_conflict`；
  从真实网关行读回指纹后确认 workspace 开启了 serde_json `preserve_order`，canonical 顺序是
  `json!` 的插入顺序。修正后 fixture 指纹等于真实值
  `42b95665…`（单测固定该值），重放得到 `202 existing`。

**限制**：本演练的源快照是脚本自建的 legacy fixture，**未对真实 Loom 生产快照验证**。
现场还需确认存储方导出格式、`-wal` 一致性、数据规模与 `inputId` 均为 UUID；导入是只读
快照 + 空库写入，旧库不被修改。

## 8. 自动化测试

```bash
# 纯单测（无外部服务）
scripts/cargo-rmcp-patched.sh test -p peri-swarm
# lib: 357 passed; 0 failed; 20 ignored
# it :  98 passed; 0 failed; 7 ignored

# 真实 PostgreSQL 契约/集成测试（每用例一个私有 schema，结束 DROP SCHEMA）
PERI_TEST_POSTGRES_URL='postgresql://peri:peri@127.0.0.1:55433/postgres' \
  scripts/cargo-rmcp-patched.sh test -p peri-swarm --lib -- --ignored store::postgres
# test result: ok. 20 passed; 0 failed
# 含：首开建表、二次开只修派生对象、缺表/缺列 fail closed、版本 2 拒绝、非空库/会话库拒绝、
#     会话身份冲突、list/metadata 过滤、input 幂等、seq 与 item 分页、append_items 回滚、
#     并发同 seq、turn 对账、删除与绑定转交、`type=none` 删除级联、同环境并发删除不留
#     悬挂 binding、外部写事务持有 binding 锁时删除等待、提交前超时不可见

# 两个真实 PG 库的部署级测试（真实 turn + 两库隔离 + 重启续写）
PERI_SWARM_SESSION_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_session' \
PERI_SWARM_STATE_STORE='postgresql://peri:peri@127.0.0.1:55433/peri_swarm_state' \
  scripts/cargo-rmcp-patched.sh test -p peri-swarm --test it -- --ignored state_store
# test result: ok. 4 passed; 0 failed; finished in 40.03s
# （含 type=none 删除的 HTTP 回归）

# 离线导入工具（fixture 驱动，无 PostgreSQL）
python3 peri-swarm/scripts/test-import-loom-state.py
# Ran 23 tests ... OK
```

未设置 `PERI_TEST_POSTGRES_URL` / 两个 DSN 时上述 ignored 用例打印一行 skip 后返回，
默认 `cargo test`（无 `--ignored`）完全跳过，无 PostgreSQL 的 CI 仍为绿。

CI：`.github/workflows/ci.yml` 的 Linux 步骤 `Run PostgreSQL session and Swarm state
contracts` 在一次性 `postgres:17-alpine` 上执行：

- `cargo test -p peri-swarm --lib -- --ignored store::postgres --test-threads=2`（与现有
  会话存储契约测试共用同一实例）；
- 另建 `peri_swarm_session` / `peri_swarm_state` 两个库后执行
  `cargo test -p peri-swarm --test it -- --ignored state_store_none_delete`（`type=none`
  删除的真实 HTTP 回归）。

`Test Loom state import tool` 步骤执行上面的 Python 单测。两个真实 PG 库的 ACP 部署级
测试（`state_store_test` / `state_store_failure_test`，需进程内宿主与模型 stub）仍未接入
CI（见 §9）。

## 9. 未跑通 / 未覆盖

- **真实 Loom 生产快照**：离线导入只对脚本自建的 legacy fixture 演练过；现场快照的
  导出方式、`-wal` 一致性、数据规模与 UUID 前提未验证（§7 限制）。
- **ACP 部署级两库测试未进 CI**：`--test it -- --ignored state_store` 需要两个数据库，
  耗时 ~40s 且会拉起进程内 ACP 宿主与模型 stub；本轮只在本地执行。`type=none` 删除回归
  已单独进 CI（无 ACP，0.1s）。其余部署级用例的接入方式：同一 CI 步骤内复用
  `peri_swarm_session` / `peri_swarm_state` 并设置模型 stub，另立任务评估稳定性。
- **跨机 / TLS / 多副本并发首启**：未验收。advisory-lock 初始化与 `swarm_meta` 唯一行
  的设计目标覆盖并发首启，但只有单进程实测。
- **导入后继续对话**：演练只覆盖 `GET`/items/SSE/幂等重放/重启。新输入需要 Peri 会话库
  里存在对应 `root_session_id` 历史以便重新 attach；fixture 的 root 只存在于状态库，因此
  没有演练新 turn。
- **首次尝试的失败与原因（环境）**：把 `PERI_SWARM_WORKSPACE_ROOT` 设为 `/tmp/...`
  （符号链接）时，`resolve_cwd` 规范化后得到 `/private/tmp/...`，落在根前缀之外，create
  返回 `409 session_conflict` 并在日志里给出 `workspace location is unavailable (code
  -32010)`。这是 macOS 上 README 已记录的 `/private/tmp` 约束，与会话库/状态库迁移无关；
  换成 `/private/tmp/...` 后同一链路通过。
