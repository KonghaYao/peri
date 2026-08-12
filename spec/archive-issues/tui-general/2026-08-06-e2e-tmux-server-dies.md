> 归档于 2026-08-11，原路径 spec/issues/2026-08-06-e2e-tmux-server-dies.md

# E2E: tmux server 在测试期间被终止，前 3 个测试基础设施不可用

**状态**：Fixed
**优先级**：低
**类型**：测试基建
**创建日期**：2026-08-06
**来源**：E2E 全量运行（2026-08-06，`e2e/e2e-results-2026-08-06.md` 问题 1）

## 问题描述

E2E 全量运行的前 3 个测试失败（10~17s 即挂，非断言失败，未重试）：

- `panels/model-switch.test.ts`（14s）：`can't find pane: tui-test-1786021672145-ppula9`
- `panels/plugin-uninstall-no-freeze.test.ts`（10s）：`no server running on /private/tmp/tmux-501/default`
- `scenarios/ask-user-question.test.ts`（17s）：`no server running on /private/tmp/tmux-501/default`

失败点均在 `TmuxTester.captureScreen`（`tui-tester/src/tmux-tester.ts:394`），即 tmux server 不可用，而非产品断言失败。从第 4 个测试起 tmux server 自动重建（`new-session` 隐式启动 server），后续测试全部正常。

## 现状

- 代码中无 `kill-server` 逻辑（`tui-tester/`、`e2e/helpers/`、`e2e/tests/setup.ts` 均无）。
- 运行前环境存在残留 tmux session `tui-test-1786021625001-jc9l1a`（内含挂起的 `dev.sh`，21:07:06 启动），残留 session 退出后 server 恢复。
- 疑因：残留 session/dev.sh 干扰导致 tmux server 被终止（如 server 内最后一个 session 退出时 server 关闭）。

## 期望改进方向

- E2E 运行前清理残留 `tui-test-*` session，避免基础设施干扰。
- 排查残留 dev.sh 挂起原因（若为测试残留，应在测试 teardown 清理）。

## 验收标准

- [ ] 运行 e2e 前自动清理残留 `tui-test-*` tmux session（在 setup/helper 或文档中）。
- [ ] 全量运行不再出现 `no server running` / `can't find pane` 类基础设施错误。

## 涉及文件

- `e2e/tests/setup.ts`、`e2e/helpers/peri.ts` —— 测试启动（候选清理位置）
- `e2e/CLAUDE.md` —— 运行前置说明
- `e2e/e2e-results-2026-08-06.md` —— 本次失败记录

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-06 | — | Open | agent | E2E 全量运行失败，创建 issue |
| 2026-08-08 | Open | Fixed | agent | 修复记录完整（killTestSessions 精确前缀匹配 `tui-test-`，e2e/CLAUDE.md 补充前置说明），手动验证残留清理生效；状态未随修复同步更新，本次盘点闭环 |

## 修复记录

### 2026-08-06 修复

**修复内容摘要**

- 说明：`e2e/tests/setup.ts` 原本已有按 session 名清理的逻辑（`killTestSessions`，beforeAll/afterEach/afterAll 三处调用），但匹配过宽（`includes` 匹配 `peri-e2e`/`tui-test`/`test-`/`minimal-`，其中 `test-` 易误伤非测试用途 session，如 tui-tester 文档示例 `test-1`）。
- 本次将清理逻辑收紧为**精确前缀匹配 `tui-test-`（`startsWith`）**：tui-tester 的 session 名生成器（`generateSessionName`，默认前缀 `tui-test`）产出的 session 均为 `tui-test-<ts>-<rand>` 格式，e2e/ 下无其他前缀使用者；非 `tui-test-*` 的 session（含用户手动创建的）不会被误杀。
- 清理保持幂等与失败容忍：`tmux list-sessions 2>/dev/null || echo ''` 在 server 不存在时静默通过，`kill-session` 失败被吞掉；不使用全局 `kill-server`（无法保证 server 内只有测试 session）。
- `e2e/CLAUDE.md` 补充运行前置说明：测试启动前自动清理残留 `tui-test-*` session。

**验证结果**

- 手动制造残留 session `tui-test-manual-1786027556` 后运行 `npm test -- tests/smoke/basic-question.test.ts`：测试通过（1 passed，19.23s），残留 session 被清理（tmux server 随最后一个 session 退出而关闭，`tmux list-sessions` 显示 `no server running`）。
- 过滤逻辑验证：同时存在 `tui-test-residual-*` 与 `my-work-session` 时，仅 `tui-test-*` 被选中，非测试 session 不被误伤。

**修改的文件**

- `e2e/tests/setup.ts`：`killTestSessions` 改为 `startsWith("tui-test-")` 精确前缀匹配，去掉宽泛 `includes` 匹配。
- `e2e/CLAUDE.md`：目标命令小节补充运行前置说明。
- `tui-tester/`（submodule）未改动。
