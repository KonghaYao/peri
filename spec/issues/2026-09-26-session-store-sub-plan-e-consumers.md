# 会话资源拆分 — 子计划 E：Agent / ACP / Controller / Middleware 消费迁移

> 状态：详细设计计划，未实施。日期：2026-09-26。
> 上级：[总计划](2026-09-26-session-store-plan.md)。接口以 [A](2026-09-26-session-store-sub-plan-a-contracts.md) 为准，资源实现见 [B](2026-09-26-session-store-sub-plan-b-local.md)/[C](2026-09-26-session-store-sub-plan-c-turso.md)，部署入口由 [D](2026-09-26-session-store-sub-plan-d-configuration.md) 负责，测试见 [F](2026-09-26-session-store-sub-plan-f-verification.md)。

## 1. 目标与不变量

一次性迁移全部生产调用点，去掉业务侧存储补偿/逐项更新/缓存失效协议；之后切换 adapter 不再改业务分支。

仍保持：TUI 交互经 ACP；循环与 compact 算法归 Agent；ACP 负责协议、冻结输入与环境装配；Controller 转发资源句柄；Runtime 管登记、取消/销毁转发但不持另一份持久化事实。文件恢复、进程关闭、MCP/LSP/hook 生命周期不是数据库事务，不交给 adapter。

## 2. 生产入口覆盖登记

| 编号 | 文件/符号 | 必须迁移的内容 |
| --- | --- | --- |
| E-01 | `peri-controller/src/controller.rs::{new,sessions}`（现 `sessions: Arc<dyn ThreadStore>`，`controller.rs:172,198,290`） | 字段与访问器类型改为 `Arc<dyn SessionResources>`；**名称保留**，不另存数据/执行注册表 |
| E-02 | `peri-agent/src/resources.rs`（`open_thread_store{,_with}`）、`peri-agent/src/thread/mod.rs` | 更名 `open_session_resources{,_with}` 并返回新门面；删除 `ThreadStore`/`SqliteThreadStore`/`FilesystemThreadStore` 生产 re-export |
| E-03 | `peri-acp/src/host/{mod,assemble,workspace,stage_builder}.rs`（`AcpServerConfig.thread_store`，`host/mod.rs:194`；`HostAssemblyInput.thread_store`，`assemble.rs:123`） | **删除 `AcpServerConfig.thread_store` 字段**，存储只经 `cfg.controller.sessions()`；`HostAssemblyInput` 字段改名 `session_resources`；per-workspace 装配（`workspace.rs:71`）复用同一 Arc，不开第二个 store |
| E-04 | `peri-acp/src/session/mod.rs`、Agent session/exec context | SessionManager、CommandContext、executor 配置携带新门面（`CommandContext.thread_store` → `session_resources`） |
| E-05 | `peri-agent/src/session/transcript{.rs,/persistence.rs}` | append/flags/rewind/compact/flush 和失败传播；writer 仍唯一（unbounded channel + 64 条/100ms 批量基线） |
| E-06 | `peri-agent/src/session/exec/{compact_pipeline,executor_helpers/intercept,executor_helpers/v2_execute}.rs` | 一致快照恢复、compact 结果、热态失效 |
| E-07 | `peri-agent/src/session/subagent/factory/{spawn,resume,claim,context}.rs`、`subagent/lifecycle.rs` | child 完整保存、resume claim、继承区、status 与根 owner；child frozen 来源见 §6 |
| E-08 | `peri-middlewares/src/subagent/tool/{execute_resume,spawn_context,configuration}.rs` | 读取 metadata 与 SubagentHost 传参走门面，不残留 raw store |
| E-09 | `peri-acp/src/host/requests/{session_lifecycle,legacy_session}.rs` | new/load/resume/fork/close/delete/rename/list/metadata 全路径（`handle_new` 的 delete_thread 补偿链，`session_lifecycle.rs:545-620`） |
| E-10 | `peri-acp/src/dispatch/{session_fork,session_load,list_sessions,rewind}.rs` | 快照/纯 fork 映射/历史回放/回退；删除 `session_fork.rs:117` 的逐条 `update_message_flags` 与 `session_fork.rs:50` 的 `delete_thread` 存储补偿 |
| E-11 | `peri-acp/src/session/command/{compact,rewind}.rs` | slash 和 RPC 语义一致，不另建存储旁路 |
| E-12 | `peri-acp-types/src/{command,session}.rs` 及实际持有 ThreadStore 的共享类型 | 逐引用替换，不因只改顶层而让 context/host 继续混用 |

