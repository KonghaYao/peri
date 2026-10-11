# schema 代数压缩：未发布中间代收敛为单条迁移

- 状态：**设计已裁决，待实施**。
- 日期：2026-10-10。
- 落点：`merge/v4-to-main`（PR #179）；pre-release/main 保持 schema 19，两线漂移以 PR 为准。
- 关联：`2026-10-07-remove-execution-recovery-schema-review.md`、`2026-10-08-remove-legacy-execution-registrations-plan.md`。

## 1. 背景

v4 期间会话存储 schema 从正式基线推进到 19；其中 11..19 共 9 代只存在于
pre-release 内部与 rolling beta 构建，从未进入正式发布（正式发布版本：
agent-v3.19.x = schema 10，agent-v3.18.0 = schema 6）。合并进 main 前压缩代数：
版本收敛、迁移合并为单条、中间代对象与迁移代码删除。

## 2. 用户裁决（2026-10-10）

1. **版本收敛 + 单条迁移**：新版本 11；删除 11..19 中间代。
2. **仅兼容正式发布库**：本地 ≤10 保留通用迁移；12..19 一律拒绝（无 β 用户库）。
3. **契约标签**：`peri.session.store/v5` 为当前；`peri.session.store/v2` 为唯一升级来源。
4. **落点**：直接在 `merge/v4-to-main` 上实施。

## 3. 目标形态

| 项 | 现状 | 压缩后 |
| --- | --- | --- |
| 版本常量 | 19 | 11 |
| 远端契约 | v4（前代 v3/v2） | v5（前代 v2） |
| 迁移路径 | 10→11→12→14→19 多段 | 10\|11 → 11 单条 |
| 最终表结构 | — | **不变**（= 现行 19 形状） |

### 3.1 最终形状（= 现行 19 形状，来自 `CREATE_V2_TABLES` 族）

machines / projects / workspaces(machine_id, path, path_source, project_id, identity,
discovery) / threads(+workspace_id, archived) / messages / session_bindings(schema_version,
project_id, workspace_id, relative_cwd, discovery_snapshot, evidence_origin；**无**指向
workspaces 的外键) / mcp_oauth_credentials(principal_id, workspace_id, server_key) /
session_close_intents。

### 3.2 输入形状（V10/V11，正式发布）

threads（无 workspace_id/archived）/ messages / projects / workspaces(id, root,
root_identity, discovery；登记语义) / session_bindings(schema_version, project_id,
workspace_id, relative_cwd + (workspace_id, project_id)→workspaces 外键) /
session_environments(thread_id, machine_id) / mcp_oauth_credentials(principal_id,
machine_id, server_key)。V10 与 V11 的差异仅为退役对象是否已清理。

### 3.3 搬运规则（本地与远端共用语义）

- 规划：`storage_v2_plan::{read_local_plan, plan_local_workspaces}` 保持不变；
  远端继续自读 + `plan_local_workspaces`（同现行 v12）。
- machines：plan 中出现的 machine_id 全集（uuid 字面量 → '我的电脑'/known，否则
  '旧机器'/legacy_unknown）+ 当前机器 `INSERT OR IGNORE`；规划用的**同一集合**也并入
  当前机器（本机与远端一致）——没有会话引用的登记行因此落在当前机器上，不会因集合为空
  被 `plan_workspace_rows` 静默跳过（登记证据丢失、版本照常推进）。
- workspaces：plan.workspaces（id 沿用旧登记 UUID，同现行 v12 语义）。
- threads：加 workspace_id（REFERENCES workspaces(id)）与 archived（DEFAULT 0）；
  workspace_id ← plan.session_workspace_ids。加列后**整体重建**到 canonical 形状
  （`workspace_id` 落在 `NOT NULL` 上，无归属的会话行拒绝升级）；重建表上不承载数据
  的挂载对象不随表搬运（索引由末尾整表重放补回，触发器随重建消失），载有数据的未知
  列没有去处、一律拒绝升级。
- session_bindings：**重建**（新形状）。workspace_id ← 归属行 id
  （plan.session_workspace_ids[thread_id]）；discovery_snapshot ← 旧登记（旧
  workspaces 行，按旧 bindings.workspace_id 匹配）的 discovery；
  evidence_origin = 'legacy_last_observation'；任一绑定取不到 discovery → **拒绝升级**
  （fail-closed，沿用现行严格性）。schema_version / project_id / relative_cwd 原样。
