# 下一版本：移除闲置会话缓存、遗留目标与执行状态表，保留配置快照

状态：2026-10-01 schema 11 首次实施已提交 `ac8430e6`；七项失败修复按用户要求于实施删执行表前提交检查点 `f0ff1f0a`。用户批准将 `execution_runs` 删除合并到同一 schema 11，不升 12；本轮实现与隔离自动化验收完成，用户已授权提交。真实远端升级及发布验收仍待完成，不关闭 issue。

跟踪位置：仓库本地 `spec/issues/`。发布目标为下一版本，不固定应用版本号。

## 用户裁决与目标

- `threads.config` 有用，保留列、领域字段及本机/远端读写契约；不能根据当前全 NULL 推断为废列，也不在本 issue 补做配置快照功能。
- 下一版本移除 `threads.cached_context`、`threads.context_cache_epoch` 和遗留 `thread_goals` 表；同步删除它们的生产读写、类型字段与无消费者接口，不保留 deprecated shim、双写或运行时新旧分支。
- 删除范围为两列及两张退役表，不删除 `threads` / `messages` 的行。遗留目标与执行表数据随表删除；不删除 Goal 功能、目标事件或当前内存目标状态机。
- 用户后续已授权实现下一版本；实施与验证限于代码及隔离 fixture，不在用户真实数据库上执行迁移或数据清理。

## 观察、推断与范围

2026-09-30 对本机默认主库的只读快照做全量统计：schema 10，8 张业务表、50 列，12,056 个会话、820,709 条消息，文件 3,485,446,144 bytes。备份库未纳入本次统计，源库未修改。

| 对象 | 已观察到的数据 | 审计时的工作树证据 |
| --- | --- | --- |
| `threads.config` | 12,056 行均为 NULL | adapter、领域字段和 `SessionMetaPatch` 仍保留读写；用户明确确认用途，必须保留 |
| `threads.cached_context` | 589 行有值，合计 227,771,803 bytes（217.22 MiB），全部属于未绑定旧会话 | 上下文从继承区和权威 payload 重建后仍写缓存；旧元数据接口和分析器可读出字段，但未发现上下文恢复命中 |
| `threads.context_cache_epoch` | 261 行非零，最大 102 | compact/projection 会递增，getter 有声明、实现和转发，未找到核心业务消费者；查看器/分析器仍投影 |
| `thread_goals` | 2 行，1 active / 1 complete，最后更新时间落在 2026-06-12 UTC | 生产代码未找到该表 SQL；ACP 当前装配 `InMemoryGoalStore`；既有迁移测试显式保留历史表 |

静态未发现消费者不代表所有历史安装版本或外部脚本都不用；实施前必须核对发布版本及仓库内消费者。本 issue 将“删除目标表”的授权作为既有“保留所有额外业务表”契约的定向例外，不扩展为任意删表。

物理缩容不是验收承诺：217.22 MiB 是缓存文本值负载，不等于必然回收的文件空间。禁止删权威消息或自动 VACUUM；执行表删除是追加明确授权，不扩展为任意删表。

## What to build

一个完整纵向切片：下一版本创建不含两列的新库，并将受支持的旧本机及远端会话库迁移到同一 canonical 形状；升级后历史恢复、父子上下文、compact/rewind、配置快照读写和当前 Goal 功能保持可核对的行为。再次打开不会重新创建或写回这些废弃对象。

### 领域与生产路径

