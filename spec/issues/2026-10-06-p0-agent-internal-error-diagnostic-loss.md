# P0：Agent 内部执行失败缺失可定位诊断

**状态**：修复已实施，提交快照编译与关键回归通过；待用户真实现场验收，P0 保持未关闭。旧现场的具体执行失败原因尚未确认。
**优先级**：P0（用户于 2026-10-06 明确指定，需要重点关注）。
**类型**：执行失败 / 可观测性 / 错误边界诊断丢失。
**范围**：运行时错误、日志、遥测、配置 Debug、MCP 工具失败与 SDK 任务状态投影；不变更权限、认证、执行所有权与恢复语义。

## 当前授权（取代此前脱敏要求）

**2026-10-06 用户最新要求**：“简单一点，移除所有的脱敏，所有东西不认为是敏感的”。本轮按此授权移除运行时诊断脱敏，不新增复杂的敏感字段识别器、allowlist 或诊断兼容层。此前“安全内部原因链 / 仅遮蔽敏感值”的方案已被此要求取代。

- 错误、日志、遥测与 Debug 输出保留实际内容，包括 URL/query、路径、headers、provider body、认证字段及 cause/context；这些输出可能包含凭据，使用者须按完整运行数据管理日志及遥测访问。
- 保留既有长度/资源限制、分类与协议形状校验、权限和身份验证、终端控制字符处理，以及 Anthropic `redacted_thinking` 等原生协议语义；它们不是诊断脱敏。
- 不主动增加全量成功请求/响应的日志，不把移除脱敏扩大为无条件记录所有业务数据；现有诊断出口不再遮蔽已有信息。
- 不向源码或 fixture 写入实际运行凭据；测试使用合成数据验证保真。不修改现场数据库、不重放副作用、不重置预算。

## 用户现场与影响

用户看到 `Agent execution failed: An internal error occurred. Check logs for details.`，但无法从提示指定的日志定位具体原因。

现场文件：`.tmp/agent-tui.2026-10-06`。用户给出的绝对路径为 `/Users/konghayao/code/ai/peri-v4p3/.tmp/agent-tui.2026-10-06`。

这不是单纯缺一条 ERROR：执行已经失败，而失败漏斗只记录与界面相同的通用文案，形成“要求查日志，但日志不提供原因”的诊断死路。无法据此判断应修复哪一层、是否可以安全重试或是否存在未确认的工具副作用。

## 已确认的证据

- 该日志中匹配 `[v2] execution failed`、`kind="internal"` 且使用上述通用文案的记录共 **17 条**（登记时扫描结果；文件后续可能继续增长）。不是仅凭截图推断日志缺失。
- `2026-10-06T12:44:19.545679Z`（北京时间 **20:44:19**），日志第 1663495 行，session `01a11139-0a5b-7ea1-b754-623f2c3ffb0b`：只记录 `kind="internal"` 和通用文案；下一行 TUI 记录 `bridge: AgentExecutionFailed`。
- `2026-10-06T12:44:31.402960Z`（北京时间 **20:44:31**），日志第 1665425 行，session `01a110e5-103d-7fd1-b645-2661f8a893a2`：同样只记录通用文案；随后 TUI 记录失败，host 收尾日志也只保留同一文案。
- `peri-acp-types/src/error.rs` 的 `AgentError::user_facing_message()` 将 `Other(anyhow::Error)` 投影为上述通用文案。
- `peri-acp-types/src/session/execution.rs` 的 `ExecutionFailure::from_agent_error()` 生成安全窄投影；完整内部错误不会进入此 DTO。
- `peri-agent/src/session/exec/executor_helpers/v2_execute.rs` 的 fatal 日志只读取 `terminal.failure` 的 `kind`、`http_status` 与 `public_message`，未提供独立于用户文案的内部定位诊断。代码明确要求日志与 wire 使用同一安全投影。

复核命令：

```bash
rg -n '\[v2\] execution failed.*kind="internal".*An internal error occurred' .tmp/agent-tui.2026-10-06
```

## 判断与限制

**已确认的诊断根因**：未知内部错误被转为安全公开消息后，fatal 日志复用该消息，具体原因或定位线索未在此出口保留。公开脱敏是必要边界，但不等于诊断也只能有通用文案。

