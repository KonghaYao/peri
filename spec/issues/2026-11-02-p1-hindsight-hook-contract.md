# P1：Hindsight 兼容：hook session_id、PERI_DB_PATH 与 hookSpecificOutput

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：错误 / 兼容性。
**创建日期**：2026-11-02。
**来源**：Hindsight 插件接入审计（外部 issue: KonghaYao/hindsight-plugin#1）。
**确认方式**：主 agent 代码复核（基线 `1655935ae`）。

## 确认证据

- 生产 hook 装配把 `session_id` 与 `transcript_path` 都写成空串：
  - `peri-middlewares/src/assembly/hooks.rs:40-45`：`cwd, "", ""`。
  - `AssemblyContext` 已有真实 `session_id`（`peri-agent/src/session/factory.rs:291` 起）。
- hook 输出只接受 snake_case `hook_specific_output`，且 `deny_unknown_fields`：
  - `peri-middlewares/src/hooks/types.rs:101-103`。
  - `peri-middlewares/src/hooks/output_parser.rs:19`。
  - 现有测试 `output_validation_test.rs::unknown_fields_cannot_silently_discard_a_decision` 把 Claude Code 标准 camelCase `hookSpecificOutput` 判为未知字段并返回 Block（已运行，1 passed）。
- Peri 没有向 hook 子进程导出会话 SQLite 路径：
  - `peri-tui/src/main.rs:495-520` 归一 `SessionStoreDeployment`；`main.rs:737-742` 只保留部署描述，没有设置 `PERI_DB_PATH`。
  - 默认本机库为 `~/.peri/threads/threads.db`（`peri-resources/src/sessions/mod.rs:62-64`）。
- Hindsight 侧依赖：
  - `retain` 读 `transcript_path`，空路径直接跳过；`SessionEnd` 也以 `transcript_path` 为门。
  - 外部 issue 约定 Peri 提供真实 `session_id`，Hindsight 改读 Peri SQLite。

## 根因链

1. producer：生产 hook 装配没有把 `AssemblyContext.session_id` 传进 `HookMiddleware`，hook stdin 的 `session_id` 恒为空。
2. parser：`SyncHookResponse` 只声明 snake_case 字段；标准 Claude Code/ Hindsight 输出 `hookSpecificOutput` 被 `deny_unknown_fields` 拒绝，`UserPromptSubmit` 命中回忆时返回 Block。
3. discovery：Hindsight 无法从环境获知 Peri SQLite 路径，只能猜默认路径；`--db-path`/非默认 HOME 场景会读错库。

## 修复方案

1. **session_id**
   - `peri-middlewares/src/assembly/hooks.rs`：从 `AssemblyContext` 解构 `session_id`，传给 `HookMiddleware::with_session_start`，替换第一个 `""`；`transcript_path` 仍为空。
   - 回归：`peri-middlewares/src/assembly_test.rs` 用 `ProductionChainAssembler` + 真实 command hook 捕获 stdin，断言 `session_id == ctx.session_id`。
2. **hookSpecificOutput**
   - `peri-middlewares/src/hooks/types.rs`：给 `hook_specific_output` 加 `#[serde(alias = "hookSpecificOutput")]`，同时保留 snake_case 兼容。
   - 回归：camelCase `UserPromptSubmit.additionalContext` → `HookAction::AdditionalContext`；camelCase `PreToolUse.permissionDecision=deny` → 拒绝；真正未知字段仍 Block。
3. **PERI_DB_PATH**
   - `peri-tui/src/main.rs` 在 `session_store` 归一后导出：
     - `SessionStoreLocator::Default` → `~/.peri/threads/threads.db`（绝对路径）；
     - `SessionStoreLocator::LocalPath(path)` → 规范化为绝对路径；
     - `SessionStoreLocator::Locator(_)`（远程/待解析）→ 不设置。
   - 使用 `unsafe { std::env::set_var }`（edition 2024）；设置点必须在启动 TUI/print/ACP 子进程之前，且不覆盖显式 `PERI_DB_PATH` 的语义需在注释中说明（本实现以解析出的本机库为准）。
   - 子进程（hooks 与插件 MCP）继承该环境变量。
   - 回归：`peri-tui/src/main_test.rs` 对纯函数 `local_session_db_path` 断言 default / local path / remote locator。
4. **文档**
   - `docs/code-index/peri-middlewares.md`：hook 输入构造处补 `session_id` 已填充、`hookSpecificOutput` 双写法兼容。
   - `docs/code-index/peri-tui.md`：session store 定位行补 `PERI_DB_PATH` 导出与本地/远程差异。
   - 如需新增环境变量条目，更新 `docs/standards/environment-variables.md` 并说明它由宿主内部设置、不是用户开关。

明确不做：

- 不生成 Claude Code JSONL transcript；Hindsight 改读 SQLite。
- 不修改 Hindsight 仓库（外部 issue 已提交，由对方完成）。
- 不处理 hook trust（独立分支 `fix/remove-hook-trust`）。
- 不改 hook 执行顺序、超时、其余输出字段语义。

## 验收标准

1. 生产 command hook 的 stdin JSON `session_id` 等于当前会话 ID。
2. `hookSpecificOutput` camelCase 的 `UserPromptSubmit`/`PreToolUse` 输出按标准语义解析；未知字段仍 Block。
3. 默认本机库或 `--db-path` 时，hook 环境含绝对路径 `PERI_DB_PATH`；远程 locator 不设置。
4. 目标检查：`cargo test -p peri-middlewares --lib -- assembly`、`-- hooks`、`cargo test -p peri-tui --bin peri -- tests::test_local_session_db_path`、`bash scripts/check-changed.sh`、`git diff --check`。

## 验证记录

- 2026-11-02，worktree `hindsight-hook-contract`（基线 `1655935ae`）：
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- assembly`：41 passed；含新增 `production_assembly_passes_session_id_to_command_hook_stdin`（生产装配 + 真实 command hook 捕获 stdin）。
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- output_validation`：10 passed（连续 5 次）；含 camelCase 正/负向用例。
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- hooks`：250 passed。
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --test hook_output_diagnostic_contract`：1 passed。原 `invalid_output_is_diagnosed_without_body` 移入独立测试二进制：同进程并行订阅者会让 tracing callsite interest 变成 never，该用例留在 `output_validation` 并行运行时会随机假红。
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --bin peri -- main_test::`：0 tests（`main_test.rs` 挂载为 `mod tests`，过滤词不匹配）；改跑 `-- tests::test_local_session_db_path`（4 passed）与 `-- tests::test_session_store_deployment`（2 passed）。
  - `bash scripts/check-changed.sh`：通过（相关 crate 反向依赖闭包 `cargo check`）。
  - `bash scripts/check-file-size.sh`：本次改动文件均未超 1000 行；报告 20 个既有超限文件（源码 9 / 测试 11），与本次改动无关。
  - `git diff --check`：通过。
  - 说明：未生成 JSONL transcript；未修改 Hindsight 仓库；未处理 hook trust。
