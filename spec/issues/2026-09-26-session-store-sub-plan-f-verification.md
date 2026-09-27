# 会话资源拆分 — 子计划 F：契约回归、故障验证与云端实验

> 状态：验证计划，所有待新增检查尚未执行；不代表已有测试通过。日期：2026-09-26。
> 上级：[总计划](2026-09-26-session-store-plan.md)。覆盖 [A](2026-09-26-session-store-sub-plan-a-contracts.md) / [B](2026-09-26-session-store-sub-plan-b-local.md) / [C](2026-09-26-session-store-sub-plan-c-turso.md) / [D](2026-09-26-session-store-sub-plan-d-configuration.md) / [E](2026-09-26-session-store-sub-plan-e-consumers.md)。遵循 [testing.md](../../docs/standards/testing.md)。

## 1. 验证层次与证据规则

1. **纯逻辑**：显式输入/输出验证 fork ID/flags 映射、继承区、字段 patch、locator 和错误映射，不依赖网络、真实时间或全局环境。
2. **数据行为契约**：相同场景与后置条件覆盖 SQLite 和远程 adapter，不断言两者共享 SQL 或事务步骤。
3. **资源/执行生命周期**：真实门面 + 数据实现 + 本机执行；OS 锁、进程重启、dirty/close 用真实进程，不以 mock lease 替代证明。
4. **跨层链路**：ACP new/load/resume/fork/prompt/close 与 Agent transcript/subagent 经真实资源门面；模型和外部工具在相应边界替换。
5. **显式云实验**：唯一外部网络验证层，使用独立授权目标、合成会话、真实 Turso adapter；不加入默认 offline CI，也不能被本机模拟通过替代。

区分已有用例、拟新增用例、实际执行结果。每项证据记录命令、exit、命中测试数量、环境、失败或 skip 理由；0 tests/ignored/缺凭证不得算通过。故障先被目标回归暴露再修复，不以多跑几轮证明确定性。

## 2. 现有回归锚点

以下文件/测试已在源码确认，迁移时保护其行为而非内部构造方式：

| 范围 | 现有锚点 |
| --- | --- |
| 默认打开/降级 | `peri-resources/src/context_test.rs` 的 `test_open_with_busy_schema_lock_degrades_to_read_only`；`sessions/default_path_test.rs` |
| schema | `sessions/sqlite_store/schema_test.rs`：`test_single_database_failed_upgrade_rolls_back_schema_and_version`、`test_single_database_future_version_is_rejected_before_writing`、保留历史及辅助表场景 |
| legacy 原子接纳 | `sessions/sqlite_store/legacy_test.rs`：`legacy_adoption_commits_snapshot_once_across_concurrent_restorers`、`legacy_adoption_failure_rolls_back_binding_and_snapshot`、`legacy_children_follow_adopted_root_execution_owner` |
| owner/close | `sessions/sqlite_store/workspace_test.rs`：`test_worktree_bound_writes_require_owner_and_metadata_cannot_rebind`、`test_worktree_clean_waits_for_admitted_mutation_before_releasing_os_ownership`、`test_worktree_cancelled_mutation_remains_dirty_and_cannot_publish_clean` |
| 跨进程 dirty | 同上：`test_worktree_execution_competes_across_processes_and_crash_remains_dirty`、`test_worktree_dirty_reset_held_stale_and_exact_generation` |
| compact | `sessions/sqlite_store_test.rs`：`test_commit_compaction_lifecycle_persists_flags_and_appended_messages_in_order`、`test_commit_compaction_lifecycle_rolls_back_flags_and_appends_when_message_is_missing` |
| inherited | `sessions/sqlite_inherited_context_test.rs`：跨 reopen payload/flags 冻结、坏版本/外来 flags、不存在截止点和循环 |
| ACP | `host/requests_test.rs`、`host/compact_recovery_test.rs`、`dispatch/session_fork_test.rs`、frozen snapshot 测试 |
| Agent | `session/transcript_test.rs`、`session/exec/executor_helpers/compact_cancel_test.rs`、subagent 测试 |
| Runtime | `peri-runtime/src/runtime_test.rs`：持久化失败保留映射、destroy 取消可重试；本次不迁移其事实所有权 |
| TUI | meta/CLI 相关测试及 load reservation、compact replay reload；不建立渲染输出单测 |

