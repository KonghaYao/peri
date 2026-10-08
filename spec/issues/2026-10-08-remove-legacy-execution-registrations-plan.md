# 移除 legacy_execution_registrations 实施计划

- 状态：**已实施（代码与文档完成，待发布验收）**。2026-10-08 用户要求「写为 P1 issue，附加 plan，准备移除这张表，使用新的 workspaces 机制」，并在当日授权「自己开个 workspace 完成」——D2 契约选择与其余裁决由实施方在独立 worktree 内作出，结果见 §2 末「裁决结果」与 §9。
- 日期：2026-10-08。
- 授权：2026-10-08 用户要求「写为 P1 issue，附加 plan，准备移除这张表，使用新的 workspaces 机制」。
- 实施分支：worktree `/Users/konghayao/code/ai/peri-remove-legacy-registrations-20261008`，分支 `refactor/remove-legacy-registrations-20261008`（基线 `29442517`）；按约束不 push。
- 问题记录：[2026-10-08-p1-remove-legacy-execution-registrations.md](2026-10-08-p1-remove-legacy-execution-registrations.md)。
- 目标态来源：`docs/design/storage-v2-machine-workspace-session.md` §3（第 115–149 行）。
- 约束：不改真实用户库做验证；不推送；未经要求不 commit。

## 1. 交付契约

**要达成的**

1. `workspaces` 是唯一 Workspace 归属权威，`id` 是唯一 Workspace ID（身份键按 §2 D2 裁决：`(machine_id, path)` 或 `(machine_id, path, identity)`）。
2. 执行证据的单一权威是 `session_bindings.discovery_snapshot`（每条绑定不可变）；归属取 `threads.workspace_id`。
3. `legacy_execution_registrations` 与其外键从 schema 19 起不存在；本机与远端建表清单同源（`canonical.rs` 一份 DDL）。
4. v18 及更早旧库可升级到 19；无法证明原对象连续性的历史会话保持「只读历史」，不补造执行资格。
5. 两份设计文档收敛到同一条 Workspace 身份定义（本次是设计契约变更，不是单方面清理：现行设计 `session-id-environment.md` §3.2 规定身份键含目录对象证据，`storage-v2` §3 规定不含——见 §2 D2）。

**不引入的**

不新增等价语义的表（含 `workspace_evidence` 之类）、不做双写、不留 deprecated shim、不把登记表保留为空壳或视图。旧库遗留的 `thread_goals`、扩展表、view/trigger/index 原样保护。

**可观察行为不变量**（迁移前后一致）

- 按 ID 加载历史会话、续聊、fork/child、归档与列表分页。
- 同 path 替换目录对象、目录换位（Git worktree）、无 Git 目录、慢 Git、只读打开、跨 compose 的准入判定结论不变。
- `WorkspaceError` 语义不变：`InvalidBinding` / `NeedsRelink` / `ExecutionBindingMismatch` / `BindingMissing` 的触发条件只允许在 §2 D2/D3 声明的范围内变化。

**明确声明的行为变化**（按 §2 D2/D3/D4 的裁决结果）

- 同 path 换新目录对象不再产生第二条身份记录；既有会话仍会失败，但依据从「旧登记行仍在」变为「绑定自身不可变快照复核失败」。
- 若取 D2 方案 A：列表分组合并（新旧会话同一 Workspace）与 `mcp_oauth_credentials` 沿用（新对象继承该路径既有凭证）——两项都需用户明确接受。
- 登记表中无法收敛到任何 Workspace 的历史行按 D4 处置。

## 2. 设计裁决（含实施结论）

### D1 本机「最近观测」基线落在哪里

问题：`legacy_execution_registrations.discovery` 是「本机对某个目录对象的最近一次观测」，被三处使用：解析期比对（`sqlite_store/workspace.rs:78-100`）、新会话归属推导（`workspace_identity.rs:71-87`）、绑定值复核基线（`workspace.rs:227-238`）。

- **选项 A（推荐）**：`workspaces` 增列 `identity TEXT`、`discovery TEXT`（允许 NULL，语义＝该 Machine/path 最近一次本机观测；远端虚拟环境可写虚拟快照）。缺失时按 `NeedsRelink`/只读历史处理，不伪造。
- 选项 B：不保留基线，只用绑定自身快照。**不可选**——`validate_binding_value_impl`（`workspace.rs:455-469`）的入参 `SessionBinding`（`peri-acp-types/src/workspace.rs:83-90`）不含快照字段，远程组合下本机没有可复核基线，会把复核退化为放行，与 `storage-v2` 第 145–147 行「不得补造执行资格」冲突。
- 选项 C：新建 per-object 证据表。与本次目标（收敛到 workspaces）冲突。

