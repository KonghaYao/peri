export const meta = {
  name: 'acp-hub-wf-b',
  description: 'acp-hub M1 阶段B：F2 认证与配置 / F3 persist / F4 state 三路并行（各 feature 内部 plan→code→review+test）'
}

const COMMON = [
  '背景：/Users/konghayao/code/ai/perihelion/acp-hub 是独立 workspace（members: proto/server/machine）。',
  'proto crate（acp-hub-proto）已完成：帧模型/Action 信封/machine 协议/Y.Doc schema 镜像/M1 帧集白名单/HMAC 线格式原语（hmac.rs 的 compute_mac/verify_mac/HKDF 派生/SeenNonces）。',
  '架构权威文档：docs/architecture.md。server crate 的 src/lib.rs 已预声明 pub mod config; auth; persist; state; protocol; channel; control;，各为单文件占位。',
  '',
  '协作纪律（必须遵守）：',
  '1. 你只操作自己负责的模块文件（见任务）；严禁修改 lib.rs、Cargo.toml（依赖已预填，如需新增依赖请记录到输出，由主管统一处理）、其他 feature 的模块文件。',
  '2. 若需要把占位单文件（如 state.rs）扩展为目录（state/mod.rs），可 git rm 原单文件后建目录，但只限自己的模块。',
  '3. 每个公共项写 doc 注释；不修改 docs/architecture.md。',
  '4. 结构化日志用 tracing，日志字段遵守脱敏（不记正文/工具参数/token/密钥）。'
].join('\n')

phase('F2/F3/F4 并行')

