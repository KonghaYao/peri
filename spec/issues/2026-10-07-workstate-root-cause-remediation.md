# 多 Subagent WorkState CPU / 内存根治：增长模式与约束裁决

> 已被 2026-10-07 删除整个执行恢复机制的裁决取代：以下为备份保留的旧提案，不再实施 Work 存储、pending 目录或现场迁移。现行规则见 `docs/design/rcra-message-activation.md`。

状态：**设计提案，待裁决，未实施、未运行实验**。作者：Astra。日期：2026-10-07。

审查基线：阶段 A 提交 `f36fbed5`；本轮读取时 HEAD 为 `67087e9a`，相关 Work 核心路径已核对。共享树既有 Cargo.lock 和其他 issue 改动不属于本任务。本轮只新增本文，不运行 Cargo、不访问现场 DB、不迁移、不提交、不分派 agent。以下计数是指定分支的源码推导，百分比收益与现场资源占比均未测得。

入口：[现有 CPU issue](2026-10-07-multi-subagent-cpu.md)、[阶段 A/B/C 提案](2026-10-07-workstate-no-schema-optimization-proposal.md)、[schema17 重设计约束](2026-10-07-workstate-schema17-redesign.md)、[Resources 索引](../../docs/code-index/peri-resources.md)、[Agent 索引](../../docs/code-index/peri-agent.md)、[Types 索引](../../docs/code-index/peri-acp-types.md)。规范已按 root/Agent/ACP/middlewares 指引及 standards 的 architecture/rust/testing/documentation 路由核对。本文不是已批准 design，也不覆盖现行契约。

## 1. 决策：拒绝把进一步省拷贝称为根治

**推荐目标是“活动状态与历史记录分离 + 可寻址历史证据 + 事务化 pending 目录”，不是跨请求完整 WorkState 缓存，也不是对整行做 JSON patch。** 固定活动工作量时，热执行不再读取、验证、物化、重写已完成历史，不再扫描全库命令 journal。历史保留，按身份点读或分页读取；恢复从当前活动检查点开始，不回放全部历史。

**最小批准集合前置：① 新持久文档协议和历史组织（DDL 仍为 schema17）；② 将无关坏历史的检查从每次 mutation 改为按需读取/完整审计；③ pending 目录随 begin/ACK 原子维护、一次性迁移和全体 writer 升级，禁止旧 writer 绕过目录混写。** 被消费文档仍保留原文 CAS。任意未消费历史的同 revision 外写不保证下一次无关 mutation 立即发现；不接受这点就保留全扫。无需批准删历史、缩短 retention、改变原 command/receipt 或削弱 Unknown/ACK。现场迁移执行仍须另行授权。

必须先承认两个结论：

1. **在当前全部约束同时成立时，不可能根治。** 保持现有单行完整 JSON、每次全量严格历史校验、原始全文 CAS、原命令 journal、无限历史及现有 pending 查找组织，就保留了历史字节扫描、整行逻辑写入和全库 journal 扫描的下界。A/B/C 可以降低常数、物化量与快照寿命，不能把成本改成仅依赖活动工作量。
2. **DDL 保持 schema17 有条件可行，但必须改变持久文档协议及历史权威组织。** 本文推荐显式版本化的活动 head、历史记录与 pending 目录复用既有主键索引；这是另一次存储协议设计，不是“表列没变，所以已获授权”。即使获批，永久保留的总磁盘量、冷历史全读、单次真实大请求和 B-tree 查找成本也不会消失。

无需放宽持久协议的候选交付，仅是第 8 节 S1 的精确读取接口与生命周期收敛；实施仍需确认，本轮仅设计。**真正改变增长模式的 S2–S6 必须在第 9 节裁决后启动；若不批准，结论就是优化有上限，而不是继续承诺根治。**

## 2. 操作级 dataflow 与重复成本

### 2.1 统一符号与口径

- `H`：本会话已经结束、但仍在 WorkState 中的历史记录及 payload 字节；`L`：活动/待结清工作及其依赖字节；`S=H+L`：聚合 state_json 字节量。
- `P`：本次完整原 command_json 字节；`D`：本次状态变更及新增数据字节；`J`：全库 session_work_commands 行数；`U`：查询子树内尚未 reconciled 的命令数；`A`：父链深度；`M`：本会话 canonical 消息数；`K`：本轮实际获准并进入 dispatch 的工具调用数，不是工具目录大小。下文沿用 `C=P` 表达 SQL 参数字节，父链深度统一记 A，与变更 D 区分。
- `R`：一次操作经历的存储 CAS 尝试次数，remote 当前上限 8；Agent 明确 stale receipt 后的新命令重试是另一层，不能与存储尝试混成“一次”。
- SQL 参数位、逻辑传输字节、实际 HTTP 次数、WAL/物理 IO 是不同量。下面参数字节不包括 transport JSON 转义、SQL 文本、TLS/压缩及底层 driver 再复制。

### 2.2 从 Agent 到两种 Store

`work_reason/work_dispatch/work_receive → WorkSession::snapshot/command → WorkMutationBarrier → SessionResources → MutationGate → SessionDataPort → SQLite 或 remote → reduce_work → mutation_effects → 持久 receipt → ACK → Agent 采纳`。

关键证据路径均为仓库相对路径：

| 操作及前提 | Agent 直接完整 snapshot 次数 | 写入次数与额外数据流 |
| --- | ---: | --- |
| `work_reason::recover_reasoning`，进入 Reason 时 | 1 | 即使当前是 ReasonReady、最后返回 None，先完整恢复查询；`reason.rs::run_reason` 调用该入口 |
| `work_reason::prepare`，已绑定 session | 1 | BeginReason 1 次；checkpoint 是实际 prepared request 的借用式序列化；预算事实读取后释放 snapshot，再提交；写入前验证请求 JSON、digest，仍复制请求到新 state |
| `work_reason::commit_response` | 1 | CommitReasonResponseAndDispatchIntent 1 次；每个 intent 绑定目标；响应进入 source work、可选 successor、projections、command |
| `work_reason::mirror_response`，Act 走该分支 | 1 | 无写入；从完整 state 校验已提交响应，然后 mirror transcript |
| `work_dispatch::begin`，每个实际获准工具 | 1 | BeginDispatch 1 次；在全量 invocations 中按 work/tool-call 找记录，复制一个 intent；`K` 个工具放大成 `K` 次完整读和 `K` 次 mutation |
| `work_dispatch::commit_results`，成功且无冲突 | 2 | CommitAct 1 次；提交前事实、提交后 settled projection 各一份完整 snapshot；每个 result 线性找 invocation，存在 `K × 历史 invocation 数` 的比较成本 |
| `work_dispatch::unknown` / `block_uncertain_model` | 各至多 1 | 条件满足再写 1 次 OutcomeUnknown / BlockWork；先冻结执行 |
| `work_receive::receive_batch` | 1 + 发布成功后的 1 + 首次 claim 后的 1 | 每个新 delivery 独立 publish mutation；首次 claim 1 次；recover_work 使用最后的同一 snapshot，阶段 A 已消除部分重复 |
| `WorkBoundary::ensure` 首次绑定 | 分支相关，不能摊成固定每轮次数 | 缺 SDK ticket 时另读 admission snapshot、持久注册；绑定后读验证 snapshot；已绑定正常分支不额外读完整状态 |

