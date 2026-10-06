# WorkState 重设计：快速验收

日期：2026-10-06。状态：**Scope completed（快速验收授权范围已完成）；Quick partial（不是最终源码全量通过）**。本人原四项 SQLite smoke PASS；主 agent 最新 Native **9/9 PASS**、Remote 本地 transport **11/11 PASS**、早先 ACP tests compile PASS 与客户端 build exit 0。**用户明确豁免最终重建，本轮据此不再运行 Cargo；最后源码未复 check/build，性能交用户实测。**

本报告由用户指定的唯一快速验收 subagent 维护。依据 [proposal](2026-10-06-workstate-redesign-proposal.md)、[implementation issue](2026-10-06-workstate-redesign-implementation.md) 与仓库 testing 标准。不是完整可靠性验收或性能研究，不据此宣布整个重构完成。

## 1. 范围与执行隔离

- 唯一工作目录：`/Users/konghayao/code/ai/peri-workstate-redesign-20261006`。没有修改原 repo，没有 commit，没有读取或迁移真实用户数据库。
- 本验收只新增本报告与 `peri-resources/tests/work_records_smoke.rs`；未修改开发 worker 的生产源码。
- 本人执行的数据库行为仅发生在 `tempfile::TempDir` 创建的新库/合成旧库。合成 SDK ticket、request 与 assistant response，不调用 provider、不启动 Peri、不访问真实 Turso。
- 源码仍并行变化。下述结果对应本次命令实际编译/执行的快照，不是对后续修改的自动背书。主 agent 结束上一轮 client check、授权独占 Cargo slot，公共 store worker 修复 `decide_control` 导入后才执行 smoke；执行完已通知主 agent 释放 slot。
- 末轮静态复查期间主 agent 串行 `build -p peri-tui` 并报告 exit 0；构建期间 Agent owner 删除了无效 head 查询，随后又发现 draft 撤回/再发送两项规则需 types owner 修复，主 agent 将在修复后补最终构建。本验收没有启动第二次 Cargo。新增的 `sqlite_work_offline_smoke` 由开发 owner 交付，后续两 suite 由主 agent 实际运行，不计作验收 subagent 写入或本人执行。
- **最终用户裁决覆盖此前重建计划**：不用重建，准备 task-only patch 合回原位置分支。本轮交接据此停止 Cargo。本验收结束范围内工作，冻结 owned 测试源码、不扩展测试，只更新本报告；没有执行合并、修改原 repo 或运行 patch dry-run。用户豁免重建不等于最终源码已构建通过。

## 2. 实际命令与结果

以下是验收 subagent 实际运行，不是建议命令或主 agent 代跑。

| 命令 | Exit | 实际结果 |
| --- | --- | --- |
| `rustfmt --edition 2021 peri-resources/tests/work_records_smoke.rs` | 0 | 只格式化新增测试文件 |
| `./scripts/cargo-rmcp-patched.sh test --locked --offline -j 4 -p peri-resources --test work_records_smoke -- --test-threads=1` | **0** | **4 passed / 0 failed / 0 ignored / 0 measured / 0 filtered out**；此次重新编译 `peri-resources`，35 个 warning；构建耗时 9.55s，测试 0.11s，仅执行记录，不是 benchmark |
| `git diff --check -- spec/issues/2026-10-06-workstate-quick-acceptance.md peri-resources/tests/work_records_smoke.rs` | 0 | owned 路径格式检查；新增文件由 apply_patch 创建、smoke 文件另经 rustfmt |

**实际测试失败 case 数：0；非零 Cargo 命令数：0（只运行上述一次 Cargo）。** 合成旧库打开拒绝、封存命令不能重应用属于测试期望的错误，不是失败 case。未运行 `--lib` 全测、workspace 全测、客户端 build 或旧 `target/debug/peri`。不把缓存中的旧可执行文件算作新实现构建成功。

### 2.1 四项 smoke