补充（无论选 A 与否都需处理的实现点）：`gate.rs:315-336` 显示远端组合已有 `validate_saved_binding(binding, &saved, owner, full)` 传入快照，而本机组合走 `validate_binding_value(binding, full)`；本机路径应改为读取本机绑定行自己的 `discovery_snapshot`（数据面已有 `binding_discovery_snapshot(id)`，见 `gate.rs:315`），不再从登记表取基线。

### D2 Workspace 身份是否包含目录对象证据（本次的核心契约选择）

问题：现行设计 `docs/design/session-id-environment.md` §3.2（第 105–132 行）规定身份键是 `(canonical locator, 文件对象证据)`，并明确「登记表允许同一路径有多个文件对象、同一对象出现在多个路径，各自得到新的 `ProjectId` 与 `WorkspaceId`」（第 116–118 行）、「工作区身份取 canonical root 路径加上该目录自身的文件对象证据」（第 124 行）。已批准目标设计 `docs/design/storage-v2-machine-workspace-session.md` §3（第 115–149 行）规定 `(machine_id, path)` 决定 Workspace 身份、`root`/`root_identity` 不再参与。两者不可同时成立，必须选一个。

- **方案 A：`(machine_id, path)` 唯一（与 storage-v2 目标态一致）**
  - 语义：`identity` 只是该行的「当前占用对象证据」，同路径换成新对象＝同一 Workspace 更新证据。
  - 需要修订：`session-id-environment.md` §3.2 第 116–128 行、§3.3 情景表（`删除后同路径重新创建`、`已登记目录随后出现或移除 .git` 等行）。
  - **后果 1（用户可见）**：删除后同路径重建的新会话与旧会话落在**同一个 WorkspaceId**，列表分组、`ThreadScope::Workspace(id)` 的成员集合随之改变。
  - **后果 2（安全相关）**：`mcp_oauth_credentials` 以 `workspace_id` 为作用域（`canonical.rs:179-187`）。同一路径重建后新对象沿用它之前的 MCP OAuth 凭证，不再要求重新授权；现行设计下新对象得到新 `WorkspaceId`，因此不会继承凭证。
- **方案 B：`(machine_id, path, identity)`（保留现行设计语义）**
  - 语义：登记概念并入 `workspaces`，同一路径可以有多个对象行；删除重建＝新行、新 WorkspaceId。
  - 需要修订：`storage-v2-machine-workspace-session.md` §3 关于「`root_identity` 不再决定 Workspace 身份」的表述，并定义 `workspace_for_path` 的消歧规则（`workspace.rs:146/432`、`remote/execution.rs:95/134/200/236` 现假定 `(machine, path)` 唯一）。
  - 需解决：远端 Virtual 环境没有本机文件证据（`remote/execution.rs:194-225`），必须给出稳定合成证据（例如按机器/路径派生），否则同路径多行无法消歧。
  - 代价：这不是「删表」而是「把登记表并入 workspaces 的键」，`workspaces` 不再是 storage-v2 描述的那张表。
- **方案 C：不删表，只改名（`workspace_registrations`）并明确它是现行设计的一部分**。代价：不解决双身份空间与两份设计并存，仅消除命名误导。

推荐：**A**（与用户「使用新的 workspaces 机制」的指示及 storage-v2 目标态一致），但 A 的两项后果必须由用户明确接受；若不接受凭证沿用与分组合并，则取 **B**。方案 C 只在放弃本次目标时成立。无论选哪个，本次都必须同步两份设计文档（§5 文档行）。

### D3 是否改写既有绑定字节

- **选项 A（推荐）**：不改写 `session_bindings` 的 `workspace_id`/`discovery_snapshot`/`evidence_origin`，只删除外键；读取侧把归属权威切到 `threads.workspace_id`（设计第 137–139 行）。旧 `workspace_id` 无法关联到任何 Workspace 时按现有 `normalize_binding_lookup`（`workspace.rs:643-650`）退化为 `InvalidBinding` → 只读历史。
- 选项 B：一次性把 `workspace_id` 重写为 `workspaces.id`。代价：改写不可变绑定字节；收益仅在旧库存在 `registration.id ≠ workspaces.id` 的行（样本库 0 条）。若迁移期实测发现此类行，作为**显式批准的例外**单独处理，不默认开启。

### D4 无法收敛的历史登记行（样本 7 行）