每次完整 snapshot 都进入 `work/query.rs::WorkSnapshot::from_state`；`limit=1/64` **只限制 candidates，不限制返回 state 或 pending 命令正文**。SQLite 是一个读取事务中的 `READ_SNAPSHOT` + `READ_PENDING` 两条 SQL；remote 是一个 consistent `read_batch` 中的同两条 SQL。pending 原命令还经过 `original_command → decode → PreparedWorkCommand::try_new`，包含完整 canonical 编码/摘要检查。

例如固定已绑定、全部 `K` 个工具获准、无冲突且确实调用 mirror_response 的 Reason→Act 片段，不计 Receive/首次绑定/compact：Agent 完整读 `K+6` 次，mutation `K+3` 次；每个 mutation 自身再完整读取一次，合计 `2K+9` 次 WorkState 解码、`K+3` 次新状态编码。这不是所有轮次的通用常数：mirror 分支、工具拒绝、恢复、SDK、错误与重试都应按真实 trace 增减。

### 2.3 SQLite 单个新命令正常成功

事实源：`peri-resources/src/sessions/resources/{gate,gate_work}.rs`、`sqlite_store/session_data/work.rs`、`sessions/work.rs`、`work/effects.rs`。

1. `apply_work → work_root → scope(root,true)` 获取**同一家族 root 排他屏障**，`check_owned_work → HAS_PENDING` 先做一次 durable pending 查询；屏障跨后面的 journal begin、effects、ACK。它承担家族 Unknown 隔离，不应替换成每 session 锁来掩盖性能。
2. 第一个 `BEGIN IMMEDIATE`：READ_RECEIPT 1、READ_COMMAND 1、INSERT_COMMAND 1、GUARD_COMMAND 1，提交 journal。完整命令在 INSERT/GUARD 两个参数位出现；GUARD_COMMAND 检查同 ID 精确身份及同 session 其他 pending。
3. 第二个 `BEGIN IMMEDIATE`：READ_SNAPSHOT 1，严格解码完整 WorkState 1 次；保留原 state_json 作为 CAS，不重新编码旧 raw（缺行才编码初始 state）；reducer 消费独占状态，accepted 后编码完整新 state 1 次。
4. effects 再次 INSERT_COMMAND/GUARD_COMMAND，接着 INSERT_STATE/GUARD_STATE；accepted 才做控制变化、事件、消息投影、UPDATE_STATE；最后 INSERT_RECEIPT。无事件/投影/控制变化的 accepted 命令是 6 条 effects SQL；rejected 是 5 条，无新 state 编码/更新、无投影事件，但有原始状态 guard 和拒绝 receipt。
5. 第三个 `BEGIN IMMEDIATE`：ACK_COMMAND 1，提交，再返回。正常最小 accepted 路径在 adapter 内是 `4 + (1+6) + 1 = 12` 条业务 SQL、3 个写事务；不计 BEGIN/COMMIT、本门面 root/HAS_PENDING 查询及额外效果。已有 receipt 重放走短路，不套这个计数。

命令 `C` 在 journal 与 effects 共出现 **4 个 SQL 参数位**；阶段 A 使它们共享 Arc，不等于 SQL 引擎只消费一次。旧 state `S_old` 在 INSERT_STATE/GUARD_STATE 两处绑定；accepted 新 state `S_new` 另绑定一次。raw、类型树、编码串和 params 的存活部分交叠，RSS 不能仅按最终 state 大小估计。

### 2.4 remote 单个新命令正常成功

事实源：`remote/{session_work,session_work_journal,mutation}.rs`。

- 未命中 receipt → READ_COMMAND → begin qualified mutation → 循环内再查 receipt → 一次 READ_SNAPSHOT consistent batch → reducer/effects → effects qualified mutation → ACK qualified mutation → 最后查原 receipt。
- 不计 `store()` 重连/身份复核与 gate，在完全正常无竞争分支是 **8 个 Store 数据调用**：4 次 fetch_row、1 次 read_batch、3 次 apply_qualified。后者各附加第一条 operation qualification SQL；不能把此计数当实测 TCP/HTTP 请求数。
- effects 编译同 SQLite。`Value::Text(value.to_string())` 在 SQL adapter 边界为每个参数位物化字符串；`apply_qualified_reporting` 又 `effects.iter().cloned()` 构造托管批。因此阶段 A 的 Arc 共享没有消除 remote 字符串深复制或发送放大。
- 仅列大参数，正常成功上行含约 `4C + 2S_old + S_new`，下行含 `S_old`；事件/投影正文各 INSERT+GUARD，另加其重复参数。每个完整 Agent snapshot 再下行整份 state，pending 存在则另加原命令。
- CAS 竞争命中目前 `rejected_statement: Some(4)` 才重试，最多 8 次。序号 0 是 operation qualification，序号 4 对应 Work GUARD_STATE。每轮重读/解码/编码/发送旧新全文；非 state guard 失败不得误重试。Unknown 走原 operation 终态封闭与 journal 对账，不能据“已缓存下一状态”宣告成功。

### 2.5 另一个独立增长轴：全库 journal 与历史规则扫描

主 Agent 的独立审查与本轮代码核验一致：

- `sessions/work.rs::GUARD_COMMAND` 显式 `NOT INDEXED` 扫描 `reconciled=0 AND session_id=?`；journal begin 和 effects 各一次。
- `READ_PENDING` 对整个 commands 表 `NOT INDEXED`，过滤子树后 `ORDER BY mutation_id`；`HAS_PENDING` 同样全表扫描，空 pending 的常见路径尤其不能提前命中返回。
- `sessions/control.rs::GUARD_STATE` 也按子树扫描 pending；它没有适用的 session/reconciled 索引，不能因未写 `NOT INDEXED` 就推断有索引。
- `MutationGate::scope → check_owned_work` 在每次 apply_work 前调用 HAS_PENDING；完整 snapshot 又调用 READ_PENDING。因此即使自身 H 很小，无关会话累计 J 也能增加 CPU/读页。
- `work/effects_test.rs::pending_sql_preserves_subtree_order_and_uses_table_scan` 显式断言 `SCAN session_work_commands`、临时 B-tree 排序且不走 mutation 主键。只删 `NOT INDEXED` 不是根治，也不能悄悄删该回归的行为依据。
- `delivery::publish` 遍历历史 delivery 验证同 event/key、再收集 pending；`processing::begin_reason` 遍历所有 works 检查 request_id 唯一；`query/availability` 复制/扫描历史 metadata；`admission::{register,bind_work_delegation}` 仍为构建 snapshot **clone 整个 state**。它们不属于阶段 A 已删的七处 WorkRecord clone。
- 消息 projection 的 `REFRESH_COUNTS` 对本 session 全部 messages COUNT，另有 projection payload 再解码取 title。仅解决 WorkState 仍不能保证整个操作不随消息数 M 增长。

