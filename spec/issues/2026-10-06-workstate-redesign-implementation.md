# WorkState 新实现：独立 worktree 并行实施

状态：代码已收敛，准备合并。2026-10-06 用户批准采用新实现、完全删除旧实现，要求大规模并行开发、单个 subagent 快速验收，完整效果由用户实测；后续明确不再重建，改为准备合并到原位置分支。

## 基线与授权

- 独立 detached worktree：`/Users/konghayao/code/ai/peri-workstate-redesign-20261006`。
- 起始 HEAD：`b7c6770d2c0a85a4e7bd5ca3fd24e28e630a6ee8`。
- 复制原工作树当时的 tracked diff 与未跟踪文件，保留已有诊断、输入与性能修复；不是只使用 HEAD。复制后的 tracked patch 已与冻结材料逐字节核对。
- 基线材料：`/tmp/peri-workstate-baseline-20261006-2iOGOn/`，包含原 status、staged patch、working patch、未跟踪清单及 HEAD。
- 原目录不修改，不 stash/reset/clean，不 commit、不 push、不直接执行用户数据库迁移。
- [设计提案](2026-10-06-workstate-redesign-proposal.md) 的推荐结构已获用户批准；首版采用推荐的保留证据/超额拒绝，以及 clear 不隐式放弃未决责任。批准迁移实现不等于授权操作真实数据库。

## 并行写入归属

| 工作 | 写入范围 |
| --- | --- |
| 领域与规则 | peri-acp-types 的 work 类型、转换、端口及对应测试 |
| 共同存储与迁移 | peri-resources 公共 planner、schema、payload、迁移与资源门面 |
| SQLite adapter | sqlite_store/session_data 下工作记录适配与定向测试 |
| Turso adapter | remote 工作记录与 journal 适配、定向测试 |
| Agent | peri-agent 的阶段、输入、委托和恢复消费者及测试 |
| ACP/SDK | peri-acp、相关 TUI/SDK、中间件消费与协议联接 |
| 主 agent | 协调接口、整合集成、现行文档与构建；不改尚由 worker 拥有的文件 |
| 快速验收 | 开发收敛后由单个独立 subagent 执行窄范围检查 |

## 实施底线

- 删除 WorkState、WorkSnapshot.state、from_state、reduce_work 和整份 state_json 运行读写，不保留别名、兼容 shim 或双写。
- 保留稳定命令身份与业务语义不等于保留旧实现。新 Interface 只允许有界 typed inspection/explicit evidence；领域转换只消费本次关联事实。
- Mailbox、Processing、Effect 分责，复用现有 Control、Transcript、命令与回执；请求/结果引用独立不可变载荷。
- 可靠投递、撤回竞争、原命令 Unknown、响应/工具屏障和精确委托不删。执行唯一性继续归 SDK。
- 普通状态变更不得读写无关历史正文；不能在分表后重建完整会话再调用旧 reducer。
- 旧库迁移仅提供明确停写入口；未知义务和原稿不静默裁剪。用户测试前明确 schema 升级与回退限制。

## 快速交付门槛

- [x] 六路生产实现集成，旧聚合入口删除。
- [x] 目标客户端首次构建通过；用户随后豁免最终重建，最后源码修正未重新构建，不把旧二进制当最终源码验证。
- [x] 单个 subagent 进行快速行为、旧路径残留及新库/迁移入口检查；不做完整生命周期矩阵、真实 Turso 或受控性能研究。
- [x] 用户获得独立 worktree、数据库风险和未验收范围；交付改为待合并材料，不再执行客户端重建。

本轮不提供 CPU/RSS 改善数字，快速验收不等于设计文档中的完整可靠性/性能验收。

## 执行证据

- 主 agent 首次 `build --locked --offline -p peri-tui` exit 0，日志 `/tmp/peri-workstate-client-build.log`；新构建 `target/debug/peri --help` exit 0，没有启动 provider 或真实会话。
- 唯一验收 subagent 的独立新 SQLite 行为 smoke 4/4 PASS，详见 [快速验收报告](2026-10-06-workstate-quick-acceptance.md)。
- 主 agent 最后运行 `test --locked --offline -p peri-resources --test sqlite_work_offline_smoke --test work_records_smoke -- --test-threads=1` exit 0，合计 9/9 PASS，日志 `/tmp/peri-workstate-final-smoke.log`。包括真实原稿/模型消息使用不同引用、opaque event 身份及发布 mutation/delivery 身份分离的撤回、stop 重排与再发送；合成旧库仅验证保留原证据、升级与失败回滚，不等于真实库迁移验收。
- Native 公开迁移入口为 `peri_resources::sessions::migrate_stopped_work_store(path, &StoppedWriterApproval)`；普通旧库打开继续拒绝自动升级。Turso 无对应公开离线 executor，不能宣称旧远端库可直接切换。
- 远端 adapter 的本地 SQLite transport 定向契约 11/11 PASS，日志 `/tmp/peri-workstate-remote-smoke.log`，不是实际 Turso 验收。首轮失败是测试 fixture 漏建元数据表，已使用生产初始化计划补齐。
- 额外 `check --tests` 发现旧聚合测试接口，已由两个追加开发 worker 迁移；它们不是额外验收者。ACP 单独测试编译通过。最后合并检查的 Agent 3 个旧字段断言及 Middleware 2 个测试错误已修正，但用户要求不再重建后没有再跑 Cargo；完整效果与最终合并快照的构建仍由用户验证。
- 最后一项 Agent 修正允许无活跃 attempt/local execution 时的显式新输入原子放弃旧 processing；冻结发布仍用原命令，未删除 Unknown journal/effect 证据。该项最后修改未重新执行定向测试。

## 合并交接

用户原目录当前为 `/Users/konghayao/code/ai/peri-v4p3`，分支 `pre-release/main`，读取时 HEAD 为 `0c6d58d709af11596e9f5b9a8796d31424db7181`，已比开发起点前进两次提交。待合并内容必须从冻结工作基线提取本次任务增量，再对当前目标作三方核对，不能把整个 detached worktree diff 覆盖原目录。原位置的 `CLAUDE.md`、其它 issue 与未跟踪文件不属于本次重构，保留；不 commit、不 push、不迁移真实 DB。

## 用户实测入口

以下入口为用户可选实测操作，不再由 agent 执行。worktree 的旧 debug 二进制不代表最终修正后的源码；用户可在合并后自行构建。

首轮推荐显式独立测试数据库，避免默认打开 `~/.peri/threads/threads.db`：

```bash
cd /Users/konghayao/code/ai/peri-workstate-redesign-20261006
./scripts/cargo-rmcp-patched.sh run --locked -p peri-tui -- \
  --session-store /tmp/peri-workstate-user-test/threads.db
```

这是交付后的用户操作，不是本任务执行记录。原仓库的 gitignored `.env` 未复制；直接使用构建脚本或新构建二进制启动，不假设 worktree 的 `dev.sh` 已具备原启动环境。需要原环境变量时由用户显式配置，不复制凭据到新文档。

使用真实旧库前停止所有旧 writer、完成一致备份，并核实最终迁移入口与回退限制；新实现产生新责任后不能仅回退旧二进制。旧库迁移与真实 Turso 的完整验收不在本次快速验证承诺中。