- mcp_oauth_credentials：DROP + 重建（不搬运；旧凭证是 machine 作用域，无法证明
  workspace 归属——沿用现行决策）。
- session_environments：迁移完成后 DROP（`IF EXISTS`——远端 v2 输入可能整表缺席，删除
  语句必须对缺表幂等；同名异形仍由删除前的形状判定拦下）。
- 退役对象：`schema_cleanup::removal_plan` + `execution_recovery_removal_plan`。

## 4. canonical.rs 收敛

- `CURRENT_SCHEMA_VERSION = 11`。
- 当前形状常量（现 `CREATE_V2_*` / `CREATE_V2_INDEXES` / `CREATE_V2_TABLES`）去掉 V2
  前缀：`CREATE_MACHINES_TABLE_SQL`、`CREATE_WORKSPACES_TABLE_SQL`、
  `CREATE_THREADS_TABLE_SQL`、`CREATE_BINDINGS_TABLE_SQL`、
  `CREATE_OAUTH_CREDENTIALS_TABLE_SQL`、`CREATE_TABLES`、`CREATE_INDEXES`、
  `CANONICAL_TABLES`（8 张，含 close_intents；顺序按依赖：machines → projects →
  workspaces → threads → messages → session_bindings → oauth → close_intents）。
- 旧形状常量加 `V10_` 前缀：`V10_CREATE_TABLES`、`V10_CREATE_INDEXES`、
  `V10_CREATE_OAUTH_CREDENTIALS_TABLE_SQL`（迁移输入 / 老库补齐使用）。
- 保留：`CREATE_ENVIRONMENTS_TABLE_SQL`、`BACKFILL_ENVIRONMENTS_SQL`、
  `CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL`、oauth 读写语句（去 V2 前缀）、
  `THREAD_CHILD_DELETES` 族、`payload_role` 族。
- 删除（中间代专用）：`ADD_WORKSPACES_{PROJECT,IDENTITY,DISCOVERY}_COLUMN_SQL`、
  `BACKFILL_WORKSPACES_EVIDENCE_SQL`、`BACKFILL_MISSING_WORKSPACES_SQL`、
  `COUNT_BINDINGS_WITHOUT_OWNER_SQL`、`COUNT_BINDINGS_AT_FOREIGN_ROOT_SQL`、
  `REBUILD_BINDINGS_WITHOUT_REGISTRATIONS_SQL`、`DROP_LEGACY_REGISTRATIONS_SQL`。

## 5. 本地路径（sqlite_store）

- `SchemaState` 收敛：`Empty / Legacy / Version2..10 / Version11`（旧内部形状）/
  `Current`。
  - version == 11 的形状判定（fail-closed）：**逐表逐列**比对一份共用声明
    （`sessions/schema_shape.rs`，本机与远端同源）——列序列、NOT NULL、主键逐项相符 →
    `Current`；压缩前登记形状（带上下界）→ `Version11`（走升级）；其余 → 拒绝，
    逐项漂移原因写日志。`mcp_oauth_credentials` 与 `session_close_intents` 允许缺表
    （前者保持缺失、凭证能力如实上报不可用；后者由写打开幂等补齐），存在时必须同形。
    索引不参与判定：它是派生对象，由新建/迁移建齐、由测试断言。
  - version 12..19 → `UnsupportedSchemaVersion { found, supported: 11 }`。
- `init_schema`：
  - `Current` → 幂等补齐（close_intents 表、当前机器行）。
  - `Empty` → 建当前形状 + `user_version = 11`。
  - `Legacy` / `Version2..10` → 单事务：补齐到 V10 形状（现 `migrate_schema` 的
    建表/补列/键修补保留）+ 单条搬运，一次提交 `user_version = 11`。
  - `Version11`（旧形状）→ 单条搬运。
- 迁移模块：`storage_v2_migration.rs` + `storage_v19_migration.rs` 合并为
  `storage_v11_migration.rs`。结构：
  `PRAGMA foreign_keys = OFF` → `BEGIN IMMEDIATE` → removals（含 execution recovery）
  → 补 `session_environments` + backfill（沿用现行保险）→ 形状判定（输入表上下界 +
  待删对象同形，判定不过即拒绝）→ 读计划（read_local_plan）
  + 读旧 bindings 与登记 discovery → DROP `session_bindings`、DROP `workspaces` →
  建 machines/workspaces + 插 → threads ALTER + UPDATE → **重建 `threads`**
  （建暂存 → 搬入 → 删旧 → 建新 → 搬回 → 删暂存；`workspace_id` 落在 canonical 的
  `NOT NULL` 上，无归属行即拒绝）→ 建新 bindings + INSERT
  （映射见 §3.3）→ oauth 重建 + close_intents → DROP `session_environments` →
  **整表重放 `CREATE_INDEXES`** → `PRAGMA foreign_key_check` 无违规 →
  `user_version = 11` → COMMIT → FK ON。