**尚未确认**：上述两个晚间 session 的原始执行失败原因；现场二进制与当前工作区源码的精确对应关系；是否有其他存储证据可以补足原因。不得把日志附近的 MCP 超时、内存告警或既有预算问题直接认定为这两次失败的原因。仅凭现有通用文案无法还原原始异常。

## 修复要求

1. 在具体错误被公开投影前建立统一诊断记录点，保留实际错误、cause/context、错误类别及关联身份；不要只在 TUI 重复打印通用消息。
2. 已知领域失败使用 typed error 与稳定诊断，不包装为无信息的 `Other`；未知错误也须留下可追溯定位线索。覆盖一类问题，而非只替换本次文案。
3. ACP/TUI、日志与遥测不再做内容脱敏：保留实际错误文本及原因链；已有类型与协议适配继续负责类别、状态和字段转换，不为此引入第二套诊断规则。
4. 明确 session、turn/run 与诊断记录的关联方式，让用户从失败提示或对应日志找到同一次失败的诊断；不得把取消、预算阻塞或输出截断误分类为未知内部失败。
5. 保留已提交结果和未知执行证据；修复诊断不得通过吞错、自动重放工具、重置预算或改写数据库来掩盖失败。

## 验收清单

- [x] 默认 `info` 日志捕获回归：不同内部 fatal 原因保留实际错误、包装链及 session/turn，不再只有“Check logs for details”；实际用户现场另行验收。
- [x] 不同内部失败来源与 anyhow 包装链的定向回归通过，统一记录点保留定位线索。
- [ ] 回归覆盖已知领域错误、LLM/provider 错误、用户取消及收尾失败的分类与关联；不把正常终止报为 fatal。
- [ ] 日志与 ACP/TUI 保真验证通过：合成凭据、路径、URL/query、provider body 与多层 cause/context 不被内容过滤，长度和协议约束仍有效。
- [ ] 使用修复后的二进制复现场景并核对实际日志；现有 17 条记录不追溯伪造原因，未确认的现场原因保持明确标注。
- [ ] 更新受影响的代码入口与稳定契约，并由用户最终验收；未通过前保持 P0 未关闭。

## 关联与优先级

- [P0：Agent 推理预算耗尽被误报为内部错误](2026-10-06-p0-agent-budget-interruption.md)：已确认其中一次旧现场的具体预算原因，是本问题的一类领域实例；该修复不代表所有未知内部失败已有可定位诊断。
- 工作区待提交参考 `spec/issues/2026-10-06-error-path-logging-gaps-p1.md` 涉及其他错误出口；本 P0 单独跟踪，不把整个 P1 批次一并升为 P0，也不因存在 ERROR 就视为诊断完整。该已有 WIP 不纳入本次提交。

**重点关注**：本 P0 的诊断闭环应优先安排；预算类实例通过验收不自动关闭本条目。

## 信息结构与验收（以当前授权为准）

用户同时要求保留正确的内部信息结构，不接受只增加通用日志、错误编号或代码位置作为完整修复；不为追求结构化而丢弃原始原因。

### 内部诊断与公开投影分离

- 原始类型化错误是诊断事实源，ACP/TUI 消息不能再把其泛化为唯一的通用提示；fatal 日志也不能只有公开消息而无实际诊断。
- 记录真实错误的结构：稳定错误类别/代码、产生模块与执行阶段、操作、具体失败原因，以及有序的 cause/context 链。保留根因与包装上下文的关系，不把全部内部失败压成 `internal` 或无法追踪的字符串。
- 按错误实际拥有的事实记录关联身份（session、turn/run、work、tool/invocation、provider request id 等）；不存在的字段不补造，无法取得的根因须明确说明诊断缺口。
- 保留实际诊断所需的非敏感事实，例如预算类型与 used/limit、状态迁移与拒绝理由、存储操作及错误码、HTTP status、传输/协议类别、重试次数。自由文本不得成为这些结构化事实的唯一载体。
- 默认日志必须能够承载或关联上述内部诊断；公开界面仍可保持简洁，但必须能关联同一次失败。仅有 error id、代码位置或笼统“存储失败”而没有实际原因，不算完成诊断闭环。