上述大括号为定位缩写；实施前全仓搜索 `ThreadStore`、`SqliteThreadStore`、`thread_store`、`open_thread_store`、`controller.sessions` 补齐新出现引用。D 负责 TUI/print/stdio/meta 的外部参数与 factory，本计划负责装配后的业务行为和错误映射。

### 2.1 已核实的生产旁路清单（review-2）

以下为静态核对到的真实 raw 调用点，迁移时必须逐个改走完整行为，不能只改类型：

| 位置 | 现状 | 目标 |
| --- | --- | --- |
| `peri-agent/src/session/transcript/persistence.rs:171-191` | `delete_messages_since` / 逐条 `update_message_flags` + `invalidate_context_cache`（部分失败仍 invalidation） | 一次完整行为（projection/compaction/rewind），cache 与 flags 同行为维护 |
| `peri-acp/src/dispatch/session_fork.rs:50,117,145,161,169-172` | `delete_thread` 补偿、逐条 flags 往返、`acquire_execution_lease` + `append_payloads` 手工分步 | 一次 `save_fork(ForkSnapshot)` + 由门面取得目标 owner |
| `peri-agent/src/session/exec/compact_pipeline.rs:131,142,168` | `supports_compaction_lifecycle()` 能力探测 + `load_messages`/`load_message_flags` 分次读 | 一致快照读取 + 领域能力判定 |
| `peri-agent/src/session/exec/executor_helpers/{intercept.rs:328-341,v2_execute.rs:155-156}` | 分次 `load_inherited_context`/`load_payloads`/`load_message_flags` | 一次一致 `load_session_snapshot` |
| `peri-agent/src/session/subagent/factory/spawn.rs:216-227` | `create_bound_thread`/`create_thread` + `store_inherited_context` + 失败时 `delete_thread` | 一次 `save_child(ChildSnapshot)` |
| `peri-agent/src/session/subagent/factory/claim.rs:105-140` | worker 内 `update_thread_status("active")` 与补偿写 | 门面 `claim_child_resume` 内部持有状态写入 |
| `peri-acp/src/host/prediction.rs:19,101` | 直接持 `cfg.thread_store` 写标题 | 经门面定向 metadata 更新 |

## 3. 冻结输入准备与新建

### 3.1 当前约束

`handle_new` 当前顺序是 resolve → create bound thread → lease → `SessionEnvironment::assemble` → build frozen → store frozen → 发布。这使 frozen 失败补偿散落在 ACP。

`host/assemble.rs::build_legacy_frozen_data` 已可不启动 MCP/hooks/tasks 地生成 frozen，但插件发现可能修复 manifest cache；`session/frozen.rs::build_frozen_data_with_config` 会读指引/skills/meta、探测环境、生成日期。它们不是纯函数，不能直接宣称可无副作用地提前执行。

### 3.2 目标准备结构

拟引入 ACP 内部 `PreparedSessionInputs`（不是数据端口类型）。**最终字段固定如下**（review-2 闭合；准备一次、后续只读消费，不再重读）：

```rust
pub(crate) struct PreparedSessionInputs {
    pub cwd: String,                          // 规范化后的执行目录
    pub config: Arc<PeriConfig>,              // 从同一 ConfigSource 读出并合并一次的视图
    pub config_source: Arc<ConfigSource>,     // 路径决策事实源（后续持久化沿用，不再判定）
    pub provider: LlmProvider,                // 由同一 config 解析（或环境变量），失败即准备失败
    pub plugin_data: Option<PluginLoadResult>,// 一次加载的插件聚合（roots/commands/hooks/lsp/mcp）
    pub skill_roots: Vec<SkillRoot>,
    pub agent_dirs: Vec<PathBuf>,
    pub frozen: FrozenSessionData,            // 由上述输入构建一次
    pub frozen_encoded: String,               // 版本化 snapshot 字节（数据端口只存不渲染）
    pub legacy: Option<LegacyAdoptionInputs>, // 仅 legacy：取自保存的绝对 cwd
}
```

