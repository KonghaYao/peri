# Micro Compact 重设计验证记录

日期：2026-10-21。实现基于 `57d85257159592b3875c5c6aa046a16bbfd42408`，行为契约见 [Micro Compact 设计](../design/micro-compact.md)。本记录只说明此次执行的验证，不替代发布验收。

## 行为验证

1. 生产 RCRA 路径验证成功响应持久化之后才确认原文暴露；失败响应、最新结果和未完成任务继续保护。门控测试覆盖有/无 usage 的 74.99%、75.00%、75.01%，沿用 75% Micro、95% Full、prepared 阶段只补检 Full 的规则。
2. planner → 原子提交 → 新 Store 恢复 → renderer → `AgentModelBridge` → Anthropic/OpenAI adapter 捕获最终请求；核对调用配对、保留参数、签名依赖、媒体与 canonical 不变。这些是离线 adapter 测试，没有请求付费 provider。
3. 两次 Micro、两次 Full、摘要不含任何引用和新进程读取仍可回查；覆盖本地/远端读取、摘要冲突、冻结继承、独立 fork、子会话 Full 后 fork、rewind 撤销、提交未知与取消。远端测试使用本地协议夹具，没有访问生产数据库。
4. v2 按既有规则恢复；v3 损坏版本、缺失目录、错误身份及 Reminder 非法动作均拒绝发送，不静默展开完整历史。

测试命令统一使用 `./scripts/cargo-rmcp-patched.sh`，`--locked`：

| 范围 | 命令后缀 | 结果 |
| --- | --- | --- |
| Agent 单元测试 | `test --locked -p peri-agent --lib` | 1,263 通过，3 项既有忽略 |
| Compact 集成测试 | `test --locked -p peri-agent --test <下列测试目标>` | 59 通过 |
| Middlewares | `test --locked -p peri-middlewares --lib -- --test-threads=4` | 1,882 通过，2 项既有忽略 |
| ACP 类型 | `test --locked -p peri-acp-types --lib` | 686 通过 |
| Resources | `test --locked -p peri-resources --lib` | 587 通过，34 项既有忽略 |
| MCP common | `test --locked -p peri-mcp-common --lib` | 32 通过 |
| MCP workspace | `test --locked -p peri-mcp-workspace --lib` | 478 通过，2 项既有忽略 |
| ACP（含 fork） | `test --locked -p peri-acp --lib -- --test-threads=4` | 1,062 通过，1 项既有忽略 |

集成目标为 `compact_authority_projection_test`、`compact_failure_adversarial_test`、`compact_followup_behavior_test`、`compact_pressure_adversarial_test`、`compact_session_adversarial_test`、`compact_session_manual_adversarial_test`、`micro_compact_wire_test`。工作区 doc tests 为 22 通过、5 项既有忽略。交付门包括 `check-changed.sh`、`verify-async.sh` 的 workspace/all-targets check 与严格 Clippy、格式检查、依赖方向检查及 pre-commit；执行结果见 PR 验证说明。

## 合成样本测量

环境：macOS 26.5.1 / arm64，Rust 1.99.0，debug test profile，`CARGO_INCREMENTAL=0`。直接运行已构建的 Agent 测试二进制，`--exact <test> --nocapture`；万次样本外包 `/usr/bin/time -l`。内存数字不包含编译器，包含测试夹具、规划与断言；当时有其他交付检查并行运行，时间不能作为生产延迟承诺。

| 指标 | 单条用户指令、100 步 | 10,000 次工具交换 |
| --- | ---: | ---: |
| canonical 消息数 | 201 | 20,001 |
| 候选 / 整组归档 | 100 / 100 | 9,998 / 9,998 |
| 同快照 before tokens 估算 | 1,050,956 | 6,057,504 |
| 同快照 after tokens 估算（含目录/收据） | 95 | 1,304 |
| 新增净收益估算 | 1,050,861 | 6,056,200 |
| 正文结果字节数 | 不单独采样 | 24,000,000 |
| canonical 序列化字节数 | 不单独采样 | 26,727,866 |
| 源身份指纹次数 / 覆盖字节 | 不单独采样 | 19,997 / 26,724,950 |
| planner 消息索引扫描数 | 不单独采样 | 20,001 |
| 规划耗时 | 不单独采样 | 5,167 ms |
| 测试进程 wall time | 不单独采样 | 8.04 s |
| 最大 RSS | 不单独采样 | 236,142,592 bytes |

100 步测试断言可回收样本净估算至少下降 80%，用户指令与全部 canonical 消息保留。最近两步是软保护，样本超出软预算时允许继续降级。万次测试断言目录根至多一个、最终估算低于 10,000 tokens，避免每次交换生成线性增长的可见收据。

源身份指纹字节只统计提交依赖的 SHA-256 输入；不代表索引、目录分块摘要、最终渲染与存储验证的累计读取总字节。测试未把 token 估算伪装成真实 provider usage，也不据此声称所有实际会话能达到该压缩率。

复现入口：`compact_v2/planner_test.rs` 的 `test_one_user_hundred_steps_archive_old_history_with_default_age` 与 `test_ten_thousand_exchanges_have_bounded_index_and_net_projection`。

## 验收边界

1. 真实 provider 请求与用量校验仍是发布验收项；本次没有调用付费模型，没有执行 E2E 或生产远端数据库测试。
2. 缺少可信完整替代证据的 Goal/Todo、进度/提醒和未结任务继续保护；错误保留独立诊断，不按相同前缀合并。独立用户媒体保留，归档附件回查恢复 canonical 数据，尚不自动重建 vision 输入。
3. 未新增数据库表或列。全量文件尺寸扫描仍有 7 个基线已有超限文件，本次修改的源码与测试均不超过 1,000 行；不把存量超限描述为全库通过。
