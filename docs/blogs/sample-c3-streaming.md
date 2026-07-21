# Peri 流式输出协议适配层——OpenAI 和 Anthropic 的 streaming 差异如何统一处理

> **[Peri Code](https://github.com/konghayao/peri)** — 用 Rust 写的开源 Coding Agent，兼容 Claude Code 生态。<https://github.com/KonghaYao/peri>

Peri 同时支持 OpenAI 和 Anthropic 两个 LLM provider，但两者的**流式输出**（streaming，模型逐 token 返回响应而非一次性返回完整结果）协议完全不一样。OpenAI 用 **SSE**（Server-Sent Events，基于 HTTP 的单向事件流），每个事件块只带一个 `delta` 字段表示增量内容。Anthropic 也用 SSE，但事件类型更多——有 `content_block_start`（内容块开始）、`content_block_delta`（内容增量）、`content_block_stop`（内容块结束），还有独立的 `message_start` 和 `message_stop` 来标记消息边界。同样的流式输出，两家的格式设计几乎没有任何共同约定。

Peri 在 invoke 层封装了一个**流式适配器**，把 OpenAI 和 Anthropic 的原始事件流统一转换为 Peri 内部的**内容块**（ContentBlock）序列。上游代码只看到一个统一的内容块流，不需要关心底层是 OpenAI 还是 Anthropic。适配层的核心工作在三个问题上——事件粒度对齐、推理内容分离、流中断处理。

## 事件粒度对齐将两家的事件块统一为 Peri 的内容块

OpenAI 的流式事件粒度很粗——每发一个 `delta`，里面可能同时包含文本内容和一个工具调用片段。如果 `delta` 里既有 `content` 字段又有 `tool_calls` 字段，调用方需要自己拆分。Anthropic 的事件粒度则很细——文本内容走 `content_block_delta` 的 `text_delta` 子字段，工具调用走 `tool_use` 类型的独立 content block，推理过程还有专门的 `thinking_delta`。

Peri 的适配器在收到每家的事件流后，先把它们聚合成 Peri 自己的 ContentBlock 结构。一个 ContentBlock 有三种类型——文本块、工具调用块、推理块。OpenAI 过来的 delta 被解析后按字段拆入对应的块类型，Anthropic 过来的事件按 `content_block_start` 标记的类型直接映射。聚合完成后的 ContentBlock 序列对上游完全透明——不管是哪个 provider 来的原始数据，上游只看到文本块、工具调用块、推理块三种统一格式。

聚合过程中有一个关键的设计决策——不等待完整消息到达再输出，而是边收边聚。每个 ContentBlock 有 `partial`（部分完成）和 `complete`（已完成）两种状态。partial 状态的块内容可以持续追加，complete 状态后锁定不变。这个设计让 TUI 界面可以在工具调用还在生成参数时就开始渲染名称和图标，用户不需要等整个工具调用 JSON 到达才能看到反馈。

## 推理内容分离让思维链在 TUI 中折叠显示

Anthropic 的 Claude 模型在 extended thinking 模式下会输出**思维链**（thinking，模型在生成最终答案前的内部推理过程）。这些内容在 Anthropic 的流式中通过特定事件类型标记——`thinking_delta` 事件带 `thinking` 字段，与正常文本内容分开发送。OpenAI 的 o1 系列模型也有类似能力，但推理内容包裹在 `reasoning_content` 字段中而非独立事件。

Peri 的适配器在聚合 ContentBlock 时识别推理内容并标记为独立的推理块类型。TUI 侧收到推理块后默认折叠显示——只展示一个可展开的折叠标记，用户点击后展开查看完整推理过程。这个设计既保留了推理内容的可审查性（用户可以确认模型的推理是否合理），又不会让推理内容在消息区中占据过多屏幕空间——一次 extended thinking 可能输出数千 token 的推理内容，如果全部展开显示会淹没正常的对话内容。

推理块和文本块的分离还有另一个好处——Langfuse 追踪系统可以单独记录推理块的 token 用量，区分推理 token 和输出 token。这对于成本分析很重要——Claude 的推理 token 和输出 token 是分开计价的。

## 流中断时保留已接收的部分内容并标记中断原因

流式输出可能在任何时点中断——网络断开、provider 返回错误、用户取消。如果中断发生时 ContentBlock 正在聚合中，直接丢弃已接收的部分内容会让 TUI 界面出现内容闪烁（先渲染了一段文本，中断后文本消失），用户体验很差。

Peri 的适配器在流中断时执行两个操作——把当前所有 partial 状态的 ContentBlock 强制设为 complete 并保留已接收内容，然后在流末尾追加一个中断标记块。中断标记块包含中断原因（网络超时、用户取消、provider 错误）和已接收的 token 数量。TUI 侧收到中断标记后，在已渲染的内容末尾显示一条原因说明，用户知道内容不完整并理解中断原因。

中断设计里有一个反直觉的决策——不重试中断的流。大多数 streaming 实现在中断时会自动重连并重新发起请求，但这在 Agent 场景下有代价——如果 Agent 已经在中断前部分接收了一个工具调用并开始执行，重试意味着同一个工具调用被执行两次。Peri 选择不重试，把中断结果原样交给 Agent 判断。Agent 看到中断标记后可以决定重新发起请求（此时工具尚未执行）或根据已接收的部分内容继续推理。

回到开头——Peri 支持两家 provider 但不暴露任何 provider 差异给上层代码，关键就在这个流式适配器。上游的 TUI 渲染、Agent 推理和 Langfuse 监控都不需要知道底层是 OpenAI 还是 Anthropic 的流式格式。

项目地址：[github.com/konghayao/peri](https://github.com/konghayao/peri)
