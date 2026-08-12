# Workflow 工具运行任何脚本都 0 agents 失败（15 秒静默完成）

# Workflow 启动握手失败时错误被静默为零 Agent 结果

**状态**：Partial
**优先级**：高
**创建日期**：2026-08-01
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

修复 #1（2026-08-02）已落地：`workflow_cmd()` 优先本地固定安装（`~/.peri/workflow/0.1.1`，完全离线秒启），registry 不可达时 e2e 0.03s 完成（对比 npx 挂起 70-120s）。残余问题：修复范围 1-7（握手失败时 stderr 传播、通知真实 error、成功路径文案）未完整落地——workflow error/stderr 未完整传播到通知，成功路径文案仍可能误报。状态记录（2026-08-11）：Open → Partial。

## 问题描述

Peri 调用 Workflow 工具时，如果 Node workflow engine 未在 15 秒内完成 `workflow/start` 握手，后台任务最终只报告 `0 agents, 0 tool calls`，没有向调用者展示实际 timeout 或 Node stderr。run 目录只包含 `script.js`，但通知仍声称结果已保存到不存在的 `state.json`。

该问题最初在 `ccb --acp` 宿主环境连续出现三次，曾被误判为 CCB/ACP bridge 缺少 Claude Code live session。后续源码检查和 ACP 实测已证伪该判断：Peri 的 Workflow agent 不依赖 `claudeCodeBackend.ts`、`toolUseContext` 或宿主 live session，同类最小脚本可在 ACP 会话正常完成。

## 症状详情

- 历史环境：宿主进程为 `node ~/.bun/bin/ccb --acp`，macOS；`bun 1.3.14`、`node`、`npx` 均可用。
- 操作：通过 `ExecuteExtraTool("Workflow", ...)` 运行 Workflow 脚本。
- 历史实际结果：约 15 秒后收到零统计通知，`0 agents, 0 tool calls`；run 目录仅有 `script.js`，没有 `journal.jsonl` 与 `state.json`。
- 期望结果：
  - engine 正常启动时按脚本派发 agent，并生成运行产物；
  - engine 启动或握手失败时明确报告失败原因和可用的 Node stderr，不声称不存在的结果文件可供读取。
- 历史失败 run：`019fbd8e-...`、`019fbd8f-...`、`019fbd90-...`。
- 历史成功 run（示例）：`019f84cc-...`、`019fa947-...`。

## 已确认事实

### 1. 故障发生在 Agent 派发之前

`peri-workflow/src/runner.rs` 会先通过 `WorkflowJournalStore::init_run` 写入 `script.js`，随后启动 `npx -y @peri-code/workflow` 并发送 `workflow/start`。只有握手成功后才启动 message loop，处理 `agent/run` 并写入后续状态。

历史失败目录只有 `script.js`，说明故障位于 Node engine 启动或 `workflow/start` 握手阶段，不在 Workflow agent、provider 或 `run_react_loop` 阶段。

### 2. 存在固定 15 秒启动握手超时

`peri-workflow/src/runner.rs` 对 `channel.send_request("workflow/start", ...)` 使用固定 15 秒 timeout。超时后内部错误为：

```text
workflow/start timed out (15s) — node process may have crashed
```

### 3. 快速失败窗口无法捕获该超时

`peri-workflow/src/tool.rs` 只等待 1 秒检测快速失败，因此工具会先返回 `Workflow started`。第 15 秒产生的 timeout 只能通过异步完成通知反馈。

### 4. 异步失败通知丢失错误信息

`WorkflowTaskResult` 保存了 `error`，但 `peri-workflow/src/registry.rs` 的 `to_notification()` 没有渲染该字段。失败通知因此只显示耗时和零统计，隐藏了真实 timeout。

同一通知还无条件输出：

```text
Results saved to .claude/workflow-runs/<run_id>/state.json
```

但握手前失败不会生成 `state.json`。

### 5. 现有测试没有验证错误文本

`peri-workflow/src/registry_test.rs` 中的 `test_notification_includes_error_when_failed` 虽然构造了 `error`，但只断言 workflow 名、`failed` 状态和 reminder 边界，没有断言真实错误文本出现在通知中。

## 已证伪假设

- **不是 CCB/ACP bridge 架构不兼容。**
- **不依赖 Claude Code live session。** Peri 的派发路径为 `WorkflowTool → WorkflowRunner → workflow/start → agent/run → WorkflowAgentExecutor → run_react_loop`。
- **仓库内不存在相关 `claudeCodeBackend.ts` 实现。** 该概念来自其他项目，不适用于 Peri。
- **不是 ACP 与 TUI 的 Workflow context 装配差异。** 两条会话路径均创建 session-scoped `WorkflowMiddleware` 和 `WorkflowAgentContext`。

## 验证记录

2026-08-01 在 ACP 会话执行同类最小脚本：

```javascript
export const meta = {
  name: 'peri-acp-workflow-smoke',
  description: 'ACP bridge smoke test'
}
const result = await agent('只回复 OK')
return { result }
```

结果：

