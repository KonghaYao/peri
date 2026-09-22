# [P1] print 模式可见流增量后中断直接终止，未保留部分输出续跑

**状态**：已实现，门禁与本地假上游 CLI 端到端（隔离 HOME）通过；真实 provider 未验证
**优先级**：P1
**类型**：Bug / 流式恢复
**创建日期**：2026-09-22

## 症状与报告证据

来源：`/Users/konghayao/Desktop/bug-peri-3.16.5-print-mode-no-retry-brief-2026-09-22.md`。该简报引用的完整报告 `docs/bug-peri-3.16.5-print-mode-no-retry-2026-09-22.md` 未在本次本地检索中找到，以下线上/假端点数据来自简报，不是本次重新实测。

被测版本 peri agent-v3.16.5，Docker Debian bookworm，Anthropic SSE，print stream-json：

| 上游断点 | 请求数 | rc | 原错误 |
| --- | --- | --- | --- |
| message_start 前 | 6 | 1 | retry exhausted after 6 attempts; last failure: transport |
| text 增量后 | 1 | 1 | model stream interrupted from anthropic |
| tool_use 参数中途 | 1 | 1 | model stream interrupted from anthropic |

半截工具表现为 `input:null`，usage 全零。线上 186 试次出现 2 次同签名错误（约 1.1%）：eicrud-keyset-pagination-cursor 第 3 轮已有 27,696 B / 8 文件补丁，评测整轮作废；numba-stencil-boundary-modes 第 1 轮失败，同配置下一轮重跑通过 29/29。报告产物根为 `artifacts/repro-upstream-err-2026-09-22/`，线上 eicrud 证据为 `artifacts/bug-peri-3.16.5-llm-api-error-abort-2026-09-22/case-eicrud-r3/`。

## 根因

- `peri-model/src/runtime/retry.rs:349`：visible 分支此前直接交付 Err 后 return，避免重放可见输出，却没有给 Agent 保留历史并续跑的终态。
- `peri-agent/src/agent/model_bridge.rs:270`：此前只转发 delta，没有累积部分正文；错误直接终止 Reason。
- `peri-agent/src/agent/stages/act.rs:147` 与 `stages/mod.rs:573`：已有 MaxTokens 的保留响应、跳过完成 hook、Defer 续跑机制，但没有流中断兄弟分支。
- print 的 session/prompt 错误继续按现有失败路径退出；本修复不在 CLI 重建会话，不回滚工作树。

## 最小修复与契约

1. `peri-model/src/protocol/model.rs:33` 新增 `Interrupted { error, attempts, max_attempts }`，不携带 usage。visible 后 transport 仍改写 stream_interrupted，其他错误类别原样保留；可见输出后的裸 EOF 也终态化，防止同一失败语义绕过恢复。当前请求立即终止，绝不 retry。
2. `Model::complete`（同文件 :229）遇 Interrupted 返回底层错误，不把部分响应伪装为成功、不解析半截工具参数。流式消费者才拥有续跑能力。
3. `peri-agent/src/agent/model_bridge.rs:348` 返回部分 Reasoning：仅累积文本；**正文为空时不制造 assistant 消息**（见下节）；非空时 source_message 复用流式 message_id，tool_calls 为空，不回灌无签名思考，不重复发 TextChunk。
4. `peri-agent/src/agent/stages/act.rs:127` 最终回答路径加空消息守卫：`message_content().is_empty()` 的 AI 消息不写入 transcript。空 assistant 消息不得进入规范历史是本路径的新不变量；中断无正文时只保留续跑提醒。
5. `Reasoning::stream_interruption` 保存 `StreamInterruption { error, attempts, max_attempts }`。Act 提交部分消息而不触发完成 hook；loop 使用可信 Guidance / model_runtime / stream_interrupted / Warning / Required 的 SystemInjected Defer 继续。
6. `peri-agent/src/agent/stages/mod.rs:957` 的预算来自事件 max_attempts，遵循 RetryConfig 包含首次的次数原则：默认第 6 次中断耗尽，最多 5 次续跑；loop 内累计、不因工具进展重置。attempts 是底层本次请求 attempt 序号，与 loop 中断次数分开。取消和语义迭代上限仍有效，完整工具继续正常执行。
7. `peri-acp-types/src/error.rs` 新增 `StreamRecoveryExhausted`，保留 ModelError；`session/execution.rs:106` 按 status 分类 Llm/LlmHttp。ACP `peri-acp/src/host/prompt.rs:59` 已有 allowlist 投影可直接复用，新增 wire 回归测试而不另造事件通道。

### 空正文中断不得写入空消息（复查新增）

`Interrupted` 分支曾无条件挂 `source_message`，正文为空时（`cut_tool_use` 形态、只收到 `ReasoningDelta` 的 reasoning 模型）会写入一条内容为空的 assistant 消息。后果链：

