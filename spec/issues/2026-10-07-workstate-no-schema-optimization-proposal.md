# WorkState 无 schema 变更的 CPU / 内存优化方案

**状态：阶段 A 首片已实施，待性能验收；B / C 及可选项待裁决、未实施。** 日期：2026-10-07。设计者：Astra。已实施 A0 / A1a / A1b / A1c；当前验收统一记录在 [CPU issue](2026-10-07-multi-subagent-cpu.md)，此文保留后续候选的约束与取舍，不作为已完成过程记录。

文中基线成本指实施前源码，不能当作当前实现清单。阶段 A 的实际接口以 code-index 为准，验证结果统一记录在关联 CPU issue；后续候选的计数是验收门槛，不是测得收益。未执行现场迁移或 CPU/RSS 性能对比。

## 1. 推荐决策与范围

首选按 **A：减少重复计算与复制 → B：收敛执行读取 → C：严格校验下的原始 JSON 保留与选择性物化** 推进。A 首片已独立交付，仍需量化；B 改善调用方数据量和快照寿命；C 才系统减少每次 mutation 对无关历史大字符串的物化、转义与重编码。SQL delta patch 是 C 之后按证据决定的 adapter 优化；跨请求 typed cache 暂不推荐默认启用。

硬约束是“不改变数据库结构”：保持 schema 17，不改表、列、索引或版本，不借外部文件、网络 side store、临时持久表外置 payload。本方案额外采用保守兼容边界：`state_json`、`command_json` 的现有字段、类型、枚举、缺省解释与既有记录保持兼容。内部 Rust interface 可变；持久 JSON 不换成压缩串、JSONB、ID 引用或新的 envelope。更宽的“同一 TEXT 列改存另一种 wire”虽然不必改 DDL，也不在本方案中。

不清理历史，不扩展终态数据删除，不删除原命令 journal，不降低并发或刷新频率。完整恢复、Unknown 原命令对账、事务 guard、回执幂等、拒绝更新隔离、生命周期与 SDK 执行所有权是实施门禁。

## 2. 实施前成本链与证据等级

证据入口：[现场调查及既有回归记录](2026-10-07-multi-subagent-cpu.md)、[Resources 代码索引](../../docs/code-index/peri-resources.md)、[Agent 代码索引](../../docs/code-index/peri-agent.md)。本文以当前源码符号定位，避免把共享树行号当稳定契约。

### 已观察到的运行事实

前次报告的两次 macOS sample 都出现 `write_work`、`read_snapshot → decode<WorkState>`、`mutation_effects → encode<WorkState>`；另有模型序列化和 TUI 绘制热点。六个近期子会话状态约 0.59–1.70 MB，进行中请求约 24–82 万字符；字符数不等于 UTF-8 字节数。这些采样没有证明唯一主因、各热点互斥占比或截图会话的精确关联。

当前状态契约与回归结果只维护在 CPU issue，不复制旧轮次计数。`processing.rs` 注释中的“95%”不是可复用的收益数据。

### 基线源码事实与优先级

下表 A1/A2 的重复处理已由阶段 A 首片消除或减少；B/C 相关整份状态成本仍存在。已删除的 helper 名称仅用于定位基线，不能继续作为当前代码入口。