## 3. 根因、渐进复杂度与不可达目标

| 根因 | 现状成本与影响 | 怎样才改变增长模式 |
| --- | --- | --- |
| 完整历史与活动检查点同一 JSON | 每次 snapshot / accepted mutation 至少 Ω(S) 文本读取、解析或编码；反序列化树通常另有 map 构建成本 | 改持久组织与读取契约，历史离开热文档 |
| 每次严格读取都检查所有历史类型 | 即使借用 raw、不分配大 String，最坏仍 Ω(H) 扫描 | 显式裁决历史校验时机；不能靠 RawValue 自动获得等价校验或免检 |
| 远端原文整行 CAS 和全文更新 | 传输 Ω(S)，冲突时乘 R；单行写并不是单条小记录写 | 缩小被 CAS 文档与写集合；同 revision 外写仍比较相关原文 |
| 全库 pending 查找 | 常见无 pending 与最坏路径 Ω(J)，返回 U 条还要排序/正文验证 | DDL 索引或新的可寻址 pending 目录；本方案只能选择后者并申请持久语义授权 |
| 领域对历史全扫描 | delivery/request 去重、availability、invocation 查找依赖历史记录数 | 可寻址历史 identity 证据、活动集合查询；不能简单删掉去重规则 |
| journal 长期保留完整请求 | 总空间 Ω(ΣC)，BeginReason 的 C 含整个模型输入 | 永久保留则接受该下界；若要消除，需另批 command/payload 语义或 retention 改变 |
| 投影重复与 canonical COUNT | 响应/结果在 command、state、event/message 多处保留；COUNT 随 M | 明确 payload 引用权威；原子增量计数独立切片，不能挪进“省序列化”名义 |
| 多子会话并发和 root 屏障 | CPU 是各会话之和；RSS 含并发对象；同 root 的三阶段写入串行，读/准备仍可重叠 | 缩小屏障内工作，保留家族冻结；不降低并发、刷新率或取消恢复能力 |

若第 t 个 mutation 的历史 `H_t` 随 t 线性增长，累积全文处理 `ΣH_t` 为二次增长；同库 J 持续增长时 pending 全扫也有同类累计放大。若模型 prompt 自身随对话增长，`C_t` 的累积也可能二次增长——把 state 历史移走不能消除模型实际输入这一项。

RSS 要分清可达性：当前完整 state 物化与 raw 缓冲需要与 S 相关的瞬时内存；共享可降低倍数，完整 typed cache 会把历史变成长期驻留。流式验证可减少 typed 历史分配，但不能同时消除完整扫描与现有 SQL/transport 大字符串。多 agent 的峰值依赖实际重叠，不能用“子 agent 数 × 固定 MB”伪造公式。

**不改 DDL 能否同时去掉历史相关 CPU/RSS、网络和物理 IO？**

- **连 JSON/权威/校验时机都不改：不能。** JSON patch 可少发 delta，但原文 CAS 仍发旧全文，数据库 JSON 求值仍扫描/产出大值；若换 revision guard 则已削弱同 revision 外写保护。压缩/JSONB 也改变格式且不能使历史不存在。
- **允许新文档协议：可以消除热 mutation 对历史 payload 的线性读写与全 J pending 扫描**，但仅在固定 L/U/本次 payload 的口径下；B-tree 查找仍 O(log 总记录数)，历史总空间仍 Ω(H+ΣC)，全历史读取/完整审计仍 Ω(H+ΣC)。因此不能宣称“所有历史成本归零”或“磁盘有固定上界”。
- 现有 UPDATE 的逻辑输入与输出下界是 Ω(S)。SQLite 页缓存、overflow 页复用、WAL/checkpoint、压缩与底层存储会影响真实磁盘写量；本文不凭源码宣称每次 fsync 必写 S 字节。应以冷缓存读页、WAL 增量、checkpoint 写页及 remote 服务端计量证实物理放大。新布局目标是只触碰有限文档/索引路径，而不是仅把应用 CPU 移到服务器。

## 4. 复用已有表的可行性：先否定不成立的前提

### 4.1 现有 journal/events 不是可直接回放的完整事件源

`session_work_commands` 只有 mutation_id 主键，没有 session/reconciled/revision 索引，没有提交序号；mutation_id 的字符串顺序或 UUID 时间不是提交顺序，非 UUID 的 admission 等命令 ID 也存在。reconciled 表示命令已完成对账，并不等于 accepted 或一次状态迁移。

`session_work_receipts` 的 before_revision/revision 有局部顺序信息，但不能据此建立“一条 Accepted 对应一个连续事件”：`reduce_work` 首先走 `admission::prior_receipt`，可直接返回历史 accepted receipt 和当前 state，不递增 revision、不产生 events/control；拒绝、NotApplied、重放也需要区别。原 receipt.mutation_id 还可能属于旧 admission 命令，不能按当前 journal 主键猜它是新 transition。

`session_work_events` 当前只是 `event_key=encode((producer_namespace,event_id))` 的投递事件和 WorkPayload；生产 `events.push` 在 `delivery::publish`，BeginReason/BeginDispatch/CommitAct 等没有相应完整事件。messages 是 canonical 展示/模型投影，不包括所有 prepared 请求、授权、budget、invocation intent、未投影 delivery 或 terminal obligation。control 有独立状态和 receipts，Work journal 没有完整的独立 ControlCommand 历史。控制 seed、缺 state 时 legacyUnknown、旧库没有原命令的 receipts 也不能凭 replay 补造。

所以 **“保留 journal，清空已结束 maps，重启从 journal 重放”不可采用**。它既可能丢恢复事实，也会让 cold 恢复变成扫描/排序全 J、回放全会话历史，并暴露 reducer 版本变化和跨 session receipt 的依赖顺序问题。

### 4.2 payload 引证候选