- **选项 A（推荐）**：一律并入 `workspaces`——有引用会话的取该会话所属 machine；无引用者取当前 machine 与 `path_source='unverified'`。保留「该 path 已有登记基线」的 fail-closed 语义。
- 选项 B：直接丢弃无引用登记。代价：这些 path 的基线消失，将来同 path 出现「Git 仓库被替换为普通目录」时不再 fail-closed。
- 推荐 A；若选 B 必须在 issue 与设计文档中记录行为变化。

### D5 远端是否同批推进

- **推荐**：同批。`REMOTE_SCHEMA_VERSION` 与本机 `CURRENT_SCHEMA_VERSION` 共用常量（`remote/schema.rs:34`），`STORE_CONTRACT` 从 `peri.session.store/v3` 推进到 v4；新增 `remote/schema_v19_upgrade.rs`，沿用 v12/v14 批次的写法（`StatementSpec` 列表 + store identity/version guard，失败结果未知时重读身份与形状）。
- 需在实施中确认：远端旧库通常没有登记行（设计第 120 行），对空表迁移应为 no-op，但仍要保证形状清单与绑定可读。

### 裁决结果（2026-10-08 实施）

| 编号 | 裁决 | 实施差异与理由 |
| --- | --- | --- |
| D1 | 选项 A + 补充实现点 | 按推荐：`workspaces` 增列 `project_id`/`identity`/`discovery`，本机复核基线改读绑定行自身 `discovery_snapshot`（`validate_resolved_on`） |
| D2 | **方案 A** | `(machine_id, path)` 是唯一归属键；`project_id`/`identity`/`discovery` 仅是该行当前证据。接受推荐列出的两项后果，并同步两份设计文档 |
| D3 | **偏离推荐，取改写** | 删表后绑定 `workspace_id` 再无登记 id 空间可解释，保留旧值会得到指向不存在 id 的悬空归属；迁移逐行改写为 `threads.workspace_id`，并在改写前加两道 fail-closed 计数校验（绑定落不到归属行、绑定记录的根≠归属行路径 → 拒绝升级）。与选项 A 相比多出一次绑定表重建，属计划内的「显式批准例外」 |
| D4 | 选项 A | 无归属的登记并入 `workspaces`（id 空闲时沿用旧登记 UUID；机器取引用会话的机器，否则取库中首台机器）；同路径多条只并 `id` 最小一条以避开 `UNIQUE(machine_id, path)`；id 已被别的路径占用则跳过该行 |
| D5 | 同批推进 | `STORE_CONTRACT` → `peri.session.store/v4`，可读区间保留 v3 的 12..=18 与 v2 的 10/11；新增 `remote/schema_v19_upgrade.rs`；分派 18+v3→v19、12..=17+v3→v14→v19、11+v2→v12→v14→v19、10→…→v19 |

## 3. 目标结构与数据流

DDL（最终以 `canonical.rs` 的 `CREATE_V2_*` 与 v19 常量为准，两端同一份）。下面是 **D2 方案 A** 的形态；若裁决为方案 B，把 `workspaces` 的唯一键改为 `UNIQUE(machine_id, path, identity)` 并补 `identity` 的消歧查询，其余部分不变：

```sql
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    project_id TEXT REFERENCES projects(id),  -- 新增：该路径当前项目（原登记的 project_id）
    identity TEXT,          -- 新增：最近观测的目录对象证据（原 root_identity）
    discovery TEXT,         -- 新增：最近一次观测快照（原 discovery）
    UNIQUE(machine_id, path)   -- 方案 B 改为 (machine_id, path, identity)
);

CREATE TABLE session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,     -- v19 迁移改写为 threads.workspace_id（D3）
    relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT,
    evidence_origin TEXT NOT NULL
);   -- 不再声明指向 legacy_execution_registrations 的外键
```

数据流（目标）：

1. **解析**：`resolve_workspace` 观测 → 按裁决的身份键 upsert `workspaces`（写 `identity`/`discovery`）→ project 归属仍按 `projects(locator, object_identity)` → 返回 `ResolvedWorkspace{ workspace_id, root, relative_cwd, discovery_snapshot }`。
2. **创建**：写 `threads.workspace_id = workspace_id`，写 `session_bindings`（`workspace_id = workspace_id`，`discovery_snapshot = 本次观测`，`evidence_origin = 'creation_snapshot'`）。
3. **复核**：归属取 `threads.workspace_id` → 取 `workspaces` 该行得 root → 与绑定自身的 `discovery_snapshot` 做完整复核（`Discovery::revalidate` / `reassert_key_objects`）。
4. **列表**：`list_scoped_threads` 由 `threads.workspace_id` 直接 join `workspaces`（去掉经登记表取 `root` 的 join，`workspace.rs:484-486`）。

## 4. 迁移（本机与远端）