| 级别 | 代码证据 | 明确成本与局限 |
| --- | --- | --- |
| A1，先消除 | `peri-acp-types/src/session_resources/work.rs::WorkCommand::digest` 使用 `serde_json::to_vec(self)`；`work_ledger.rs::commit`、`resources/gate_work.rs::apply_work`、SQLite `write_work`、`reducer.rs::reduce_work` 重复调用 | 当前列出的正常路径有四个直接摘要调用入口；每次涉及整个命令序列化和分配。独立核对提示还有其他静态位置，不能把静态位置数当实测固定调用数；重放、拒绝、嵌套命令与重试路径另计 |
| A1，先消除 | `sessions/work.rs::command_effects_with_digest` 编码命令后 `params.clone()`；SQLite journal begin 和 `work/effects.rs::mutation_effects` 分别调用它 | 同一 BeginReason 原文重复序列化、复制并绑定；remote 的 begin/ack/seal/receipt 检查还有摘要重复。完整 journal 本身是必要证据，不能删除 |
| A1，先消除 | `WorkMutationBarrier` 的 unconfirmed 与 `MutationGate` 的 pending 都 clone `WorkCommand`；`commit_execution_transition` 先 clone | 大请求进入多个内存副本；改共享不可变命令可减少副本，不能削弱 Unknown 持有责任 |
| A1，先消除 | `work/processing.rs` 的 `begin_reason`、`commit_reason`、`begin_dispatch`、`commit_act`、`unknown`、`resume`、`settle` clone 完整 WorkRecord | 多数仅需 stage、ID、budget、batch 或 invocation ID，却复制 request / response；请求正文已在部分终态清除，响应仍可能很大 |
| A2，先消除 | `work_reason.rs::prepare` 用 `json!` 包装 `prepared.checkpoint()`，随后 `work_pipeline.rs::request_checkpoint` 再 `to_string` | 构建一棵额外 Value 树；`peri-model/src/protocol/prepared.rs::checkpoint` 也复制 body，provider 闭包同时保有 built request。Agent 外层包装先优化，provider 共享另设切片 |
| A2，可局部优化 | `work/query.rs` 多个方法分别构造 `WorkAvailabilityState::from(self)`；该转换复制全量 metadata map | 不包含大 payload，仍随历史条数增加；改借用事实视图，复用同一领域谓词。嵌套 admission/batch 扫描仍需独立计量 |
| B，热点放大器 | `work_reason.rs` / `work_dispatch.rs` 多处 `session.snapshot()`；准备、响应、逐工具 begin、结果提交及提交后投影均可能全量读 | 仅消费局部信息却持有完整历史；有些 snapshot 生命周期跨 mutation await，与 adapter 的工作副本重叠 |
| C，结构性应用成本 | SQLite `session_data/work.rs::read_snapshot/write_work`；remote `session_work.rs::read_work_state/write_work`；`work/effects.rs` | 读取完整原 JSON，解码所有 WorkState，reducer 原位更新，接受后编码整个新状态；历史 delivery / response / invocation 与进行中请求均参与。reducer 已消费独占状态，并非仍 clone 整个 WorkState |
| 保留且测量 | `work.rs::READ_REVISION/READ_DELIVERY/READ_RESOURCE_OWNER_FACTS`，`work/availability.rs::READ_AVAILABILITY` | 已有窄读取减少 Rust 返回量，但数据库仍解析 JSON、扫描相应 map；不能标成索引化 O(1) |
| 潜在后续瓶颈 | `HAS_PENDING`、`READ_PENDING`、`GUARD_COMMAND` 的 `NOT INDEXED`；effects 的消息 count/title；remote 的事务与请求数 | 有增长型扫描、重复 payload 校验/解析或网络成本。属于源码支持的成本假设，当前采样没有量化各自占比；本方案不删 guard 或擅改索引 |

现行 SQLite 流程为 journal 独立提交 → `BEGIN IMMEDIATE` 读状态/归约/效果提交 → ACK 独立提交；remote 先 begin qualified mutation，再一致读取、带 guard 的 qualified mutation、ACK 与原回执读取。保留这些耐久与不确定性语义，先减少每个阶段内部的重复工作。

`WorkSnapshot::from_state` 的 `query.limit` 只限制 candidates，不限制返回的 state；`session.snapshot()` 即使使用 limit=1，也不是窄读取。当前 diagnostics 日志字段为 `submitted_sql_count`、`transaction_scope_count`；它们表达提交的语句与事务作用域，不证明这些语句/事务已成功提交，不能直接当成功计数。

## 3. Module 与 seam：复杂度集中在既有责任层

依照 codebase-design：Module 应提供小而完整的 Interface；SQLite 与 remote 是真实的两个 Adapter，因此存储编解码/效果编译有现成 Seam。不要让 Agent 学习 JSON path、cache token 或 SQL 重试规则。

| Module / 拟议内部 interface | 落点 | 所有权与职责 |
| --- | --- | --- |
| `PreparedWorkCommand::try_new(WorkCommand)`，只读 `command()/encoded()/digest()` | `peri-acp-types/src/session_resources/work/command.rs`（拟新增） | 私有字段持有不可变命令、唯一 canonical JSON 与摘要。消费输入所有权，调用方拿共享 handle；没有修改 guard 后沿用旧摘要的入口 |
| 同一 `reduce_work` 领域实现及事实访问 | `work/reducer.rs`、`processing.rs`、`query.rs`、`availability.rs` | 保持所有业务判定单一权威；A 只调整命令验证入口/借用，C 改内部状态存取。Adapter 不实现 stage/lifecycle/预算规则 |
| `load_work_execution(query) -> WorkExecutionView` | `SessionResources` / `SessionDataPort` 契约、`resources/gate_work.rs` | 一次一致读返回 control、revision、所需 work/batch/budget/invocation 与阻断事实；不把缺省空历史伪装成完整 WorkSnapshot |
| `WorkDocument::decode_strict(raw)` 与 `WorkEdit::finish()` | resources `sessions/work/document.rs`，领域侧 work 内部状态访问（拟新增） | 封装 wire 校验、原文生命周期、按需物化与接受后变更集；原始 guard 归 resources，业务规则归 types |
| `mutation_effects(prepared, before, reduction)` | 既有 `sessions/work/effects.rs` | 一处编译 state/control/event/projection/receipt 原子效果，两个 Adapter 只绑定参数与执行。参数共享、SQL patch 顺序也在此收口 |

上述名称是设计草案。实施时直接替换内部调用点，删除旧的重复实现；不增加 deprecated shim、双写或第二份 reducer。保留完整 WorkSnapshot 查询作为真正的恢复/检查能力，不把它改成内容残缺但同名的对象。