| 候选 | 可用性与限制 | 裁决 |
| --- | --- | --- |
| 引 messages 的 message_id | 已投影 response/result 可按主键查；prepared request、未投影 delivery、intent 不能保证在 messages；消息 flags/更新/删除与 Work 恢复的生命周期不同 | 不作通用 payload 权威，不把所有历史消息解码/改写捆绑进迁移 |
| 引 command 的 mutation_id + typed field selector | 可精确找已知原命令；读取仍要校验完整大 command，且同一 payload 多次使用仍付 C；legacy 可能无命令 | 新格式可以选择用于历史佐证，但不能作为全部 payload 的恢复基线 |
| 只把大字段从 state 换成引用，metadata 永久留在一个 map | 去掉大字节重复，但 maps、严格历史校验和全 J 扫描仍增长 | 部分优化，不作为最终架构 |
| 复用 state 表多行做 head/record/blob | 主键支持点读，物理可行；原 session_id 列不再只指 session，任意旧 session ID 与内部 key 可能冲突；旧 reader/seed/list/清理逻辑需整体升级 | 是新 row/document 协议，不能称为原格式内优化；本文不推荐同时复用两套载体 |
| 复用 events 表主键承载明确分型的内部文档 | 两 TEXT 列与已有 PK 可容纳定址记录、分页 manifest、pending 目录；但 event_json 将不再只表示 WorkEvent | **推荐的无 DDL 载体，须单独批准新 namespace/文档语义及全体 reader 升级** |

### 4.3 推荐布局：活动 head + 定址历史记录，不以 command replay 为恢复权威

以下类型和字段都是**拟新增协议**，当前未存在、未授权。schema 版本、表列索引保持 17；另设显式文档 format 版本，不能偷塞进现有 WorkState 的未知字段或借现有字段改变解释。

- `session_work_state[session_id]` 改为 `WorkHeadV2`：format、domain revision、独立 storage sequence、limits/next_admission_sequence、活动集合目录、历史目录根、基线引用。头只存有限标量和目录引用；活动列表按条数和字节分页，不把无限历史 ID 留在头中。storage sequence 记录本存储提交顺序，**不替代 WorkReceipt revision**。
- `session_work_events[event_key]` 中新建 `WorkStorageDocumentV2` 的带类型文档：实体 head、不可变实体版本、payload、identity 索引、history manifest 页、pending head/页及迁移 manifest。原 WorkEvent 继续原路径，不混用其领域语义。
- key 使用规范序列化的带类型 tuple，例如 `['peri.work.storage/v2', kind, session_id, identity, version]` 的 JSON 表示；现有 event_key 是二元 tuple，新键有不同 arity，避免字符串拼接转义与合法旧二元 key 冲突。上线前检查既有非规范/外写 key；禁止覆盖冲突。namespace 属私有持久协议，不暴露给模型/ACP。
- payload 优先引用**经证明的原不可变 command**：拟议 `CommandPayloadRef { session_id, mutation_id, command_digest, selector, payload_digest, byte_length }`，selector 是 typed action/field 定位（如 BeginReason.request、CommitReasonResponseAndDispatchIntent.response、CommitAct.results 的 invocation_id），不是任意 JSONPath。点读后沿用 canonical command digest 校验、核对 session/action/target/字段身份及 payload 内容；新 head 只能在对应 command effects 与 receipt 同事务确认后引用。BeginReason 请求、已提交响应/结果逐类证明可以恢复才启用；保留原 journal 不再为这部分另造 blob 副本。
- 缺原命令的 legacy 基线或不能精确映射的字段，才由一次性转换/新实体写入显式的会话范围 immutable blob，引用含 key/长度/digest/kind；不存在的来源必须 fail closed，不能自动用 messages 猜补。每份 payload 只有一个选定来源，不能长期双写 command/blob 再比较选 winner。引用 command 仍需读取和验证整个 P，可能含同批其他结果；引用只能避免额外复制/反复 state 编码，不保证只读取一个字段大小。该选择是 payload source，不把 journal 提升为全状态事件源，也不改变原 command wire。
- 历史实体保留足以复原原 WorkState 记录的内容，按 `(session, entity_kind, identity)` 点查实体 head，指向不可变版本；manifest 采用有界页及可寻址 next/root，不把所有 version/ID 放入一份 growing JSON。单页预算是实现门禁，遇到巨大单实体由 blob 承载，不能截断。
- “归档”只转移热集合成员资格，不删除事实。已 Settled/Abandoned 但仍关联 DispatchAccepted/OutcomeUnknown、terminal obligation 或迟到结果的记录仍属于待结清依赖图；旧生命周期的未决义务不能因 reopen 消失。纯历史实体如需被合法迟到结果更新，生成新版本、原子换实体 head，保留生命周期检查，不重新激活旧执行。
- 历史去重不是概率集合：delivery_id、`(producer,event,lifecycle,purpose)`、request_id、invocation_id、admission_id、binding owner/task 等按现行规则建立精确 identity 文档。缺记录与记录冲突可区分；同一 canonical 事实只有一个实体权威，identity/head/manifest 是同事务维护的索引，不能独立修改成第二套业务规则。
- 不承诺所有 L 有硬常数上限：未结清义务、活跃子树、输入 backlog、单请求 C 可以真实增长。目标是历史 H/J 不混入每次工作集；若要再限定 L/U/C，需要另批容量/拒绝或 retention 语义。

这相当于在既有 PK 表上实现有限的文档目录/索引，复杂度明显高于新建规范化表。**若不接受此维护代价，就应另行请求解除 DDL 限制；不得把复杂索引藏进 JSON 再称“零架构改动”。** 本文在 DDL 不变的条件下明确选择这一布局，不并行保留另一个权威存储方案。

## 5. Module、interface、ownership 与事务

### 5.1 责任与接口

| 层 / 拟议模块 | 小接口与责任 | 删除目标 |
| --- | --- | --- |
| types `session_resources/work/{execution_view,records,reducer}.rs` | `WorkExecutionQuery/View` 按 work/invocation/purpose 返回 guard、budget、阻断事实和必要 payload；`WorkFacts` 提供活动集合与精确历史证据；单一 reducer 产出私有 `WorkDelta` | 业务代码中按完整 history maps 扫描找目标；不得复制一套 SQL 业务 reducer |
| resources `sessions/work/{document,repository,pending,history,effects}.rs` | 版本化 codec、`load_execution`、`load_recovery`、`read_history_page`、`prepare_transition`、`compile_effects`；返回 read-set 原文 token 与 effects role | 热路径 `decode<WorkState>/encode<WorkState>`、隐含完整 snapshot 依赖、magic index 4 |
| Agent `work_pipeline/work_reason/work_dispatch/work_receive/work_boundary` | 一次取需要的事实；只持当前工作与不可变 prepared 命令，receipt 确认后消费已提交 projection | 每个工具和提交后为了一个结果读全部 WorkSnapshot；保留明确的完整历史查询语义 |
| gate / SQLite / remote | root 屏障、Unknown 持有与事务执行；共用 effects 编译；adapter 不认识 stage 规则 | 各种 pending 全表 SQL 与分散的目录更新规则 |
| ACP cold execution/terminal + subagent settlement | 通过同一 recovery/exact-receipt 端口做生命周期与跨 session 对账 | 从完整 state 任意拆字段的恢复代码；不新增 ACP 调度权威 |