- 从 `ThreadMeta` 移除缓存字段，从 `ThreadStore` 移除缓存正文/epoch 专属接口及转发；删除读后写缓存与清空/递增缓存 SQL，保留 compact/projection 真正的业务状态更新。
- 移除旧缓存写入顺带刷新的 `updated_at`：仅读取历史不修改会话时间或最近列表顺序，真正的消息/元数据变更仍按现有规则更新时间。这是本次删除读后写副作用带来的明确行为变化，不另加 touch 写入来保持旧副作用。
- 本机元数据 SELECT、INSERT、UPDATE，filesystem adapter，远端 SQL 和 codec，创建/发现/恢复路径，read-only shape probe 均不再要求废弃字段。
- 保留 `config` 的字段、序列化及 `SessionMetaPatch.config` 定向写入语义；以非 NULL 数据验收，不能只验证空库。
- `messages.content`、`projection`、`truncated`、`excluded`，`frozen_context`、`inherited_context`、`snapshot_at_message_id` 与 binding/env 均不属于删除范围。删除 `execution_runs` 的 generation/clean 持久化，不迁入新表或列；保留实例内活跃句柄、未知效果门禁、取消与排空/关闭。
- 查看器、缺陷分析器与导入脚本同步移除废弃列的依赖，保留配置快照、消息角色及现有正常展示/分析行为。旧 JSON/对外协议的兼容义务单独评估，不为内部删除再造兼容层。

### 一次性 schema 升级

- 本机/远端共用版本常量已从 10 推进到 11；不能因为同样能开库就继续冒用 10。应用发布版本号不由本 issue 固定。
- 新 canonical DDL 不含退役两列、目标和执行表；旧库迁移删除两列及有明确归属的 `thread_goals` / `execution_runs`，兼容已带旧执行表的开发版 11，版本仍为 11。删除目标缺失时幂等；无关扩展对象/数据保留，未知 DDL/约束或外部依赖 fail-closed。
- 删除 `thread_goals` 前核对对象类型、已知历史列/约束布局和归属；不能只凭同名或少数列存在就认领，未知额外布局拒绝升级。额外表入向外键或未知 view/trigger 若依赖待删对象，拒绝本次迁移并保持原库/版本不变，不级联删数据、不关闭外键强行删除、不自动改写扩展对象；执行器无法可靠检查这些依赖时，不宣称可以安全自动删除。
- 本机迁移必须覆盖现有 legacy / 受支持旧版本到新版本的入口，不能只处理 schema 10；迁移失败不发布新版本标记，不留下半删状态。保持既有支持版本边界，不额外承诺未知更老版本。
- 远端已补齐 canonical v2 / schema 10 → 11 的写打开升级；只读接受该旧形状但不写入，其他旧 contract/版本仍拒绝。保持 `store_id`、幂等账本及现有执行器契约；不将本地执行器等价测试视为真实远端发布验收。
- 根据本机及远端执行器实际 DDL/事务能力选择迁移方式；如重建表，保留全部非目标字段、外键和有效索引，不重写 `messages` 或改变其 rowid 历史顺序。
- 一次性版本升级可以理解旧对象；生产 schema 和业务查询只维护新形状，不新增兼容表、缓存字段回填或双写。
- 按现有只读降级契约验证旧库/新库读取和错误分类；只读打开不执行清理、建表或迁移，不因残留 required-column probe 拒绝正确的新库。
- 升级前核对所有写入实例已停止或完成版本切换，不能仅依赖旧进程“应该拒绝新版本”；明确当前库升级后旧版本不能继续使用，回退依赖迁移前一致性备份，不支持逆向补列/双写。
- 本轮复用尚未发布的 schema 11：上一个开发版 11 的 writer 不会被同版本标记阻断，必须显式停止，不能并行写入。对已有开发库的退役执行表清理仍在同一受控事务内完成。
- 备份与发布流程须覆盖 SQLite WAL 一致性；本轮 clone 的前提是无 WAL 且源文件稳定，不能把该方法直接用于活跃写库。远端使用其存储服务的一致性快照/备份能力，不宣称仅凭复制主文件即可安全恢复。

## Acceptance criteria

以下已勾选项表示隔离自动化及代码验收，不替代真实远端发布门禁。