## 4. 阶段 A：确定收益来源最清楚的最小改动

### A1. 一次冻结命令，一次序列化、摘要，多处借用

在 Agent 命令交接或其他真实命令入口消费 `WorkCommand`，构造不可伪造的 `PreparedWorkCommand`。先执行现有身份合法性检查，再用与现行相同的 serde 格式编码一次，对**这份字节**计算 SHA-256，共享给 barrier、gate、journal、reducer、effects、remote phase identity 和 ACK。它不是持久 DTO；保存的仍是原 WorkCommand JSON。

`WorkCommand::digest` 目前也承担输入校验。不能只删除它的调用：将便宜的身份校验与完整编码分开，所有未受信输入必须经过构造器；需要跨 crate 的 handle 保持字段私有。内部 `SessionResources`、data port、reducer 参数改为 prepared handle 后一起迁移所有实现与调用方，不在层与层之间反复把 raw command 包装回来。

barrier/gate 的 pending 改持 `Arc<PreparedWorkCommand>`；正常提交、错误及 Unknown 都持有同一 handle。已确认 stale receipt 后若生成新 mutation ID/guard，必须构造新命令、重新编码与摘要；Unknown 不允许换 ID、换 qualifiers 或复制后修订正文。原命令深度相等检查保留，不以仅比较 ID 或外部传入 digest 替代。嵌套 parent command 有独立身份，必要校验独立计数，不计作父命令重复编码。

数据库读出的 `command_json` 是不受信输入：严格解码一次，按原规则 canonical 编码并核对存储 digest；保留读出的原字节供 journal 比较，不直接对非 canonical 原文散列以替代现行语义。`original_command` 与 `owned_command` 间的重复 digest 可合并。日志记录原命令的职责不变。

effects 参数改为借用或 `Arc<str>` 的内部表示，避免 command/state/event/projection 为 INSERT 和 GUARD 组装时深 clone。SQLite 能借用时借用；remote 的 `Value::Text(String)`/transport 可能仍要求拥有字符串，必须到最后绑定处才物化，并把 driver/HTTP 复制单列。不要许诺把相同参数多次发送也变成一次。

**A 阶段也必须保护 remote CAS 重试定位：** 当前 effects 前序是 command INSERT/GUARD、state INSERT/GUARD，remote 使用 `NotApplied { rejected_statement: Some(4), .. }` 分类后重读重试。首片仅共享参数/编码，保持 SQL 布局；任何删除重复 effects 或重排，都必须由 compiler 返回语句角色到现有 transport index 的映射，并让 Adapter 按 `StateGuardConflict` 等内部角色分类。不改远端协议，覆盖 qualified mutation 附加语句造成的索引偏移；command guard、event guard 失败不能误判为 state CAS 可重试。不能等 C3 才检查这个契约。

`PreparedWorkCommand` 多保留一份 canonical 文本有成本，但当前路径已多次生成该文本及命令副本；prepared 只在本次提交、未决原命令及必要持有者中存活，不建立历史 handle registry。成功/确定失败清理原 pending；Unknown 保留至对账，不能以节省内存丢弃证据。

### A2. 缩小借用与临时值

- 七处 WorkRecord clone 改为先只读校验，提取标量及必要小 ID，再分阶段借用不同 map 更新；`successor` 接收 work/budget/batch 身份，不接收携带 request/response 的整个 source clone。保留现有校验顺序、错误种类及拒绝后丢弃私有状态的行为。
- `WorkAvailabilityState::from` 的多次构造改为借用事实视图或一次调用内构造一次；两种存储表示共享谓词，避免 SQL、完整 state 与窄 DTO 各自维护业务规则。metadata 上的历史扫描仍在。
- Agent 外层 checkpoint 改为借用式 `Serialize` wrapper，借用 prepared checkpoint 和 authorizationRef，直接输出字符串；保留 toolDefinitions 原顺序及完整内容。必须与旧 `json!` 的实际字段输出顺序、转义、数字序列化逐字节比较，不能因为换 struct 的字段声明顺序改变 request digest。
- `begin_reason` 对请求的 JSON 合法性检查保留；若去掉 `Value` 树，可用完整消费的校验 visitor，但必须先证明畸形 JSON、非法转义/数字、尾随数据与旧解析的接受集一致。它不免除请求 digest 检查。该优化单列，不把“少构造 Value”当成可以不校验。
- 将实际需要的 target/guard/budget/intent 提取后尽早释放 snapshot，再 await 写入；有效性仍由事务内 guard 判定。保持 snapshot 到命令之间允许并发变化的现有冲突处理，不把缩短生命周期误称为原子读写。
- model 内 `checkpoint(body)` 与 send closure 共享 frozen request 属后续 A3：provider builder 持有同一个不可变 body，借用序列化 checkpoint。不能缓存不同模型配置的 body 或重建发送内容；保留 prepared checkpoint = 实际发送 body 的端到端验证。