规则：

- **lease 之前只读**：准备阶段不启动 MCP/LSP/hook/cron tick/Workflow/Agent loop，不创建 thread、不占 lease、不做 cache repair，也不写任何会话数据或本机登记。
- **插件 manifest 合成修复移出准备路径**：已核实 `PluginLoadResult` 的加载会经 `try_generate_synthetic_manifest_fallback` 往插件缓存目录写 `plugin.json`（`peri-middlewares/src/plugin/loader.rs:91-140,620-630`）。准备阶段改用**严格只读**加载入口（新增，例如 `load_enabled_plugins_readonly`）；清单缺失/非法在准备期直接失败并定位插件，不做静默跳过、不做修复。修复只在授权后的原责任层（插件管理命令/交互路径）发生，且不得改动已冻结的输入。
- **同一对象消费到底**：frozen 字节由同一 `PreparedSessionInputs` 产出（`frozen` / `frozen_encoded` 同源），环境装配使用同一 `config`/`provider`/`config_source`/roots，不第二次 `ConfigSource::load_at`、不第二次加载插件。装配期新增的只有既有副作用资源（MCP pool、LSP pool、hooks、cron），它们仍在发布之后创建。
- **date/env 一次定格（review-3 明确）**：日期与运行环境探测只在准备阶段求值一次，结果随 `frozen` 一并定格，装配与后续持久化一律消费该结果，不重新取时间、不重新探测环境。已核实现状：`build_frozen_data_with_config` 已冻结 `frozen_date`（`session/frozen.rs:43,73`）与 cwd，但 `PromptEnv::with_frozen_date` 仍在**调用时**重新探测 `platform`/`os_version` 与 `is_git_repo`（`prompt/mod.rs:103-113`，源码注释已写明“调用方若需冻结也应缓存”）；准备结构必须使这些取值与 frozen 同源，装配期不得再调 `detect`/`with_frozen_date` 各取一份。若装配期确需同一事实，从 `PreparedSessionInputs` 读取；两处取值不一致即视为准备结构缺陷，停止该批次而不是让两处各写一份。
- 若准备无法与写副作用分离，停止该实施批次并回到 A/B 调整初始化领域状态；不得退回公开 create/transaction/frozen/rollback 拼接。
- **三条路径明确区分**：
  | 路径 | 准备输入 | frozen 来源 | 消费行为 |
  | --- | --- | --- | --- |
  | new | 上述完整结构 | 由同一输入构建一次 | `create_session(NewSession)` |
  | legacy | `LegacyAdoptionInputs`：保存的绝对 cwd + 该目录的 config/plugin/frozen 构建结果 | 按保存的 cwd 构建（现有语义） | `adopt_legacy_session`，返回权威快照 |
  | fork | 仅 source 一致快照 + 领域 ID 映射 | **不构建**，直接复用 source 的精确 frozen 字节 | `save_fork(ForkSnapshot)` |
  | child | parent/root 关系 + 继承 payload/flags | 由不可变 parent/root 的**已持久化** frozen 源取原字节（见 §6） | `save_child(ChildSnapshot)` |

### 3.3 新建目标流程

1. 解析期望 cwd、只读完成 workspace 发现与 `PreparedSessionInputs`（含 frozen 字节）；此步不占 lease、无 cache repair、无执行资源。
2. 生成一次 ThreadId 与完整 `NewSession`，调用门面 `create_session`；门面内部按 B §5.1 顺序执行 creation intent → 完整数据保存 → 执行代际 → owner，并返回权威身份/owner（或 `SavedButNotAdmitted`）。
3. 使用同一 `PreparedSessionInputs`（不重读配置/插件）装配环境，并按返回的身份/owner 复核目录。
4. 环境准备成功后注册 SessionManager/live state，建立实际需要的 Workflow/LSP 句柄；激活与发布保持现有顺序。
5. `session/new` response 成功后发送首个 commands snapshot，再允许 MCP prewarm；不让发现结果抢到初始化事件前。
6. 环境装配失败：ACP 排空已实际建立的资源，未完成时保留关闭 owner；只有执行资源已排空才调用门面的初始化失败行为，由内部处理数据撤销/恢复。ACP 不发原始 delete/frozen/CAS 序列。

