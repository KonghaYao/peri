# P1：移除 legacy_execution_registrations，工作区身份收敛到 workspaces

状态：**已实施（代码、测试与文档完成；发布验收未做）**。2026-10-08 用户批准在独立 worktree 实施（「完成了就自己开个 workspace 完成」），D2 契约选择与 D1/D3/D4/D5 由实施方裁决，结果见 §7 与 plan §2/§9；表结构变更只在 worktree 分支，未 push，真实用户库未写入。

日期：2026-10-08。
授权：2026-10-08 用户要求「写为 P1 issue，附加 plan，准备移除这张表，使用新的 workspaces 机制」。

关联文档：
- 实施计划（本 issue 的操作面）：[2026-10-08-remove-legacy-execution-registrations-plan.md](2026-10-08-remove-legacy-execution-registrations-plan.md)
- 冲突的两份设计（须择一，见 §1 第 2 条）：`docs/design/session-id-environment.md` §3.2（现行设计，身份含对象证据）与 `docs/design/storage-v2-machine-workspace-session.md` §3（已批准目标设计，身份为 `(machine_id, path)`）
- 结构基线：`2026-10-07-remove-execution-recovery-schema-review.md`（其「保留该表」的结论由本 issue 取代；2026-10-08 已在该文补 superseded 注记，不改写历史记录）

## 1. 问题

`legacy_execution_registrations` 是 schema 12 期间 `ALTER TABLE workspaces RENAME TO legacy_execution_registrations` 的产物（本机 `peri-resources/src/sessions/storage_v2_migration.rs:48`；远端 `peri-resources/src/sessions/remote/schema_v12_upgrade.rs:193`）。改名是为了把 `workspaces` 这个名字让给 v2 的 `(machine_id, path)` 归属表，同时保留旧登记 UUID 与不可变绑定的外键。

今天它是**在用的第二身份空间**，不是遗留空壳：

| 角色 | 位置 |
| --- | --- |
| 本机写 | `peri-resources/src/sessions/sqlite_store/workspace.rs:94`（更新 `discovery`）、`:133`（插入登记） |
| 本机读 | `sqlite_store/workspace.rs:78/192/228/269/336/424/486/589`、`sqlite_store/workspace_identity.rs:73`、`sqlite_store/session_data.rs:278`、`sqlite_store/session_rows.rs:85`、`sqlite_store/local.rs:122`（共 8 个生产文件） |
| 外键 | `peri-resources/src/sessions/canonical.rs:201`：`session_bindings(workspace_id, project_id) → legacy_execution_registrations(id, project_id)`；本机强制（`sqlite_store/connection.rs:146` 设 `foreign_keys=ON`），远端服务端不强制（`canonical.rs:104`） |
| 类型层 | `ResolvedWorkspace` 同时携带 `workspace_id` 与 `execution_registration_id`（`peri-acp-types/src/workspace.rs:110-122`）；`SessionBinding::from_workspace`（同文件 `:97-105`）取后者写入绑定字节 |

由此产生三个问题：

1. **双身份空间并存**。同一工作区可能有两个 ID，归属判定要跨两表拼接，例如 `validate_binding_relation_on`（`workspace.rs:419-448`）先用登记行的 `root` 再去 `workspaces` 查 owner。
2. **两份设计并存且互相冲突，实现同时承载两者**。
   - `docs/design/session-id-environment.md`（README 列为**现行设计**）§3.2 规定登记表就是身份权威：登记键是 `(canonical locator, 文件对象证据)`，「登记表允许同一路径有多个文件对象……各自得到新的 `ProjectId` 与 `WorkspaceId`」（第 116–118 行），第 124 行「工作区身份取 canonical root 路径加上该目录自身的文件对象证据」，第 42 行并明说「`ResolvedWorkspace` 区分归属身份与本机发现登记」。这与 `legacy_execution_registrations` 的 `UNIQUE(root, root_identity)` 和 `ResolvedWorkspace` 的双 ID 完全对应——**双 ID 是现行设计的意图，不是未完成的迁移**。
   - `docs/design/storage-v2-machine-workspace-session.md`（README 列为**已批准目标设计**）§3 第 115–149 行规定相反的目标态：`workspaces` 以 `(machine_id, path)` 为归属键，`root`/`root_identity`/`discovery` **不再决定 Workspace 身份**，绑定需要 Workspace ID 时从 thread 读取。
   - 实现同时满足了两者的一半：`workspaces` 按 `(machine_id, path)` 建，登记表按 `(root, root_identity)` 建。
3. **文档漂移**。`docs/design/storage-v2-machine-workspace-session.md:306` 把它描述为「仅本机迁移辅助；Turso 模式不访问该表或本地库」，但远端建表清单同样包含它（`remote/session_shape_test.rs:180`），且远端迁移在 `schema_v12_upgrade.rs:255` 用它回填 `discovery_snapshot`。

因此本次不是「清理未完成的迁移」，而是一次**已批准设计的契约变更**：按 `STD-INDEX-002`，须先由用户裁决采用哪一份设计，再同步实现、测试与两份设计文档。

## 2. 现状证据（只读核实）

样本：本机 `~/.peri/threads/threads.db`（schema 18，2026-10-08 查询，未写入）。