`WorkFacts` 不让 reducer 同步偷偷发 IO：先由领域计划列出 action 所需键，repository 一致读取；reducer 如需额外事实返回明确 NeedFacts，adapter 补读后重新验证整个 read-set，随后仍执行同一 reducer。全局谓词消费活动集合及精确 identity 证据；不会为了“方便”临时拼回全部历史。限制补读轮次并以固定动作 fixture 验证，防止 N+1 历史查找。

`WorkDelta` 对 caller 不可变，包含写集合、投影、控制效果、receipt 与历史版本变化。拒绝时 drop 私有 edit，不允许 state 部分修改进入 cache、head 或任何目录；只持久拒绝 receipt 及既有 journal/ACK 效果。缓存非必要，若后续引入只缓存有大小预算的不可变文档，不能用 revision 作为全文校验的替代。

### 5.2 Pending 目录：这是根治必要条件，不是可选后续

现有 gate 的 root 排他语义保持。新 `PendingHead(session)` 是可定址目录的根，包含该 session 自有 pending 索引与子树 pending 索引/计数；目录成员只有尚未 ACK 的 `(owner_session, mutation_id, digest)`，不能永久追加所有已结清 ID。正文仍唯一由 commands 主键定位。

begin 事务同时插入原 command、核对精确身份、登记自身 pending 成员，并沿**可信且在事务中校验的父链**更新祖先子树目录；ACK 事务同时置 reconciled=1 和删除该命令的 pending 成员/收敛空页。每个目录页有字节与条数上限；大小依赖 U，更新访问依赖 A 与索引深度，不依赖历史 J。哈希/计数不能单独证明完整性；目录页丢失、counter 不符、journal identity 不符都 fail closed，不能当“没有 pending”。

`HAS_PENDING(root)` 读 root 的 head；`READ_PENDING(any_subtree)` 从其 head 枚举未决成员、按 mutation_id 保持原排序，再点读原命令；同 session GUARD_COMMAND 查自有 pending。可在内部分页，但现有完整 pending 返回仍需 O(U) 内存/正文，不承诺常数。控制 GUARD_STATE 在**同一事务**检查相同 pending head 原文与空集事实。begin/ACK 与 root、child 目录跨行原子提交，两端都必须成立。

为何不能仅在 journal 写入后异步补目录：崩溃会留下不可发现的原命令；先删除目录后独立 ACK 则会过早放开 root。目录必须随 begin/ACK 同生共死；操作重放只能幂等地得到原成员结果，不能再次加减计数。原命令的效果已生效而 ACK 未知，目录仍保持冻结直到真实 receipt 与 ACK 被确认。

同一 root 的进程内锁仍跨三阶段；跨实例不信任此锁，事务 CAS 与目录守卫处理竞争。**不扩大现行同 session journal guard 成跨实例 root 执行租约**：SDK 继续负责执行唯一性；root 目录是 durable pending 发现与控制冻结事实。需要更强的跨实例家族准入原子性时另按既有契约验收，不能顺手改变父子可并存 pending 的语义。

目录完整性仅在所有 writer 遵循新协议时可保证。任意外部 SQL 只插 command、不更新目录的能力与“不扫描 journal 却必然发现遗漏”不相容；不能用 WAL/data_version、单个 revision 或进程 cache 假装证明。迁移必须重建/核对目录、切断旧 writer；外写完整性边界是明确待裁决项。

目标成本表达为：读取本次需要的活动字节 `L_needed`、原命令 `P` 与被消费历史证据，写入 `D`、被修改的有界页及原命令；另付目录更新父链 A、真实未决 U 的枚举和 B-tree 查找。无需把所有 live L 每次全部返回，但 cold 恢复完整活动闭包仍至少承担其实际大小。**没有 H/J 的线性热扫描，不等于 O(1)，也不等于 P/L/U 或永久保留的 journal 磁盘量零成本。** 显式全历史读取/审计保留 O(H) 字节成本，若审计 journal 则另计 J 与其正文。

### 5.3 SQLite 路径

1. root scope 内，begin 的 `BEGIN IMMEDIATE` 同时写完整原命令和上述 pending 目录，提交；仍有独立 durable begin 阶段。
2. effects 的 `BEGIN IMMEDIATE` 读当前 WorkHead、control、活动记录、必要历史证据与 parent receipt；严格验证被消费文档、原文 token、absence 事实。reducer 生成 delta；同事务写实体版本/blob/索引、canonical 投影、control、receipt，并原子发布新的 head。
3. 原文 CAS 保留在 head、实体 head、pending head 等可变 read-set 上，版本字段不能单独代替原文字节；只改小集合。不可变版本命中须身份/正文一致；声明不可变不等于允许忽略被读取 blob 的损坏。
4. ACK 独立事务原子更新 reconciled 与 pending 目录。三阶段间断电由原 journal/receipt 与 pending 目录恢复。拒绝不发布新的历史页/实体效果；发生 SQL guard 失败整批回滚。

### 5.4 remote 路径

保持 operation identity、begin/effects/ACK/seal 与 Unknown 终态封闭。begin/ACK 目录更新进入各自同一个 qualified mutation；effects 的一致读使用 `read_batch`，再提交含全部 read-set/absence/control/parent-receipt guards 的原子批。动态发现引用需两轮以上读取时，使用 immutable 文档引用并复核所有可变 head；读结果不是天然同一快照，提交必须 guard 全部 token。

guard 失败仅在 transport 明确确认 NotApplied 时允许重读/reduce；未知回滚不归类 stale。compiler 返回 `EffectRole → submitted_index`，含 qualification 偏移；StateConflict、PendingConflict、ParentReceiptConflict、IdentityConflict 不能靠硬编码 4 混为一类。原 command 与 digest 不变；只有 Agent 收到明确 stale 拒绝后才按现行规则生成新命令 ID。

热网络目标为本次 read/write-set、活跃页与 C 的字节量，不含无关 H；大 command 在 begin/effects 中的重复参数仍需如实计量，保留原命令 wire 时不能说已被 blob 引用消除。父 child receipt 验证与 head 更新在同一个 remote batch 中完成，不拆为“先看成功再提交”。

### 5.5 冷恢复、基线与历史权威