- [x] 新建本机/远端库共享新的 canonical 形状，两列及目标表不存在；`threads.config` 存在且可写可读。
- [x] 含非 NULL `config`、非空旧缓存、非零 epoch、历史 goals 的受支持旧库升级成功，目标对象删除，其余事实及扩展数据逐项保持；重复打开幂等。
- [x] legacy / 支持旧版本迁移覆盖，失败/中断/重复尝试不会留下部分删除与错误版本标记；未知更高版本及不支持形状拒绝写入。
- [x] 同名异形 `thread_goals`、额外表引用目标表及依赖目标列的 view/trigger 均有拒绝/回滚回归；扩展行、对象定义和旧版本标记逐项不变，不能把关闭外键后的 DROP 成功当验收。
- [ ] 本机与远端升级都保留 store 身份、历史消息 ID/内容/rowid 顺序、projection flags 与 binding/env；执行代际按追加授权删除。远端测试不是仅验证 SQL 字符串，真实服务发布验收仍需完成。
- [x] 只读打开和写失败后的降级读取仍可恢复历史，不创建目标对象；新库不再被旧 required-column probe 判为不兼容。
- [ ] 用户可观察路径：按 ID 恢复、自有/继承上下文、compact/rewind、父子会话、配置快照 round-trip 和 Goal 创建/更新/状态事件不因删除退化。
- [x] 读取历史不写 `updated_at`，重复读取不改变按更新时间排序的会话列表；真实历史/元数据 mutation 仍正常更新时间，不能一并删掉业务更新。
- [x] 查看器/分析器/导入工具能处理发布后的 schema；所有核心 crate 生产代码不再有废弃字段访问或 SQL。允许迁移、旧形状测试 fixture 和本 issue 保留名称，不以全仓零命中替代行为验收。
- [x] 原有“保留 thread_goals”的测试改为新的定向删除契约，并另保留无关扩展表不丢失回归；不得简单删除迁移测试来获得绿灯。
- [x] 同步受影响的 canonical 注释、现行设计与 code-index；明确旧版本共库/回退限制，记载验证结果与未验证项，不宣称预估缩容为实测。
- [x] 实施验收只在隔离 fixture / 数据库副本或明确授权环境进行，不将用户真实主库作为测试库。

## 验证计划

先补能揭示旧字段依赖/迁移回滚/配置快照丢失的回归，随后按 `docs/standards/testing.md` 扩大范围：

1. `peri-resources` 定向 legacy/schema、schema v10、read-only、context/compact、session_id_environment 和 remote SQL/事务/重试 seam 测试；每个场景精确断言保留数据与版本，不仅 `is_ok()`。
2. `peri-acp-types` 类型/serde/契约及 `peri-acp` 恢复、Goal 生命周期测试；按修改范围验证 `peri-agent` 及调用方编译/回归。
3. 查看器和缺陷分析器按各自本地 manifest/测试命令验收，不纳入根 workspace 的替代门禁；导入脚本使用隔离数据库验证。
4. 若修改 doc comment，运行受影响 crate doc tests；最终检查格式、定向 clippy、Markdown 本地链接和 `git diff --check`。真实远端升级未验证时保留为发布阻塞项，不将 mock/字符串测试等同于实库通过。

## 实施入口与关联契约

入口只用于导航，不替代领域验收条件：

- `peri-resources/src/sessions/canonical.rs`，`sqlite_store/{schema,connection,context,row_mapping,session_data,session_data_helpers,compaction,workspace}.rs`，`sqlite_store.rs` 与 `filesystem.rs`。
- `peri-resources/src/sessions/remote/{schema,session_schema,session_sql,session_codec,session_history,session_data}.rs`。
- `peri-acp-types/src/{thread/types,store/mod,session_resources}.rs`，`peri-acp/src/session/construction.rs` 与 `goal_state/`。
- `side-projects/peri-db-viewer/`、`side-projects/agent-defect-analyzer/`、`scripts/migrate-opencode-to-peri.ts`。
- [现行资源入口](../../docs/code-index/peri-resources.md)、[会话身份与工作区设计](../../docs/design/session-id-environment.md)、[元数据控制设计](../../docs/design/meta-control.md)、[架构契约](../../docs/standards/architecture-contracts.md)。