| 事实 | 值 |
| --- | --- |
| `legacy_execution_registrations` | 34 行；34 个不同 `root`；`root_identity` 无重复 |
| 与 `workspaces.path` 匹配 | 27 行，且 **`id` 与 `workspaces.id` 完全相同**；另 7 行无对应 `workspaces` 行 |
| 被 `session_bindings` 引用 | 27 行；其余 7 行无人引用，且这 7 行即「无对应 workspaces 行」的那 7 行 |
| `session_bindings` | 1380 行；`discovery_snapshot` NULL 0 条；悬空外键 0 条 |
| `evidence_origin` | `creation_snapshot` 259 / `legacy_last_observation` 1121 |
| `threads` / `workspaces` | 12353 / 106；悬挂 `threads.workspace_id` 0；未被引用的 `workspaces` 0 |
| 库活跃度 | `threads.updated_at` 最大 2026-10-07，近 7 天 341 个会话 |

**这是单机单库样本，不能代表其他库**；迁移实现必须按通用形状处理（见 plan §4 夹具矩阵）。样本能说明的是：在这台机器上，绑定证据已经自足，登记行没有承载绑定缺少的信息。

## 3. 为什么现在具备删除条件

1. **绑定自带不可变证据**。每条绑定行都有 `discovery_snapshot`（样本 1380/1380 非 NULL），`validate_session_binding_impl`（`workspace.rs:351-368`）本来就是用它做完整复核；登记表的 `discovery` 只是「本机最近观测」的基线。
2. **归属已有单一权威**。`threads.workspace_id` 在样本中 12353/12353 非空且无悬挂，这正是设计第 137–139 行指定的归属来源。
3. **登记的独占职责已被识别且可转移**。登记表的独占信息只有 `root_identity`（目录对象证据）与 `discovery`（最近观测）两项，均可落到 `workspaces` 的同键行上（plan §2 D1/D2）。

## 4. 目标

1. `workspaces` 成为唯一 Workspace 归属权威，`legacy_execution_registrations` 不再存在（Workspace 身份的具体键取决于 D2 裁决）。
2. 执行证据的单一权威是 `session_bindings.discovery_snapshot`（每条绑定不可变），归属取 `threads.workspace_id`。
3. 删除该表及其外键，schema 18 → 19。
4. 不引入兼容层、双写、影子表；旧库历史会话仍可按 ID 加载与续聊，无法证明原对象连续性的会话保持只读历史。
5. 让两份设计文档与实现收敛到同一条契约：按 D2 裁决修订 `session-id-environment.md` §3.2/§3.3 或 `storage-v2-machine-workspace-session.md` §3，并修掉 `storage-v2:306` 的漂移表述。

## 5. 不在本次范围

- 不动 `messages`、frozen、继承上下文、父子链、归档与列表语义。
- 不动 `projects`（`bindings.project_id`、Project scope 仍依赖）。
- 不动 `mcp_oauth_credentials.workspace_id → workspaces(id)`（本来就是 Workspace 级）。
- 不改 ACP wire 协议与 `session_bindings` 的对外列语义。
- 不恢复任何执行恢复账本（沿用 `2026-10-07-remove-execution-recovery-schema-review.md` 的九表结论，仅移除其中一张的身份歧义）。
- 不改 SDK 仓库。

## 6. 验收

1. **形状**：schema 19 下 `legacy_execution_registrations` 不存在；`session_bindings` 无指向它的外键；`canonical` 建表清单在本机与远端一致（单一来源）。
2. **迁移**：v18 旧库按 plan §4 的夹具矩阵升级后——历史会话按 ID 加载、续聊、列表、归档行为不变；`threads`/`messages`/`session_bindings` 行数与绑定字节守恒（除计划明确声明的重映射）；失败注入后整体回滚、`user_version` 仍为 18。
3. **行为**：同 path 替换目录对象 → 既有会话按其自身快照复核失败（不静默改绑）；Git worktree 换位、无 Git 目录、慢 Git、只读打开、跨 compose（本机/远端）等既有场景语义不变。
4. **文档与契约**：按 D2 裁决同步 `session-id-environment.md` §3.2/§3.3 与 `storage-v2-machine-workspace-session.md` §3/§6.2（含 `:306` 行）；`architecture-contracts.md` 中 `session_bindings.workspace_id` 的表述同步；`docs/code-index/peri-resources.md` 同步；两份设计文档不得再对 Workspace 身份给出不同定义。
5. **不改真实用户库做验证**：迁移验收使用夹具副本；对真实库只做只读核对。

## 7. 决策与结果

| 编号 | 决策 | 结果 |
| --- | --- | --- |
| D1 | 本机最近观测基线落在哪里 | `workspaces` 增列 `project_id`/`identity`/`discovery`；本机复核基线改读绑定行自身 `discovery_snapshot` |
| D2 | **契约选择**：Workspace 身份是否包含目录对象证据 | **方案 A**（`(machine_id, path)`）：同路径重建复用同一归属行与新证据；已接受的两项后果（列表分组、MCP OAuth 凭证沿用）记入设计文档 |
| D3 | 是否改写既有绑定字节 | **改写**（偏离 plan 原推荐）：删表后旧 `workspace_id` 再无登记 id 空间可解释，迁移逐行收敛为 `threads.workspace_id`；改写前两道 fail-closed 计数校验（无归属、异根），任一非零拒绝升级 |
| D4 | 无法收敛的历史登记行（样本 7 行） | 并入 `workspaces`（id 空闲时沿用旧 UUID；机器取引用会话的机器，否则取库中首台）；同路径多条只并 `id` 最小一条；id 被占用则跳过该行 |
| D5 | 远端是否同批推进 schema 19 与 contract 版本 | 同批：`STORE_CONTRACT` → `peri.session.store/v4`，可读区间保留 v3/v2；新增 `remote/schema_v19_upgrade.rs` 与分派 |

## 8. 实施与验收记录

见 [实施计划](2026-10-08-remove-legacy-execution-registrations-plan.md) §9：实施产物、回归结果、真实库只读核对、未完成项（真实大小副本迁移演练、e2e 实跑）与文档同步清单。
