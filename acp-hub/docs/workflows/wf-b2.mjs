export const meta = {
  name: 'acp-hub-wf-b2',
  description: 'acp-hub M1 阶段B 补单：F3 persist 与 F4 state 实现（code→review，显式 sonnet 模型）'
}

const COMMON = [
  '背景：/Users/konghayao/code/ai/perihelion/acp-hub 是独立 workspace（members: proto/server/machine）。',
  'proto crate（acp-hub-proto）已完成；F2 config/auth 已实现（52 tests 绿）；server/src/lib.rs 已预声明 pub mod config; auth; persist; state; protocol; channel; control;。',
  'F1 的 proto 提供帧模型/HMAC 原语/Y.Doc schema 类型镜像（proto/src/schema.rs）。架构权威文档：docs/architecture.md。',
  '',
  '协作纪律（必须遵守）：',
  '1. 你只操作自己负责的模块文件；严禁修改 lib.rs、Cargo.toml（依赖已预填：server 已有 toml/acp-hub-proto/crc32fast/yrs 等；如需新增依赖请记录到输出，由主管统一处理）、其他 feature 的模块文件。',
  '2. 若需要把占位单文件扩展为目录，可 git rm 原单文件后建目录，但只限自己的模块。',
  '3. 每个公共项写 doc 注释；不修改 docs/architecture.md。',
  '4. 结构化日志用 tracing，日志字段遵守脱敏（不记正文/工具参数/token/密钥）。',
  '5. 设计稿与架构文档冲突时，以架构文档为准；设计稿明确标注的 M1 简化点可以按简化实现，但公共接口形态不得缩水。'
].join('\n')

phase('F3/F4 实现补单（并行）')