| Case | 实际断言 | 结果与局限 |
| --- | --- | --- |
| `fresh_database_publish_claim_and_synthetic_response_settle_once` | 新库创建 workspace/session → 发布 required delivery → Availability → 合成 admission → ClaimBatch → BeginReason → 手造无工具响应提交；Processing 为 Settled、required/effect 计数归零、current processing 清除；重复同命令只产生一条 assistant transcript | PASS。覆盖 public resources 到 SQLite planner/adapter；**不等于真实 SDK、Agent loop、provider 或工具执行通过** |
| `duplicate_receipt_keeps_one_draft_and_exact_original_evidence` | 相同原命令重复提交返回相同 receipt；只一份 Queued draft；正文 evidence 字节保持；没有 pending command | PASS。未注入真实 commit acknowledgement 丢失 |
| `sealed_original_cannot_be_reapplied_or_replaced` | 缺席原命令显式 seal 为 NotApplied；后来 apply 拒绝；同 mutation ID 换动作拒绝；无 draft 写入 | PASS。不是完整 Unknown/故障恢复矩阵 |
| `schema17_open_is_rejected_without_changing_original_blob` | 仅含合成旧 `session_work_state` 与 user_version 17 的库，普通 open 拒绝；打开失败后版本仍 17，原始 blob 字节一致 | PASS。这是小型拒绝门槛检查，**不覆盖完整 schema17 迁移/恢复** |

### 2.2 外部运行证据，不能算本人复跑

- 主 agent 报告 types check PASS。
- 领域 worker 报告 26 个 domain + 4 个 admission 测试 PASS；本报告未复跑，不把 30 项并入上述 4 项统计。
- 主 agent 报告已修复 client check 的 TUI 枚举错误，并实际完成 `build -p peri-tui`：日志 `/tmp/peri-workstate-client-build.log`，`Finished dev` 1m09s，**exit 0**。这是主 agent 提供的实际运行证据，本验收没有重跑或独立读取该 worktree 外日志；耗时只是记录，不是性能结论。
- 构建期间 Agent owner 又删除无效 head 查询，主 agent 要求最终增量 build。首轮客户端 build PASS 不自动证明该后续修改已编译；最终增量结果待补。
- 主 agent 后续实际运行 Cargo 子命令 `test --locked --offline -p peri-resources --test sqlite_work_offline_smoke --test work_records_smoke -- --test-threads=1`，报告日志 `/tmp/peri-workstate-final-smoke.log`、**exit 0，3 migration + 4 original smoke = 7 passed / 0 failed**。这是 **main-run**，本人没有复跑或独立读取 worktree 外日志；原四项属于同一 case 集合的再次执行，不与本人 4 项相加声称 11 个不同 case。
- 三项迁移实测包括：新 schema17 只有引用消息列/无旧 work 表；合成未知证据与 canonical 正文字节保留；坏正文失败后合成库 schema/原始行回滚。此前“迁移 suite 尚未运行”的证据空缺已解除；不扩大为真实库、完整职责恢复或全部终态分类通过。
- Agent 测试发现的 draft 撤回/再发送两项规则正在由 types owner 修复。上述 smoke 和首轮 build PASS 不覆盖后续修复；ACP/Agent 旧测试仍在迁移，**不声明所有 tests 可编译或全测 PASS**。
- 主 agent 新增报告 **Remote local transport 11/11 PASS**，日志 `/tmp/peri-workstate-remote-smoke.log`。初轮实际为 **10 passed / 1 failed**：测试 fixture 未创建 `peri_store_meta`；主 agent 修复 `session_work_test.rs`，改用生产 schema initialization plan，复跑 11/11 PASS。按主 agent 定位，这是 fixture 初始化问题，**不是已确认的生产 compaction 错误**。本人没有复跑或独立读取外部日志；主 agent 此条未提供完整命令或数字 exit，本报告不补造。保留初轮非零 case 数 1，不把复跑通过写成从未失败。
- Remote 结果只证明本地 transport/合成后端测试，不证明真实 Turso 服务、网络故障或部署接管已经验收。
- 主 agent 再确认旧 schema17 拒绝与 Native 7 项 PASS；这是上述 Native 两 suite 的同组证据，不新增计数。最新状态为 Agent/ACP test 编译进行中、binary 将再次 rebuild；目前还不能确认该编译或最终 binary 完成。
- 主 agent 最新确认 **ACP tests compile PASS**，Agent 仍有 12 个 compile errors 正收尾；ACP 编译通过不代表执行全部测试 PASS，Agent 测试编译尚未通过。
- **最终 main-run 更新**：日志 `/tmp/peri-workstate-final-smoke.log` 的两套 Native tests 实际 **9/9 PASS = 3 offline + 6 work_records**，包含本人新增的普通 withdrawal/resend 与 stop withdrawal 两项。该结果取代前一轮 7/7 的覆盖集合，不与旧轮次累加；本人未复跑或独立读取 worktree 外日志。主 agent 此次更新未另报数字 exit，本报告不为这一轮补造 exit；前一轮 7/7 的 exit 0 仍只归属前一轮。
- 主 agent 报告最后 **Agent 3 个编译错误、middleware 2 个编译错误已修**，但按用户要求**未复 check**；另有最小 fresh blocked enqueue publication 修复已落源码但未跑。这里只记录 owner 修复声明，不把“已改代码”写成编译/行为 PASS。
- **最终 binary 未重建**。早先 `Finished dev`、exit 0 证明的是早先构建快照，不证明这批最后源码或合并后分支。最终 revision 的 Agent/middleware 测试编译与 fresh blocked enqueue 行为仍未验证。