A 不避免整份 WorkState 解码/编码；它减少次数、深拷贝、Value 树及重叠驻留。没有测量前不承诺 CPU 百分比或 RSS 降幅。

## 5. 阶段 B：让热执行只消费所需事实

优先迁移 `prepare`、逐工具 `begin`、`unknown`、提交后读取 settled projection，随后覆盖 `commit_response` / `commit_results`。拟议 `WorkExecutionQuery` 由 session/work 身份与显式用途组成；`WorkExecutionView` 包含一次一致读取的 control、work revision、stage、budget、batch 生命周期、目标 invocation 及用途所需的 response/result。不会为 BeginDispatch 返回模型请求正文。

用途至少区分“执行事实”“已提交投影”“恢复”；恢复必须拿到关联 delivery/projection/version、request/response 与 invocation 完整证据，不为降低大小截断恢复数据。全局候选和 admission 的阻断仍通过现有领域查询计算，不能只看目标 work 忽略其他 live work、LegacyUnknown、terminal obligations 或 pending journal。

首个 B 切片可以在 adapter 内复用现有严格 `read_snapshot`，只向 Agent 返回 owned 小视图并立即释放完整状态，先稳定 interface 和缩短驻留；此时明确仍全量解码。C 完成后替换为严格校验的原文视图。**不能直接用 SQL 只取局部字段，悄悄改变此前全量 snapshot 会暴露的坏历史记录行为。** 已有专用窄 query 按自己的既有契约继续运行，不能据此放宽 mutation。

SQLite 在同一读取事务内组合事实；remote 使用现有 `read_batch` 的 consistent read，不能用多个独立 HTTP SELECT 拼 control/revision/work。经过 `MutationGate` 维护当前 root/后代 pending 与 Unknown 屏障；轻视图不是执行资格或 SDK admission 的替代物。原回执通常足够提供下一目标 revision，但后续需要重新验证 control/pending 时仍读库，不能复用回执推导“没有外部写入”。

## 6. 阶段 C：严格校验、原文保留、选择性物化

### C1. 先解决“避免分配”，不跳过历史校验

当前 mutation 对全量 WorkState 及嵌套记录执行 serde 类型解码，包含 `deny_unknown_fields`。改用 RawValue 不会自动保留这层校验：`json_valid`、合法 JSON 或 revision 相等均不证明是合法 WorkState。

建议引入借用型 wire 读取表示：所有已知对象/枚举/集合仍按**同一领域类型定义**遍历校验；少数大文本字段用 `PersistedText` 表示“已校验 JSON string 的原文切片或新建 decoded 文本”。覆盖 request serializedRequest、WorkPayload.serialized、arguments JSON、owner/metadata JSON、staged input 与嵌套命令中的大字符串。普通 ID、stage、revision、policy 等仍为类型化小字段。

为避免复制整个 serde schema，先对实际大文本承载类型引入默认为 String 的内部泛型文本参数，向 WorkRecord/DeliveryRecord/InvocationRecord/嵌套 WorkCommand/WorkState 传播；借用 wire 表示与 owned 表示复用字段、serde attribute 与枚举定义。不得手写一张与业务类型独立维护的“允许字段列表”。若这一传播不能保持清晰、局部且同源，C1 暂停，不以第二套 schema 校验器换性能。

原始 JSON 放入单一拥有者；借用 `&RawValue` 或校验后范围定位大字符串，领域视图只持该拥有者及范围，不创建每个字段一份 `Box<RawValue>`。不得构造不安全自引用；可在持有 raw 的同步作用域内借用归约，完成后产出 owned delta/effects 再 await。需跨 await 的切片使用拥有者 + 校验后的字节范围。

`raw_value` 当前未在 workspace `serde_json` 声明中启用（根 Cargo.toml 为 `serde_json = "1.0"`）；若采用 RawValue，必须显式启用该 feature。这是依赖 feature 改动，不是 schema 改动；需检查 feature 统一后 native/remote/Emscripten 构建及 JSON 接受集，不顺带启用 preserve_order/arbitrary_precision 或升级版本。

严格解码仍需：

1. 扫描整个 JSON；每层对象执行原 unknown/duplicate/missing/default 规则，枚举与整数范围不变，map key 按原 serde 语义处理。非 canonical 空白、字段排列与转义不应仅因此被拒绝。
2. 大字段验证为 JSON string，完整验证转义、Unicode、尾随数据等与原 String 解码等价；RawValue 的“合法 JSON”不足以替代此检查。转义字符串的 parser scratch 仍可能分配，冷态峰值至少受最大被校验字符串影响，不能承诺零分配。
3. 不新增或删减嵌套字符串内部的语义校验：例如 WorkPayload.serialized 在原全量读取时只是 String，而 action 在需要时再 `validate`；新的读取不能擅自把所有历史正文都升级为 payload 解码，也不能跳过 action 本来执行的校验。
4. 缺失字段的默认语义、未知字段拒绝及错误类型保持；发现任何历史坏记录时整次 mutation 在业务效果前失败。journal 若已单独 ACK，继续按现有 pending/对账协议处理，不伪造成功或自动删除原命令。