### 内容不脱敏，执行与协议边界不变

- 删除字段/值遮蔽、凭据形状识别和固定通用错误替换；完整原因链可以输出，不新增自由文本安全扫描。
- 保留长度限制并明确诊断是否有界；不要把脱敏函数原名保留为无操作兼容层。
- 认证、请求权限、执行生命周期与取消语义不变；未收集到的信息不得伪造，移除脱敏不能还原此前已经丢失的旧日志内容。
- 修复证明诊断有效性与内容保真；同一次失败必须能够解释实际原因。

### 补充验收

- [ ] 构造不同根因但相同公开通用文案的内部失败，默认日志能区分真实原因与产生阶段，并关联正确会话/执行。
- [ ] 多层包装错误在内部诊断中保留有序原因链与上下文；不得只留下最外层消息或 opaque 编号。
- [ ] 同一错误含合成 token、连接串与路径时保留全部内容，错误类别、根因、结构和关联仍可核对。
- [ ] 长度限制或缺失信息明确表达，不将无法获取的原因伪装为已知原因。
- [ ] 实际复现后的日志足以回答“哪次执行、哪一层、哪个操作、为何失败”；不依赖临时打开 debug、重新猜测或直接改库才能定位。

## 全系统排查与修复分工

按 workspace Rust crates、MCP capability packages、SDK 和辅助 gateway 扫描运行时诊断出口；`case_insensitive`、终端 sanitize 与原生 provider redacted block 等词法命中不视为诊断脱敏。

| 分域 | 已定位的入口 / 待核对链路 | 修复方案与 owner |
| --- | --- | --- |
| Model / HTTP / 重试 | `peri-model/src/runtime/{error,request,retry}.rs`、`transport/http.rs`、provider request/response；上下文过滤、Debug 隐藏、HTTP/transport 原因丢弃 | subagent：删除内容过滤与 Debug 隐藏，保留实际失败详情及重试/中断来源；分类与长度约束不变 |
| Agent / 契约 | `peri-acp-types/src/{error,session/execution}.rs`、Agent fatal 漏斗；`Other` 泛化、正则脱敏、窄诊断入口 | subagent：公开消息保留原文和 anyhow 原因链，删除遮蔽函数，fatal 默认级别记录真实错误及 session/turn |
| ACP / TUI / 配置 / MCP host / 遥测 | `peri-config` Debug 与 parse 错误、`peri-middlewares/src/mcp/client/status.rs` 与调用方、ACP error data、TUI失败日志、Langfuse request诊断 | subagent：移除运行时遮蔽和错误替换，检查完整信息跨层传递，不改变协议分类和权限 |
| MCP capabilities / SDK / gateway | `mcp-packages/common/src/{failure,result_mapping}.rs`、各 handler 固定失败文本；`npm-packages/@peri-sdk/src/state/task-projection.ts`；gateway headers | subagent：保留工具实际失败原因，删除 SDK 正则遮蔽、gateway header 截断；保留结果形状/长度约束 |
| 契约与集成 | standards、code-index、本 issue、跨域 API 与定向验证 | 主 agent：更新本次授权后的单一契约，集成审阅，执行最小回归及必要跨层构建；记录未覆盖与失败 |

实施前各 subagent 返回源码证据和独占修改范围，主 agent 写回方案后再放行修复。工作区已有用户修改不覆盖、不回滚；全系统扫描不等于全系统真实运行验证。

### 源码核对与独立复核补充

