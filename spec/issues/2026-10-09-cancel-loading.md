# Cancel 后仍 loading：排查与现场验收

状态：阶段 hook 取消漏洞已提交；流式取消终态过滤漏洞已复现并修复，现场复核待验收。

## 问题与边界

用户报告：有时 cancel 后仍显示 loading。不能把取消通知发送成功当成执行停止，
也不能仅在 TUI 强制关闭 loading，掩盖服务端还在运行的事实。

实际路径：TUI cancel consumer → `AcpTuiClient::cancel` → ACP notification →
`host/notify.rs::handle_notification` → mailbox / 当前 turn token → Agent 退出 →
执行结果分类与事件排空 → `EventSink::push_done` → TUI 匹配请求并终态化。

## 已确认原因

模型请求、Full Compact 的模型等待和工具 invoke 已有 cancel 竞争；但阶段
middleware runner 原先直接 await hook，没有统一监听当前 turn cancel。
因此 cancel 已触发仍可无限等候输入准备、启动 MCP、模型前后处理或回答后处理。
RCRA 不返回，执行器无法进入最终结果分类，done 通知无法发布，TUI 正确地继续 loading。

生产对应例：`peri-middlewares/src/mcp/middleware.rs::before_react_start`
等待 System MCP readiness；`StartupState` 不携带 turn token，取消责任必须由
Agent runner 承担，而不是要求所有 middleware 各自实现取消。

复现证据：在真实 `run_react_loop` 中，分别令 `before_agent`、
`before_react_start`、`before_model`、`after_agent` 挂起；确认进入 hook 后触发
cancel。修复前四项均未在测试的 200ms 防挂死窗口退出，预取消启动测试也失败。
200ms 只用于回归测试，不是产品取消超时或性能指标。

## 修复方式

- runner 在 hook 等待边界优先处理 cancel，返回 `AgentError::Interrupted`。
- 覆盖首轮 reminder、输入准备、启动闸门、Compact、模型、审批、工具后处理与错误 hook。
- `before_agent` 的正常软失败行为不变；Interrupted 不降级为警告继续执行。
- 输入准备已经完成的替换仍同步回 transcript；启动候选在取消后不发布。
- 批量审批取消返回与输入调用等量的 Interrupted，不能破坏审批数量契约。
- 不直接 drop 整个 Act/执行器，不跳过工具结果结算或持久化，不关闭 session MCP owner。
- 不改变 ACP wire、数据库结构或独立子 Agent 的 continuation 规则。

回归入口：`peri-agent/src/agent/stages/middleware_cancel_test.rs`。

## 流式 thinking / 正文的新增确认根因

用户进一步明确：thinking 和正文流式输出期间取消有问题。
沿真实 ACP transport → client pump → notifier → bridge handler 建立回归，
分别发送 ExecutionStarted、agent_thought_chunk / agent_message_chunk、调用
`AcpTuiClient::cancel`，再发送服务端匹配的 cancelled done。
修复前，两种场景都因终态未交付而触发测试防挂死窗口，loading 保持 true。

原因在 `InteractionLifecycle::cancel_active_prompt` → `close_prompt_exact`：
关闭交互 owner 时同时把执行 request 标记 retired；随后
`should_forward_prompt_terminal` 把这个请求真正的结束通知当作旧请求丢弃。
这不是模型流没有收到 cancel，而是客户端丢失了已经到达的执行终态。

修复为 `ExecutionRuns::pending_terminal_request` 独立保存当前待确认执行终态：
cancel 仍关闭交互并阻止迟到 start / snapshot 重开，但匹配的 done 可透传一次；
done 消费后结清 pending，重复 done 被过滤；新的 execution 替换 pending，
旧终态不能清除新 turn 的 loading。短 prompt RPC lease 收尾也不能替代 done 确认。
没有在发送 cancel 时强制把 UI 变为空闲。

回归入口：`peri-tui/src/kit/acp_bridge_cancel_test.rs`、
`peri-tui/src/acp_client/client/cancel_test.rs` 和 `interaction_lifecycle_test.rs`；
同时扩展 Agent ModelStream 测试，验证 thinking 中途取消与正文一样停止继续发 chunk。

两位受派审查 subagent 均因 token refresh 的 403 鉴权错误中断，未产出报告；
本节证据由主 agent 本地复现获得，不宣称已完成独立对抗审查。

## 尚未证明的现场因素

以下是链路上仍可能延迟终态的等待边界，不是本次已复现的根因：

- TUI operation gate 与 reverse interaction settlement：取消通知在这些操作之后发送。
- Agent 退出后的 forwarder 排空和 transcript flush：done 发布仍必须等待这些步骤。
- 首次会话创建或普通 prompt 尚未绑定执行 token 时的取消：需要现场时序再定位；
  现有 managed mailbox 预留 ticket 撤销测试不代表全部 legacy prompt 准入窗口已覆盖。
- 独立子 Agent 完成后的已授权 continuation 可以再次进入 loading；这是现行契约，
  必须用 request identity 区分新的内部执行与旧执行没有停止。

## 验收与现场取证

运行：

```sh
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --doc
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp -p peri-tui --lib -- cancel
```

2026-10-09 首次 hook 修复验证：Agent 1042 项单元测试、15 项 doc tests 通过；
其中新增取消回归 13 项通过。ACP 与 TUI 的 cancel 过滤各命中 29 项并通过，
不代表两 crate 全量测试或真实终端现场已验收。依赖方向门、文件规模扫描、
变更 Rust 文件格式检查和 `git diff --check` 均通过。

流式终态修复后再次运行全量单元测试：Agent 1043 项通过；TUI 1724 项通过，
6 项 ignored。新覆盖 thinking、正文、混合流、managed request、重复与旧终态、
重复 cancel、prompt lease 收尾，以及模型 thinking 取消停止继续输出。
这验证客户端终态消费链及模型取消边界，不替代真实外部 provider/终端现场验收。

现场复核普通问答、System MCP 尚在连接、工具审批挂起和回答后处理时的取消；
检查取消后能提交下一轮，历史与已完成工具结果未丢失。若仍复现，保留 session ID、
request ID、取消时间点，以及 `Cancel requested`、`middleware hook interrupted`、
done 通知和 `TurnInterrupted` 的对应日志，判定卡在通知前、hook 内、收尾还是新 continuation。

验收关闭后按 DOC-HISTORY-001 收敛入口并删除此过程文档。