初始化失败使用 A 的 `abandon_initialization` 领域行为；它不是 rollback 别名，只针对本次未发布的 new/fork/child，不能用于撤销已发布会话的任意历史。执行资源是否排空仍由实际 owner 证明，不用一个未经验证的布尔值绕过生命周期。

## 4. Load / resume / legacy / fork

### 4.1 Load 与 resume

- 从一致会话快照读 binding/frozen/history/flags，metadata 读取仍可走轻量入口。
- 已绑定、真正本机 legacy、外来登记、持久化未决分别处理。不得把缺 binding/未支持当 legacy。
- 本机 legacy 输入只从保存的绝对 cwd 构建，经门面专用接纳返回权威快照；不让 ACP 处理 winner bool 或二次写冻结。
- 未知远程写先由门面恢复；若仍阻塞，不触发普通 dirty 自动 reset，不装配写执行资源。
- owner 不可得时保留已有只读准入和历史回放；只读路径不启动环境、Workflow/LSP/MCP，也不回填 frozen/登记。
- 请求 cwd 仅作期望校验；成功提交 live state 后才公布 active identity/cwd。load reservation、操作 gate、失败时旧状态恢复/NoSession 保持。
- 原 `session/load` 先回放后响应的顺序、TUI reset 后再次回放的清边界规则不变。

### 4.2 普通 fork

1. source 必须 idle 且具有 owner，持现有生命周期 gate 至快照取得/复制完成。
2. 门面提供一致 source snapshot；领域纯函数验证工具往返完整性并产生固定新 ID 映射。
3. 一次传完整 `ForkSnapshot`（payload、flags、source 原 binding/frozen），门面保存并取得目标 root owner。
4. ACP 用 source 精确 frozen 装配新会话，不按当前日期/目录重冻；failure cleanup 与 new 相同。
5. 删除 `cleanup_failed_fork` 式存储补偿和逐条 flags 往返。普通 fork 不引入 inherited ancestor；owned child 才有该区分。

## 5. Agent transcript 与运行期

### 5.1 保留一个 writer

保留 FIFO、当前 64 条/100ms 的批量基线、Barrier/Shutdown 语义；不要在资源层再建一条可独立漂移的 transcript 队列。adapter 内部网络任务不拥有第二份业务历史。

- append 使用 canonical payload（包括可信 reminder），成功后计数/标题由数据行为维护。
- `PersistOp::ApplyCompactionBatch` 转完整 projection 行为，不逐条 `update_message_flags` 再 invalidation。
- Full compact 先 flush，再 `apply_compaction`，确认成功才改变内存 flags/摘要；未知或 writer failure 保留磁盘事实并使热态失效。
- compact 输入来自一致 snapshot，不拼先 messages 后 flags 的远程跨时刻结果。
- 异常分类在资源边界转安全错误；不要把 SDK 原始错误直接写 tracing 或 error body。

### 5.2 有界积压

当前 unbounded channel + 失败后 256 条缓冲不限制 in-flight 慢网络期间积压。计划使用**共享的待持久化条数/字节预算**覆盖 channel、pending batch 和 in-flight；具体阈值由 F 测量后确定。

不在 transcript 写锁内 await bounded sender。同步追加先预留预算；无法预留时设置 sticky 持久化失败，并由最近的循环/flush 边界停止后续模型/工具工作，丢弃热态信任。已在运行的工具按原生命周期排空，不声称取消了已发生副作用。若数据已进入 canonical 内存但未获准持久化，必须明确报未保存，绝不能算成功。

预算释放以行为效果已确定/缓冲已真实释放为准，取消调用方不等于释放仍被 adapter 持有的 payload。目标测试用可控暂停证明积压有界及错误及时到达，不只断言队列容量常量。

## 6. 子 Agent 与 middleware

