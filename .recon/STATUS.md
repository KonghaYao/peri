# 会话状态：Transcript 收缩 + ACP 历史分页

更新时间：2026-10-13 16:36（CST）

## 目标

实现 `/Users/konghayao/Desktop/peri-transcript-history-requirements.md`：
Full Compact 成功后核心 Transcript 真正释放已退出有效上下文的 own 历史；恢复只读有效范围；ACP `session/load` 支持 cursor 分页。

## 位置

- worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination`（基于 `origin/main` = `7f0de56e8`，与需求核查基线一致）
- 实现蓝图：`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`（含 S6/S7/S8 后续设计）
- 侦察报告：`.recon/A-core-transcript.md`、`B-acp-host.md`、`C-storage-contract.md`、`D-test-infra.md`、`E-tui-sdk.md`

## 切片进度

| 切片 | 状态 | 产出 |
| --- | --- | --- |
| 侦察（5 路并行） | 完成 | 五份报告，1274 行 |
| S1 契约层 | 完成 | `ActiveSessionSnapshot`、`load_active_snapshot`、`load_message_flags`（`peri-acp-types` +76） |
| S2 存储层 | 进行中（写测试） | SQLite/Remote 存储侧过滤 + `content_size` 热路径移除（`peri-resources` 15 文件 +771） |
| S3 核心收缩 | 排队 | `transcript.rs` `shrink_after_full_commit` + `persisted_payloads` → `active_payloads` |
| S4 消费者 | 排队 | 进度水位改 `MessageId`、delivery 三态去重、`v2_execute` 改用 `load_active_snapshot` |
| S5 验证 | 排队 | 全 workspace check + 三 crate 测试 + 不变量核对（产出 `.recon/S5-verification.md`） |
| S6 ACP 分页 | 设计已定 | 蓝图 §6：`HistoryCursor`/`HistoryPage`、keyset SQL、`dispatch_request` 模式前置分流、连接级 caps |
| S7 前端 | 设计已定 | 蓝图 §6：TUI 滚动加载、SDK 合并去重、取消 Compact 强制 reload |
| S8 rewind 有效视图 | 设计已定 | 蓝图 §6：边界重建 + `structure_version` 失效旧 cursor |

## 活动 workflow

- run_id：`01a1345b-65c1-7c30-abb5-b143a06fb612`
- 结构：串行 S1 → S2 → S3 → S4 → S5，每切片带 cargo 编译门禁
- 结果落盘：`.claude/workflow-runs/01a1345b-65c1-7c30-abb5-b143a06fb612/state.json`

## 关键命令

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
./scripts/cargo-rmcp-patched.sh check --locked -p <crate>
./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib
```

## 环境注意

- workflow 内 agent 的 cwd 默认是 `/Users/konghayao/code/ai/peri-v4p3`（主仓库，另一 checkout），所有路径必须显式使用 worktree 绝对路径。
- 主仓库有未跟踪改动（`.mcp.json`、`CLAUDE.md` 修改，若干 spec/issues 文件），与本任务无关，不要动。