## Blocked by

代码已进入 schema 11。发布仍须在隔离的真实远端服务验证 DDL/托管事务能力、失败边界及完整恢复，兑现一致性备份与旧 writer 停止策略。七项在未修改基线复现的失败已按用户追加授权处理，结果见文末；资源库通过不等同于真实远端或全仓测试全部验收。环境归属及 OAuth 凭证接线的已批准范围保持不变。

## Luna 对抗审查

2026-10-01，`gpt-6-luna` 已完成独立只读代码对抗审查；没有运行测试或访问真实数据库。原判定为“须补充后可实施”，提出两项具体补强，本 issue 已纳入约束与验收：

| 发现 | 证据 | 处理 |
| --- | --- | --- |
| P1：目标表同名不能证明归属，入向外键可能造成扩展数据丢失/引用损坏 | `sqlite_store/schema.rs` 的 `drop_remote_local_state` / `require_columns` 是现有迁移形状保护入口 | 加入已知布局/对象归属校验，以及额外外键/view/trigger 依赖时拒绝迁移并保持数据/版本的回归；不以关闭外键规避 |
| P2：删除缓存写入将同时移除读取历史刷新最近排序的副作用 | `sqlite_store/context.rs` 的 `save_context_cache` 更新 `updated_at`，`workspace.rs` 的 scoped 列表依赖该时间排序 | 明确读历史不 touch 时间/排序；要求真正 mutation 保持更新时间，增加区分读写的行为回归 |

Luna 同时确认现稿已覆盖非 NULL 配置保留、只读 probe、投影/继承恢复、远端旧版拒绝、发布回退和消费者清理。修订后已复审，结论为“可进入下一版实施；发布条件仍需验收”，原两项文档缺口已闭环。以上为实施前审查结论；后续实现及运行验证见下一节，真实远端升级与发布策略不因对抗审查结束而免除。

## schema 11 实施与验证记录

### 已实现

- 本机受支持版本（legacy / 2..10）沿既有升级链，在同一 `BEGIN IMMEDIATE` 事务删除目标并提交 `user_version=11`；新库不建缓存列或目标表，不重写 `messages`。
- 共用删除规划核验历史目标表完整已知 DDL、缓存列类型/约束及外部依赖；拒绝单引号/大小写表名、`SELECT *` 视图、`threads` 自身指向目标表的扩展外键及相似 `sqlitex_*` 前缀扩展，不遗漏可能被级联删掉的行。
- 远端 canonical v2/schema 10 升级保留身份与账本，guard 形状及旧版本后托管批提交；旧初始化仅写下身份的中断窗口可在同一批补齐 canonical schema。缺失权威历史列拒绝升级，丢失或不完整回复报告持久化未决。
- 删除缓存领域字段/API、生产 SQL、读后写缓存和消费者废列投影；保留非 NULL 配置 round-trip、真实 mutation 更新时间。查看器保持原有轻量 metadata 投影，不新增配置内容对外暴露。
- 原迁移测试改为定向删除并保留扩展数据；按 `STD-SIZE-001` 拆分原有过大测试文件，没有删掉现有恢复测试来掩盖失败。

### 自动化证据