这样可以不为无关历史 payload 建立长期驻留的解码 String/Value 树，但**仍完整扫描、验证所有记录**，仍构建与历史条数相关的 metadata/index。冷态不做到 O(1)，首次变更也不依赖 cache。

### C2. 同一 reducer 在私有编辑视图上生成变更

将现有 reducer 的 state 存取收口到内部 `WorkEdit`：只读 metadata、按 action 需要物化正文、首次写记录时进入私有 overlay，成功后输出 `WorkDelta`、receipt、control、events、projections。工作集必须包含所有真实依赖：BeginReason 的全历史 request_id 唯一性、ClaimBatch 的全局候选和容量、CommitReason 的 batch obligations/intent 冲突、CommitAct 的同 batch/admission 结算，以及 terminal parent receipt 证明等。

这不是给 reducer 填一份缺历史的 WorkState。迭代/存在性/相等比较必须覆盖完整原状态；无法用 metadata 回答的比较才物化对应文本。字符串相等比较必须按解码值，不能直接比较不同转义写法的 raw token。全局谓词继续使用 types 的一份实现。

拒绝时丢弃整个 overlay、control、event、projection，仅保留原行为所需的拒绝回执；版本溢出、最后一条 intent/result 失败等晚期错误同样如此。accepted_state 对 receipt/state 配对的保护迁移为等价的 accepted_delta 检查。不能在 cache 或共享底层文档上先原位改再“尝试回滚”。

第一步写回推荐继续 `UPDATE_STATE`：拼接或流式输出完整新 JSON，未变动记录/大字符串原文直拷，变更字段使用类型 serializer；无关历史正文不解码后再 escape。没有修改的原 token 保留，字段类型与逻辑值不变；已有可缺省字段保留缺失或按现有默认编码规则输出须经过往返等价测试。不要求旧整行所有空白在接受变更后保持，但 guard 必须使用读取时原始整行。

这仍生成并提交完整新 TEXT，仍有 O(状态字节数 + metadata + delta) 的扫描/复制成本；收益来源是少物化、少转义和少重复序列化，不是历史字节消失。

### C3. SQL delta patch：候选，不作为首轮承诺

在 C2 变更集稳定之后，可由共用 effects 编译器把变更编译成 `json_set`/必要的精确路径操作。原 state/control guard、command guard、event/projection guard、receipt 和 qualified mutation 保持同一事务；patch 没有通过 guard 时不得提交任何业务效果。除既有终态 request trimming 等已存在动作外，不增加删除。

SQL 只承载已由 reducer 决定的变更，不重写领域规则。正确区分 JSON null、SQL NULL、JSON 字符串与嵌套对象；字段及 ID 路径由结构化表示生成并绑定。包含引号、点、方括号、反斜杠的 key 必须支持或使用经验证的整文写回路径，不假设所有 key 是 UUID。固定顺序与父子路径覆盖冲突检查由 compiler 负责；大 delta 触及参数/表达式限制时，明确选择整文写回，不能悄悄分拆成多个已提交状态。

**保留 guard：** 当前 `GUARD_STATE` 使用读出的原始 `state_json` 作相等比较；`pre_state_json` 已保留非 canonical 旧 JSON。不得换成仅比较 revision、重新编码旧 WorkState 或缓存摘要。control 比较继续沿用当前 SQL/default 与 control 编码行为；本次不改变其规范化或缺失值语义。拒绝路径同样受 guard 保护。

SQLite JSON 函数可能仍完整解析原 JSON、生成完整新文本并触发整行/overflow page/WAL 写入；多次函数组合可能重复遍历。不能承诺原地修改几个字节、O(delta) I/O 或 O(1) 查询。必须测数据库 CPU、写入字节与事务时长，而非只测 Rust 的 encode 次数。

remote 也需要完整读取原 state 才能严格校验，并在提交时发送完整旧 state guard。patch 至多省去新整行文本的部分发送/客户端编码，**不能声称跨网络只传 delta**。同一 SQL patch 在本地有效不证明实际 Turso 的 SQL 能力、限制与成本；未验真实后端前保留整文写回这个明确 Adapter 策略，共用同一个 reducer。

remote 当前用 `rejected_statement: Some(4)` 识别特定 guard 重试。效果编排变化必须同时将编译结果中的语句角色映射给错误分类，或证明原索引仍准确；不能让 patch 新语句位移触发错误的重试。重试仍先查原回执，不因 guard 冲突重新签发原 Unknown 命令。

## 7. 缓存：暂缓 typed cache，给出严格可选边界

warm typed cache 能省 decoder，但保留 raw + 全量 typed state + 在途编辑副本可能增加 RSS，尤其多 subagent。当前没有净内存收益证据，因此 A/B/C 默认不依赖跨请求 cache。一次事务内的借用/共享不是跨请求缓存。

