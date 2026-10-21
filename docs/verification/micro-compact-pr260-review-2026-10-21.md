# PR #260 对抗审查修复验证

日期：2026-10-21。修复基点 `1f730e2453f5b476a690ced0814a015b9a22afd5`，在独立 worktree 完成。原PR已合并，交付改用 `fix/micro-compact-pr260-review`，基于 `27c35b6d9`（PR #260 合并提交，与修复基点的完整Git tree相同，既有测试源码没有变化）。对应外部审查的两项 P1、三项 P2；初始交付记录见 [原实现验证](micro-compact-redesign-2026-10-21.md)。

## 修复与行为证据

| 审查项 | 修复后的行为 | 回归入口 |
| --- | --- | --- |
| P1 跨用户 turn 消费证据丢失 | ACP 会话持有有界账本，显式注入新 Session / StageContext；普通 turn 保留 preview 升级证据。冷加载只在成功响应持久化后，根据重新完整曝光的 canonical typed 同步终态确认；未知、运行中和未结后台结果继续保护 | `peri-agent/src/agent/stages/micro_turn_exposure_test.rs`、`compact_v2/exposure_test.rs`、`planner_test.rs` |
| P1 fork 把授权归档碎片当普通可执行历史 | 冻结授权补入的正文标记 `recovery_only`，保持 excluded；不参与普通调用闭合，rewind/refork 不激活。普通历史仍校验闭合，授权外父数据不补读 | `peri-acp/src/dispatch/session_fork_archive_test.rs`、`peri-acp-types/src/store/history_archive_test.rs` |
| P2 权限收紧后投影恢复仍发送失效回查入口 | Reason 在最终权限过滤目录发布后、补充 Full / 模型请求前校验可直接调用的 ArchiveRead 和有界存储端口；低于75%同样检查。缺少能力明确失败，恢复权限可继续使用原投影与引用 | `peri-middlewares/src/subagent/tool/tool_test/resume_archive_test.rs`、`peri-agent/src/agent/stages/archive_capability_test.rs` |
| P2 大量 block 的元数据挤满回查预算 | 新目录记录不同 block 类型集合；旧目录只在响应中去重，不改变已有引用。按实际元数据、游标和 JSON 转义计算剩余片段预算，提交前证明最大预算可返回非空片段 | `peri-agent/src/agent/compact_v2/micro_block_budget_test.rs`、`context_archive_test.rs` |
| P2 每4KiB串行远端验证 | 整个计划固定同一 committed tail，每批最多32块/128KiB，remote合为一次 read_batch；逐块与完整成员 SHA256均核对，一批在飞，取消不发布投影，最终一次原子提交 | `peri-agent/src/agent/compact_v2/micro_batch_test.rs`、`peri-resources/src/sessions/remote/session_archive_test.rs`、`peri-acp-types/src/context_archive/batch_test.rs` |

大 block 回归使用2500个各600 ASCII字符的 text block，跨生产 planner、原子提交和新 reader，在4096 token预算下分页重建精确 canonical 字节及摘要。权限回归覆盖已提交 preview 与整组省略：收紧子 Agent 定义后 resume 在模型调用前失败，恢复权限后原引用可读。fork 回归覆盖 mixed batch 与 result-only，经 Full → child → fork → cold load → rewind → refork 验证。

## 完整路径测量

[实验结论与边界](../experiment-micro-archive/conclusion.md)记录两轮实际命令、原始输出、独立验证及对抗审查。两轮均经过 planner → 实际 SQLite append/flush → 验证 → 原子提交；正文读取用受控端口，未访问真实 Turso。

1. 500个目录/1500块：metadata API 500→1，body batch API 1500→47。
2. 32个大结果目录/3968块：metadata API 32→1，body batch API 3968→124。新总wall4377ms高于旧2648ms，总CPU2633ms高于旧2345ms；保留反证。
3. 两轮只证明调用数下降、固定尾界、有界 canonical 响应及单次提交。不证明生产延迟、吞吐、RSS或稳定CPU收益；单样本、固定旧→新顺序及旧baseline没有新增完整成员SHA/响应预算检查等局限均已披露。

## Rust 回归结果

命令均设置 `CARGO_INCREMENTAL=0`，Cargo 经 `./scripts/cargo-rmcp-patched.sh`，使用 `--locked`。

| 范围 | 实际通过 | 忽略 |
| --- | ---: | ---: |
| peri-agent lib | 1281 | 5（含2个本轮手动执行的benchmark） |
| peri-acp lib | 1070 | 1 |
| peri-acp-types lib | 688 | 0 |
| peri-resources lib | 588 | 34 |
| peri-middlewares lib | 1884 | 2 |
| peri-mcp-common lib | 32 | 0 |
| peri-mcp-workspace lib | 478 | 2 |
| Compact集成测试 | 59 | 0 |

七个crate的lib合计6021通过。首次七crate组合命令以101结束：旧Compact夹具缺少`is_direct`，造成4条断言失败及1条探测等待；provenance夹具携带v3投影却没有恢复权限，也被新校验正确拒绝。等待的Agent测试二进制被显式终止，其余六个crate均得到非空完整的成功终态。补齐两处夹具权限声明后，仅重跑受影响的Agent全量lib（exit0，1281通过），不放宽生产校验或原断言。两次命令为：

```bash
CARGO_INCREMENTAL=0 ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent -p peri-acp-types -p peri-resources -p peri-middlewares -p peri-acp -p peri-mcp-common -p peri-mcp-workspace --lib --no-fail-fast
CARGO_INCREMENTAL=0 ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib --no-fail-fast
```

集成命令exit0，逐目标2/21/7/10/8/7/4通过，无忽略、无过滤：

```bash
CARGO_INCREMENTAL=0 ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --test compact_authority_projection_test --test compact_failure_adversarial_test --test compact_followup_behavior_test --test compact_pressure_adversarial_test --test compact_session_adversarial_test --test compact_session_manual_adversarial_test --test micro_compact_wire_test -- --test-threads=1
```

文档测试命令exit0：`test --locked -p peri-agent -p peri-acp-types -p peri-resources --doc`，21通过、2项既有忽略；Resources实际0项，未计入有效测试数量。没有把历史workspace的22项结果当成本轮重跑。

`check-changed.sh` exit0；格式检查、22条依赖方向规则（0违规）与本次新增Markdown本地链接检查均通过。文件尺寸全量扫描exit1：仍有7个基线已有超限文件；本次53个修改源码/测试文件全部≤1000行。没有通过清理无关WIP解决检查问题。

全工作区 `check --locked --workspace --all-targets` 与 `clippy --locked --workspace --all-targets -- -D warnings` 已在修正权限夹具后通过（回执 `20261021T064454Z`：exit0/0，10s/10s）。该回执早于最终Unix-only基准辅助函数标注；提交后再次生成与提交快照一致的回执，结果在PR说明核对。最终标注仅改变Windows测试编译范围，Mac测量未重采样，报告保留采样指纹并列明交付指纹。`CARGO_INCREMENTAL=0 lefthook run pre-commit` exit0：check、Clippy、fmt、layer-imports四项均通过（39.71s）；提交hook仍正常执行。

## 验证边界

- 75% Micro入口、95% Full升级与45%尽力目标未改变；74.99%无新增自动投影。
- 未变更数据库表或列，隔离标记存于现有 projection JSON。
- 本轮不调用付费 provider、不执行TUI E2E、不访问生产远端数据库；真实provider用量验证仍为发布验收项。