- spawn：领域侧确定 parent/root、保存 cwd、frozen 继承和 inherited payload/flags，一次 `save_child`；成功后才构造执行 session、注入首轮消息并启动。
- child frozen 来源固定为**不可变 parent/root 的已持久化 frozen 字节**：数据行为解析 parent（无 parent 则 root）链并校验根 frozen 存在且有效，必要时原样复制字节；禁止重新扫描目录、禁止用当前目录/日期重冻。已核实现状是**父 session 内存副本**（`peri-agent/src/session/subagent/factory/spawn.rs:114-123,239-247` 从 `p.store().frozen.*` 取），这在冷恢复（父会话未加载）时没有来源，必须改为从持久化快照解析；同一会话内若使用内存副本，必须证明它与已保存快照逐字节相同（同一快照加载而来、期间未重渲染）。Agent 不依赖 ACP codec 编码 frozen。历史子会话沿原父链恢复，根 frozen 缺失/损坏时明确失败，不偷偷增加新的 JSON 格式。
- resume：保留 parent/root 与绑定一致性验证；读取一致 own/inherited/flags，先拿有效 root 执行权，再恢复模型/工具集合。
- `ResumeClaim` 的 worker 目前持有 active 写入及失败补偿，避免调用 future drop 丢失收尾。迁到 A 的 `claim_child_resume`，Agent 只报告开始运行/移交后台/准备失败/终止等领域结果，门面内部持有状态写入与恢复工作。数据端仍只接定向状态行为，不接远程 operation identity；不能只换类型后留下 status Unknown 仍开跑。
- done/cancelled/error status 定向更新，不覆盖并发标题/计数；写失败进入既有可信终态处理。
- middleware 的 `execute_resume` metadata 读取、`SubagentHost` 注入和工具 configuration 同步切门面；Workflow 内产生的 Agent 同样消费这一路径，不设独立后端。

## 7. Rewind、删除、关闭与协议错误

### Rewind

区分 transcript `KeepThrough` 与用户 `RemoveFrom`；历史侧是一次门面行为，cache 与 flags 一起维护。本机文件复原仍由现有执行层完成，它与远程数据库不构成单事务。文件已改而历史保存失败时报告真实部分效果、阻止不可信热态，不由 adapter 宣称全部回滚。

### Close/delete

维持既有 ACP MCP session close、SessionEnd、task drain、dynamic MCP、MCP pool/LSP 等排空顺序。只有拥有者给出真实执行收尾证据后才请求门面结清持久化并 clean；任一未完成保持 Closing/Incomplete 和唯一 owner。

delete 是显式生命周期行为，执行先关闭，数据删除由门面协调并在同事务写入墓碑（B §4.4）；远程删除已发生但未知时不提前清本机 pending/dirty，也不提前删墓碑。rename/list/metadata/history 的短时准入或只读规则同样迁移，不能只覆盖 new/load。

### 错误映射

- 新 `PersistenceUncertain` 独立于普通 dirty，不进入 `ReadOnlyAdmission::from_workspace_error` 的自动 reset 分支。
- ACP 只映射领域错误，现有只读 reason 与能力协商保持兼容；必要新增 wire 字段必须按 caps 门控并覆盖客户端消费，不能直接塞 SDK 内容。
- 执行不可用不等于历史不可读；已保存未准入返回可定位 identity，而不是误报“会话不存在”。
- 不新增用户必须操作的数据库恢复令牌；可见恢复行为围绕 session identity。

## 8. 施工顺序与退出检查

1. E-01…04：门面类型/注入归一，删除 `AcpServerConfig.thread_store`，禁止 raw-store 旁路；与 D 串行协调共享装配文件。
2. E-05…06：transcript/compact 输入与保存行为、积压预算及 Unknown 传播（`MutationOutcome` 三态）。
3. E-07…08：child/resume claim/status/middleware/Workflow 传播。
4. E-09…11：准备输入、新建/恢复/fork/close/rewind 全路径；保留响应与资源生命周期顺序。
5. E-12：扫描共享类型、测试替身和旧出口，消除生产 old/new 双写路径（符号删除 + 全 target 编译证据，A §7.1）。

F 的 V-02…15 与既有 ACP/Agent/Runtime 回归保护。搜索旧方法只能用来发现遗漏，不能以零字符串匹配代替行为测试。完成标准：SQLite/Turso 选择不出现在这些业务文件中；调用方不再编排 flags/cache/补偿/事务协议；默认与远程都使用同一条受授权的会话路径。