若后续测量证明重复相同状态读取主导，可单独试验资源工厂持有的**有界、可完全禁用**文档缓存：

| 项目 | 必须满足的设计 |
| --- | --- |
| Key | 部署 Store 身份/实例打开代际 + session ID + state 存在性；revision/lifecycle 仅作快速 hint。条目内保留完整原 JSON、类型验证版本、metadata/可选物化结果。相同 session ID 不跨 store 复用 |
| 有效性 | 每次仍从数据库一致读取当前 raw/control/pending 事实；先长度/摘要筛选再原字节相等确认。相同 revision 但不同 raw 必须 miss。不能以进程内“我是唯一 writer”、TTL、SDK admission 或通知到达代替验证 |
| 原文 | 保存实际 `state_json`，包括空白、顺序和转义；不从 typed state 重建 guard。state 缺失/legacy seed 另标记。control/pending 不从 state revision 推导，必须按当次真实事实判断 |
| 写入与失败 | 编辑私有 overlay；确认 Applied 且 ACK/原回执已按现协议确认后，才可发布新条目。拒绝、guard 冲突、确定失败、取消、提交/ACK Unknown 均清掉本次关联候选条目；Unknown 原命令由 barrier/journal 持有，不能随缓存淘汰 |
| 外部写/冷恢复 | 外部写使 raw 比较失配；同 revision 改写也失配。读后到写前的竞争仍由事务 guard 捕获。新实例或进程重开无 cache，按完整严格校验+原命令对账恢复；cache 丢失不改变正确性 |
| Ownership | owner 为 Resources 部署工厂，非 Agent/Harness；不强持 resources/gate/SDK 执行 owner 或后台任务。关闭停止新增条目并释放 cache；不得因 cache 中有会话而认领执行 |
| 预算草案 | 每部署最多 16 MiB、最多 8 个 session、单条最多 2 MiB，先到者生效；这是待测的试验上限，不是经验最优值。按 raw capacity、物化字符串、metadata、索引及条目开销保守计费，预算含 pinned 条目和替换中的旧版本 |
| 超预算 | LRU 淘汰无持有者条目；无法腾出或单条过大则不准入，继续无 cache 路径。不能为了保持 cache 命中限制 agent 并发。cache 的计费 owner 随最后共享引用释放，不以移出 map 当作已释放；active operation 临时内存另测 |

这个保守缓存每次仍读取/比较 O(状态字节数)，remote 仍下载原 JSON；只减少 decoder/分配，不能减少冷态成本，也无法承诺总进程内存恒定。更激进的 revision-only 校验需要更强存储契约，本方案不推荐。优先缓存 raw + metadata，而非全量 typed payload；不能证明新增驻留预算换来净收益时保持禁用。

## 8. 不变量与失败矩阵

| 场景 | 必须维持的结果 |
| --- | --- |
| Begin journal 已 ACK，业务未提交就崩溃 | 原命令完整可读，冷进程只对账原 mutation/qualifiers；不推测执行成功，不自动重新发模型请求 |
| Begin ACK Unknown | remote 不发送业务效果；冻结并查询/关闭原 operation，缓存无权解冻 |
| 效果提交或 ACK Unknown | content/event/projection/state/control/receipt 仍是原原子组合；原 ID 对账，迟到 ACK/receipt 不制造重复效果 |
| 已确认 rejected receipt | 与现有幂等回放一致；原 state/control/消息/事件不受 reducer 的部分更新污染。允许原协议必要 journal/拒绝 receipt，不能宣称“零数据库写入” |
| 原命令重放、同 ID 不同正文/生命周期 | 前者返回原 receipt，后者按现有冲突拒绝；digest/字节缓存不掩盖身份冲突 |
| 非 canonical 旧 JSON | guard 用实际读出字节；不能用重新序列化的旧 state 比较。合法旧格式能继续读，缺省规则不变 |
| 冷态/历史未知字段、坏枚举、坏嵌套类型/整数、坏字符串 | mutation 完整验证失败，报原类别错误并记录诊断；不得因为该记录不在 delta 中而忽略 |
| request/response/intents 不匹配，后置版本溢出 | 私有编辑视图完全丢弃，业务效果不发布；失败发生在写入事务前或事务回滚内 |
| pause/close/reopen、旧 attempt、旧生命周期迟到结果 | 保持当前推进与仅结算的不同规则；旧 work 不重标当前生命周期；取消不清空整个处理义务 |
| ReasonInFlight / Blocked / ActReady / terminal | 前两者原有请求证据保留；ActReady 依已提交响应/intents 恢复；只沿用四入口现有 request trimming，保留其他历史 |
| 多实例或父子会话并发、control-only 写 | state 与 control guard、root/后代 pending 与原回执检查保持；不用缓存/进程锁代替跨实例事务事实，不引入 Peri owner lease |
| 删除、重建、read-only、shutdown | Store 身份/存在性/关闭与访问模式按原职责检查；缓存不能延长执行权或拥有部署关闭能力 |