- 当前执行权威是已提交 head 可达的活动实体版本及 control，pending 目录定位原 journal，receipt 证明 mutation 结果；这三类职责不同，没有相互覆盖的第二份执行状态。commands 仍是命令身份/Unknown 证据，不负责重建全部活动状态。
- cold 先读取持久 head/pending，按原命令 resolve；再恢复活动闭包的 request/response/batch/projection_version/budget/invocation/授权/owner locator/terminal handoff。ReasonInFlight 不重发模型；DispatchAccepted/OutcomeUnknown 查原 owner；未知不猜成成功。
- 历史查询按 entity/manifest 分页；确需原完整 WorkSnapshot 时从同一 committed head 构造完整结果，明确仍 O(H) 并执行完整校验。它是现有外部读取契约的显式实现，不是热执行的空历史冒充。SDK admission 若只需摘要，应消费精确投影；若外部 wire 仍要求完整 state，不能未经授权去掉，且其成本单列。
- 新 history commit 元数据记录独立 sequence、前驱、command/receipt 引证、实际 delta 身份与所见 control token；用于顺序与审计，不声称所有现有控制变更都已形成事件源。Accepted 原 receipt 重放可为 no-change，不伪造 Work revision 增长。
- 如果未来要把 journal 升级成“从零重放的唯一状态权威”，还需完整 control 事件、基线、reducer/codec 版本、跨 session causal order、可索引的 commit 顺序和 compaction 规则。这是另一授权项，本推荐不需要也不默认包含。

## 6. 严格坏历史校验、legacy 与切换

**默认仍保留现行严格契约；目标布局必须单独批准下列取舍。**

现行 `work::state → serde_json::from_str<WorkState>` 每次整读拒绝历史记录中的未知字段、错类型、非法枚举等，但不等于逐个深度验证所有历史 payload 字符串内部语义；不要把现行契约夸大成已存在的全历史密码学审计。

推荐新契约为：写入时完整验证新文档；每次 mutation 严格验证活动闭包及本次消费的历史证据；按需历史读取完整验证被读取记录；显式完整审计遍历全部历史。这样会把“一个未被使用的旧记录坏了，下次任意 mutation 立即失败”改成“访问该记录或完整审计时失败”。**这是可观察的校验时机变化，必须获批；若拒绝，保留全历史扫描，根治门槛不能通过。** digest/immutable 声明只能验证读取到的数据，无法发现未读取记录被同 revision 外写。

迁移不是启动期扫描全 messages：

1. 显式维护窗口冻结全部 writer（SDK/其他实例/旧程序），先对合成/副本库验证，再获得独立现场迁移授权。未知 pending 保留原命令并完成对账，不能删 pending 为迁移开路。
2. 对每个 Work 会话读取原 raw state/control，严格解码建立**完整 Work 基线**，直接从它形成版本记录、identity 索引和活动闭包；保留 limits/预算/序号/legacyUnknown/资源 owner/child metadata/所有 handoff 事实。不能从 messages 或不完整 journal 推断缺失状态。
3. 旧 WorkState 已裁剪的 request 不补造；旧 receipt 无 command 按现有 QUARANTINE_UNOWNED_COMMANDS 保持 LegacyUnknown；缺 state 但有 messages 仍是 pre-ledger-history，不伪造完成。无需解码全部旧 messages；历史坏消息不阻断无关 Work 基线转换。
4. 必要的全库 journal 扫描只在受控迁移/审计执行一次，用于构建并核对 pending 目录与历史引用，不伪装成冷启动免费操作。分批写有版本的 staging 文档与迁移进度；旧 head 在最终 cutover 前仍唯一权威，staging 对运行时不可见。
5. 每 session 最终发布使用**原 raw state + control + pending 基线精确 guards**，不比较重新编码后的 raw；大存量单行首次迁移仍有 O(S) 工作及一次网络 guard 成本。跨会话 root pending 目录的切换必须在维护窗口全家族一致，完成全部验证后一次发布可用目录；不能让已切换 child 和未切换 root 混跑。
6. 全部 writer 升级后才恢复执行。旧 reader 见新格式须明确拒绝，不能悄悄返回缺历史对象；schema17 数字不能作为兼容证明。旧 writer 在读 state 之前就能写 journal，因此“deny_unknown_fields 会拒绝旧程序”不足以保证无污染，必须有部署切换与写入版本准入。
7. 不部署长期 v1/v2 双写、双 reducer 或自动 fallback。一次性显式转换器是迁移工具，旧布局停止写入；若需旧格式导出由离线工具完成。cutover 后出现新写入，不能把旧 raw 直接覆盖回去作为回滚。迁移中断按 manifest 续进/核对，不抹除已确认记录。

保留期限默认**不变**：历史文档、原 commands/receipts 与 remote operation ledger 不做 TTL/删除。总存储持续增长；pending 页只保留未决成员，清理已 ACK 的目录空页是索引收敛，不删除命令证据。将来保留期限收缩须单独裁决跨 session 引用、去重、迟到结果、未知操作和法律/产品历史需求，不混入本次建议。

## 7. 故障矩阵与不可放宽的不变量

| 注入/竞争 | 必须观察的结果 | 新布局的证明点 |
| --- | --- | --- |
| begin 发出前丢失 / 提交后丢 ACK | 调用方保持 Unknown；cold 能定位原命令或证明 begin 未生效 | command 与 pending directory 原子；原 begin operation identity 封闭，不以目录未读到推断 NotApplied |
| effects 已提交、回复丢失 | 返回原 receipt、投影/预算/工具副作用不重复 | 原 command digest、operation ledger、receipt 一致；不得根据本地 next-head 猜成功 |
| ACK 已提交回复丢失 / ACK 未提交 | 前者重放幂等确认；后者继续家族冻结 | reconciled 和全部祖先 membership 同事务；不能重复减计数或把空页当成功 |
| ACK/seal 与另一个 writer race | 被确认操作仅发生一次，错误原命令不能解冻 | guard 精确成员及 command 身份；同 root 进程屏障保留，跨实例靠事务 |
| 同 session 两实例；兄弟写入推进 revision | stale 走既有领域规则；无关兄弟 binding 不因全局新 revision 被错误否定 | 保留 `ReconcileTaskBinding expected_revision <= current` 及 immutable binding 验证；不新增 Store owner lease |
| 同 revision 外写 head/被消费实体 | 整批冲突，不覆盖外写 | raw read-set guards；revision cache 不作为证据 |
| 同 revision 外写未消费的冷历史 / 只插 journal 绕过目录 | 现行严格模式应发现；新模式不能保证下一次无关 mutation 发现 | 第 9 节必须显式接受验证时机与 writer 边界变化；未获批则不可采用目标快路径 |
| reducer 部分更新后拒绝 / SQL 中途 guard 失败 | state/control/projection/history 业务效果全部不发布 | 私有 delta 丢弃、事务回滚；拒绝 receipt 与 pending/ACK 仍按旧协议，不做内存逆操作伪回滚 |
| 停止、关闭、重开、进程退出再开 | old lifecycle 不复活，新执行需 SDK admission；未决义务不丢 | control token、活动依赖闭包及 pending head 冷恢复；历史终态不能仅按 stage 清理 |
| 工具迟到结果 / Abandoned 后 settlement | 保存合法结算但不启动下一工作、不再次执行工具 | 旧 invocation 身份、scope epoch、授权和 lifecycle 精确匹配；必要历史实体点读生成新版本 |
| child terminal handoff / parent receipt | 伪 receipt、跨 session/cross-binding receipt 被拒绝；真实父提交后 child 才 ACK | 保留 `BindWorkDelegation` 对父 command+receipt+binding 的事务内联合 guard；AcknowledgeTerminalObligation 对原父 digest/session/resolution 精确 guard |
| parent receipt 被删除或归档引用不可达 | fail closed，不能用 child 内存 receipt 当权威 | 默认不删 receipt，目录/retention 必须保留跨 session 引证 |
| 成员页丢失/重复、counter 不符、blob digest 不符 | 明确损坏错误并阻止依赖执行，不回落空状态 | 校验 scope、版本、长度、摘要、可达目录与成员数；完整性审计另计成本 |
| remote timeout / busy / rollback 失败 / 旧连接迟到 | 保持 Unknown 与原身份，不自动重新派发 | 保留 connection generation 及 close_operation；role mapping 只在确定 NotApplied 时分类重试 |
| Accepted admission 原 receipt 重放 | 不新增 domain revision、不多记预算/历史 transition | 独立 storage commit 语义，不能按 Accepted 数推导事件序 |
| 迁移页写入后 crash / cutover 回复丢失 | 旧 head 或完整新 head，不能半转换可用 | staging 不可见、最终 raw guards 与明确 cutover receipt；未知先 resolve |

