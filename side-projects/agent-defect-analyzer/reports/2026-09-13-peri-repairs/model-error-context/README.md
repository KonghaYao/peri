# 模型错误的安全诊断边界

对应 `PERI-20260913-LLM-ERROR-CONTEXT`，原问题记录保存在 `73df17f7`。研究日期为 2026-09-13，最终集成验收在 2026-09-14 进行。实现已提交为 `d370c4bf`，正常提交 hooks 全部通过，提交 tree 与已验证快照完全一致；详见 [commit.json](commit.json)。

最终隔离快照的全部八条验证命令以 0 退出：3,676 项库测试与 8 项文档测试通过，库测试 4 项、文档测试 3 项既有 ignored 用例未计入通过。全工作区、全目标 clippy 使用 `-D warnings` 通过。独立 Luna 复核未发现本次范围内的剩余阻断。

| 最终验证 | 通过 | ignored | 退出状态 |
| --- | ---: | ---: | ---: |
| `cargo test -p peri-model --lib` | 152 | 0 | 0 |
| `cargo test -p peri-acp-types --lib` | 417 | 0 | 0 |
| `cargo test -p peri-agent --lib` | 778 | 0 | 0 |
| `cargo test -p peri-middlewares --lib` | 1,670 | 4 | 0 |
| `cargo test -p peri-acp --lib` | 658 | 0 | 0 |
| controller 的 `test_turn_error_reason_is_safe_in_error_span` | 1 | 0 | 0 |
| `cargo test --workspace --doc` | 8 | 3 | 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 检查通过 | — | 0 |

完整命令、计数和日志 hash 见 [validation.json](validation.json)，对应 [源码快照](source-snapshot.json)、[环境与 PTC 构建前提](environment.json) 及 [独立复核](review.json)。`validation/` 保存实际输出，仅去除末尾空行，原始与保存后 hash 均记录。这里只运行五个完整库、一个 controller 回归和 workspace doc/clippy，没有宣称完成整个 workspace 的全部集成或 TUI E2E 测试。

要验证的结果是 ModelRuntime 中的安全事实经 Agent bridge、SubAgent 与父工具结果后仍可供消费，而不是只在中间 DTO 中存在。子任务身份、错误类别、HTTP status、受限 provider/request identity 和 retry 分类应来自类型；原始 provider body、headers、prompt 和凭据不能成为诊断字段。

协议错误的任意 summary 不进入 Agent/ACP 诊断，跨边界仅提供稳定 protocol kind。HTTP 400 本身不能判定具体请求拒绝根因；请求 ID 也不能证明请求随后重试成功。

本轮测试将外部模型替换为确定性 fixture，内部链路使用真实实现；400、429、500 与可见 delta 后中断分别核验。retry、子任务失败、调用终态与父 Agent 的后续决策是不同层次：保留事实不等于增加自动恢复策略，也不构成生产任务质量提升的因果证据。

## 审查发现与验收边界

第一轮 canonical/后台链路验证发现：仅校验诊断字符串不够，类别与字段可能互相矛盾；同时，持久化结果有 typed facts 不代表 live/ACP 事件也有。独立审查因此没有接受局部测试通过为 issue 完成。最终验收因此涵盖字段矩阵拒绝、实时/回放/ACP allowlist 与 retry 事件的实际消费，并记录四类 fixture 的具体起止边界。

第二轮审查进一步发现：收紧字段矩阵可能拒绝生产者本身产生的合法 retry-exhausted 诊断。写出 JSON 成功不是持久化兼容证明，必须将真实 producer 的诊断和父消息序列化后再读取，并检查全部事实仍相等。该发现作为最终修复的必验项。

最终核心复核确认：直接 retry exhausted 与保留原始类别、附加匹配 retry pair 的诊断都能通过校验，矛盾组合仍被拒绝。公开 `ModelError::retry_exhausted` 改为可失败构造，零次尝试返回 `None`；现有调用方显式处理有效输入。`RetryObservation::from_model_error` 从同一个错误派生类别与诊断，取消可自由拼接两套事实的入口。基础错误类别覆盖 safe serde 往返，真实 HTTP retry 耗尽形态在父 canonical 消息处验证重读。