const [f3, f4] = await parallel([
  () => (async () => {
    const code = await agent(`你是 acp-hub 实现 agent，负责 Feature F3：实现 server 持久化层（persist）。

${COMMON}

任务：
1. 先 Read docs/plans/f3-persist.md（设计稿 435 行，必须遵守），再按需核对 architecture.md §4.4（command outbox 状态机与崩溃点×状态×重试行为表）、§8.4（blob+CRC32+fsync+compact）、§8.4.1（原子性边界与恢复不变量 1-5）、§4.5.1（(epoch, last_seq) 水位）、§8.5（补推）、§16（持久化相关配置项）。
2. 主管对设计稿审查缺口的裁决（必须执行）：
   - 【H1 裁决】outbox 状态机补「投递后 retryable 失败 → 清除投递位回可重试」迁移：delivery_confirmed 之后收到 retryable 失败 → 状态回退（记录保留、去重索引不删、状态标记可重发）；非 retryable 失败 → failed。以 architecture.md §4.4 崩溃点表的 retryable 分类为准。
   - 【M1 裁决】水位 CRC 损坏 → degraded 符合架构 §17.2（「启动恢复不变量失败」触发 Degraded），保持设计稿，不降级到告警。
3. 实现 server/src/persist.rs（git rm 占位单文件后建 persist/ 目录，或单文件内组织子模块——推荐目录）：update 日志（blob：len:u32 LE + crc32:u32 LE + body，CRC 覆盖记录体、尾部截断恢复、损坏段移至 corrupt/ + 告警 + degraded 信号）、command outbox（追加式状态快照日志 + 内存索引、状态机全迁移 API 含 H1 裁决路径、启动重放重建 Map<command_id, record>、tombstone 删除、清理策略接口）、(epoch, last_seq) 水位（每 session 独立小文件、较小者为准对齐）、compact 原子流程（temp→fsync→rename）、恢复编排（RecoveryResult 聚合不变量 1-2，ReplayOutcome.records 交 doc-manager，recover 完成信号供 channel 门禁，Store::status 供 Registry degraded）。
4. 写测试（设计稿 §12 的 T1-T11 清单为基线）：update 日志 roundtrip/CRC 损坏尾部截断/corrupt 归档、outbox 状态机合法迁移与非法迁移拒绝（含 H1 新路径）、跨重启（新实例重放同一目录）重建去重索引、水位对齐较小者为准、compact 后数据完整、RecoveryResult 聚合。用 tempfile。
5. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。

输出：模块文件清单、公共 API 签名摘要、测试结果（测试名列表）、clippy 结果、遗留问题。`, { label: 'f3-code', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    const review = await agent(`你是 acp-hub 代码审查 agent，负责审查 Feature F3（server 持久化层）。

${COMMON}

任务：
1. 对照 architecture.md §4.4/§8.4/§8.4.1/§4.5.1 与 docs/plans/f3-persist.md，审查 server/src/persist/（含子模块）：blob 线格式自描述性、CRC32 覆盖、fsync 纪律、outbox 状态机完备性（全部迁移路径含 H1 裁决路径、崩溃点语义、非法迁移拒绝）、去重索引重建正确性、水位对齐规则、compact 原子性（rename 前旧日志完整）、RecoveryResult 聚合与 degraded 信号路径。
2. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。失败/警告直接修复（只改 persist 模块）。
3. 输出：结论（通过/不通过）、问题清单按严重级、已修复项、未修复项及理由。`, { label: 'f3-review', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    return { code, review }
  })(),

  () => (async () => {
    const code = await agent(`你是 acp-hub 实现 agent，负责 Feature F4：实现 server 状态层（state）。

${COMMON}

任务：
1. 先 Read docs/plans/f4-state.md（设计稿，必须遵守），再按需核对 architecture.md §5（数据模型与 schema）、§6（聚合层：6.1 事件映射表、6.3 幂等与终态守卫含 interrupted 校准例外、6.4 微批次 16ms 与控制类先 flush、6.5 单写与绑定）、§7.2（Turn 状态机）、§7.4（每 session 单写者、chat→session 双事务顺序、禁止跨 await 持有事务、入队检查同一临界区）、§8.5（gap 标记）、§17.2（Degraded）与 proto/src/schema.rs（Y.Doc schema 类型镜像作为类型事实源）。
2. 实现 server/src/state.rs（git rm 占位单文件后建 state/ 目录）：NormalizedEvent（§6.1 事件表全子集，由本层定义供 F5 ACPChannel 产出）、ViewStore（yrs 薄封装：创建 Doc/encode_state_as_update/merge_updates_v1/apply_update/observe_update 经 channel 送出）、Factory（schema_version 幂等补结构）、ChatWriter（entry/block/文本增量 Y.Text/reasoning 可见性/tool_call upsert/终态迁移写入原语）、DocManager（每 session 单写者 mpsc + 16ms 微批次合并 + 控制类先 flush + 广播 channel + chat→session 事务顺序 + 唯一提交边界）、Aggregator（纯函数 apply(&mut DocPair, &NormalizedEvent) -> ApplyResult{applied, reason}；幂等键 turnId/entryId/toolCallId/permissionId；终态守卫「状态位 + 重放序 (session_id, seq) 单调」双条件；interrupted 恰一次校准例外 CalibrationDone 拒绝二次迁移；gap 计数写回与 uncalibratable 上报）、Permission CAS（pending→resolved 单次原子迁移 + expired）、SessionList（10s 轮询全量同步投影纯函数：diff 计算、旧条目删除）、Registry（machine 视图 + 活跃 session 摘要 + global.status Healthy/Degraded/Restarting，单写接口）。
3. 关键实现纪律：yrs 的 transact_mut 并发 panic——所有写入必须经每 session 单写者串行化；observe_update 同步回调中只经 channel 送出不得做其他 IO；跨 Chat/Session 双事务必须 chat→session 顺序；无跨 await 持有事务。
4. 写测试（架构 §12 测试前提：P0 契约测试为纯函数测试，内存 Y.Doc；设计稿测试清单为基线）：幂等（同事件重放两次不重复创建 entry/tool_call）、终态守卫（cancelled 后晚到 delta 丢弃、interrupted 带序依据终态事件恰一次校准、无依据拒绝）、gap 计数写回与清除、permission CAS 一次迁移、session_list 全量同步旧条目删除、微批次合并与控制类先 flush、Factory 幂等补结构。
5. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。

输出：模块文件清单、公共 API 签名摘要、测试结果（测试名列表）、clippy 结果、遗留问题。`, { label: 'f4-code', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    const review = await agent(`你是 acp-hub 代码审查 agent，负责审查 Feature F4（server 状态层）。

${COMMON}

任务：
1. 对照 architecture.md §5/§6/§7.4/§8.5 与 docs/plans/f4-state.md，审查 server/src/state/（含子模块）：幂等键正确性、终态守卫（状态位+重放序双条件、interrupted 恰一次校准 CalibrationDone 拒绝二次迁移）、微批次与事务边界（每 session 串行、chat→session 顺序、无跨 await 持有事务、控制类先 flush）、广播背压路径（observe_update 同步回调仅经 channel 送出）、gap 计数与 uncalibratable 上报、permission CAS 单次原子迁移、Registry 单写、ViewStore 对 yrs 的类型隔离承诺、Factory 幂等补结构。
2. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。失败/警告直接修复（只改 state 模块）。
3. 输出：结论（通过/不通过）、问题清单按严重级、已修复项、未修复项及理由。`, { label: 'f4-review', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    return { code, review }
  })(),
])

return { f3, f4 }