### 2.3 追加两项 withdrawal smoke：main-run 已执行通过

按用户授权，只在 owned `work_records_smoke.rs` 补两项小型 native case；主 agent 统一运行，本人不新开 Cargo。文件共有 **6 项**，此前本人 4/4 与 main Native 7/7 的结果不包含新增两项；**最终 main-run Native 9/9 已包含两项，均 PASS**。

- `ordinary_withdrawal_disposes_draft_and_resend_allocates_new_fifo_sequence`：Stage → Publish → 未 claim 普通 WithdrawDelivery；检查 receipt 返回后 draft Withdrawn、旧 delivery Abandoned，再用新 command/同 input ID Stage 为 Queued，sequence 递增，并核对 SQL `fifo_seq` 与 record 一致。
- `stop_withdrawal_requeues_draft_and_abandons_original_unclaimed_delivery`：带当前 `expected_control_generation` 的 stop withdrawal 后 draft Queued、publication 引用清除、原 delivery Abandoned，evidence 字节保持。
- 两项共用小 fixture，publish mutation ID 与 delivery ID **故意不同**；publication event ID 为 `user-input:<uuid>:<command>`，**不等于 input ID**。Stage source 使用公开 `UserInput` 序列化的原稿 envelope（含 originalDraft 与 enqueue_publication），publication 使用同稳定消息 UUID 的 canonical Human payload，**两个 PayloadRef 不相等**。按已落地 public `UserInputPublicationIdentity` / `StagedUserInputPublicationBinding` 提供 draft revision/fingerprint 与 source/canonical 引用，不再用同 ref、同 event/input ID 的捷径。
- 只验证成功返回后的持久化关联；没有注入事务中断，不宣称完整原子性故障矩阵。fixture 按 resources/domain API 构造，不等同于真实 Agent mailbox/SDK 入口实测。
- 单文件 rustfmt 与 owned 路径 `git diff --check` exit 0；源码 615 行，低于 STD-SIZE-001 的 1000 行限制。新增两项的 main-run 结果为 **2 passed / 0 failed**，只归属当次测试快照，不覆盖后续源码；现已冻结测试文件，不继续扩展。

## 3. 独立静态侧查

### 3.1 旧聚合与读写形状