## 3. 新增行为覆盖表

编号是计划覆盖项，不是已经存在的 test 名称。

| 编号 | 场景与断言 | 负责计划/测试层 |
| --- | --- | --- |
| V-01 | 接口审阅：无事务句柄、CAS/隔离参数、SQL batch、底层 retry token；业务点无 adapter 类型分支 | A + 静态审阅 |
| V-02 | fork 给定 ID 映射保持工具往返、projection 引用及 flags；ancestor 不变 | A/E + 纯逻辑 |
| V-03 | 新建/child/fork 完整保存后才可正常加载；初始化失败/取消不能留下可执行半成品 | B/C/E + 契约/生命周期 |
| V-04 | append 混合 Message/Reminder 保序、flush 后新连接重读；批次失败不假成功 | B/C/E + 契约 |
| V-05 | compact 整体生效、计数/缓存一致；不存在/跨 thread 的 flags 目标整项失败 | B/C + 契约 |
| V-06 | frozen 不覆盖，legacy winner 返回同一权威快照；绑定缺失/损坏/外来不混为 legacy | B/C/E + 契约/ACP |
| V-07 | inherited payload+flags 冷恢复，子会话写入仍要求根 owner | B/C/E + 契约/Agent |
| V-08 | rewind/delete 精确目标，删除树后计数/查询一致；未知截止点保留现行行为 | B/C/E + 契约 |
| V-09 | 状态/标题定向更新不覆盖并发字段；列表不读取大 blob，分页不全量下载 | B/C + 契约 |
| V-10 | 同 StoreId 多 locator/进程仅一 owner；工作区替换/移动拒绝；只读历史独立 | B/C + 真实进程 |
| V-11 | root close 等待所有已准入写入与子任务；取消 future 不释放真实后台写入 | B/C/E + 确定性屏障/进程 |
| V-12 | 不支持/只读/只写在副作用前失败；确定缺失与未知/不支持不同 | A/B/C/D + 契约 |
| V-13 | `.env` 不自动切存储；locator 冲突、UUID 错误在连接前失败；meta 不创建登记 | D + CLI/资源 |
| V-14 | 外来 installation 或 registry 丢失：历史可读、执行拒绝，不自动 rebind | B/C/E + 重启 |
| V-15 | 生产 ThreadStore 旁路消失：旧符号已删除，`cargo build --workspace --all-targets` 与 Clippy 通过；grep 只用于发现遗漏 | E + 编译证据（A §7.1） |
| V-16 | 默认 SQLite schema、旧历史和降级语义不变；跨平台锁/路径保证保持 | B/D + 既有回归 |
| V-17 | 写入三态：只有 `Applied` 或 `NotApplied` 释放准入；取消/超时/丢响应后 `finish` 未发生，同根后续写入与 clean 被拒；普通 `Err` 不再自动 finish | B + 契约/注入 |
| V-18 | v7 迁移：v6 库（含 `clean=0` 的 dirty 行与辅助表）升级后 dirty 代际原样保留、行数不变；迁移中途失败整体回滚且 `user_version` 仍为 6；只读打开 v6/v7 均不迁移、不建表；future 版本被拒 | B + 契约 |
| V-19 | 删除墓碑：`DELETE FROM threads` 后 `session_lifecycle_commitments` 墓碑仍在（不被 cascade），`deleting` 半态按 `deleted` 幂等处理，同 identity 不可重新登记；未决持久化存在时删除被拒 | B/C + 契约 |
| V-20 | 登记链拒绝路径：无本机登记 + 远端已有数据 → 只读历史且不自动登记；locator 别名不产生第二个锁域；显式只读不初始化 schema/StoreId/登记/owner、不建锁文件 | B/C/D + 契约/真实进程 |
| V-21 | 准备阶段只读：`PreparedSessionInputs` 准备期不写插件缓存（清单缺失直接失败）、不启动 MCP/LSP/hook/cron、不创建 thread/lease；装配与 frozen 使用同一对象，无第二次 config/plugin 读取 | E + 契约 |
| V-22 | C-01 前置条件 P1–P7 有实测证据（原子提交、唯一键竞争、写串行化、冷连接权威读、重试策略、超限拒绝、记录不清理）；未证明时远程写保持关闭 | C + 隔离实验 |