- Model 中 `transport/http.rs::map_reqwest_error` 只保留传输分类，非 2xx 的 `runtime/stream.rs::response_to_sse_stream` 原先未读取错误 body；上层无法通过增加日志恢复这些已经丢失的内容。本轮在产生处保留实际错误与有界 body，再经重试、中断和终态投影传递。
- `runtime/error.rs` 的上下文内容白名单、`runtime/request.rs` 的 endpoint/metadata/body 遮蔽与 provider config Debug 均属于移除范围；`protocol/prepared.rs` 不因 userinfo/query 内容拒绝有效 checkpoint。
- 非 Model 的内部错误不能只靠一个限长公开字符串跨层：`ExecutionFailure` 增加实际 `error_category` 与逐节点 `causes`，ACP 和遥测显式传递；Model diagnostic 增加 `message`、`body`、`causes`。保留既有 `kind/status` 与有效性校验。
- Subagent 失败保留未知内部错误及原因链，不再只允许 Model 类失败进入结构化结果；MCP 工具失败保留实际 detail 与既有 recovery 指引，不用固定文案覆盖真实原因。
- 独立只读复核发现 TUI `cli_meta.rs` 和 SDK session/transport 的静态失败替换，已补入对应 worker 修复范围；这是实际原因丢失，不只是词法上的 `redact` 命中。
- 独立复核要求补足容量与关联：错误 body 收集有总时间/字节边界，文本与深原因链裁剪明确可见，MCP 原因链不能无界遍历；成功重试场景也须关联 session/turn，不依赖最后一定会出现 fatal 日志。
- 旧脱敏测试必须按新授权改为内容保真测试；删除脱敏函数后不留下空 `redacted_paths` 等无操作兼容字段。测试拆分仅为遵守文件规模约束，不丢弃既有生命周期断言。

### 定向验证计划

1. Model：非 2xx 多 chunk body、body 读取失败/超时/取消、传输 source 链、实际重试耗尽和可见 delta 后 interruption、内容保真与显式裁剪。
2. Types/Agent：不同 `Other` 根因、cause 包装、非 Model 结构化 failure、预算/取消分类、默认级别日志与 session/turn 关联。
3. ACP/配置/遥测：error data 中分类、body 与 causes；配置解析实际错误与 Debug 原文；Langfuse 保真、不重复实现错误分类。
4. MCP/SDK/gateway：实际 tool failure 与 recovery、live/snapshot task summary、JSON-RPC `RpcError.data`、session/stdout 解析失败、header 原文；长度与状态机不变。
5. 集成：只提交本次路径或 hunks，核对提交快照与其他任务未提交改动。用户允许 `git commit --no-verify`，不等于自动宣称测试或真实运行验收已通过。

## 本轮实施与验证记录

- 实施：删除内容遮蔽与凭据形状过滤；保留实际错误、provider 错误正文及有序原因链，经重试/中断、Agent、ACP、遥测和 SDK 传递。配置 Debug、MCP 工具失败与 gateway header 保留原文。
- 默认日志：fatal ERROR 带 session/turn、类别与原始原因；retry/interruption WARN 即使没有事件 handler 也保留关联身份和底层诊断；取消及非 fatal 不误报 ERROR。
- 边界：认证、权限、状态/协议有效性、取消、不重放及容量约束保持独立；body 读取有时间/字节边界，文本和深原因链裁剪明确标记。
- Model 全部 lib 测试：**173 passed / 0 failed**；覆盖 HTTP body、原因链、实际重试/中断、取消和裁剪。
- 独立提交快照 `check --locked --offline --workspace --all-targets`：**通过**。该快照排除了其他任务的 work/budget/staging/effects/steer 改动。
- 提交快照关键回归：Agent 默认 INFO fatal/cancel 日志 2、retry 日志/投影 2、terminal mapping 12、Types error 13，合计 **29 passed / 0 failed**。先前过滤器匹配 0 项的命令不计入通过数量。
- 提交快照 `test --locked --offline --workspace --doc`：**通过**；既有 ignored 文档例未执行，不把 ignored 算成通过。
- SDK 定向测试 **37 passed**，`bun run typecheck` 通过；gateway 定向测试 **38 passed**（由对应 subagent 实际执行）。真实子进程 RPC 回归保留 `error.data` 内容，不等于真实 provider 故障端到端验收。
- 工作区首次 all-targets 检查还发现其他任务 `peri-resources/src/sessions/work/effects_test.rs` 的借用签名错误；本次未修复、未暂存该文件，独立提交快照不含该 WIP，不能宣称原脏工作区整体已通过。
- 本轮修改的源码/测试文件均不超过 1000 行；未跑完整 workspace 单元/E2E 测试、真实模型或 Emscripten 验收。旧日志已丢弃的原始原因不能追溯恢复。
- 用户已授权提交及 `--no-verify`；仅提交本次诊断修复与对应文档，保留其他任务 WIP，不 push。
