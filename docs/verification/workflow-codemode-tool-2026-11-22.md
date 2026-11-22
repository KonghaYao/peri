# Workflow 脚本工具调用（`tool()`）验收记录

- 状态：验收记录（本地实现自检；工作树未提交）
- 日期：2026-11-22
- 工作树：`/Users/mino/code/peri`，分支 `feat/peri-swarm-production` @
  `26d48ec98` + 本任务未提交改动；任务开始前既存的 `.mcp.json` 改动不属于本任务，
  未纳入验证结论。
- 范围：issue #407 阶段 1+2——`globalThis.tool` 原语、`tool/task` 通道、宿主消费者、
  `tools.jsonl` resume 回放、未知副作用投影、文档与 `codemode` skill。
  审批拒绝、真实外部 deferred MCP 工具与固定测试仓 SHA 对账不在本次证据内。
- 产物：`npm-packages/@peri-workflow` 构建的 sidecar
  `dist/workflow-sidecar/peri-resources/workflow/b7fcaf3e1cbc/`；入口
  `dist/peri-workflow.js` sha256
  `b7fcaf3e1cbccd2629254a071b5d41a808464ba20548346851f538762e0bdd89`，与
  `peri-workflow/src/runner/artifact.rs` 的 `WORKFLOW_ENTRY_SHA256` 一致。
- 复现环境：macOS 26.6.2；仓库规定的 `./scripts/cargo-rmcp-patched.sh`；
  `@peri-code/workflow` 0.3.0（协议 3）；Node.js 与 Bun。

## 1. 结论

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| 1. 脚本只调 `tool()` 即可完成编排并返回结构化结果 | 通过 | `tool_e2e::tool_only_workflow_completes_through_host_consumer`：`agent_count=0`，脚本收到 `structuredContent`，`tools.jsonl` 记 `toolSeq=0` / `agentSeq=0` |
| 2. workspace 与 deferred 外部 MCP 工具按 effective 名调用 | 部分覆盖 | effective 名 `Read` / `Bash` 经录制 invoker 走通宿主通道；真实外部 deferred MCP 工具未纳入 E2E |
| 3. 审批：批准、零/多端点、拒绝/超时 fail closed | 未覆盖 | 脚本侧类型化错误可 `try/catch` 已由 `tool_error_is_catchable_in_script` 验证；真实 `interaction_router` 的审批拒绝/超时路径未运行 |
| 4. 结果 >1 MiB 不截断 | 通过 | `large_tool_result_is_not_truncated`：1.5 MB content，脚本 `len=1_500_000`，`tools.jsonl` ≥ 1.5 MB |
| 5. resume 不重复执行已记录副作用 | 通过 | `resume_replays_recorded_tool_without_reinvoking_host`：二次 run 的宿主 invoker 调用数仍为 1，回放条目落新 `tools.jsonl`（`toolSeq=0` / `Read`）；前缀选择与 Node 深比较由单测覆盖 |
| 6. kill 中断执行中的工具后标记未知副作用 | 通过 | `kill_during_tool_marks_unknown_side_effects`：`Killed` 且 `state.json` 的 `unknownSideEffects=true`；`unknown_side_effects_is_projected_only_when_set` 覆盖进度投影（`false` 不落 JSON） |
| 7. 脚本工具不污染 `maxToolCalls`、不受 `maxConcurrency` 拒绝 | 通过（单测） | `runtime/tasks_test.rs`：`maxConcurrency=1` 时 tool 仍可 claim，agent 受限；tool 任务无 agent 槽位；`note_task_execution` 只接收 `AgentRunResult`，脚本工具不进入 tool-call 计数 |
| 8. 文档声明非安全边界、`codemode` skill 可加载 | 通过 | `prompt_tool_name_lock_tests::managed_prompt_files_use_direct_builtin_names`；workspace builtin skill 索引测试；`docs/design/workflow.md` 明确边界 |