## 4. 可控远程故障矩阵

在 C 的私有传输 seam 注入延迟/断连，控制本地 adapter 真实数据行为的进度；测试不能只让一个 mock 返回预设成功后再断言成功。使用屏障/通知，不以 sleep 猜测请求到了哪一步。

| 故障 | 观察点 |
| --- | --- |
| 发送前失败 | 无远端数据变化，无无谓 dirty 清理授权 |
| 中途约束失败 | 整项行为无部分可见结果；pipeline 后续 SQL 不错误提交 |
| 保存成功但响应丢失 | 原行为只生效一次；恢复得到原结果，热态不能假回滚 |
| 延迟写晚于客户端取消/重启 | 恢复封闭与原操作竞争后至多一方生效，迟到写不能污染新执行 |
| 本机登记落盘失败 | 发送前失败或持续阻塞，不能丢失未决操作证据 |
| 已收到成功但本机结清失败 | 保留未确认状态，重开可收敛，不发布错误 clean |
| 权限拒绝/限流/服务不可用 | 安全类型化结果，没有本地库 fallback、无秘密输出 |
| 永久网络阻塞 | 请求/close 有界返回未完成，积压策略可见且内存有界，不悄悄丢消息 |
| new/fork 清理失败 | 不发布可执行会话，失败资源仍有 owner 与恢复路径 |

使用相同 scenario 函数验证 SQLite 与选定 SDK adapter 的行为：限定在一个契约测试目标内参数化，不建立全仓库共享 `test_helpers` 框架。私有实现测试可断言收据/封闭细节，但外部契约测试只断言用户可观察结果。

## 5. 真实 Turso Cloud 实验方案

### 5.1 运行前置条件

- **G-03 已按用户授权解除（仅限下述范围）**：用户确认 `.env` 指向**测试库**，允许初始化本次 schema、写入/读取合成会话，并**只清理本轮创建的数据**。仍不得：读取或上传本机真实历史、真实项目指引/frozen；清空共享库；drop 未知表；新建计费资源。凭证只在 C 阶段脚本/测试进程内经 dotenv parser 注入必要键，禁止 Read/cat `.env`、env dump、shell source/eval、打印 URL/token，也不把真实值放进工具参数、源码、fixture 或报告——只输出键是否存在、脱敏引擎与验证结果。
- 库引擎不由计划预设：C-01 对授权测试库做**只读**、最小化的引擎探测（版本/引擎标识），据此在 Turso 引擎与 libSQL 引擎两条 SDK 路线中选定一条并记录脱敏结论（[C §5.0](2026-09-26-session-store-sub-plan-c-turso.md)）；探测失败即记阻塞，不靠「拒绝另一条路线」代替交付。若发现目标库中存在本任务之外的数据，保留并停止任何破坏性初始化，改申请专用测试库。
- runner 显式启用云模式，通过安全 dotenv parser 加载指定路径，只注入必要环境；禁止 shell source、env dump。
- 测试工作区使用 tempdir + 合成 Git/非 Git目录；生成合成 frozen、消息和 tool results，不读取真实仓库指引作为云 payload。
- 测试 run ID 与所有创建对象登记在本机临时 manifest；只能清理该清单内对象。不创建账户、计费资源或数据库，除非另获授权。

### 5.2 链路

1. 用新门面/真实 Turso adapter 创建会话；经脚本模型执行一轮含工具往返的 Agent/ACP 流程，确认 flush。
2. 更新标题、追加 reminder，执行 Full compact；校验摘要与 flags、缓存视图一致。
3. 创建独立 fork 与 owned child，比较身份映射、binding/frozen 与继承区；原会话保持不变。
4. clean close，关闭连接并终止测试宿主；由新进程仅凭相同定位和本机执行登记 cold load/resume。
5. 比对 canonical payload、flags、frozen/inherited 字节和列表摘要；执行 rewind，再从独立连接确认结果。
6. 只读连接验证所有读取和写拒绝；如不能获得只读凭证，记录该云场景未验证，不用本机模拟替代。
7. 删除本轮对象并独立重读确认清理；失败列出安全的残留标识，不输出正文或连接字符串。