## 9. 最小实施切片与确定性验收

按下表逐片独立可审查、可回退；不把所有阶段捆成一次大重构。生产只保留一份业务规则，旧实现作为测试基准可通过固定 fixture/预先生成的 golden 结果对比，不保留长期双轨实现。

| 切片 | 范围 | 必须达成的确定性门槛 |
| --- | --- | --- |
| A0 | 利用当前 `work/diagnostics.rs::WorkPhase` 增加按请求局部计数，分开耗时与 CPU | 记录命令 encode/hash、state decode/encode、payload materialize/copy bytes、SQL/事务/重试数；计数不得为测长度重新序列化。Drop 时间是含等待的 wall time，不能标成 CPU |
| A1a | `processing.rs` 元数据借用 + Agent checkpoint 外层包装 | 七处仅取 metadata 的路径完整 WorkRecord clone = 0；因 metadata 读取而复制 request/response 字节 = 0；外层 checkpoint Value 树复制 = 0，输出字符串序列化 = 1，旧/新 checkpoint 字节和 digest 相同 |
| A1b | prepared command 从 barrier 到两个 Adapter 与 reducer 全链迁移 | 每个新冻结命令完整 canonical encode = 1、摘要计算 = 1；同 handle 的 journal/effects/ACK/retry 重用额外 encode/hash = 0；两个 pending 和错误对象对正文的深 clone = 0。显式换 ID 后的新命令单独算一次 |
| A1c | SQL effects 参数共享、重复解析/availability 借用、缩短 snapshot 寿命 | effects 组装阶段同一 encoded command/state guard 不产生第二个完整 buffer；driver 最后物化独立报告。单次查询重复构造完整 availability owned map ≤ 1，借用版本目标 0；无关旧 snapshot 不跨写入 await 存活 |
| A3 | provider frozen body 共享（单独评审） | checkpoint 与 send closure 不各自 clone 一棵 body；发送前修改外部配置不改变冻结请求；实际发送请求与 checkpoint 内容、顺序及授权引用按现有契约一致 |
| B1 | 一个真实 BeginDispatch 用户路径贯穿 query/gate/两个 Adapter/Agent | Agent 完整 WorkSnapshot 返回数 = 0，返回 request 正文 = 0；所需全局阻断/guard 完整。初版内部 full decode 次数不超过旧基线；C 接入后按下一行验收 |
| C1/C2 | 全类型校验原文视图，再接同一 reducer + 整文写回 | 针对不读正文的 BlockWork/BeginDispatch：无关历史大正文持久 String/Value 物化 = 0、无关 payload escape/reencode = 0；完整结构校验覆盖全部记录；扫描/原文拷贝字节如实计数。拒绝时 state UPDATE = 0 |
| C3 | 两后端 delta compiler 与 SQL 差分测试 | 应用侧整份新 state serialize = 0；数据库最终类型化 state/receipt/effects 与基准一致，失败无部分效果；旧 raw guard 字节与传输量单列，SQL 重写字节不设“零”假门槛 |
| 可选 cache | 有界 cache 试验 | 相同 raw 的重复 decode = 0；外部同 revision 改 raw 必须 miss；冷启动/Unknown/淘汰与禁用时业务结果相同；所有驻留及 pinned cache 字节 ≤ 配置预算 |

计数以一次有固定动作序列的正常无冲突操作为基准，并分别测试 stale 后重签、同 ID 重放、Unknown 对账与冷 journal 读取；不得把这些必要额外验证混入“正常请求只一次”断言。command digest 与请求正文 request_digest、工具参数 digest 分别计数，后两者的必要检查不能被“一次 command hash”门槛抹掉。可用测试内部计数 sink/分配统计，不设 workspace 共享 test helper，不在生产导出 test-utils 接口，不靠 grep 调用点宣称运行计数达标。

## 10. 正确性与真实 benchmark 计划（本轮未执行）

先运行既有 request retention、reopen/terminal、journal 崩溃、effects guard、remote transport、Agent recovery/barrier 回归。新增用例集中验证新风险：从 schema17 fixture 打开且 sqlite_master/schema version/既有 command_json 不变；未知字段放在未触及的最旧记录；坏嵌套类型/重复字段/极值整数/Unicode 转义；非 canonical 原文 guard；同 revision 外部写；control-only 写；晚期归约拒绝；两实例竞争；begin/commit/ACK 三处故障注入；进程退出后的原命令重放。字段顺序与数字/转义 golden 必须验证 digest，不仅比较 Value。

对完整 action 集做差分：输入相同合法/非法状态和命令，比较 decision、revision、语义 state、control、events、projections、原 receipt 及错误类别；原未变 token 能保留则另断言 raw。特别覆盖当前 `staged_user_inputs`、`user_input_publications`、嵌套 terminal commands，不能仅覆盖 Reason/Act 主线。严格解码的接受集差异必须先解决，不能以“冷历史不相关”豁免。

