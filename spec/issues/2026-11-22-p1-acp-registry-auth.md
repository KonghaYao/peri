# P1：ACP Registry 认证握手与首次配置入口缺失

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：功能缺口 / 协议兼容。
**创建日期**：2026-11-22。
**来源**：用户报告 [Issue #531](https://github.com/KonghaYao/peri/issues/531)。
**确认方式**：主 agent 以基线 `46f477c56` 的真实 ACP 握手复现并核对代码路径；explorer 只读复核。

## 确认证据

- 已配置环境（临时 HOME + 合成 provider，不发起模型请求）运行 `peri acp`，注入 Registry 验证器形状的 `initialize`（`clientCapabilities.terminal=true` 且 `_meta["terminal-auth"]=true`），响应为 `"authMethods":[]`。
- 干净临时 HOME 运行 `peri acp`，退出码 1，stdout 为空，stderr 为
  `Error: No LLM provider configured. Configure ~/.peri/settings.json ...`。
  Provider 解析位于 `peri-acp/src/host/stdio/mod.rs:170-176`，先于 stdio transport 与 `initialize` 处理。
- `peri-acp/src/dispatch/init.rs:30-55` 的 `build_initialize_response` 只设置 agent capabilities；`handle_initialize`（`peri-acp/src/host/requests/session_lifecycle/session_lifecycle_dispatch.rs:133-149`）没有读取 `clientCapabilities.auth.terminal` 或 `_meta["terminal-auth"]`，也没有写入 `authMethods`。
- 无 provider 时宿主不启动，`session/new` 无法返回 `AUTH_REQUIRED`；正常宿主中的会话创建依赖 `AcpServerConfig` 与 session manager。
- 现有 CLI 只有 `peri auth chatgpt` 凭据登录（`peri-tui/src/cli_auth.rs:7-25`），没有可独立退出的首次配置入口。TUI 首次向导位于 `peri-tui/src/kit/entry.rs:216-228`，由 `needs_setup`（`peri-tui/src/launch.rs:128-140`）触发，只能在交互式 TUI 内进入。

## 根因链

观察：

1. stdio 启动把 provider 配置视为整个宿主的硬前置；无配置时进程在 JSON-RPC 握手前退出。
2. `initialize` 响应构建器没有认证方法输入，客户端能力中的终端认证标记也未被解析，因此即使配置可用也返回空 `authMethods`。
3. 首次配置向导只挂在交互式 TUI 的启动检查上，ACP 静态入口无法复用，也没有独立退出码。

推断：ACP Registry 的认证校验只读取 `initialize.authMethods`；上述三点叠加导致 Registry 校验失败，且首次安装用户无法从客户端完成登录与模型配置。

## 影响面

- 用户影响：首次安装的用户在完成登录/模型配置前无法建立 ACP 会话（宿主在握手前退出）；已配置用户会话可用，但 `initialize.authMethods` 为空，按 Registry 只读取 `initialize.authMethods` 的校验推断同样无法通过认证检查。两者是不同阶段的失败，不能互相替代。
- 受影响模块/调用点：
  - `peri-acp/src/dispatch/init.rs`（initialize 响应）
  - `peri-acp/src/host/requests/session_lifecycle/session_lifecycle_dispatch.rs`（initialize 处理）
  - `peri-acp/src/host/stdio/mod.rs`（stdio 启动装配）
  - `peri-tui/src/startup.rs`、`peri-tui/src/cli_tui.rs`、`peri-tui/src/launch.rs`、`peri-tui/src/kit/entry.rs`（启动与首次配置生命周期）
- 是否可规避：预先手写 `~/.peri/settings.json` 或设置 `MODEL_PROVIDER`/`MODEL_TYPE`；可规避，但违背首次启动目标。

## 修复方案

- 范围：
  1. `initialize` 在客户端声明 ACP v1 `clientCapabilities.auth.terminal=true` 或 Registry 旧标记 `clientCapabilities._meta["terminal-auth"]=true` 时返回一个 `type:"terminal"` 认证方法；方法描述符使用 `args:["--setup"]` 与 `env:{"PERI_ACP_SETUP":"1"}`，以同时适配客户端追加参数、替换参数和仅应用环境变量的调用方式。
  2. 无 provider（常规配置文件路径）时 stdio 宿主进入显式降级应答：`initialize` 正常返回最小 agent capabilities 与认证方法；`session/new` 等会话方法返回 JSON-RPC `-32000`（`AUTH_REQUIRED`）并携带 `peri --setup` 提示。`--settings-stdin` 注入路径缺少 provider 时仍失败退出，不降级。
  3. 新增全局 `--setup` 与 `PERI_ACP_SETUP` 开关；`peri --setup`、`peri acp --setup`、`PERI_ACP_SETUP=1 peri acp` 均复用现有 TUI 首次向导（provider、model、profile、凭据与 ChatGPT 登录），成功保存后退出 0；取消或失败退出非零。
- 明确不做：
  - 不实现 `authenticate`/agent auth；只提供 terminal auth。
  - 不改变 provider 解析、模型调用或已配置用户的会话创建流程。
  - 不自动写 ACP Registry 的 `agent.json`；仓库外登记另行处理。
  - 不新增第二套非 TUI 配置向导，不复制 setup wizard 的 provider/model/profile 逻辑。
  - 不改变 `--settings-stdin` 的失败语义。
- 回归测试：
  - `peri-acp`：initialize 认证方法的两种能力标记与缺失标记；降级宿主的 initialize/session-new 报文；正常宿主 initialize 形状。
  - `peri-tui`：CLI 解析（`--setup`、`peri acp --setup`、环境开关）；setup-only 退出决策（保存成功 0、取消非零）；`needs_setup` 已配置时不触发 setup。
  - 进程级：干净 HOME 真实 `peri acp` 握手中 `authMethods` 与 `session/new` 错误；已配置 HOME 的 `session/new` 不进入 setup。

## 验收标准

1. 干净环境中，标准 ACP v1 终端认证能力与 Registry 旧标记分别能在 `initialize` 握手中拿到有效的 `type:"terminal"` 方法；未声明能力时 `authMethods` 为空。
2. 干净环境中 `initialize` 成功；`session/new` 返回 `-32000` 与可操作提示。完成配置后重连，`session/new` 成功。
3. `peri --setup`、`peri acp --setup`、`PERI_ACP_SETUP=1 peri acp` 均进入同一首次配置流程；成功保存退出 0，取消或失败退出非零。
4. 已配置用户直接创建会话，不触发登录或配置流程。
5. 定向测试、类型检查与真实握手验证通过。

## 验证记录

基线 `46f477c56`，分支 `fix/acp-registry-auth`。以下由主 agent 在实现后独立复跑
（实现 workflow 在 50 分钟上限被终止，未采信其自检报告）。

- 真实 ACP 握手（隔离 HOME，当前 `target/debug/peri acp`）：
  - clean + `clientCapabilities.auth.terminal=true`：`initialize` 返回 1 个
    `type:"terminal"`、id `peri-setup`、`args:["--setup"]`、
    `env.PERI_ACP_SETUP="1"`；`session/new` 返回 `-32000`，message 以
    `AUTH_REQUIRED: ` 开头，`data.reason="auth_required"`、
    `data.setupCommand="peri --setup"`；未知方法 `-32601`；EOF 后退出 0。
  - clean + `clientCapabilities._meta["terminal-auth"]=true` 与顶层
    `_meta["terminal-auth"]`：同上。
  - clean + 无标记：`authMethods=[]`；`session/new` 仍返回 `-32000`。
    `clientCapabilities.terminal=true`（terminal/* 能力）单独不触发认证广告。
  - 已配置 + terminal 标记：`authMethods` 1 个；`session/new` 成功返回
    `sessionId`。已配置 + 无标记：`authMethods=[]` 且 `session/new` 成功。
- 真实 PTY（隔离 HOME）：
  - `peri --setup`（clean）Esc 两次：退 1，stderr
    `Error: Setup cancelled or incomplete`。
  - `peri --setup`（已配置，向导预填）Down/Enter/Enter 保存：退 0，
    `~/.peri/settings.json` 实际更新（向导补全 models/profile）。
  - `peri acp --setup`（客户端追加参数）与 `PERI_ACP_SETUP=1 peri acp`
    （客户端替换参数）：均进入向导而非 ACP JSON-RPC；取消退 1。
  - `peri acp --settings-stdin --setup`：clap 参数冲突退 2；仅环境来源冲突退 1。
- 自动化测试：
  - `peri-acp --lib init_auth_tests` 7 passed；`auth_required_gate_tests` 8；
    `run_server_integration` 15；`settings_bootstrap` 3。
  - `peri-tui --lib setup_tests` 4；`--test acp_registry_auth` 4。
  - `peri-acp --lib`（全量）1531 passed / 0 failed / 2 ignored。
  - `peri-tui --lib -- --test-threads=1`（全量串行）2804 passed / 0 failed /
    7 ignored。
- 类型与文档门：`bash scripts/check-changed.sh`（含反向依赖 `peri-swarm`、
  `peri-wasm`，1m11s）、`cargo fmt --check`、`git diff --check`、
  `python3 scripts/check-feature-catalog.py`：通过。
- 已知限制：
  - 默认并行 libtest 全量在本机两次各命中 `interaction_test` 的全局
    `VIEW_MODELS` 空快照越界（两次为不同用例，均在 `interaction_test.rs:39`）；
    该模块隔离 5/5、模块 10/10、`--test-threads=1` 全量通过，且未被本次改动
    触及；CI 使用 nextest 进程隔离（`.config/nextest.toml` 已记录该机制），
    按既有并发脆弱用例记录，不在本次范围内修改。
  - 非 TTY 的 `--setup` 在 ratatui fullscreen 初始化处 panic（exit 101），
    属既有全屏 TUI 的 TTY 前提；终端认证客户端提供 TTY，验收路径已由 PTY 覆盖。
  - 真实 ACP Registry `agent.json` 端到端登记在仓库外，未复验。

### 第 1 轮 review 修复（2026-11-22）

CodeRabbit 对 head `9fddfedaa` 提交 3 条意见，逐条核对后全部成立：

1. `peri-acp/src/host/stdio/mod.rs`（Major）：transport 先于装配构造；
   `--settings-stdin` 缺 provider 时 stdin pump 已启动，runtime 关闭可能等待
   阻塞式读取。修复：先 `assemble_stdio_config`，成功后再构造 transport。
   复验：同场景保持 stdin 打开 0.02s 退出 1，stderr
   `Error: No LLM provider configured in injected settings`。
2. `peri-tui/src/kit/entry.rs`（Major）：setup pass 置 `from_command=false`，
   使 `merge_setup` 对已配置用户无条件重置 `active_alias` 并覆盖自定义档位。
   修复：新增 `offline_completion` 标志，与 `finish_setup` 的在线激活/离线
   完成解耦；`from_command` 保持 true，只有配置不可解析时才修复档位。
   复验：已配置 HOME 经真实 PTY 完成向导后退出 0，`active_alias` 仍为
   `sonnet`，`profiles.sonnet.effort=high`、`profiles.opus.effort=max` 保留。
3. `spec/issues/2026-11-22-p1-acp-registry-auth.md`（Minor）：用户影响把
   "已配置用户不受影响"与空 `authMethods` 的 Registry 校验影响混为一谈。
   修复：区分首次配置失败与已配置环境的认证校验失败。

回归测试：新增 `setup_pass_draft_keeps_profile_merge_and_marks_offline_completion`
（entry）与 `test_wizard_preserves_resolvable_selection_when_reopened`（wizard）。
复跑：entry 5、wizard 33、`acp_registry_auth` 4、`run_server_integration` 15、
`settings_bootstrap` 3，全部通过；真实 ACP 握手 6/6 PASS。

- PR/CI/CodeRabbit：随本 PR 提交，以 PR checks 与 review 为准。
