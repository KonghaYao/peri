# Peri LLM 录制与重放

本目录实现本地 HTTP 录放代理。代理使用 Node 标准库；真实终端用 Python PTY 与 `@xterm/headless`。不依赖在线 LLM judge，也不修改业务数据库 schema。

## 快速运行

先在仓库根目录构建真实 Peri：

```bash
bash scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri
git submodule update --init e2e/tui-tester
cd e2e
npm ci
npm run e2e:replay:unit
npm run e2e:replay:check
npx tsc --noEmit --project tsconfig.replay.json
npm run e2e:replay
```

需要 Node、Python 3 与可用的 PTY。`PERI_E2E_BINARY` 可以指定已经构建的 Peri 二进制。默认 `e2e:replay` 只读取已封存的 v2 fixture，不启动模型上游，不补录缺失请求，不回退在线模型。显式更新 fixture 使用 `npm run e2e:replay:record`；CI 拒绝 record 模式。`npm run e2e:replay:smoke` 保留旧版同进程录制后重放的实验用例。

基础目录包含 30 个固定场景：S01–S06 流式与协议，T01–T06 工具与环境，A01–A06 子 Agent，U01–U09 会话与交互，I01–I03 并发隔离。v3 另外补充 64 个异步任务、monitor、长上下文与存储代际场景。完整目录见 [fixture 说明](../fixtures/replay-v2/README.md)。这些场景通过本地合成模型响应驱动真实模型适配器、ACP、Agent、工具与 TUI；录制来源标记为 `local-fixture-model`、`realLlm: false`，不代表线上模型质量或可用性验证。实际通过情况、耗时和证据以对应 invocation 的报告为准。

每个场景的 fixture 包含初始化配置与工作文件 seed、原始 HTTP cassette、动态值声明和 portable 响应计划。seed 在创建会话前保存，不迁移或修改业务数据库。record 和 replay 分别创建新目录、新 HOME 与真实 ACP 会话，通过显式绑定复用录制。record/replay 成功后删除临时 HOME/work，避免重放意外依赖录制环境。配置指纹、seed 哈希或请求发生变化时失败，必须显式审核并重录。

## 入口与职责

| 入口 | 用途 |
| --- | --- |
| `proxy.mjs` | `startReplayProxy`；record/replay HTTP 服务及 runner API |
| `run-state.mjs` | 每个 run 独立的身份映射、消费状态、门控及完成核验 |
| `cassette.mjs` | 原始请求/响应字节、观察分块、时间、终结原因与完整性校验 |
| `bindings.mjs` | 按 owner/type/alias/创建因果锚声明与绑定动态值 |
| `request-bindings.mjs` | 生成并冻结精确字段模板、工作区/会话绑定和工具参数 portable 计划 |
| `portable-sse.mjs` | 从原始录制派生工具参数可移植版本，保留来源哈希 |
| `../helpers/llm-replay-suite.ts` | record/replay-only 生命周期、seed、fixture 发布和逐场景报告 |
| `../helpers/llm-replay-*.ts` | 场景身份与真实终端驱动 |
| `../tests/replay-v2/`、`../tests/replay-v3/` | 94 个业务场景；检查真实屏幕、ACP 事件、历史与文件效果 |

部署入口同时设置 `PERI_MODEL_TRANSPORT_PROXY` 和 `PERI_REPLAY_RUN`。非法或不完整配置在启动时失败；不回退到真实模型。模型原始 endpoint/profile/credential identity 保持不变，网络路由在 HTTP 请求构建完后才生效。普通运行未设置变量时保持原路径。

## 匹配与并发契约

调用键为 `(owner, execution, call, attempt, purpose)`。`run` 只隔离本次运行状态，不写入稳定匹配键；父调用另行核验。每个键只消费一次，取消不回卷，代理不替 Peri 重试。

会话必须绑定到 runner 已观察的 create/load 动作；子 Agent 必须绑定到父调用及原始 tool call。不能按 HTTP 到达顺序、UUID 排序或请求正文哈希分配身份。主 reason 使用真实执行的 `attempt_id` 和 `reason:<step>`，HTTP 重试保持逻辑 call，增加物理 attempt。