**本机 v18 → v19**（单次写打开，`BEGIN IMMEDIATE` 内，失败整体回滚且不推进版本）

1. 预检：确认 `legacy_execution_registrations`、`workspaces`、`session_bindings` 形状符合 v18 预期；统计行数与引用完整性，作为验收基线。
2. `ALTER TABLE workspaces ADD COLUMN identity TEXT` / `ADD COLUMN discovery TEXT`。
3. 回填：按 `path` 匹配登记行 → 更新 `project_id`/`identity`/`discovery`（已有值时 `COALESCE` 不覆盖）；无匹配者按 D4 规则插入新行；同 path 多对象 —— 方案 A 收敛为一行（取 `id` 最小的一条），方案 B 各保留为独立行并确保 `WorkspaceId` 不冲突。
4. 前置校验：`COUNT(*)` 统计「绑定落不到会话归属行」与「绑定记录的根≠归属行路径」，任一非零即 `bail`，库留在 18。
5. 重建 `session_bindings` 去掉外键并把 `workspace_id` 收敛到 `threads.workspace_id`（建新表 → `INSERT INTO ... SELECT ... JOIN threads` → `DROP` → `RENAME` → 重建索引）。复用 `sqlite_store/schema.rs` 已验证的「`PRAGMA foreign_keys=OFF` + 提交前 `foreign_key_check` + 恢复连接设置」模式。
6. `DROP TABLE legacy_execution_registrations`。
7. `PRAGMA user_version = 19`。
8. 提交前校验：`PRAGMA foreign_key_check` 无结果；行数守恒断言。

**实施点**：`sqlite_store/schema.rs` 的 `SchemaState` 分派新增 18 分支（现 `:53` 的 `CURRENT_SCHEMA_VERSION` 分支与 `:225` 的 `user_version = 18` 均需改），迁移体建议落在新的 `sessions/storage_v19_migration.rs`（同 `storage_v2_migration.rs` 的组织方式），删表清单若并入 `schema_cleanup.rs` 需保持其「显式删除语句」约定。

**既有 11→12 路径不动**：`storage_v2_migration.rs:48` 仍会把 pre-v12 库的旧 `workspaces` 改名为登记表，再由 v19 步骤删除。理由是让已发布的旧库升级链保持原样、把回归面压到最小；净效果是在同一次打开内建后即删。若实施时发现该链在某形状上不可收敛，再单独裁决「11 直接直升 19」。

**只读打开**：不迁移（既有语义），v19 库的只读读取走新查询投影；v18 库的只读读取路径在本次改动后必须仍可用（`read_only` 分支不得引用已删除的表）。

## 5. 代码 write set

| 面 | 文件 | 内容 |
| --- | --- | --- |
| 领域类型 | `peri-acp-types/src/workspace.rs` | `ResolvedWorkspace.execution_registration_id` 去除或改名并明确语义；`SessionBinding::from_workspace` 取 `workspace_id` |
| 本机存储 | `sqlite_store/workspace.rs`（11 处）、`workspace_identity.rs`、`session_data.rs:278`、`session_rows.rs:85`、`local.rs:122` | 解析/复核/列表/归属推导改为只读 `workspaces`；基线改读绑定自身快照（D1 补充项） |
| schema 与迁移 | `canonical.rs`（DDL、`CREATE_V2_TABLES`、`CURRENT_SCHEMA_VERSION=19`、FK 移除）、新增 `storage_v19_migration.rs`、`sqlite_store/schema.rs` | 见 §4 |
| 远端 | `remote/session_write.rs:35-71`（登记语句改为纯 workspaces upsert）、新增 `remote/schema_v19_upgrade.rs`、`remote/schema.rs`（`STORE_CONTRACT` → v4） | 与 §3 目标结构对齐 |
| 测试 | 见 §6 | |
| 文档与契约 | 按 D2 裁决修订 `docs/design/session-id-environment.md`（§3.2 登记键与多登记表述、§3.3 情景表；方案 A 下必改）或 `docs/design/storage-v2-machine-workspace-session.md`（§3；方案 B 下必改）；`storage-v2` 的 `:306` 行两方案下都要修；`docs/design/README.md` 仅在该文档状态变化时更新（`维护要求` 第 2 条）；`docs/standards/architecture-contracts.md:22`、`docs/code-index/peri-resources.md` | 按 `DOC-UPDATE-001` 核对路由；同一变更内文档与实现保持一致（`STD-INDEX-002`） |

write set 之间共享编译错误由协调者整合，不保留被删除字段的 mock 或 fallback。

## 6. 验证与验收