完整库验收还发现一处 Middleware 的旧选择指南断言，已与 `9495928b` 中的提示词来源契约对齐，独立复核通过后重新运行整个最终矩阵。[集成检查发现记录](integration-findings.json) 保留修复前快照、失败命令与退出状态，不能用较早版本的局部通过替代本表的最终结果。

## 确定性试验设计

`LocalProvider` 仅替换外部 HTTP/SSE provider，使用本机 loopback 端口返回固定响应。`RuntimeFailureModel` 持有公开 `OpenAiModel`，保留真实 HTTP、SSE 解码、ModelRuntime retry、AgentModelBridge、SubAgentTool、子任务循环与父工具 dispatch。`ParentDriver` 替换父模型的下一步决策，使父任务确定地调用一次子任务并消费结果；它没有替换子模型 runtime。retry delay/jitter 设为零以消除墙钟影响。

| 场景 | 总 HTTP 请求数 | 其中重试数 | 应保留的诊断 |
| --- | ---: | ---: | --- |
| 同步 HTTP 400 | 1 | 0 | `http_status`、400、`req-400` |
| 同步 HTTP 429 | 6 | 5 | `http_status`、429、`req-429`、retry 耗尽信息 |
| 同步 HTTP 500 | 6 | 5 | `http_status`、500、`req-500`、retry 耗尽信息 |
| 可见 delta 后 SSE 中断 | 1 | 0 | `stream_interrupted`；已可见 delta 仍转发 |
| 后台 HTTP 429 | 6 | 5 | typed 后台结果、受限通知和子任务身份 |

所有场景的受限 provider 值为 `openai-compatible`。SSE 中断的 runtime 错误出口当前没有 request ID，断言为空，不从外部 fixture 或显示文本补造。表中的 6 是包括首次请求的总尝试数，不是 6 次重试。

同步场景从真实父 transcript 取 canonical `BaseMessage`，JSON 写出、读回、再写出结果相等；后台场景检查 `BackgroundTaskResult` 与 safe notification 的对应往返。同步与后台都断言每个 child 恰好一个 Stop，同步还核对 Started、可见 delta 与 Stop 的顺序。后台错误路径通过 callback/TaskManager 交付终态，没有发送 `BackgroundTaskCompleted`，测试不补造该事件或以其缺失判产品失败。

父 canonical 消息与 live 事件在上述真实内部链路检查；ACP allowlist、回放和 retry 事件由相邻映射/消费契约测试组合验证。不能把这组组合验证描述成一次外部客户端到生产 provider 的端到端测试。

ACP wire 用例经过真实 `TransportEventSink::push_event`，从 `peri/agent_event` 的 `event_json` 重新读取 `AcpEvent::SubagentStopped`，保留安全诊断。`peri/agent_activity` 沿用状态与哈希关联投影：输入实际结果标记后，活动载荷中没有结果、原始 instance ID 或 child ID；完整 Agent 事件仍保留其预期 result 和受控诊断。未输入 secret 的字符串缺失断言已移除，不能用它证明执行了脱敏。安全构造/ingress 拒绝与活动投影裁剪分别由对应负例验证。

## 复现与限制

最终验证使用隔离 Git 快照。Middlewares 的 PTC fixture 需要先在 `npm-packages/@peri-ptc` 执行 `bun run build`；使用已安装依赖，从隔离目录中的当前源码生成 dist。缺失忽略的 dist 曾造成测试准备失败，补齐后再运行，未复制旧 dist 作为新代码的证据。

loopback fixture 需要本机监听权限。沙箱拒绝 bind 属于测试准备受限；授权本机监听后的最终退出状态才计入结果。没有调用外部模型服务。完整库测试保留四项既有 ignored 用例，不将它们计为通过。

本次修复提供可核对的失败事实，并不新增父 Agent 的自动恢复决策。尚未证明生产任务有效性提高，也不能据此认定样本中的任一 HTTP 400 根因。新版本更完整地记录失败后，旧新版本错误数可能上升；比较任务质量必须同时控制生产者、观测契约、样本和评价规则。