「两 adapter 同一契约」和「真实云链路」分别记录；SDK smoke test、HTTP 200 或本机服务端通过均不足以代替此链路。

### 5.3 性能观察

相同合成数据规模按小/中/长历史采样，固定字段内容和消息形状，记录规模、运行次数和环境，不预填通过阈值或收益：

- new / load / append+flush / fork / compact / list / close 的 p50、p95 或逐次样本（样本不足时不造百分位）。
- 各行为请求数、读写量、排队消息数/字节、峰值内存与关闭等待。
- SQLite 迁移前/后对比与 Turso 单独报告；云 RTT 不拿来掩盖本地退化。
- 慢网络下队列增长、批量写/读取是否避免每条消息一次往返。

## 6. 命令与测试落点

以下命令是后续执行方案；本轮只跑了 §6.1 的基线，且没有运行任何云相关命令：

```bash
cargo test -p peri-resources --lib
cargo test -p peri-acp-types --lib
cargo test -p peri-agent --lib -- transcript
cargo test -p peri-agent --lib -- subagent
cargo test -p peri-acp --lib -- frozen_snapshot
cargo test -p peri-acp --lib -- compact_recovery
cargo test -p peri-acp --lib -- session_fork
cargo test -p peri-tui --lib -- load_reservation
cargo test -p peri-runtime --lib
cargo test -p peri-controller --lib
```

新增集成目标（尚不存在；review-2 固定了精确命令与目标名）：

```bash
# 共同行为契约（两 adapter 用同一 scenario 参数化）
cargo test -p peri-resources --test session_resources_contract -- --list   # 先确认命中数 > 0
cargo test -p peri-resources --test session_resources_contract

# 显式云实验（本轮绝不执行；需 G-03 解除 + 隔离测试库授权）
cargo test -p peri-acp --test session_resources_turso -- --ignored --list  # 先确认命中数 > 0
cargo test -p peri-acp --test session_resources_turso -- --ignored
```

命名与命中规则：

- 目标名与命令逐字固定为 `session_resources_contract`（`peri-resources/tests/`）与 `session_resources_turso`（`peri-acp/tests/`）；改名必须同步 F 与本表，否则证据无效。
- 执行前必须 `--list`：**目标不存在时 cargo 以退出码 101 报 `no test target named …`**（§6.1 已实测）；`--list` 命中 0 条或退出码非零一律不算通过，不看 grep。
- 选择器执行前同样以 `--list` 确认命中；新增文件必须在 module 中挂载。CLI meta 测试需按现有 binary/integration 目标运行，不能假定 `--lib` 会覆盖。最终按改动范围跑各受影响 crate 全测试、doc tests、格式和 Clippy，命令只支持其覆盖的结论。
- 确定性故障测试使用 peri-resources 的私有传输 seam，经 `#[cfg(any(test, feature = "test-support"))]` 启用（A §7.1）；该 feature 不被生产二进制启用，F 记录实际启用方式与命令。若 SDK 无本地无云服务端，只把传输故障归默认测试，SQL/服务端语义归显式实验，报告该证据边界。
- 云测试未显式选择时 ignored；显式选择后缺凭证/未隔离须报告阻塞或非零退出，不静默通过。

### 6.1 本地基线（review-2 实测，2026-09-26）

| 命令 | 退出码 | 结果 |
| --- | --- | --- |
| `cargo test -p peri-resources --lib -- --list` | 0 | 156 tests, 0 benchmarks |
| `cargo test -p peri-resources --lib` | 0 | 156 passed; 0 failed; 0 ignored（7.37s） |
| `cargo test -p peri-resources --test session_resources_contract -- --list` | 101 | `no test target named … in peri-resources package`（目标尚未创建） |
| `cargo test -p peri-acp --test session_resources_turso -- --ignored --list` | 101 | 同上；peri-acp 现有目标仅 `concurrent_bg_agent_test`/`integration_test`/`prompt_cache_boundary` |