**迁移夹具矩阵**（新增测试，覆盖 v18 旧库形状）

| 形状 | 断言 |
| --- | --- |
| `registration.id == workspaces.id`（样本 27 行形态） | 升级后绑定可复核；workspaces 保留原 ID 与证据 |
| `registration.id ≠ workspaces.id` | 按 D3 裁决结果断言（默认不改写字节，退化为只读历史） |
| 有登记、无 `workspaces` 行（样本 7 行形态） | 按 D4 裁决结果断言 |
| 同 path 多目录对象登记 | 方案 A：收敛为一行且取最近观测；方案 B：保留多行且 `WorkspaceId` 各自独立 |
| 绑定 `discovery_snapshot` 为 NULL | 只读历史，不复制登记值为「创建时证据」 |
| 无 `session_bindings` 行 / 无 `threads` 行 | 表删除后无悬空引用 |
| 迁移失败注入（中途报错） | 回滚后仍为 `user_version = 18`，表与行数不变 |

**按 D2 裁决追加的场景断言**

| 场景 | 方案 A | 方案 B |
| --- | --- | --- |
| 删除后同路径重建（§3.3 情景表） | 新会话与旧会话同一 `WorkspaceId`；旧会话仍按自身快照失败 | 新对象得到新 `WorkspaceId`；新旧会话分组不同 |
| 该路径已有 MCP OAuth 凭证时重建目录 | 凭证沿用（需断言这是预期行为） | 凭证不继承，要求重新授权 |
| `ThreadScope::Workspace(id)` 列表 | 新旧会话同组 | 分组隔离 |
| 远端 Virtual 环境的同路径解析 | 单行，无歧义 | 需要合成证据消歧，须显式断言 |

**行为回归**（优先复用既有场景，先保留失败证据再改断言）

- Rust：`schema_test.rs`、`schema_v10_test.rs`、`schema_registration_test.rs`、`connection_open_test.rs`、`session_shape_test.rs`、`resources_composition_test.rs`、`storage_v2_migration_test.rs`。
- e2e：`workspace-directory-moved`、`workspace-git-init`、`workspace-no-git`、`workspace-slow-git`、`workspace-worktree-layouts`（现直接查登记表，改为查 `workspaces` 并保持原行为断言）。
- 只读打开、跨 compose（本机/远端）、`read_only` 解析失败语义。

**文档验收**：§5 文档行同步完成；`bash scripts/check-file-size.sh` 对变更文件不新增超限。

## 7. 风险与处理

| 编号 | 风险 | 处理 |
| --- | --- | --- |
| R1 | DROP 不可逆：升级后旧版本二进制打不开该库（既有语义：旧 writer 对未支持版本报 `UnsupportedSchemaVersion`） | 迁移前备份（`VACUUM INTO` 或文件拷贝）；发布说明写明单向升级；不承诺降级 |
| R2 | 11→12 路径与 v19 叠加后的形状收敛 | §4 的夹具矩阵显式覆盖 11 → 19 全链；不可收敛时单独裁决直升方案 |
| R3 | D3 改变归属来源后，远程组合的本机复核基线缺失 | 按 D1 补充项改端口/实现，读本机绑定行快照；缺快照时只读历史 |
| R4 | 大库迁移时长与 WAL 增长（样本库 11 GB，但本次只重建 `session_bindings` 与 `workspaces`，不触碰 `messages`） | 在真实大小副本上测一次耗时、WAL 增长与锁窗口，结果记入 §9 |
| R5 | 只读节点/只读打开引用已删表 | §6 显式验证只读路径；代码审查检查 `read_only` 分支 |
| R6 | 文档漂移未同步导致后人重复决策 | §5 文档行 + issue 的 superseded 注记 |
| R7 | 两份设计文档对 Workspace 身份的定义冲突（`session-id-environment.md` §3.2 含对象证据 vs `storage-v2` §3 不含）未在开工前裁决 | D2 必须在实施前裁决；同一变更内同步两份文档，文档未同步不算完成（`STD-INDEX-002`） |
| R8 | MCP OAuth 凭证作用域随 D2 变化（方案 A 下同路径重建目录继承既有凭证） | 按裁决写入测试断言与发布说明；若安全上不可接受则取方案 B |

## 8. 完成定义与提交约定

完成＝§6 全部通过且 §5 文档同步；未获批准前不实施任何 DDL 或代码改动。提交按用户指示执行，不推送；真实用户库不作为迁移试验场。

## 9. 进度与证据

### 9.1 实施产物（worktree 分支，未 push）