- 检索 `peri-agent/src`、`peri-acp/src`、`peri-resources/src`，排除文件名含 `test` 的测试/fixture，未发现 `WorkState` / `WorkSnapshot` / `reduce_work` 生产运行残留。旧测试仍有旧符号，不能据此宣称整个测试树已迁移或可全量编译。
- 末轮重新扫描上述路径并扩展至 `peri-acp-types/src`；同样零匹配。另查 `peri-middlewares/src`、`peri-runtime/src`、`peri-controller/src`、`peri-tui/src`、`peri-workflow/src` 的上述三符号及 `from_state`，排除测试文件后零匹配。离线 executor 中有意读取/删除旧 `session_work_state` 表是停写迁移路径，不是普通运行旧 reducer 残留。
- `peri-resources/src/sessions/work_store/selection.rs:49` 从 selector 生成 SQL。Inbox、Processing、Effect、Draft 与 Command 查询在 SQL 层使用定向条件与 LIMIT；`read_set` 选择本命令关联事实，而不是载入全会话后调用旧 reducer。
- `peri-resources/src/sessions/work_store/payload.rs:9` 按 scope/id 精确读取不可变 bytes，引用 guard 不重读正文；`plan.rs` 写入单记录与引用。`selection.rs` 的 `READ_AUX`、Command/PendingCommands 显式读取命令/责任证据，不能误标为“所有路径零 payload 读取”。Transcript 提交仍会校验本次相关正文，属于当前边界，不是无关历史正文重编码。
- 没有做 EXPLAIN、历史量扩张、heap/RSS/CPU 基准；SQL 有 LIMIT **不证明**所有内部扫描成本 O(1) 或达到性能目标。历史 mutation 的保护检查也不纳入普通小命令性能结论。

### 3.2 优先警告：未确认终态 outbox 与显式新输入

**静态确认该行为符合目标，不登记为缺陷；Unknown/current barrier 没有因此直接删除。**

- `peri-acp-types/src/session_resources/work/lifecycle_test.rs` 已有 `unacknowledged_terminal_outbox_does_not_block_explicit_new_input`，检查新输入发布后终态 obligation 仍未 acknowledgement，而原 processing 被显式打断。此测试属于 worker 报告范围，本人未单独复跑。
- `work/mailbox.rs:123` 的显式发布验证 control generation、expected attempt、draft 身份及内容；没有把“允许新输入”实现为清空终态 outbox。
- `work_store/selection.rs:155` Availability 的 pending 判断来自 `kind='mutation' AND reconciled=0`；blocked 或 pending 时清候选。Terminal obligation 不单独阻断新候选，但仍保留在 head/独立记录。
- `peri-agent/src/agent/stages/work_ledger.rs:62` 保存未确认原命令，拒绝另一条命令绕过；Unknown 仍保留原命令，receipt 校验 session/mutation 身份后才放行。`work_boundary.rs:93` 检查当前 ledger pending，`:164` 检查持久化 PendingCommands。
- `work_reason.rs` 的 response commit 必须 await ledger 成功才替换 invocations；`work_dispatch.rs:20` 先 `ensure`、读取已提交 effect、核对不可变有效参数，再提交 BeginDispatch。未看到通过 outbox 例外直接跳过响应提交/工具屏障的路径。
- 限定：仅静态侧查，加上原命令 seal smoke；没有实测未知 receipt 注入、跨进程 restart 或多个实例的全部组合。

### 3.3 clear 与原稿保护

- `work_store/lifecycle.rs:8` 的 history guard 保留 required、unresolved effect、terminal obligation、legacyUnknown、current processing 及未确认 mutation 的拒绝条件，SQLite 与 remote 历史修改路径都调用同一 guard。
- 原稿作为 `session_inputs` 与不可变 evidence 保存；本次重复提交 smoke 实测保留。没有把终态 outbox 的准入例外当作 clear 自动放弃未决责任的授权。
- clear/rewind/reopen/父子删除仍只静态侧查，没有执行完整生命周期矩阵。

## 4. 最终状态与移交