场景内的共享 workspace/store 保持共享。不同测试 run 只共享只读 cassette；绑定、消费状态、门控互相隔离。独立会话可反序到达，但同一调用的重试顺序和父归属不能被反序放宽。

I01 验证相同输入的双会话反转请求与响应顺序后，各自恢复正确 TUI 历史；I02 在同一代理上并行运行同份录制的两个新 world，分别完成真实重试与 Write，并检查文件互不覆盖；I03 验证一会话取消及迟到取消信号不会终止另一会话或新的执行。身份匹配不会代替这些业务断言。

`completeRun()` 同时核验未消费、额外、失败及在途请求。HTTP 终态与录制落盘/读取清理分别跟踪：已完成的 HTTP 请求可以触发真实重试，但 `pendingDurable` 或 `pendingCleanup` 未清零时不能完成 run。收到一段预期文本不等于用例通过。请求正文、工具定义、错误或终态变化都会使回放失败；提示词发生合理变化时应审核并重录，而非扩大忽略字段。

## 流式与取消

raw cassette 保留 HTTP body 观察到的原字节；HTTP 完成、下游提前关闭、上游中断分开记录；下游关闭不自动等同用户取消，取消意图由真实键盘／ACP 动作证明。捕获字节和已提交给 HTTP writer 的字节分开记录，均不代表应用已消费。正常 HTTP EOF 不代表 LLM 协议完成。seal 校验完成前的目录不能用于 replay；响应不会被补 JSON、换行或终止标记。

fast 模式去掉录制延时，recorded 模式按观察时间调度，实际时刻仍受当前 I/O 影响。两者都不宣称复现 TCP 包或客户端 read 的边界。fast 完整回放在末字节提交时同步提交 HTTP end，避免文件清理制造虚假的断流窗口；末尾仍有控制门时不会提前结束。竞争场景通过门控释放响应前缀，等待真实屏幕/ACP 状态，再触发取消、切换或继续；代理 write 完成不是应用已消费的证据。HTTP 错误响应也受 Peri 自身的错误正文读取期限约束，不能任意长期暂停后仍假设客户端继续等待。

## 动态值与可移植录制

只有显式声明的 JSON Pointer 可以规范化。动态 UUID/路径需先按因果锚绑定，响应工具参数中的对应引用也必须改写。禁止全局字符串替换，未知引用和别名冲突直接失败。JSON 对象键序不影响匹配，数组顺序、字符串、未声明字段和数值字面量保持严格。

`createRequestBindingAdapter({ mode, runId, declarations, owner: "world" })` 提供 `declareAndBind`、`onRequest`、`requestRules`、`snapshotDeclarations` 和 `responsePlans`。录制阶段根据已登记的真实 world/session/child 身份生成精确路径规则，封存在 cassette 中；重放不重新发现或放宽规则。`typed-template` 包含原样文字和 `{ kind, alias }` 引用，完整字段必须等于当前绑定渲染出的模板，才能转换引用。根目录、HOME、工作区按最长合法路径边界识别；同路径片段、`bg-<UUID>` 等派生 ID 不能隐式冒充已声明身份。

日期只允许系统环境 `<env>` 段中带 `Today's date: ` 标签的快照字段绑定。场景固定 `TZ=UTC`，其 ambient UTC date 不是业务断言；用户日期、工具时间输出和其它时间戳仍严格匹配。monitor 时钟是单独显式声明的 `timestamp` 类型：必须先验证真实 task、event seq、字段归属与合法 RFC3339，再对精确 JSON Pointer 使用完整字段模板。时间戳不会自动发现；非时间字段、事件顺序、正文与未声明字段继续逐字严格。相同时间可以属于不同事件，不能反推身份。同样的窄规则只适用于 `<env>` 段中的 `Platform: ` 与 `OS Version: ` 两行：镜像补丁版本随 CI 升级变化，不代表业务行为；其它系统提示、工具列表和消息仍严格匹配。未声明的提示词或其它环境变化会产生 fixture drift。