1. `visible_model_messages()`（`peri-agent/src/session/transcript.rs:310`）与 provider 编码器（`peri-model/src/anthropic/request.rs:211`）都不过滤空消息。
2. 复查实测（本地 `AnthropicModel::prepare_request`）：空正文 assistant 消息在 wire body 里就是 `{"content":[{"text":"","type":"text"}],"role":"assistant"}`。编码形态是实测；「空 text block 会被 Anthropic 拒收」依据其公开 API 约束（`text content blocks must be non-empty`），未对真实 provider 实测。注意相邻的空 `thinking` 占位块来自既有的 `ensure_thinking_blocks`（anthropic/cache.rs:167），不是本次引入。
3. 于是恢复路径把「本来能续跑的断流」变成「下一轮请求硬失败」，比原缺陷更糟。

修法（最小）：桥接层正文为空时 `source_message = None`、`final_answer = None`；`act.rs` 最终回答路径对 `message_content().is_empty()` 的 AI 消息跳过 `transcript.append`。

纯空白（`"  "`）按 `MessageContent::is_empty()` 判为**非空**（该判空不 trim，ARC-KEEPGOING-001 明确禁止用 `trim()` 替代），因此保留 `source_message` 写入历史。理由：provider 只拒绝零长度 text block，非零长度空白是合法内容；用 trim 判空会掩盖既有契约。副作用是空白文本不发 TextChunk（该分支沿用既有 `trim` 判空），即「写入历史但不渲染」，本 issue 不改动该既有语义。

测试：`test_stream_interruption_without_text_keeps_history_clean`（`peri-agent/src/agent/stages/truncation_test.rs:337`，走真实 `AgentModelBridge`，断言第 2 次请求无空 assistant 消息、transcript 无空 AI 消息、半截工具未执行、完成 hook 仅一次）、`test_bridge_stream_interruption_preserves_only_partial_text`（`peri-agent/src/agent/model_bridge_test.rs:94`，`""` 分支断言 `source_message.is_none()` 且 `final_answer.is_none()`，`"  "` 分支断言保留）。

事实源同步：`docs/code-index/peri-model.md`、`docs/code-index/peri-agent.md`、`ARC-OUTPUT-COMPLETION-001`。不修改 --max-turns，不增加 provider 特例，不修改 Bug 3 已有实现。

## 验证与未验证项

### 最终门禁（2026-09-22，macOS，本工作区）

下列命令均实际执行，最终 exit=0；未运行 cargo test --workspace。

| 命令 | 结果 | 原始日志 |
| --- | --- | --- |
| `cargo fmt --all --check` | 通过 | `/tmp/peri-stream-fmt.log` |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 | `/tmp/peri-stream-clippy.log` |
| `cargo test -p peri-model` | 156 passed；doc tests 0 | `/tmp/peri-stream-model.log` |
| `cargo test -p peri-agent --lib` | 842 passed | `/tmp/peri-stream-agent.log` |
| `cargo test -p peri-acp-types` | 433 单测 + 3 集成 + 1 doc passed；2 doc ignored | `/tmp/peri-stream-acp-types.log` |
| `cargo test -p peri-acp` | 689 单测 + 8 集成 passed；doc tests 0 | `/tmp/peri-stream-acp.log` |
| `cargo build --workspace` | 通过；macOS linker 提示 __eh_frame 超过 16MB，可能影响异常处理性能 | `/tmp/peri-stream-build.log` |

### fail-before / pass-after

保留新类型和新测试，临时回退关键实现行为，避免用“新类型不存在”的编译错误冒充回归证据；每次均恢复实现。以下四项均编译成功后在行为断言失败，测试进程 exit=101：

| 临时回退 | 命令 | 实际失败片段 | 日志 |
| --- | --- | --- | --- |
| visible 分支恢复 Err(error) | `cargo test -p peri-model visible_anthropic_delta_then_transport_failure_is_interrupted_without_retry` | `0 passed; 1 failed`；末事件不是 `Interrupted { attempts: 1, max_attempts: 2 }` | `/tmp/peri-stream-fail-before-model.log` |
| bridge 禁用正文累积 | `cargo test -p peri-agent --lib test_bridge_stream_interruption_preserves_only_partial_text` | `0 passed; 1 failed`；left 空串，right 部分正文部分正文 | `/tmp/peri-stream-fail-before-bridge.log` |
| loop 禁用中断续跑分支 | `cargo test -p peri-agent --lib test_stream_interruption_` | `0 passed; 3 failed`；请求数 left 1 / right 2；耗尽得到 Completed | `/tmp/peri-stream-fail-before-agent.log` |
| ExecutionFailure 移除新错误映射 | `cargo test -p peri-acp-types test_stream_recovery_exhausted_public_projection` | `0 passed; 1 failed`；left Internal / right Llm | `/tmp/peri-stream-fail-before-projection.log` |
| bridge 恢复无条件 source_message + act 移除空消息守卫 | `cargo test -p peri-agent --lib test_stream_interruption_without_text_keeps_history_clean` | `0 passed; 1 failed`；`下一轮请求不得包含空 assistant 消息：[""]` | `/tmp/peri-empty-text-fail-before.log` |
| bridge 恢复无条件 source_message | `cargo test -p peri-agent --lib test_bridge_stream_interruption_preserves_only_partial_text` | `0 passed; 1 failed`；`空正文不得制造 assistant 消息` | `/tmp/peri-empty-text-fail-before-bridge.log` |