| 项目 | 当前证据 | 主 agent / owner 下一步 |
| --- | --- | --- |
| 最终客户端 check/build | 首轮 client build exit 0；用户明确不用最终重建，最后源码未构建 | **用户豁免，不再执行**；不把早先 binary PASS 扩大到最终 revision |
| Agent/ACP/middleware test 编译 | 早先 ACP compile PASS；最后 Agent 3 errors/middleware 2 errors 据主 agent 已修但未复 check | 修复声明与验证结果分开；不声明最终测试树编译/全测 PASS |
| draft 撤回/再发送规则 | 两项新增 native withdrawal/resend smoke 包含于 main-run 9/9 PASS | 小边界闭环已验证，不扩大为真实 Agent/provider 全流程 |
| fresh blocked enqueue publication | 主 agent 报告最小修复已落源码，未运行 | 保留未验证风险，后续由用户实测；本轮不追加 Cargo |
| 显式可调用停写 17→18 executor | **此前“仅未调用 planner”阻塞解除**：public API 接通；主 agent 合成迁移 suite **3/3 PASS，exit 0** | 小型迁移行为已由 main-run 验证；不扩大到真实库或完整分类矩阵，不另开 Cargo |
| legacy 判定 | `migration_classification.rs` 已增加已证终态/未决分类；`import_blob` 与 `import_head` 按 unresolved 更新 `legacyUnknown`，不再一律置 1 | 仅静态确认；现有三项 public smoke 不包含已证终态不阻塞 case，不宣称该分支已行为验收 |
| Remote 本地 transport | main-run 初轮 10/11，修 fixture 缺失 `peri_store_meta` 后 11/11 PASS | 小型本地 transport 已验证；真实 Turso 仍未覆盖，不登记为生产 compaction 缺陷 |
| 新两项 withdrawal smoke | 已包含于 main-run Native 9/9；fixture 使用不同 event/input ID、mutation/delivery ID 与 source/canonical ref | **PASS（main-run）**；冻结 owned 源码，不扩大严谨矩阵 |

### 4.1 末轮迁移静态复查

- public 路由：`peri-resources/src/sessions/mod.rs:42` → `sqlite_store.rs:21` → `sqlite_store/session_data/work/offline.rs:12`。这次可以确认存在**可调用执行器**，不是只有 SQL 草图。
- `migration.rs:142` 校验 source_version 17、writers_stopped、backup_verified；执行器以 `create_if_missing(false)` 连接显式路径、单连接 `BEGIN EXCLUSIVE`，再检查库内 `PRAGMA user_version=17`。失败走 rollback；成功完成最终证据 guard 后才写版本 18 并 commit，commit 不确定作为错误返回。
- `offline.rs:7` 开始的 work/command/receipt/event 扫描使用 keyset、每页 64，消息按 rowid 分页导入。迁移允许遍历全部旧证据，但这是显式停写操作，不能混算普通变更性能。
- `migration_classification.rs:49` 判定可证终态历史：不识别的形状/字段、未结责任/queued 原稿等保守标未决；已证终态可归档而不强制阻塞。command/receipt 分类核对原 command 的 SHA256、session/mutation 身份及 resolution；未知/冲突保持未决。orphan event 存入独立 closed archive scope，不直接激活历史会话。
- 已保存 owner/child-resume 元数据、原 blob 与 canonical message 内容引用，不代表恢复了旧 invocation/processing 的执行权，也不代表 queued 原稿已映射为可直接发送的新 draft。未知义务依然需要显式对账，不会自动重放 provider/工具。
- **操作前置条件的限制**：approval 的两个布尔值是调用方声明，函数不验证备份文件的真实性或旧 writer 已退出。SQLite EXCLUSIVE 是事务互斥，不是外部进程停写证明；真实迁移仍需操作者实际停写和一致备份。
- 开发 owner 的 `peri-resources/tests/sqlite_work_offline_smoke.rs` 有 3 项：新 schema17 引用消息形状、合成未知原证据/正文迁移、坏正文失败回滚。静态可见使用 TempDir；主 agent 后续实跑 **3/3 PASS**，本人未运行，不扩展为严格历史分类/全部回滚矩阵。
- resources code-index 仍有 schema14/自动升级历史说明，不能作为本次 schema17 或 public executor 事实源；文档同步由主 agent 收尾，本验收不改其 owned 文档。

### 4.2 withdrawal 领域与 store 关联静态复查