建议命令使用仓库补丁脚本并加 `--locked`：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- session_resources::work::
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --test durable_work_contract
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- session_work
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- sessions::work::effects::tests
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- agent::stages::work
./scripts/cargo-rmcp-patched.sh test --locked -p peri-model --lib -- prepared_stream
```

新增测试须确认挂载和命中非零用例；若涉及 public/doc example，补对应 crate doc tests。native 与 Emscripten feature 闭包按已有构建路由核对；真实 Turso 必须单独报告，mock transport + SQLite 合同不能冒充线上网络部署验收。

benchmark 使用隔离临时数据库、合成请求和固定模型/工具结果，不访问现场 HOME/DB/凭据，不需要本轮执行。至少分三个层次：纯命令/归约/编解码、实际 SQLite/remote transport 存储、真实多 Agent 用户行为。最后单独做已配置测试部署的 Turso 回环，不能混用模拟延迟的数字。

工作负载建议：1/3/6/12 agent；每会话固定 live work 与 32/128/1024/4096 历史记录；请求 64 KiB/256 KiB/1 MiB；响应/intents 分别控制为小/大载荷。固定总工作量比较 CPU 秒/完成动作，再以固定每 agent 工作量测并发总 RSS；两种口径不混淆。覆盖无冲突、可控冲突、重放、Unknown、冷启动、热读取和 cache=0。长跑保留 journal 历史增长，不能每轮清空历史伪造 steady state。

分别记录进程 CPU user/system、分配次数/字节、峰值与稳定 RSS、请求吞吐、p50/p95 延迟、各阶段 wall time/锁等待、读取与发送字节、SQL/事务数、DB/WAL 写入字节及实际数据库 CPU（若可获得）。remote DB CPU 无观测权限则明确未测，不能把客户端下降当总成本下降。采样火焰图只定位，不把包含栈计数相加充当耗时比例。

使用同一构建配置与 workload、同机交替基线/候选，预热后至少 5 次独立运行；报告中位数、离散程度、冷态单独结果和原始计数。A 的确定性门槛必须全部通过，端到端 CPU/内存效果再看真实数据。建议性能晋级条件：目标大载荷多 Agent 的 CPU/动作与峰值 RSS 均有可重复改善，且小载荷/冷恢复无超过预先设定噪声界限的退化；若没有改善则停在已证明有价值的切片。具体百分比阈值待基线噪声测出再批准，不在无数据时许诺“降低 50%”。

## 11. 候选取舍、待裁决与不可承诺事项

| 候选 | 推荐程度 | 原因 |
| --- | --- | --- |
| clone/digest/serialize 减少 | 首选 A | 可定位、可计数、风险最小；不改持久形式，直接减少大命令重复处理。仍有整份 state 的线性成本 |
| 执行小视图与短生命周期 | 接续 B | 减少调用方历史驻留与无用途返回；先保留严格读取，再接 C，避免一次跨太大范围 |
| 严格校验的原文/选择性物化 + 原文写回 | 中期首选 C1/C2 | 冷态也可减少历史大字符串的分配与重复转义，正确性不依赖进程缓存；需要文本承载类型与 reducer 存取的实质重构 |
| SQL delta patch | 有条件延后 C3 | 能减少新状态客户端构造/发送，但 raw guard、严格校验、SQLite JSON 扫描和物理写入仍在；收益和 SQL path 兼容必须实测 |
| 全量 typed warm cache | 默认不启用 | decoder 降低可能换来更高 RSS和一致性复杂度；只有预算、外部写、Unknown 与净收益证明齐全才进入可选试验 |
| payload 外置、记录级表、压缩/JSONB wire、清历史 | 排除 | 违反本任务 schema/保守 wire/证据保留边界 |
| revision-only 缓存、仅校验触及记录、SQL 复制业务规则、跳 journal/guard/ACK | 拒绝 | 不能保持现有坏历史失败语义、跨实例一致性或 Unknown 证据链 |
| 降低并发、合并掉语义 mutation、降低 TUI 刷新频率 | 不作为本方案 | 改变工作负载或可观察行为，未解决已证实的重复数据处理 |

需用户裁决的是**后续阶段和复杂度预算**：A1a/A1b/A1c 已实施；B1 是否继续以性能验收决定，A3 与 C1/C2 各自评审，C3/cache 以测量结果再决定。对 raw_value feature 和内部文本泛型传播须明确包含在 C 授权内。保守 wire 约束如需放宽，应另起设计，不能把“没改 DDL”当自动授权。

不能承诺消除历史增长、journal 保留成本、数据库整行读写、跨网络旧 guard 传输、模型实际 HTTP 序列化或 TUI 绘制；这些下界与本方案范围并存。无 schema 路径有现实优化空间，但不是记录级存储的等价替代。

DOC-UPDATE-001 核对：阶段 A 的实际接口与入口已同步 types/resources/agent code-index；本方案其余候选仍待裁决，不进入已批准设计目录。完成记录与验收统一指向 CPU issue，避免多份进度事实源。