const [f2, f3, f4] = await parallel([
  () => (async () => {
    const plan = await agent(`你是 acp-hub 架构规划 agent，负责 Feature F2：server 认证与配置模块设计。

${COMMON}

任务：阅读 docs/architecture.md 以下章节（Read 分段）：§4.7（keep_alive 与关闭码——超时判定时钟）、§9.2（token 模型与双向认证全流程、线格式精度、协议级属性）、§9.2.1（token 运维流程：0600/宽限期轮换/备份/失败计数）、§9.2.2（client token 分级 full/read-only）、§9.3（数据脱敏）、§9.4（审计最小集）、§9.5（M1 身份与授权边界：token 即身份、非 loopback 拒绝）、§9.6（spawn env 白名单）、§16（配置默认值全表）。再 Read proto/src/hmac.rs、proto/src/conn.rs、proto/src/whitelist.rs 了解已有类型（HMAC 原语、Auth/AuthResponse/Ready 帧、Role、DocId、关闭码常量）。

产出设计文档（Write 到 docs/plans/f2-auth-config.md），覆盖：
1. Config 模块：Config 结构体（§16 全表项映射为 Rust 字段+类型+默认值）、加载优先级（CLI clap > 环境变量 > ~/.config/acp-hub/config.toml > 默认）、toml 解析（serde，未知键报错或忽略的取舍）、数据/配置目录解析（dirs-next，0600 权限）、日志初始化（tracing_subscriber env-filter）。
2. auth 模块：Token 角色枚举（machine/full/read-only）、TokenRecord 结构（token_id/role/name/created_at/是否吊销）、TokenStore（文件格式、路径、0600、加载/生成/校验/宽限期共存/吊销）、CLI 子命令（如 server 启动自动生成缺失 token + token list/generate/revoke 子命令）、HMAC 双向认证服务端流程（machine/hello → 校验 token+版本+角色 → 生成 session_context → compute_mac 应答 → machine 校验）、SeenNonces 的 TTL（30s）清理、认证失败计数（§17.1 指标字段）、审计日志最小集（动作/commandId/token_id/结果/耗时）、连接上下文 ConnectionCtx（token_id/role/绑定信息）。
3. 非回环拒绝策略（allow_non_loopback 默认 false，§9.5）在 gateway 的挂钩点（gateway 是 F5，这里只设计接口）。
4. 测试清单（配置优先级、0600 权限断言、token 生成/校验/宽限期、HMAC 握手成功/失败/重放/过期 nonce/错误角色、脱敏断言）。

输出：设计文档路径 + 3-5 条关键决策摘要。`, { label: 'f2-plan', allowedTools: ['Read','Write','Glob','Grep'] })

    const code = await agent(`你是 acp-hub 实现 agent，负责 Feature F2：实现 server 认证与配置。

${COMMON}

任务：
1. 先 Read docs/plans/f2-auth-config.md（必须遵守其设计），按需核对 architecture.md §9.2/§16 与 proto/src/hmac.rs（HMAC 原语在 proto，直接复用，不得重写）。
2. 实现 server/src/config.rs（或 config/ 目录）：Config 结构体 + 加载（CLI clap derive > env > toml > 默认）+ 目录 0600 创建 + 日志初始化。
3. 实现 server/src/auth.rs（或 auth/ 目录）：TokenStore（生成 32B CSPRNG、0600 文件、宽限期共存、吊销）、角色枚举、HMAC 握手服务端逻辑（hello 校验 → session_context → compute_mac 应答 → 返回握手结果）、SeenNonces TTL 30s 清理、认证失败计数、审计日志。提供 gateway（F5）需要的接口：如 authenticate_hello / verify_client_token / ConnectionCtx。
4. 写测试：配置优先级与默认值、toml 解析、0600 权限断言（unix 下 stat mode）、token 生成/校验/宽限期轮换/吊销、HMAC 握手成功与各失败路径（坏 token/重放 nonce/过期 nonce/错误角色/错误版本）、脱敏断言。
5. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。

输出：模块文件清单、公共 API 签名摘要、测试结果（测试名列表）、clippy 结果、遗留问题（含需要的但缺失的依赖）。`, { label: 'f2-code', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    const review = await agent(`你是 acp-hub 代码审查 agent，负责审查 Feature F2（server 认证与配置）。

${COMMON}

任务：
1. 对照 architecture.md §9.2/§9.2.1/§9.2.2/§9.5/§16 与 docs/plans/f2-auth-config.md，审查 server/src/config.rs、server/src/auth.rs（含子模块）：token 0600 与生成强度、宽限期轮换、HMAC 握手全流程正确性（复用 proto 原语、nonce 单次使用+TTL、常量时间比较路径）、失败计数与脱敏、配置默认值与 §16 一致、非回环拒绝钩子。
2. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。失败/警告直接修复（只改 config/auth 模块）。
3. 输出：结论（通过/不通过）、问题清单按严重级、已修复项、未修复项及理由。`, { label: 'f2-review', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    return { plan, code, review }
  })(),

  () => (async () => {
    const plan = await agent(`你是 acp-hub 架构规划 agent，负责 Feature F3：server 持久化层（persist）设计。

${COMMON}

任务：阅读 docs/architecture.md 以下章节：§4.4（command outbox：去重记录持久化、outbox 记录状态机 received→accepted→intent_durable→dispatched→delivery_confirmed→projection_committed→completed/failed/delivery_unknown、delivery 三级 L1/L2/L3、崩溃点×状态×重试行为表、retryable 分类）、§4.5.1（(epoch, last_seq) 持久化）、§8.4（Y.Doc 持久化规范：blob 长度前缀+CRC32、尾部截断、fsync per-commit、compact 契约 64MB/24h 原子 rename、磁盘预算 2GB、归档 90 天与 outbox 解耦、0600）、§8.4.1（原子性边界：单文件单记录、恢复不变量 1-5 条、degraded）、§8.5（last_seq 与补推）、§16（fsync 模式/compact 触发/磁盘预算/归档保留配置项）。

产出设计文档（Write 到 docs/plans/f3-persist.md），覆盖：
1. persist 模块划分（如 update_log.rs/outbox.rs/watermark.rs/store.rs）与公共类型（UpdateLog/OutboxStore/Watermark/StoreError/RecoveryResult）
2. update 日志：blob 线格式（长度前缀 u32 + CRC32 + payload，自描述）、append + per-commit fsync、启动回放（尾部截断恢复，保留损坏段并告警）、degraded 标记
3. command outbox：按 session 分片文件、记录结构（command_id/type/turn_id/status/dispatched_at/retryable 分类）、状态机迁移 API（mark_dispatched/mark_delivery_confirmed/mark_projection_committed/mark_failed/mark_delivery_unknown）、启动重放重建去重索引（Map<command_id, record>）、committed 记录清理策略（session 关闭后保留 7 天，磁盘预算淘汰——M1 可实现简化版但接口完整）
4. (epoch, last_seq) 水位文件：每 session 独立小文件、加载与对齐规则（不一致以较小者为准并告警）
5. 恢复不变量顺序（§8.4.1 的 1-2 条在 persist 内实现，3-5 条与其他层协作——标注接口）
6. compact 流程（原子：临时快照→fsync→rename→截断）、触发条件
7. 测试清单（CRC 损坏截断、fsync 语义、状态机非法迁移拒绝、重启重放重建索引、水位对齐）

输出：设计文档路径 + 3-5 条关键决策摘要。`, { label: 'f3-plan', allowedTools: ['Read','Write','Glob','Grep'] })

    const code = await agent(`你是 acp-hub 实现 agent，负责 Feature F3：实现 server 持久化层（persist）。

${COMMON}

任务：
1. 先 Read docs/plans/f3-persist.md（必须遵守其设计），按需核对 architecture.md §4.4/§8.4/§8.4.1。
2. 实现 server/src/persist.rs（或 persist/ 目录）：update 日志（blob+CRC32+append+fsync+尾部截断恢复）、command outbox（按 session 分片、状态机全迁移 API、启动重放重建去重索引、保留策略）、(epoch, last_seq) 水位文件、compact 原子流程、恢复不变量 1-2 条与恢复结果类型（供上层决定 degraded）。
3. 写测试：update 日志 roundtrip/损坏截断/CRC 校验、outbox 状态机合法迁移与非法迁移拒绝、跨「重启」（新实例重放同一目录）重建索引、水位对齐（较小者为准）、compact 后数据完整。用 tempfile 临时目录。
4. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。

输出：模块文件清单、公共 API 签名摘要、测试结果（测试名列表）、clippy 结果、遗留问题。`, { label: 'f3-code', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    const review = await agent(`你是 acp-hub 代码审查 agent，负责审查 Feature F3（server 持久化层）。

${COMMON}

任务：
1. 对照 architecture.md §4.4/§8.4/§8.4.1/§4.5.1 与 docs/plans/f3-persist.md，审查 server/src/persist.rs（含子模块）：blob 线格式自描述性、CRC32 覆盖、fsync 纪律、outbox 状态机完备性（全部迁移路径、崩溃点语义）、去重索引重建正确性、水位对齐规则、compact 原子性（rename 前旧日志完整）、degraded 信号路径。
2. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。失败/警告直接修复（只改 persist 模块）。
3. 输出：结论（通过/不通过）、问题清单按严重级、已修复项、未修复项及理由。`, { label: 'f3-review', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    return { plan, code, review }
  })(),

  () => (async () => {
    const plan = await agent(`你是 acp-hub 架构规划 agent，负责 Feature F4：server 状态层（state：Y.Doc 聚合）设计。

${COMMON}

任务：阅读 docs/architecture.md 以下章节：§5（数据模型：5.1 规范化聚合视图裁决、5.2 文档拆分与 sessions 投影位职责、5.3 Chat Doc schema、5.4 Session Doc schema、5.5 Registry Doc schema、5.6 写入边界：唯一提交边界 DocManager、server-authoritative、ViewStore 隔离范围）、§6（聚合层：6.1 ACPChannel 规范化边界与事件映射表、6.3 幂等聚合规则与终态守卫（含 interrupted 校准例外）、6.4 顺序/微批次/事务边界（16ms、控制类先 flush、广播背压经 channel）、6.5 单写与绑定）、§7.2（Turn 状态机）、§7.3（session 生命周期与分区恢复）、§7.4（并发规则：每 session 单写者 writer task、跨 Chat/Session 双事务顺序 chat→session、禁止跨 await 持有事务、入队检查同一临界区）、§8.5（gap 结构化标记）、§17.2（Degraded 判定）。

产出设计文档（Write 到 docs/plans/f4-state.md），覆盖：
1. NormalizedEvent 定义（§6.1 事件表全子集：message_delta/reasoning_delta/user_message/tool_call_started|updated|completed/permission_requested|resolved|expired/agent_status/capabilities/session_info/session_list_response/turn_terminal，字段按 §5.3/5.4 投影需要）——此类型由 state 层定义，供 F5 的 ACPChannel 产出
2. ViewStore：yrs 薄封装（创建 Doc、encode_state_as_update、merge_updates_v1、apply_update、observe_update 经 channel 送出），隔离聚合器与 yrs 类型
3. DocManager：doc 生命周期（Chat/Session 双 Doc + Registry）、每 session 单写者（mpsc 通道或 tokio Mutex<DocPair>）、16ms 微批次合并、控制类先 flush、广播 channel、唯一提交边界（所有写入必须经它）、跨 Doc 事务顺序 chat→session
4. Factory：doc 创建 + schema_version 幂等补结构
5. ChatWriter：doc 写入原语（entry 创建/append block/文本增量 Y.Text/reasoning 可见性/tool_call upsert/终态迁移）
6. Aggregator：纯函数 apply(&mut DocPair, &NormalizedEvent) -> ApplyResult{applied, reason}（架构 §12 测试前提）；幂等键（turnId/entryId/toolCallId/permissionId）；终态守卫（状态位 + 重放序 (session_id, seq) 单调，interrupted 恰一次校准例外）；gap 标记写回
7. Permission：CAS（pending→resolved 原子一次，decision 写入，expired）
8. SessionList：10s 轮询全量同步投影（纯函数：给定响应与现有 Map 计算 diff，旧条目删除）
9. Registry：machine 视图 + 活跃 session 摘要 + global.status（Healthy/Degraded/Restarting），server 状态源单写接口
10. 测试清单（P0 契约测试：幂等重放、终态守卫 cancelled 晚到丢弃、interrupted 校准恰一次、gap 计数；单写者串行化）

输出：设计文档路径 + 3-5 条关键决策摘要。`, { label: 'f4-plan', allowedTools: ['Read','Write','Glob','Grep'] })

    const code = await agent(`你是 acp-hub 实现 agent，负责 Feature F4：实现 server 状态层（state）。

${COMMON}

任务：
1. 先 Read docs/plans/f4-state.md（必须遵守其设计），按需核对 architecture.md §5/§6/§7.4 与 proto/src/schema.rs（Y.Doc schema 类型镜像，作为类型事实源）。
2. 实现 server/src/state.rs（或 state/ 目录）：NormalizedEvent、ViewStore、DocManager（每 session 单写者通道 + 16ms 微批次 + 控制类先 flush + 广播 channel + chat→session 事务顺序）、Factory（schema_version 幂等补结构）、ChatWriter、Aggregator（纯函数 apply(&mut DocPair, &NormalizedEvent) -> ApplyResult）、Permission CAS、SessionList、Registry（单写接口）。注意：yrs 的 transact_mut 并发 panic 问题——所有写入必须串行（每 session 单写者）。
3. 写测试（架构 §12 测试前提：P0 契约测试为纯函数测试，内存 Y.Doc）：幂等（同事件重放两次不重复创建 entry/tool_call）、终态守卫（cancelled 后晚到 delta 丢弃、interrupted 带序依据终态事件恰一次校准、无依据拒绝）、gap 计数写回与清除、permission CAS 一次迁移、session_list 全量同步旧条目删除、微批次合并与控制类先 flush（tokio::time::pause 或同步路径）。
4. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。

输出：模块文件清单、公共 API 签名摘要、测试结果（测试名列表）、clippy 结果、遗留问题。`, { label: 'f4-code', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    const review = await agent(`你是 acp-hub 代码审查 agent，负责审查 Feature F4（server 状态层）。

${COMMON}

任务：
1. 对照 architecture.md §5/§6/§7.4/§8.5 与 docs/plans/f4-state.md，审查 server/src/state.rs（含子模块）：幂等键正确性、终态守卫（状态位+重放序、interrupted 恰一次校准）、微批次与事务边界（每 session 串行、chat→session 顺序、无跨 await 持有事务）、广播背压路径（observe_update 同步回调中只经 channel 送出）、gap 标记、permission CAS 原子性、Registry 单写、yrs 类型隔离（ViewStore 承诺范围）。
2. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。失败/警告直接修复（只改 state 模块）。
3. 输出：结论（通过/不通过）、问题清单按严重级、已修复项、未修复项及理由。`, { label: 'f4-review', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

    return { plan, code, review }
  })(),
])

return { f2, f3, f4 }