root pending membership 对历史有界的证明要求：固定未决集合 U，执行任意多 begin→effects→ACK 后，目录可达成员/页不随已 ACK 命令数增长；允许原 journal/receipt/history 自然增长。若 manifest 把每次已删除 pending 的旧指针一直挂在 head 下，该方案验收失败。

## 8. 可独立验收的实施 slices（均未执行）

| Slice / 授权 | 修改路径 | 独立验收与删除目标 |
| --- | --- | --- |
| S0：基线与约束验证，设计完成后另行执行测量 | `work/diagnostics.rs`、`prepared_effects_test.rs`、durable_work/remote/Agent 行为 fixture | 捕获调用图、字节/分配/扫描量；明确 root 屏障与跨实例界限。禁止把旧 issue 测试数字复用为本方案通过证据 |
| S1：现行格式内，精确执行视图 | types `session_resources.rs` 与 work 新 view；resources `data.rs`、gate_work、两端 read；Agent `work_reason/work_dispatch/work_receive` | 与完整 snapshot 得到同结果，坏历史仍失败、冷恢复完整；先复用严格全读而立即释放。删调用方全量 snapshot 及 admission 的无必要整 state clone。可单独交付，但明确 S/J 下界保留 |
| S2：裁决后建立文档协议与一次性转换器 | resources `work/{document,history,repository}.rs`、effects；types records；仅独立工具负责迁移 | 原 Work 基线无损、页有界、key 冲突拒绝、旧格式明确拒绝、迁移中断恢复；不改 messages，不改 DDL。删除新运行时对旧 state 文档格式的直接写入；不同时维护两个 reducer |
| S3：裁决后事务 pending 目录，贯穿控制与工作 | `work.rs`、`resources/{gate,gate_work}.rs`、`control.rs`、SQLite session_data、remote session_work_journal/session_control | 无全 J 热扫描；child/root begin/ACK crash 矩阵、cold 找原命令、U 固定时目录不增长；替换 HAS_PENDING/READ_PENDING/GUARD_COMMAND 和 control pending SQL。旧 table-scan 测试改为保留子树/排序/身份断言 + 新查询计划门禁；变更依据是获批目录协议 |
| S4：裁决后单一 delta reducer 与两个 adapter | types `work/{reducer,processing,delivery,admission,bindings,query,availability}.rs`；resources effects 和两端 work adapter | 所有 WorkAction 的 accepted/rejected/Unknown 等价，热写不 materialize 历史；删除热 `encode/decode<WorkState>` 和 history maps 全扫描，统一精确 identity 访问；EffectRole 替代 remote magic 4 |
| S5：冷恢复、SDK/ACP 与 subagent 跨 session 闭环 | Agent work_recovery、work_boundary、`session/subagent/{factory/cold,settlement}.rs`；ACP `host/{cold_execution,cold_terminal,execution_finish,work_recovery}.rs`、session_control | 真实停止/进程重开/父子回执/迟到结果；SDK 摘要是否外部 wire 变化单独评估；删除恢复全历史依赖，不删除完整历史显式查询；所有权继续 SDK |
| S6：canonical 投影写路径消除剩余扫描 | resources `work/effects.rs` 与所有消息计数 owner 写入口 | 用事务内实际新插入数维护 message_count、逐作用域协调现有 append/delete/compact；重放不多计，外写策略明确。删热 REFRESH_COUNTS 全 COUNT；只做 Work 路径而其他写入口不一致则不能交付 |
| S7：受控性能与 rollout 验收 | 隔离 benchmark/故障 fixture、active issue、受影响 code-index/design/standards | 先通过第 10 节再申请现场切换；文档切换时只保留一个当前权威。无需为性能证明开启无限 cache、关日志校验或调低并发 |

S2/S3 可以先在不可激活的合成/副本库分别验收；运行时发布必须 S2–S5 合起来满足完整 writer 协议，不能把“slice 独立验收”误解成允许线上混合存储权威。S6 如影响超出本问题的 canonical 写入契约，应独立审查；未完成时仍如实列出 M 相关余项。

按 DOC-UPDATE-001，实际实施时更新受影响 types/resources/Agent code-index、恢复契约与批准 design；本轮仅提案，无模块入口变更，不改其他文档以免越过文件所有权。

## 9. 需要主 Agent / 用户明确裁决的边界