| 面 | 文件 | 内容 |
| --- | --- | --- |
| canonical | `peri-resources/src/sessions/canonical.rs` | `CURRENT_SCHEMA_VERSION = 19`；`CREATE_V2_PROJECTS_TABLE_SQL`；workspaces 证据三列；v19 九条常量（补列 / 回填 / 并入 / 两道计数校验 / 重建绑定 6 条 / 删表） |
| 本机迁移 | `sessions/storage_v19_migration.rs`（新，151 行）+ `storage_v19_migration_test.rs`（新，9 测试） | `PRAGMA foreign_keys=OFF` → 事务内补列、回填、校验、重建绑定、删表、`foreign_key_check`、`user_version = 19`；失败不推进版本 |
| 本机分派 | `sqlite_store/schema.rs` | `SchemaState::Version18`；18→v19；12..=17 先 owner removal 再 v19；11 先 v2 迁移再 v19 |
| 本机语义 | `sqlite_store/{workspace,workspace_identity,session_rows,session_data,local}.rs` | 归属键 `(machine_id, path)`；证据列读写；绑定复核基线取绑定自身快照；绑定与归属行一致性 fail-closed |
| 远端 | `remote/schema.rs`（`STORE_CONTRACT = peri.session.store/v4`、可读区间）、`remote/schema_v19_upgrade.rs`（新，181 行 + 5 测试）、`remote/schema_upgrade.rs`（分派）、`remote/schema_v14_upgrade.rs`（改判 v3）、`remote/execution.rs`（绑定归属一致性校验） |
| 类型层 | `peri-acp-types/src/workspace.rs` | 删除 `ResolvedWorkspace.execution_registration_id`；`SessionBinding::from_workspace` 取 `workspace_id` |
| 夹具改写 | `peri-agent`（`test_resources*`、`compact_*_adversarial_test`）、`peri-middlewares`（`at_mention/work_fixture.rs`、`subagent/tool/tool_test.rs`）、`peri-tui`（3 个 JSON 夹具） | 旧双 ID 字段名清除 |
| e2e | `workspace-{directory-moved,git-init,no-git,slow-git,worktree-layouts}.test.ts` | 查询改为 `workspaces` 归属行；`node --experimental-strip-types --check` 通过；实跑见 §9.5 |

### 9.2 回归结果

| 范围 | 命令 | 结果 |
| --- | --- | --- |
| peri-resources | `test --locked -p peri-resources --lib` | 447 passed / 0 failed / 21 ignored |
| 远端全组 | `test --locked -p peri-resources --lib -- sessions::remote` | 129 passed / 0 failed / 21 ignored |
| 全仓编译 | `check --locked --workspace --all-targets` | 通过 |
| peri-acp | `test --locked -p peri-acp --lib` | **基线红 1 项**：`host::mcp_v4_builtin::closed_web_tool_call_never_reaches_approval_or_wire`；主树提交 `0c514539`（只改 `host/mcp_v4_builtin_test.rs`，+34/−12）已修正该期望，分支基线 `29442517` 落后该提交，且分支未改 `peri-acp/` |
| peri-agent | `test --locked -p peri-agent --test compact_session_adversarial_test` | **基线红 2 项**：错误脱敏断言失败（生产消息已带 provider 上下文与 request id，测试仍断言旧文案）。断言两端所在的 `peri-acp-types/src/{error,event}.rs`、测试文件在分支与主树**逐字节相同**（`diff -q` 无输出），属基线既存 |
| peri-middlewares | `test --locked -p peri-middlewares --lib` | **基线红 10 项**：`assembly::tests::workflow::mcp_owner` 5（断言会话未注册）、`mcp::workspace_recovery_tests` 2、`mcp::workspace_builtin_tests::bridge_accepts_workspace_owned_task_handle` 1、`subagent::tool::tests::{resume_test,resume_failure_test}` 2；归因见 §9.7 |
| peri-middlewares 集成 | `-p peri-middlewares --test mcp_isolation_contract` | **基线红 2 项**：`disabling_one_instance_leaves_the_other_transport_intact`、`each_instance_wire_carries_only_its_own_requests`（夹具 `invoke_named` 未传会话身份，桥以 "MCP tool call requires a trusted session binding" 拒绝） |
| peri-tui | `test --locked -p peri-tui --lib` | **1 项负载相关 flake**：`kit::markdown::tests::test_unclosed_fence_stays_mutable_until_closed`（指针身份断言）。隔离复跑分支/主树各 3 次全过；`kit::markdown` 16 线程并行 2 轮 133/133 过；该测试与主树逐字节相同 |

### 9.3 真实库只读核对（`~/.peri/threads/threads.db`，schema 18，未写入）