恢复后的上述用例均在最终全 crate 门禁中通过。模型测试同时锁定只请求一次、transport 类别改写、protocol 类别保留；桥接覆盖文本/空白/空串（空串不产生 source_message）、消息身份、半截工具、思考不回灌和无重复 chunk；loop 覆盖多种事件预算、可信 Defer 字段、完整工具只执行一次且不重置预算、取消与总迭代限制、无正文中断的历史洁净度；ACP 覆盖公开文案、诊断、HTTP 分类和拒绝身份字段不泄漏。

### 本地假上游 CLI 端到端（复查补充，2026-09-22）

harness（临时目录 `/tmp/peri-e2e-stream/`，不入库）：`fakesrv.py` 监听 `127.0.0.1:39001` 应答 Anthropic SSE；第 1 次请求按形态吐增量后直接断开（不发 `message_delta` / `message_stop`），其后请求返回完整响应；每次请求体追加到 `requests-<mode>.log`。隔离运行：`HOME=/tmp/peri-e2e-stream/home`（**未触碰真实 `~/.peri/settings.json`**）+ `--settings` 指向临时配置文件。

```bash
HOME=<tmp>/home target/debug/peri -p 'say hi' --settings <tmp>/settings.json \
  --output-format stream-json --model opus
```

| 形态 | 断点 | rc | 请求数 | stdout 关键行 |
| --- | --- | --- | --- | --- |
| `none`（基线，不断流） | — | 0 | 2 | `result … is_error:false` |
| `cut_text` | text 增量后断流 | **0** | 3 | `text:"我先看一下"` → `text:"已完成"` → `result stop_reason:end_turn` |
| `cut_thinking` | 仅 thinking 增量后断流 | **0** | 3 | `text:"已完成"` → `result` |
| `cut_tool` | tool_use 参数吐一半后断流 | **0** | 3 | `text:"已完成"` → `result` |

对照被测行为：报告里同形态在 3.16.5 是 1 次请求 + `rc=1`。现在第 1 次请求**不被重放**——第 2 次请求体已含部分 assistant 消息与 `stream_interrupted` reminder，属续跑；相对基线只多消耗 1 次请求，最终 `stop_reason=end_turn`、`is_error:false`。

请求体不变量（逐次请求解析 `messages`）：

- `cut_text` 第 2 次请求：`user | assistant[thinking, text "我先看一下"] | user[reminder]`（部分正文保留、身份连续）。
- `cut_thinking` / `cut_tool` 第 2 次请求：`user | user[reminder]`——**没有** assistant 占位消息，空正文不制造空消息。
- 三种形态的所有请求中都不存在空 text block；`cut_tool` 的半截 `tool_use` 未进入任何后续请求，也未执行工具。

局限：假上游不校验 payload（无真实 provider 的 400 校验），因此「空 text block 被拒收」这条在本端到端中未被触发，它由编码实测 + API 约束支撑。

### 未验证

- 未访问真实 provider：假上游不校验 payload，因此 provider 侧的接受/拒绝行为未在线验证；thinking 模型的真实恢复形态仍待线上复跑确认。
- 未运行真实 TUI 视觉验证，半截工具卡片的展示/清理没有新增视觉证据；本次保证半截工具不执行、不进入后续模型历史。
- 未修改真实 `~/.peri/settings.json`（端到端全程使用隔离 HOME 与 `--settings`）。
- 简报所引用的完整报告及原始线上产物未取得。

## 遗留后续项

### Bug 2：idle / 读超时待决，不在本次实现

报告 `out-hang/requests.log` 只有 1 次请求；`out-hang/rc.txt` 为 `peri_rc=124`，由外层 timeout 强杀；日志末尾停在 `session_start=true`（03:36:46.569），03:36:46.582 发出请求后无新日志。连接静默没有错误事件，本次 Interrupted 恢复不会触发。

建议另行决定 idle/read timeout 的配置事实源、取消优先级及超时分类，并以隔离 HOME 的静默 SSE 假端点验证：可见输出前按 RetryConfig 重试，可见输出后按本契约续跑。不得在本 issue 中把 idle 超时标为已修复。

### Bug 3：已修复

用户可见错误文案丢重试信息已由 `95b752bd` 修复。本次仅复用 `model_error_facts` 增加恢复耗尽文案，不重做既有 retry 文案契约。