| 裁决 | 本文推荐 | 未获批的结果 |
| --- | --- | --- |
| D1：允许 schema17 下更换持久 JSON 与 row/document 含义吗？ | 批准显式 WorkHeadV2 与 events 内部文档 namespace、有限页目录；command/receipt 及外部 ACP wire 尽量保留原样 | 只能 S1 与常数优化；不能用原字段藏引用/目录 |
| D2：允许热 mutation 只校验活动闭包和被消费历史，其他历史按读/审计校验吗？ | 批准，并更新相应坏历史回归的预期；不声称等价于原即时全历史拒绝 | 历史 Ω(H) 扫描保留，根治目标不可达 |
| D3：允许把 pending 全扫改成同事务维护的持久目录并要求全体 writer 升级吗？ | 批准；保留家族 root 屏障和原命令/ACK，不支持旧 writer 绕目录混写 | 全 J 扫描保留；缓存目录不能作为权威 |
| D4：允许改变历史组织/恢复基线与显式迁移吗？ | 批准基于完整旧 WorkState 的一次性 Work 转换，保留 LegacyUnknown；现场执行再单独授权 | 不清 maps，不从 journal 猜基线；新旧混跑不自动放行 |
| D5：缩短 retention、删除原命令、改变 command 为 payload refs？ | **本推荐不需要批准，默认不做**；接受 Ω(ΣC) 持久空间与真实大命令成本 | 维持现状，不影响热历史分离的有限目标；不能承诺磁盘总量有界 |
| D6：严格保留任意 SQL 外写的即时发现，包括未消费冷历史/只插 journal？ | 对被消费文档保留 raw CAS；对未消费历史接受显式审计发现、writer 遵守目录协议 | 若要求任意外写即时发现，就必须继续全扫；不存在仅靠一个 head revision 的证明 |

D1–D4/D6 均不是“数据库结构不变”一句话隐含的授权。若这些放宽全部被拒绝，本文建议明确收敛为“约束内降低常数，无法根治”，而不是转向 cache/JSON patch 宣称已解决。

## 10. 测量门槛与反证实验（方案，未执行）

### 10.1 隔离负载与指标

使用临时库/固定 provider 与工具回放，不读取现场 DB/HOME，不发真实模型请求。SQLite 与 remote mock-transport 跑同一 WorkAction 序列；真实 Turso、操作系统 crash 与网络 runtime 单独验收，mock 不能替代。

负载维度分开控制：单 Agent/多个子 Agent，固定 L/P/D/K/U 扩 H；固定目标 session 的 H/L 扩其他会话的 J；固定历史扩真实大 P；扩 U 与父链 A；单 root 多 child 与不同 root；冷进程/暖进程/受限页缓存；正常、拒绝、Unknown 与 CAS 冲突。历史规模按至少三个数量级梯度设计（例如 10²/10³/10⁴ 个实体，再按资源许可扩大），正文使用普通/大量转义/Unicode/大工具输出；数字只是实验参数，不是结果。

逐 action 记录：CPU time、wall/p95、分配次数/字节与峰值 live heap/RSS、typed state decode/encode 次数与字节、read-set/write-set、完整原命令字节、SQL submitted 与真实 committed 分开、fullscan steps/读取行页、事务等待时间、网络上下行与 driver 拷贝、WAL/checkpoint、cold 恢复时间。分离 model prepare、Work 存储、transcript、UI；同负载对比 f36fbed5，不以不同并发/刷新频率创造收益。

### 10.2 硬门槛

1. schema/sqlite_master、版本、表列索引清单保持原样；messages 原始内容无需批量重写。未批准的 wire/digest 字节差异为失败，不是可接受性能代价。
2. 固定 L/U/C 的热 BeginReason、BeginDispatch、CommitAct、PublishDelivery、HAS_PENDING、控制 guard：**无全 WorkState 历史 decode/encode，无 commands 全表扫描，无历史长度数组/manifest 重写**。以查询计划、扫描计数及字节计数证明，不能只看耗时下降。
3. H/J 扩大时，同一热 action 的逻辑 payload 读写字节和内存工作集不应线性增长；允许按索引深度增加有限页面访问。出现 O(H)/O(J) 的斜率即未根治；绝不以“10 倍历史但只慢一半”当通过。
4. 原 command encode/hash 继续单次共享，cold 原命令验证仍正确；不要求 C=0，不把 BeginReason 大输入成本算成历史优化失败或成功。固定输入下真实端到端 CPU/RSS 是否改善须报告置信区间/重复次数，不能预设降幅。
5. cold 执行恢复读取活动闭包和未决原命令，不回放历史 journal；显式完整历史/审计则承认 O(H+J)，并验证完整性。每次 cold 都全量重建 pending 索引属于失败。
6. Pending U 固定并连续完成大量命令后，可达目录大小收敛；root begin/ACK 事务窗口与所有第 7 节故障均通过。无 drop 后遗留不明工具副作用、无错误 ACK、无跨 session receipt 误认。
7. canonical COUNT 若仍与 M 增长，独立报告，不宣称整个操作历史无关。所有新 protocol/document/index 必須只有一个写入 owner 与事务编译来源。

### 10.3 专门推翻“看起来根治”的实验

- 给一个很小的新 session 所在库塞入大量**其他 session 的已 ACK journal**；若 HAS_PENDING/BeginDispatch 变慢且 fullscan steps 增长，说明只解决了 H，没有解决 J。
- 固定活动状态，增加十万级已结束 records 或 metadata，而 payload 几乎为空；检测仅外置 payload 后 maps、去重、availability 的增长。
- 在相同 revision 改 raw 空白/字段顺序或合法值，在读取与提交之间抢写；相关 read-set CAS 必须拒绝。另破坏无关冷历史，按 D2/D6 获批语义断言检测时机，不能混称原语义。
- 重放 Register/FinishAdmission 的原 accepted receipt；把 mutation_id 排序反转、制造明确拒绝与 NotApplied；验证历史顺序不由 accepted 个数或 UUID 顺序推导。
- 仅保留旧 events/commands/receipts，删除待测副本里的必要基线或独立 control 信息；期望明确不可恢复/LegacyUnknown，禁止测试为“顺利回放”补造数据。
- begin 或 ACK 的 child/root 目录更新每条 SQL 后注入 crash；冷进程找回原命令，不能出现 journal pending 但目录空，或 ACK 后祖先计数永久非零。
- 改某个分片的 namespace、scope、page link、blob digest；fail closed。history manifest 与 pending manifest 分开测增长，不能让已 ACK 历史留在 pending 可达链。
- 8 次 remote CAS 冲突、ACK 回复丢失和 rollback 未知；核对 identity 不变、role mapping、重试预算与网络放大，禁止 cache 直接确认 next-state。
- 停止并重开父会话后给子 invocation 迟到结果，再投伪造/真实 parent receipt；只结清允许的旧义务、不重开旧执行，目录空之前 root 不能放行。
- 用极大 C、小 H 与大 H、小 C 两组分开比较；若收益只来自小模型上下文或 fewer tools，不算持久布局收益。检查 remote server CPU/读页，防止 JSON patch 把成本转移到服务端。

交付证据应是可核对的最终退出状态、非零目标用例数和实际 trace/计量文件；本文没有运行这些实验，没有给出任何 CPU/RSS 降幅。主 Agent 可据此做约束整合与裁决，再形成获批实施契约。