- 普通 withdrawal 与 stop withdrawal 在 `mailbox.rs` 以 `expected_control_generation` 是否存在区分，分别写 Withdrawn / Queued；拒绝已 claim/projection 的 delivery、校验 generation，draft 与 dispose 产物进入同一 transition 的 writes。
- `work_store/selection.rs` 已按 delivery ID 定向读取 publication 对应 draft（LIMIT 2，用于拒绝歧义）；Stage 同 input ID 则定向读取 prior delivery，不扫所有原稿/投递。`schema.rs` 新增 `idx_inputs_publication`，`plan.rs` 的 draft UPDATE 同步 `fifo_seq`，不是只更新 record JSON。
- SQLite apply_original 仍将转换写集与 receipt 在同一事务内执行；这是静态事务结构证据。新增 smoke 后续 main-run 成功，但没有失败注入，不宣称事务中断完整验收。
- 前一静态快照发现 Publish `publication_id` 写 mutation ID，已通知 main/Bohr。后续读到的 `mailbox.rs` 已改写 delivery ID；`entities.rs` 已 public 导出 typed publication identity 与 draft binding，领域按 identity.input_id 取 draft，验证 revision/fingerprint、source ref 与 canonical ref，并允许 opaque event ID。先有静态修复证据，后由 main-run 新两项 smoke PASS 补齐小边界行为证据。
- publication producer/定向 read_set 由 Plato 与 main 继续联接；本验收不改 owner 源码。读取时 PublishStagedUserInputs 的 read_set 尚选择有界 Drafts 页，最终是否改为 typed input ID 定向读取仍由 owner 收尾，不扩大“所有命令已精确窄读”的结论。

**Scope completed**：用户授权的唯一 subagent 快速验收范围已完成，合成迁移、两项 withdrawal 与 Remote 本地 transport 证据已补齐。**Quick partial** 是最终验收结论：用户豁免最终重建/复 check，最后修改仍未构建，fresh blocked enqueue 修复未跑。各轮 smoke 只证明各自执行快照；不因准备合并而升级为完整通过。合并/dry-run 由主 agent 执行，不属于本人执行记录。

## 5. 用户实测入口与 DB 风险

实测入口以 implementation issue 为准；以下是后续用户自行选择的重建/启动方式，不是本任务继续执行指令。用户已豁免本轮最终重建，使用早先 binary 时须知道它不包含所有最终源码修复。若后续允许重新构建，可用独立新路径：

```bash
cd /Users/konghayao/code/ai/peri-workstate-redesign-20261006
mkdir -p /tmp/peri-workstate-user-test
./scripts/cargo-rmcp-patched.sh run --locked --offline -j 4 -p peri-tui -- \
  --session-store /tmp/peri-workstate-user-test/threads.db
```

这是用户后续操作，**本验收没有运行该命令**。该路径首次使用应为独立新库，不复用旧测试文件或默认 `~/.peri/threads/threads.db`。原仓库 `.env` 未复制，provider 凭据由用户自行显式配置。

新 schema 仍为 17；普通打开通过表形状识别并拒绝旧 schema17。显式 SQLite 入口现为 `peri_resources::sessions::migrate_stopped_work_store(path, &StoppedWriterApproval)`（library API，本文不臆造 CLI 迁移命令）；主 agent 已在合成 fixture 实测迁移/回滚，本验收本人未跑该入口。真实旧库迁移先停所有旧 writer、做一致备份、核实分类/显式调用与回滚限制；布尔 approval 或小 fixture PASS 不是授权、停写证明或真实库迁移成功证据。新实现写入新责任后不能只换回旧二进制。原 debug cache 是克隆缓存，不可作为本次新客户端已构建的证明。

## 6. 未覆盖

不覆盖最终源码 check/build、最终 Agent/middleware 测试编译、fresh blocked enqueue 最后修复、全 workspace/lib tests、真实 SDK/client/Agent 小循环、provider、真实 Turso、工具执行、跨实例接管、进程重启、多 fault injection、全部 clear/撤回/取消/委托竞争、完整 legacy migration、长期历史压力、heap/CPU/RSS/性能比较。没有改善百分比或资源收益数字。**性能与完整效果交用户实测**；不是将未经测量的性能改善作为合并结论。