## 2. 运行命令与结果

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh check --locked -p peri-workflow -p peri-tui -p peri-acp -p peri-middlewares -p peri-mcp-workflow -p peri-acp-types` | 通过 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib -- --test-threads=1` | 285 passed / 0 failed |
| `PERI_WORKFLOW_ARTIFACT_DIR="$PWD/dist/workflow-sidecar/peri-resources/workflow/b7fcaf3e1cbc" ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::workflow_consumer_tests` | 11 passed / 0 failed（tool E2E 5 个全部通过） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- prompt_tool_name_lock` | 1 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-workspace --lib -- builtin` | 14 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- workflow` | 21 passed |
| `bun test`（`npm-packages/@peri-workflow`） | 136 pass / 0 fail / 462 expects / 10 files |
| `bun run typecheck`（同上） | exit 0 |
| `cargo fmt --check -p peri-workflow -p peri-tui` | 无差异 |
| `python3 scripts/check-feature-catalog.py` | 通过 |
| `git diff --check` | 无输出 |
| `shasum -a 256` sidecar 入口与 `WORKFLOW_ENTRY_SHA256` | 一致 |

真实引擎用例要求 `PERI_WORKFLOW_ARTIFACT_DIR` 指向包含 `package.json` 与 `dist/` 的
包目录。**必须使用绝对路径**：cargo test 进程 cwd 是 crate 目录，相对路径会在
`peri-acp/` 下解析而找不到 sidecar。

## 3. 失败与复跑记录

1. **一次性全量 `peri-acp --lib`（并行）**：1500 passed / 8 failed / 2 ignored。
   8 个失败逐组串行复跑全部通过：
   - `host::requests::tests::workspace_cases::worktree`：6/6（含此前失败的 3 个）；
   - `host::stdio::run_server_integration_tests`：14/14（含此前失败的 3 个与 rename）；
   - `host::mcp_v4_wire_fixture::wire_fixture_script_logs_wire_and_answers_tools_call`：1/1。
   失败信息为 `Git discovery timed out` / stdio 通道关闭，判定为并行满载下的环境
   超时，未观察到与本改动的因果关系。
2. **`journal::tests::strict_resume_read_rejects_missing_and_malformed_journal`**：
   `init_run` 为支持零 agent run 的 resume 而创建空 `journal.jsonl` 后，旧断言
   「初始化过的 run 必须读取失败」失效。已更新用例：空 journal 是可读的空前缀；
   目录存在但文件缺失仍报错。`journal::tests` 50/50 通过。
3. **`peri-workflow --lib` 并行全量偶发**
   `cli::tests::workflow_cli_spawns_prepared_artifact_for_help` 返回 1：
   `--exact` 单独运行与 `--test-threads=1` 全量均通过。该用例使用独立 node stub，
   与 serial 修改 HOME 的用例存在并行竞争，属既存测试隔离问题，本次未修改。
4. **E2E 首次重跑失败**：因 `PERI_WORKFLOW_ARTIFACT_DIR` 使用相对路径导致 sidecar
   解析失败；改为绝对路径后 11/11 通过。

## 4. 未覆盖与限制

- 真实审批拒绝/超时：脚本侧 `policy_denied` / `approval_unavailable` 的 wire 与
  catch 路径有单元测试，但未走真实 `interaction_router` 零/多端点与超时。
- 真实外部 deferred MCP 工具：只验证了 effective 名通道，未接第三方 MCP server。
- TUI：`unknownSideEffects` 已投影到运行标签（`⚠`），但按测试规范没有渲染断言，
  仅眼测入口与投影单测。
- 固定测试仓 SHA 对账：归 peri-e2e #10，本记录不声称完成。
- `read` CLI 的文本摘要未增加该字段；现有完成通知与 `state.json` 已包含。
- 工作树未提交；`.mcp.json` 保持任务开始前原样。