原始录制保持不变。portable 派生当前支持 Anthropic 和 Responses 的完整工具参数；Responses 的 delta、arguments.done、item.done、completed 副本必须一致。只修改声明的字符串叶节点，并保留工具参数各份副本的键序、空白和未修改的数值字面量，避免下一轮 history 字符串因重新序列化而漂移。每份计划保留原始响应 SHA-256、规则版本和新的分片计划；重放核验来源，不保留原始分块时间。

默认响应白名单为 Write/Edit/Read 的 `file_path`、Agent 的 `resume_thread_id`/`prompt`、folder_operations 的 `path`。结构化路径使用完整路径规范性校验与 `path-prefix` 规则，不能通过空格或 `..` 越出声明的根。Bash `command` 不自动转换，场景优先使用相对工作区命令；任意 shell 字符串替换不具备引用或转义语义。其它工具字段需要场景显式配置。半截参数、未知协议、thinking/reasoning、`reasoning_content`、签名和加密状态不自动转换。

## 使用边界

- 模型代理只替换 LLM 网络响应。文件操作、MCP、时间和外部服务需要场景提供独立环境与断言。
- TUI runner 的一次性执行绑定只用于明确会话、无遗留执行且单个输入动作的场景；同会话重叠执行需要实际生命周期证据，不允许首请求猜测。
- U06 验收手动 compact 的辅助模型调用、摘要提交及续聊。T06 验收 `before_tool` prompt hook 的真实模型判定与 Bash 输入改写；不能由此声称任意 hook 阶段、任意 hook 配置或 prediction 分支均有业务覆盖。
- 跨进程迟到请求的 generation fencing 仅有代理侧机制，未声称已完成 Peri 端到端覆盖。进程重启后的历史续聊、双进程独立 run 和旧执行取消分别由指定业务场景验收，不能与通用 generation fencing 混同。
- v2 已使用不同 world 路径与 fresh session ID 重放，并验证 portable 子 Agent resume 与文件路径。跨操作系统、不同提示词版本或任意外部工具环境的迁移仍需明确契约；portable 不是自动修复环境漂移的功能。
- 只约束场景声明的真实因果边。独立调用可反序，但合法调度若改变下一轮实际正文，仍严格失败，需要审核并录制对应分支；不承诺任意并发调度都能共用一条轨迹。
- 模型凭据仍走原认证流程。离线 fixture 使用隔离 HOME 和 synthetic key；线上录制使用用户明确配置的上游。认证 header 不写入 cassette，prompt、工具结果与原始响应仍属于录制内容。
- 模块测试、真实业务场景、最终统一 record/replay invocation 分别报告；模块测试通过不能代替真实 TUI 验收。

## 独立代理 CLI

```bash
node e2e/llm-replay/cli.mjs record --cassette ./case-tape --run recording --upstream http://fixture-model.test=http://127.0.0.1:9000
node e2e/llm-replay/cli.mjs verify --cassette ./case-tape
node e2e/llm-replay/cli.mjs replay --cassette ./case-tape --run replaying
```

以上 record 示例要求调用方已启动 9000 端口的本地模型源。`--upstream` 只在 record 使用；replay 不接受上游配置。可选 `--port`、`--timing fast|recorded`、`--gates <JSON文件>`。`verify` 校验已有封存及 body 完整性，不执行 Peri 业务。

CLI 输出代理地址、独立管理地址及控制 token。管理请求携带 `x-peri-replay-admin`：`GET /status`，`POST /bind`、`/release`、`/complete`、`/seal`。身份绑定仍是显式步骤；CLI 不会根据请求到达自动推断两个运行中的会话关系。CLI 是原始代理入口，不负责创建 world、typed 请求绑定或业务断言；复用 v2 Peri 用例使用场景 runner 及持久 fixture。

`PERI_E2E_REPLAY_FIXTURES` 可指定 fixture 根目录，`PERI_E2E_REPLAY_RESULTS` 可指定报告根目录，`PERI_E2E_REPLAY_INVOCATION` 可命名一次运行。默认报告位于 `e2e/results/replay-v2/<invocation>/<id>/`，包含准备、驱动、清理耗时，以及请求、原始 PTY、可见屏幕、ACP/代理状态和最终文件证据。I02 的第二个并行 run 在同一场景证据目录下单独保存。