- run_id：`019fbda7-5949-7b52-b294-644dc595ff31`
- 状态：`completed`
- 耗时：8046ms
- agents：1
- 返回值：`OK`
- `state.json` 与 `journal.jsonl` 均已生成

独立 runner 测试也通过：

```text
cargo test -p peri-workflow -- --ignored --nocapture test_e2e_simple_workflow
1 passed; finished in 2.35s
```

这证明 ACP 模式能够正常运行 Workflow。历史三次失败的直接触发因素只能确定为 Node engine 未在 15 秒内回复 `workflow/start`；由于 timeout 路径没有保留 stderr，现有证据不足以继续区分 `npx` 冷启动、网络、包解析或 Node 子进程异常。

### 2026-08-01 补充：启动超时的实证（npx 联网依赖）

对“dev 模式 cargo 构建慢导致超时”的假设做了实测，结论：**机制不成立，但启动阶段环境耗时的大方向正确**。

- `@peri-code/workflow` 是纯 Node npm 包（源码在本仓库 `npm-packages/@peri-workflow/`，发布版本 0.1.1，`dependencies: {}`，bun build 自包含）。`workflow/start` 握手完全发生在 Node 进程内；cargo 构建只发生在 peri 进程启动前，workflow 调用时 peri 已运行，不存在构建。
- `npx -y @peri-code/workflow` 每次调用都联网 resolve registry：
  - 正常网络下 registry 元数据请求约 **1.3 秒**（curl 实测）；
  - 冷启动 **6.5 秒**、热启动 1–2 秒（实测 6.53s / 1.81s / 0.97s）；
  - registry 不可达时（指向 127.0.0.1:9）`npx -y` **挂起 120 秒不退出**（npx 不快速失败，而是重试网络）——15 秒硬超时必然被打穿。
- `git log` 显示 07-21 提交 `151bafc0` 将 `workflow_cmd` 从 bunx 统一改为 **npx**；本机 bun 缓存已有 `@peri-code/workflow@0.1.1`（bunx 时代的离线缓存），而 npx 缓存没有。这解释了时间线：07-21 前 bunx 走本地缓存秒启；07-21 后每次 npx 都依赖网络，网络慢时必现 15s+ 超时；08-01 三次失败即发生在此窗口。

已证伪假设追加：

- **不是 dev 模式 cargo 构建耗时**：engine 启动不需要 cargo；cargo 构建与已运行进程内 spawn npx 无时间关系。


### 2026-08-02 修复 #1：本地固定安装优先（离线缓存）

实现 `peri-workflow/src/runner.rs`：

- `workflow_cmd()` 优先返回 `node <~/.peri/workflow/0.1.1/node_modules/@peri-code/workflow/dist/peri-workflow.js>`，完全离线、秒启，不依赖 npm registry；未安装时回退 `npx -y`（联网兜底，行为同旧版）。
- 新增 `ensure_workflow_install()`：首次调用时自动 `npm install --prefix ~/.peri/workflow/0.1.1 @peri-code/workflow@0.1.1`（90s 超时、Mutex 串行、失败仅告警），之后永久离线。
- 单测 2 个（`workflow_local_dist_in` 存在/缺失）。

验证记录（2026-08-02）：

- `cargo test -p peri-workflow --lib`：39 passed（含新单测）；`cargo clippy -p peri-workflow --all-targets -- -D warnings` 通过。
- 预热安装后，`npm_config_registry=http://127.0.0.1:9`（registry 完全不可达）下 e2e 测试 **0.03s 完成**（对比 npx 联网挂起 70–120s）。
- ACP 会话真实调用 `peri-acp-workflow-offline-smoke`：`completed`，1 agents，6682ms（run_id `019fbdb7-25ea-7991-9877-cdccd55dcc26`）。

说明：`bunx`（0.04s 快速失败但每次解析 manifest，不走缓存）与 `npx --prefer-offline`（无缓存时仍联网挂 70s）均实测不可离线，故采用固定目录安装 + `node` 直跑方案。

## 修复范围

1. `workflow/start` RPC 出错或超时时，将已收集的 Node stderr tail 写入失败结果。
2. `WorkflowTaskResult::to_notification()` 在失败时展示真实 `error`。
3. 仅在 `state.json` 确实生成时显示结果文件提示；握手前失败应明确说明状态文件未生成。
4. 修正 `test_notification_includes_error_when_failed`，断言通知包含真实错误文本。
5. 增加启动握手失败的回归测试，覆盖 timeout、stderr 和异步通知传播。
6. 评估将 15 秒启动 timeout 配置化或适当延长，但不能以延长 timeout 代替错误传播修复。
7. `workflow_cmd()` 优先使用本地缓存（`bunx` 或 `npx --prefer-offline`），避免每次调用都联网 resolve registry；同时保留 npx 作为无缓存时的回退。

## 验收标准

- `workflow/start` timeout 后，调用者能看到明确 timeout 原因。
- Node stderr 可用时，失败结果包含有界的 stderr tail。
- 失败通知不再引导读取不存在的 `state.json`。
- 失败通知测试能在删除错误文本渲染时可靠失败。
- ACP 模式下的最小 Workflow 冒烟测试继续成功派发至少一个 agent。