- 11.8 GB；`legacy_execution_registrations` 34 行 / 34 个不同 `root` / `root_identity` 无重复；
- 27 行与 `workspaces.path` 匹配且 `id` 完全相同，7 行无对应归属行（走 D4 并入）；
- `session_bindings` 1380 行全部可归属（守卫计数 N = 0）；绑定记录的根与归属行路径一致（M = 0）；悬空外键 0；`threads` 12353 行全部有 `workspace_id`；`mcp_oauth_credentials` 0 行。

结论：真实库形状在守卫可判范围内，迁移不会因校验拒绝。

### 9.4 真实大小副本迁移演练（R4 闭环）

- 快照：`VACUUM INTO` 从 `~/.peri/threads/threads.db`（11.84 GB，`user_version=18`）取一致快照到 `~/peri-v19-rehearsal/threads.db`（3.44 GB，`integrity_check=ok`；说明源文件约 71% 为空闲页）。**源库只读，未写入**。
- 迁移一次（临时 `#[ignore]` 演练用例调用 `SqliteThreadStore::new`，跑完已删除、未提交）：**6.38 s**；该段是单个 `BEGIN IMMEDIATE` 写事务（`PRAGMA foreign_keys=OFF` 在事务外），即真实库上的写锁窗口约 6.4 s；WAL 模式下读者不被阻塞。
- 迁移后校验：`user_version=19`；登记表 0 行；`threads=12359`、`session_bindings=1386`、`projects=32`、`workspaces 106 → 113`（+7 = §D4 并入的无归属登记行）；守卫复核：无主绑定 0、外部根绑定 0、`pragma_foreign_key_check` 0 行。
- 空间：迁移期间 WAL 峰值约 1.1 MB；连接关闭后主库 +0.83 MB、WAL 归零（checkpoint）。二次打开（已是 v19）：1.66 ms，无迁移（幂等）。
- 形状：8 张业务表；`workspaces.identity/discovery/project_id` 可空——34 行有证据（27 行匹配既有登记 + 7 行并入），79 行历史惰性行（`path_source='unverified'`、无任何 `session_bindings` 引用）保持 NULL，待该路径首次解析时按新证据补齐（`workspace.rs` 的 `(machine_id, path)` 命中分支）。
- 与 §9.3 行数差异（12353→12359、1380→1386）：库处于活跃使用中，副本是 08:43 的一致快照。

### 9.5 e2e 实跑（5 个 workspace 用例）

`cd e2e && npm install` 后逐个 `npm run e2e -- --file tests/scenarios/<name>.test.ts --serial --retry 0`：

| 用例 | 结果 |
| --- | --- |
| `workspace-git-init` | 通过（11 s） |
| `workspace-no-git` | 通过（14 s） |
| `workspace-directory-moved` | 通过（9 s） |
| `workspace-worktree-layouts` | 通过（10 s） |
| `workspace-slow-git` | **失败且为基线既存**：`一次准入至多一轮发现（每轮三条命令）：窗口内发现 8 次、调用 9 次`（断言 `discovery.length <= 3`，见 §9.7 对照） |

运行前置：worktree 内子模块 `e2e/tui-tester` 未初始化，用主树同一子模块提交（`1d73f755`）的副本支撑运行，未写主树；`target/debug/peri` 以符号链接指向构建产物（`CARGO_TARGET_DIR` 共享目录），构建命令仍是仓库规定的 `scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri`。

### 9.6 未完成与发布门槛

- `workspace-slow-git` 的「一次准入一轮发现」契约在基线（主树二进制 + 主树用例文件）同样失败，需由归属方另案修复（本变更不摘取，见 §9.7）。
- 未 push，未提交主树改动；真实用户库未写入。

### 9.7 文档同步（STD-INDEX-002）

`docs/design/session-id-environment.md`（§3.2 身份键、§3.3 情景表、§4/§6.3 措辞、§8 版本行）、`docs/design/storage-v2-machine-workspace-session.md`（状态块后续变更注记、§6.2 两行、§6.3 注记、§6.2 版本段）、`docs/standards/architecture-contracts.md`（ARC-WORKSPACE-001、ARC-TURSO-STORAGE-001）、`docs/code-index/peri-resources.md`（速览、schema 行、归属行、契约版本、当前版本号）、`spec/issues/2026-10-07-remove-execution-recovery-schema-review.md`（superseded 注记）。

### 9.8 基线红归因（对照实验，2026-10-08）

分支基线 `29442517` 落后主树 `9aace572` 五个提交（`498fd7d2`、`0c514539`、`809ffe13`、`71f67086`、`9aace572`）加一项未提交改动。§9.2 的基线红项按「同文件字节对比 + 主树已编译二进制复跑」逐项归因：