- 重建表上不承载数据的挂载对象不随表搬运：索引由末尾整表重放补回，触发器随重建消失；
  载有数据的未知列没有去处，一律拒绝升级（见 §3.3）。
- 不引入 `legacy_execution_registrations` 中间名（直接重建，迁移过程内部也不出现
  登记表概念）。

## 6. 远端路径（remote）

- `schema.rs`：`STORE_CONTRACT = "peri.session.store/v5"`、
  `PREVIOUS_STORE_CONTRACT = "peri.session.store/v2"`。
  - `matches_build`：v5 && version == 11。
  - `readable`：matches_build || (v2 && version ∈ {10, 11})。
  - `acceptance`：11 → Accept；10 → Upgradeable；> 11 → TooNew；其余 → Unusable。
  - v3/v4 契约、12..19 → 不可读 → `open_step` 拒绝（unrecognized remote store schema）。
  - 打开时（含只读）用同一份形状声明做一次**只读探测**：v5 判当前形状、v2 判迁移输入
    形状，判定不过即拒绝——不猜形状，也不在形状不明时读写。
- `schema_upgrade.rs` 改写为单条迁移（`10|11 → 11`，契约推进 v2 → v5）：
  guards（对象数、逐对象定义、行数、meta 快照）→ 读取与规划（同现行 v12）→ 建新形状
  + 搬运（规则同 §3.3，`threads` 同样走重建）→ **结果守卫**（重建链之后核对 `messages`
  行数：父行检查被打开时 `DROP TABLE` 会隐式删除并级联清空历史，前置守卫看不到这一步）
  → DROP 旧对象 → removals → 整表重放 `CREATE_INDEXES` → 推进 `(11, v5)`
  （版本推进仍是批内末条）。
- 删除 `schema_v12_upgrade.rs`、`schema_v14_upgrade.rs`、`schema_v19_upgrade.rs`。
- 空库分支（v2 契约但无 canonical 表）→ 直接建当前形状 + 推进 `(11, v5)`。

## 7. 拒绝面

| 库 | 判定 |
| --- | --- |
| 本地 ≤10 | 通用迁移 → 单条搬运 → 11 |
| 本地 11（旧内部形状） | 单条搬运 → 11（宽容，成本为零） |
| 本地 11（新形状） | Current |
| 本地 12..19 | 拒绝（supported: 11） |
| 远端 v2 + 10\|11 | 升级 → (11, v5) |
| 远端 v5 + 11 | 接受 |
| 远端 v3/v4 契约、版本 >11 | 拒绝 |

## 8. 测试

- 重写 `remote/schema_upgrade_test.rs`：V10/V11 夹具 → 11（形状 + 数据保义）、
  空库、拒绝面（12..19、v3/v4 契约、TooNew）。
- 新增 `storage_v11_migration_test.rs`（合并现 storage_v2_migration_test /
  storage_v19_migration_test 语义）；删除旧两个测试文件。
- 删除 `remote/schema_v19_upgrade_test.rs`；`remote/schema_v18_test.rs` 改为"拒绝 18"
  的拒绝面断言或删除。
- 调整版本/契约断言：`remote/schema_test.rs`、`remote/session_shape_test.rs`、
  cloud 系列测试、`sqlite_store/schema_v11_test.rs`（按内容改写或删除）。

## 9. 文档

- `docs/code-index/peri-resources.md`（迁移链与文件名）
- `docs/design/storage-v2-machine-workspace-session.md`（schema 代数与升级路径）
- `docs/standards/architecture-contracts.md`（schema/契约引用）

## 10. 验收

1. `./scripts/cargo-rmcp-patched.sh build --locked -p peri-resources` 通过。
2. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib` 通过。
3. 新库初始化后的表/列集合 == 压缩前 19 形状（由形状常量与形状测试断言）。
4. V10 夹具（含登记、绑定、环境、退役对象）升级后：绑定收敛到归属行、
   discovery 复制、机器/路径与 plan 一致、退役对象清除。
5. 拒绝面：12..19（本地/远端）、v3/v4 契约（远端）被拒。