| 范围 / 命令 | 结果 |
| --- | --- |
| `cargo check --workspace --all-targets` | 通过，所有消费 `ThreadMeta` / `ThreadStore` 的目标编译通过 |
| `cargo test -p peri-resources --lib schema_v11` | 5 通过；覆盖非空数据、配置、rowid/flags、只读、排序、异形目标/依赖、前序 DROP 回滚 |
| `cargo test -p peri-resources --lib schema_upgrade` | 8 通过；生产 RemoteStore/升级计划驱动真实 SQLite 事务执行器，含 binding/env/OAuth/frozen 数据保留、形状快照竞争、回滚/重试与丢回复 |
| `cargo test -p peri-acp-types --lib` | 516 通过，含旧 JSON 忽略废字段并保留配置 |
| `cargo test -p peri-acp --lib goal_state` | 25 通过，当前 Goal 状态机不依赖被删历史表 |
| `cargo test -p peri-agent --lib compact_v2` | 179 通过 |
| `cargo test -p peri-tui --test meta_session_cli` | 19 通过，真实子进程只读 metadata/降级行为 |
| 两个 side project 各自 `bun test` / `bun run typecheck` | 分析器 58 通过，查看器 10 通过，两者类型检查通过；含不带废列的 schema 11 fixture |
| 导入脚本，隔离 OpenCode 源/输出库 smoke | 1 会话 / 3 条 user、assistant、tool 消息；配置保留，不生成废列。输出保持 legacy version 0，由应用补齐完整 schema，未冒用 11 |
| `cargo clippy -p peri-resources -p peri-acp-types --all-targets -- -D warnings` | 通过 |
| `cargo test -p peri-resources -p peri-acp-types -p peri-agent --doc` | 11 通过、2 个既有示例忽略 |

schema 11 初次实施时资源库完整串行回归为 369 通过 / 7 失败 / 21 忽略。七项失败全部在独立 detached HEAD 基线 `fd6464da` 复现；初次提交未改这些独立行为：

- 三项 `legacy_tests`：并发 adoption、cwd/child/native binding 拒绝、child execution owner。
- `test_single_database_upgrade_preserves_history_and_binds_only_new_sessions`。
- `test_adopt_legacy_session_refuses_to_bypass_dirty_execution` 与 `test_load_snapshot_reports_missing_rows_and_unregistered_bindings`。
- `test_worktree_concurrent_registration_reuses_winner`：并发新库连接遇到 SQLite code 5；旧基线重复运行亦出现相同锁失败。

初次实施代码显式跳过上述七项既有失败后，其余资源库串行回归为 369 通过 / 0 失败 / 21 忽略 / 7 过滤；这不是全量绿灯。修改的 Rust 文件格式与 1000 行上限检查、Markdown 本地链接、`git diff --check` 均通过。临时 baseline 工作树已移除。用户随后授权的提交为 `ac8430e6`，没有创建分支或混入原有 WIP。

### 追加授权：七项失败修复，待用户审阅

用户要求 subagent 快速修复并保留审阅控制，三个 worker 采用不重叠写范围；修复已在本轮实施前提交检查点 `f0ff1f0a`。

- 六项 legacy / snapshot / migration 失败来自已退役的跨实例独占、dirty / 绑定登记读门槛以及引用不存在父会话的夹具。依据 `ARC-WORKSPACE-001` 修正场景与断言，而非恢复旧限制；加强配置、消息原始字节、绑定、env、frozen 首次提交与真实拒绝边界。新增损坏/未来绑定拒绝及 lost native binding 独立场景，没有删除或 ignore 失败用例。
- 并发新库失败发生在 WAL 初始化早于 schema 协调。恢复 canonical 数据库路径级初始化 OS 锁，覆盖预检、WAL 与升级；有限等待、阻塞系统调用在 blocking worker 执行，异常/取消释放。不是 session sidecar 锁，不改变多实例执行政策。
- 新增 12 项开库回归，包括冷启动、单次迁移、取消、持有者进程退出、只读、别名及关闭收尾。定向模块重复 10 轮全部通过（1280 次并发冷开库）；原失败并发用例重复 30 次全通过。
- 汇总命令 `PERI_MACHINE_ID=00000000-0000-4000-8000-000000000001 cargo test -p peri-resources --lib -- --test-threads=1`：**390 通过 / 0 失败 / 21 忽略 / 0 过滤**。`cargo clippy -p peri-resources --all-targets -- -D warnings` 通过。真实远端测试仍未运行。
- 汇总后的 `cargo check --workspace --all-targets`、修改文件定向格式/行数检查及 `git diff --check` 通过；`cargo test -p peri-tui --test meta_session_cli -- --test-threads=1` 为 19 通过 / 0 失败。
- 用户批准优化移除 `execution_runs`，同 schema 11 实施。新建重试核对绑定、冻结及父链等不可变事实，不覆盖既有会话；无绑定但已有 frozen 的记录拒绝自动 legacy 接纳。活跃/未结清句柄强引用登记，丢弃 caller Arc 或重新取得不能绕过未知效果门禁；正常关闭后可重新取得，旧句柄不能关闭新句柄。远端账本保持，重开不证明前次未知写入终态。
- 检查点提交时另一任务正在修改 ACP/Agent/Middleware，workspace hooks 遇到其在途编译/格式错误；在 staged tree 的独立快照运行 `cargo check --workspace --all-targets`、`cargo fmt --all -- --check` 与 `cargo clippy --workspace -- -W clippy::all` 全通过后提交，仅将上述重复 hooks 排除，本任务 typos/layer-imports hooks 仍通过，未撤销或混入对方改动。