本 crate 基线无预存在失败。工作树中 `peri-middlewares/src/mcp/mod.rs` 的修改与 `builtin_spike_test.rs` 属他人任务，本组测试不依赖、不修改；跑全 workspace 测试时若出现该处失败，按外部改动记录而非本 issue 结论。

review-3 复核（2026-09-26，本轮，同样未执行云命令）：把 `HOME` 隔离到 `mktemp -d`（保留真实 `CARGO_HOME`/`RUSTUP_HOME`，避免任何默认路径落到真实家目录）后复跑——`--list` exit 0 / 156 tests；`--lib` exit 0 / 156 passed / 0 failed（8.03s）；`cargo test -p peri-resources --test session_resources_contract -- --list` 与 `cargo test -p peri-acp --test session_resources_turso -- --ignored --list` 均 exit 101（`no test target named …`）。与 review-2 记录一致，无新增预存在失败。


## 7. 完成审查

2026-09-27 compact / cancel 专项已修复并验证，修复提交 `61315df8`：手动 compact 校验 canonical 历史，命令 done 与实际取消终态保持一致。稳定入口见 [Agent 索引](../../docs/code-index/peri-agent.md) 与 [ACP 索引](../../docs/code-index/peri-acp.md)。专项 issue 按 `DOC-HISTORY-001` 关闭并移出活动列表；完整调查与修复记录可用 `git show 61315df8:spec/issues/2026-09-27-session-adapter-compact-cancel-behavior-audit.md` 查看。

| 专项验证命令 | Exit | 结果 |
| --- | --- | --- |
| `cargo test -p peri-agent -p peri-acp --lib -- --test-threads=1`（沙箱外） | 0 | Agent 863 / ACP 725 passed；含新增 4 项 Host 跨轮、冷读与 MPSC wire 回归 |
| `cargo test -p peri-acp --test compact_command_contract_test -- --test-threads=1` | 0 | 原始 2 项复现由 failed 转为 passed |
| `cargo test -p peri-tui --lib kit::acp_notifier::tests::test_agent_done -- --test-threads=1` | 0 | 2 passed，cancelled 通知转为 TurnInterrupted |
| `cargo clippy -p peri-agent -p peri-acp --all-targets -- -D warnings` | 0 | 通过 |
| `cargo fmt --check` / `git diff --check` / commit pre-commit hooks | 0 | 格式、check、Clippy、typos 与依赖门通过 |

首次沙箱内 ACP 全库运行因本地 HTTP mock server 被禁止监听而有 6 项失败，获准沙箱外重跑后全通过。本次 10 个 Rust 文件均低于 1000 行；全仓 size 扫描仍有 43 个本任务范围外超限文件（exit 1），不宣称全库满足规模限制。真实云、UI 按键 E2E 与跨平台矩阵未执行；冷读场景是新连接读快照后重建 Host，不冒充完整 session/load wire 或跨进程恢复。本专项不替代本计划其余 V 项验收。

- F-01：实施前基线，登记已有测试实际结果和非本任务失败（§6.1 已完成本 crate 基线）。
- F-02：A/B 的纯逻辑、门面/SQLite/owner 及 schema 回归（含 V-17/18/19）。
- F-03：C 的确定性故障与 C-01 SDK 前置证据（V-22 的 P1–P7）。
- F-04：D/E 跨入口、跨层和新旧旁路清理审查（V-15 用编译证据、V-20/21）。
- F-05：显式云实验及性能/清理证据；G-03 未解除前保持 blocked，不写“已验证”。
- F-06：核对所有 V 项有归属和结果，同步受影响 standards/design/code-index；不把计划表勾选当运行证据。

没有真实 Turso 冷恢复证据、未知写入收敛证明或整体旧旁路清理，就不能关闭母 issue。任何 skip/ignored/0 tests/缺凭证都记为未通过。
