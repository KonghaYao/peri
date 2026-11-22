# Workflow 脚本工具调用（`tool()`）验收记录

- 状态：验收记录（PR #539 已合并 `1b7d3557b`；2026-11-22 修订：E2E 夹具重锚、
  CodeRabbit 评审修复、后续 PR 收尾示例精度与 e2e pin）
- 日期：2026-11-22
- 工作树：`/Users/mino/code/peri`，分支 `feat/workflow-codemode-tool`（PR #539）；
  任务开始前既存的 `.mcp.json` 改动不属于本任务，保持未提交、未纳入验证结论。
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
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib -- --test-threads=1` | 286 passed / 0 failed |
| `PERI_WORKFLOW_ARTIFACT_DIR="$PWD/dist/workflow-sidecar/peri-resources/workflow/b7fcaf3e1cbc" ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::workflow_consumer_tests` | 12 passed / 0 failed（tool E2E 5 个；新增回填重投 1 个） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- prompt_tool_name_lock` | 1 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-workspace --lib -- resources::` | 通过（builtin skill 索引与 include_str! 装配） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-workflow` | 34 passed / 0 failed（含断开置未知副作用用例） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- workflow` | 21 passed |
| `bun test`（`npm-packages/@peri-workflow`） | 138 pass / 0 fail（新增键序与 Date 回放匹配用例） |
| `bun run typecheck`（同上） | exit 0 |
| `./scripts/cargo-rmcp-patched.sh clippy --locked --workspace --all-targets -- -D warnings` | 通过（CI 同口径；此前漏掉 `--all-targets` 暴露的测试目标 lint 已修） |
| `cargo fmt --all -- --check` | 无差异 |
| `bash scripts/check-layer-imports.sh` | 22 规则 / 0 违规 |
| `PERI_SOURCE_ROOT=… node scripts/verify-replay-fixtures.mjs --drift-policy=source-anchored`（peri-e2e PR #23） | passed：62 cassettes / 305 entries / 0 worldDrift |
| `PERI_SOURCE_ROOT=… node scripts/refresh-cassettes.mjs --check`（peri-e2e PR #23） | `stale: false`，无 mapped/unanchored drift |
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
5. **PR CI 首轮 E2E（62 场景）全失败**：根因是新增 `codemode` 使系统提示词
   +71 字节（catalog 行、resource URI、resource 计数），而 peri-e2e 的 world 是
   录制快照而非源码映射。修复在 peri-e2e PR #23（重锚 317 个 leaf / 305 个
   stub，响应字节不变），该 PR 已 squash 合并为 `ad069780a`，本仓
   `.github/peri-e2e.rev` 指向该提交；产品代码无缺陷。
6. **PR CI 首轮 Clippy 失败**（ubuntu/macos）：本地此前只跑
   `clippy --workspace`，漏掉 `--all-targets`；失败点是测试目标
   `clippy::await_holding_lock`（`drop(guard)` 后仍有 await 判定）。已改为块作用域。
7. **CodeRabbit 评审 8 条处置**：
   - 断开时放弃执行中的 tool 任务未置未知副作用：成立，`WorkflowConsumerReady
     {ready:false}` 路径先 `mark_unknown_tool_side_effect` 再结算，补 wire 用例；
   - 回填耗尽重试后任务永久 executing、脚本 `await tool(...)` 悬挂：成立，改为
     保留已计算 outcome、有界退避持续重投，服务器接受前不重新执行工具；
   - `settle_failed_terminal_state` 读 journal 失败时丢失未知副作用：成立，回落到
     内存投影，补单测；
   - journal / tools.jsonl 的 `BufWriter` 未显式 `flush`，写入失败被 drop 吞掉：
     成立，两个 append 均显式 flush；
   - 终止判定先读未知副作用位再读执行中计数存在竞态：成立，改为先读计数；
   - `jsonDeepEqual` 让 `Date` 等非纯 JSON 对象匹配 `{}`：成立，非纯对象 fail closed，
     补键序与 Date 用例；
   - `codemode` SKILL 示例把 `Glob` 文本当数组：成立，示例与说明改为按行解析；
   - `tools.jsonl` 追加失败仍投递结果、resume 会重放未记录前缀：不改变，属已文档化
     的 journal 边界（`tool_journal_append_failure_does_not_change_the_terminal`
     锁定），改变它需要另行裁决磁盘失败时的 run 语义。
8. **CodeRabbit 追加轮 7 条（`codemode` SKILL 示例）**：均成立；前 5 条随 #539
   合并，后 2 条因评审轮次上限在同日后续 PR 修复。
   - `Glob` 无匹配返回 `No files found.`、超限时附 `[Output truncated: ...]` 说明，
     示例会把说明当路径传给 `Read`：示例先判空返回，命中截断说明时抛错要求收窄
     pattern。
   - `parallel()` 结果未绑定、`agent()` 提示词为固定串，每文件行数未送达：
     示例绑定为 `counts` 并写入提示词；同时说明失败 thunk 解析为 `null` 需过滤。
   - `Read` 超限时同样返回 `[Output truncated: ...]`，按部分文本计行会低估：
     命中截断说明的条目把 `lines` 记为 `null`（未知），不参与按行数排名。
   - 过滤失败 thunk 会丢失未读文件名：按索引从 `targets` 还原为 `unread`，
     与 `counts` 一同写入提示词并返回。
   - 失败 thunk 解析为 `null` 会丢掉 `WorkflowToolError.code`：thunk 内捕获已知
     code（`policy_denied` / `tool_unavailable` 等）随文件名返回，未知异常仍抛出；
     按索引兜底为 `unexpected_error`。
   - 截断判定用 `includes` 会误匹配正文里的同名字面量：`Read` 内容行带
     `{:>6}\t` 行号前缀，状态行不带；改为只匹配行首，`Glob` 同步收紧。
   - 空文件 `Read` 返回 4 行 `[EMPTY FILE]` 状态文本，示例会数成 4 行：计行前
     判首行 `[EMPTY FILE]` 并返回 0 行；截断条目改记 `truncated: true`。

## 4. 未覆盖与限制

- 真实审批拒绝/超时：脚本侧 `policy_denied` / `approval_unavailable` 的 wire 与
  catch 路径有单元测试，但未走真实 `interaction_router` 零/多端点与超时。
- 真实外部 deferred MCP 工具：只验证了 effective 名通道，未接第三方 MCP server。
- TUI：`unknownSideEffects` 已投影到运行标签（`⚠`），但按测试规范没有渲染断言，
  仅眼测入口与投影单测。
- 固定测试仓 SHA 对账：归 peri-e2e #10，本记录不声称完成。
- `read` CLI 的文本摘要未增加该字段；现有完成通知与 `state.json` 已包含。
- `.github/peri-e2e.rev` 现指向 peri-e2e main 的 `ad069780a`（PR #23 squash 合并）；
  #539 合并时该 pin 曾短暂指向未合并分支 head，后续 PR 已对齐到 main。
- `.mcp.json` 保持任务开始前原样，未提交。