### 追加授权：执行表删除与运行时生命周期验收

- 本机 legacy / 2–10 与已有开发版 11 的受控清理在同一 schema 11 事务内完成；不再创建/重建执行表。识别历史有/无 threads FK 两种 DDL，额外列/约束、同名 view、外部 FK/view/trigger 拒绝，失败恢复执行行及旧版本。远端升级共用删除规划，保留 store 身份和操作账本。
- 同实例 active Arc 复用，uncertain Arc 强引用保留；取消写入/撤销、caller 丢弃句柄或 reacquire 均不能伪造结清。精确 owner 的冻结 CAS、关闭旧 Arc 不影响新运行、readonly/closed 拒绝、真实子进程重开及取消收尾均有行为回归。
- 重新创建仅核对一致的不可变事实；冲突不覆盖 canonical 数据。无绑定但有 frozen 的会话不能自动 legacy 接纳，但历史读取仍成功并表达 `BindingState::Missing`，避免把执行完整性门槛变成数据读取门槛。

| 最终验证（隔离 fixture，不运行真实库迁移） | 结果 |
| --- | --- |
| `PERI_MACHINE_ID=00000000-0000-4000-8000-000000000001 cargo test -p peri-resources --lib -- --test-threads=1` | **410 通过 / 0 失败 / 21 忽略 / 0 过滤**；包括本机及远端 SQLite 事务等价迁移测试 |
| `cargo test -p peri-resources --test session_resources_contract -- --test-threads=1` | 6 通过；真实进程生命周期与公开数据契约 |
| `cargo test -p peri-acp-types --lib -- --test-threads=1` | 516 通过 |
| Agent `provenance` / `compact_v2` / `compact_cancel` | 5 / 179 / 6 通过 |
| ACP `compact_recovery` / `goal_state`；TUI `meta_session_cli` | 14 / 25 / 19 通过 |
| `cargo check --workspace --all-targets`；`cargo clippy -p peri-resources -p peri-acp-types --all-targets -- -D warnings` | 全部通过 |
| Resources / Types / Agent doc tests | 11 通过，2 个既有示例忽略 |
| 修改 Rust 文件格式 / 1000 行上限、Markdown 本地链接、全边依赖门与 `git diff --check` | 全部通过；全仓大小扫描另有 15 个既有或其他任务超限文件，不宣称全仓大小合规 |

### 发布前尚需验收

- 真实远端服务的旧 v2/schema 10 升级（尤其 DROP COLUMN 与托管 DDL 的事务/失败能力），不能以 SQLite 测试传输等价物替代。
- 在发布环境停止旧 writer、完成包含 WAL 的一致性备份并演练从备份回退。不得让新旧二进制同时写升级后的库。
- 本轮执行表重构已完成并获用户授权提交；跨层及真实远端完整恢复仍按现行 active spec 验收。用户真实主库未升级，未测量文件缩容，未运行 VACUUM。
