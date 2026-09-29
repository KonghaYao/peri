# OpenAI 兼容流式响应：无参工具被误判为中断

状态：待实施。独立于流中断断点保留修复。

## 已确认行为

`openai_compatible/stream.rs::completed_response` 无条件对累积工具参数执行 JSON
解析。完整工具响应的参数为 `""` 或 `"  "` 时，产生 `Interrupted(Provider)`；
相同响应的参数改为 `"{}"` 时正常完成。

`openai_compatible/response.rs::decode_assistant_message` 已把缺失或空白参数解释为
空对象，流式路径与这项既有兼容规则不一致。

最小响应（两个 SSE 事件之间有空行）：

```text
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"NoArg","arguments":""}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

```

在基于 `origin/main` 的 `e1630dcb` 上，通过公开 Model API 与本地 HTTP fixture
复现：空参数响应交付 `ToolCallDelta → Interrupted`；连续两次响应在恢复预算为 2
时使真实 Agent 循环返回 `StreamRecoveryExhausted { attempts: 2 }`，完成 hook 为 0。
该问题在流中断边界修复前已经存在；未访问真实 provider。

## 实施与验收

- 流式和非流式路径对无参工具复用同一参数解释规则，避免两份规则再次漂移。
- 覆盖参数缺失、空串、纯空白、合法空对象；验证完整无参工具实际执行一次并继续完成。
- 非空但残缺的 JSON、非对象参数仍须拒绝；不可把半截参数统一降级为空对象。
- 验证缺少完成证据的半截工具仍进入中断续跑，不能提前执行。
- 模型回归放在 `peri-model/src/openai_compatible/mod_test.rs`，完整行为覆盖真实
  HTTP → provider → Agent → 工具执行。遵循 `docs/standards/testing.md`。