| 现象 | 证据 | 归因 |
| --- | --- | --- |
| `workflow::mcp_owner` 5 项（`sessions.len() == 0`） | 主树 `target/debug/deps/peri_middlewares-acf2f432b8d40093`（构建 08:28，晚于 `peri-agent/src/agent/workflow/agent.rs` 的 08:21 修改）复跑 **5/5 通过**；该文件是分支与主树唯一差异（+15 行：`pool.bind_agent_session(...)` 在线准入登记） | 主树**未提交**的 workflow agent 修复所致，非本次改动 |
| `workspace_recovery_tests` 2 项、`bridge_accepts_workspace_owned_task_handle` 1 项、`resume_test`/`resume_failure_test` 2 项 | 主树二进制复跑**同样失败**（`workspace_recovery_test.rs:70:5` 断言文本一致）；相关 4 个测试文件与 `mcp/tool_bridge.rs`、`subagent/factory/resume.rs` 在分支与主树 `diff -q` 无输出 | 基线既存红，与本次改动无关 |
| 集成 `mcp_isolation_contract` 2 项 | 主树二进制复跑同样 2 项失败（`invoke_named` 未带会话身份） | 基线既存红 |
| `peri-acp` 1 项 | 主树 `0c514539` 只改该测试文件即修正期望 | 分支基线落后该提交 |
| `peri-tui` 1 项 | 隔离复跑分支/主树各 3 次全过 | 负载相关 flake |
| e2e `workspace-slow-git`（发现 8 次 > 上限 3） | 把 `target/debug/peri` 指向主树二进制、并用主树版本的用例文件（查 `legacy_execution_registrations`）复跑：**同样失败**，窗口内发现 9 次、调用 10 次 | 基线既存红（工作区被重复解析），非本次改动 |

分支侧 `peri-middlewares` 非测试源码零改动（`git status` 只列夹具与 `subagent/tool/tool_test.rs`），故上述失败不进入本次变更的收敛范围；上游修复不在本分支摘取，以保持变更聚焦。

### 9.9 合并回主树与合并后验证（2026-10-08）

分支 `refactor/remove-legacy-registrations-20261008`（`ee312ae9`）以 `git merge --no-commit --no-ff` 合入 `pre-release/main`（`d25f7e15`），自动合并无冲突；唯一两侧共同改动文件 `peri-resources/src/sessions/sqlite_store/schema_v18_test.rs` 的合并结果正确（保留主树 `absolute_test_path` 与分支 v19 用例），暂存集与分支改动集一致（59 文件 / +2251 / −424）。合并提交 `02df0f7c`（双亲 `d25f7e15` + `ee312ae9`），pre-commit 全绿：fmt、全量 `cargo check --locked`、clippy（命中 crate）、typos、layer-imports，合计 169.5 s。**未 push。**

合并后复跑（主树自身 target，`env -u CARGO_TARGET_DIR`）：

| 范围 | 结果 | 与 §9.2 对比 |
| --- | --- | --- |
| `check --locked --workspace --all-targets` | 通过 | 一致 |
| `-p peri-resources --lib` | 447 passed / 0 failed / 21 ignored（首跑 1 项失败，隔离复跑通过） | 一致（首跑为并发 cargo 负载 flake） |
| `-p peri-middlewares --lib --no-fail-fast` | 1582 passed / 9 failed | `workflow::mcp_owner` 5 项在主树 `d25f7e15` 已修复，本次复跑不再失败；hooks 3 项（`test_tolerant_mixed_valid_and_invalid_events`、`test_tolerant_non_array_rules_skipped`、`test_async_hook_receives_correct_event_name`）隔离复跑全过（并发 cargo 负载 flake）；`workspace_recovery_tests` 2、`bridge_accepts_workspace_owned_task_handle` 1、`resume_test::...parent_mismatch` 1、`resume_failure_test` 1 与 §9.2 一致，仍为基线既存红 |

`resume_test::test_resume_thread_id_fork_title_uses_parent_tools_and_200_iterations` 是合并树上新出现的失败项（`tool_test/resume_test.rs:590` 断言 `left: 2, right: 200`）：在**不含本次合并**的 `d25f7e15` 上建临时 detached worktree 复跑**同样失败**，且该用例代码在分支与主树相同 → 由主树侧（`9aace572`/`d25f7e15`）引入或暴露的基线既存红，非本次合并引入，未在本次修复。

收尾：对照用临时 worktree、v19 演练库（§9.4 的 3.2 GB 副本）与草稿备份已清理；已合并的本地分支 ref 保留。
