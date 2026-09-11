# TUI 与 ACP 数据流

> 状态：现行设计
> 范围：`peri-tui` 的 ACP 请求、通知解码、状态归约、交互响应与渲染发布
> 代码定位：[`peri-tui` 代码索引](../code-index/peri-tui.md)

Peri TUI 是 ACP client 前端。用户交互和 Agent 执行都经 ACP transport；TUI 不直接驱动
ReAct loop 或 middleware。Agent 产出 canonical 事件，ACP 负责协议映射，TUI 只把通知归约为
本地视图状态。跨层执行归属以 `ARC-BOUNDARY-001`，事件链完整性以 `ARC-EVENT-001` 为准。

## 分层与所有权

```text
peri-tui                 peri-acp                    peri-agent / middlewares
────────                 ────────                    ─────────────────────────
输入与交互请求 ────────→ ACP client / transport ──→ session 执行入口
组件 ← atoms/ViewModel  ← notification / mapper    ← canonical Agent events
         ↑
  acp_notifier → acp_bridge / BridgeState
```

| 层 | 稳定职责 | 不拥有 |
| --- | --- | --- |
| TUI | 输入、焦点、组件、通知解码、本地状态与渲染 | ReAct、工具策略、middleware 顺序 |
| ACP | session lifecycle、transport、请求转发、事件协议化 | TUI ViewModel、业务执行语义 |
| Agent | session runtime、RCRA loop、canonical 事件与终态 | TUI atom、布局与渲染队列 |
| Middleware | prompt contribution、工具、审批、MCP/plugin 等能力 | ACP/TUI 协议投影 |
| `peri-acp-types` | 跨 crate identity、事件、命令和 DTO | TUI 私有渲染类型 |

TUI 可以在 deployment composition root 复用配置和初始化类型，但 prompt、cancel、session、
interaction response 与详情请求等运行时动作仍必须经 ACP client/transport。

## 核心链路

### 用户提交

```text
InputArea
  → SteerCommand / STEER_TX
  → steer_consumer
  → AcpTuiClient::ensure_session + session/input/enqueue
  → Agent session runtime
```

上述普通输入路径在协商 `peri.userInputQueue` 后启用；未协商时仍经
`SubmitRequest / SUBMIT_TX → submit_consumer → session/prompt`，已注册 slash 命令也保留原路由。
新路径的用户气泡由 canonical `UserInputDelivered` 经 bridge 使用既有渲染器生成；
旧路径仍通过 TUI local event 进入 bridge。组件不直接改写 transcript。
待发送投影、完整取回与运行身份见[用户待发送队列](user-input-queue.md)。
加载、取消、rewind、AskUser 和 permission response 各有独立 consumer/channel，但都经 ACP
执行，不建立旁路。

### Agent 输出

```text
Agent canonical event
  → ACP mapper / session notification
  → AcpTuiClient notification pump
  → acp_notifier（wire DTO → AcpEventData）
  → acp_bridge（session/owner 检查 + 状态归约）
  → VIEW_MODELS / domain atoms
  → components
```

`acp_notifier` 只解码和转发，不写 UI。`BridgeState` 是消息事件到 transcript/atoms 的状态
边界；组件只订阅状态并渲染。新增或变更事件必须同时覆盖 producer、协议类型、ACP mapper、
capability gate（如适用）、TUI decoder、reducer 和终态测试。

### 渲染发布

```text
BridgeState.committed + CurrentTurn
  → TuiRenderUnit projection
  → ViewModelsSnapshot { items, generation }
  → message_area incremental cache
  → wrap/slot index + viewport clipping
  → ratatui render
```

`VIEW_MODELS` 是消息区单一发布面；`BridgeState` 保存 committed 历史与当前回合，终态时将
当前回合归档。流式 mutation 与 ViewModel projection 分离，bridge publication scheduler 可在
固定帧预算内合并连续主 Agent 更新，但首块边界、终态和 reset 必须及时发布。Markdown、wrap
和 viewport 性能契约见 [TUI 流式 Markdown 性能设计](tui-streaming-markdown-performance.md)。

## 状态模型

### Transcript

`TuiRenderUnit` 表达 user、assistant、reasoning/tool、system、SubAgent、group、divider 和
interaction 等可渲染语义；具体枚举与字段以源码为准。动态 inventory 不在本文复制。

`CurrentTurn` 按到达顺序保存 assistant text/reasoning、tool 和 SubAgent segment。新的
message identity 或 tool/SubAgent 边界会冻结前一文本段；chunk 只增长当前段，不为每个 chunk
创建 entry。projection 必须维持 segment 顺序和独立 reasoning slice。

`ViewModelsSnapshot` 使用持久化容器与单调 generation，使组件能够比较 revision 并复用稳定
渲染缓存。只有 bridge 的 canonical publish 路径可以替换该 atom；session reset、rewind 和
replay 也必须通过同一所有权边界。

### UI domain atoms

Transcript 之外的状态按 domain atom 分离，例如 session/loading、context usage、notification、
interaction、panel/popup、background task、plugin 与 workflow snapshot。atom 只是 TUI 投影，
不得成为服务端事实的第二份持久化；刷新、重连和 session transition 后应由 owner-aware
snapshot/event 重建。

组件 render body 不写 atom。所有写入发生在 consumer、bridge event、effect 或明确用户 action
边界；相关约束见 [TUI 规则](../standards/tui.md)。

## 事件语义

| 事件族 | TUI 归约 |
| --- | --- |
| assistant text / thought chunk | 追加到当前 message segment；维持 message 与 source identity |
| tool start/update | 按调用 identity 原地更新 tool activity；历史 replay 走同一语义归约 |
| plan / usage / context metadata | 更新计划或资源投影，不伪装成普通消息 |
| turn terminal / suspended / execution failure | 归档、回滚或保留当前回合，并退出 loading |
| compact / rewind | 显示系统语义或以服务端 snapshot 重建 transcript |
| SubAgent | 按 source/instance 归组并更新生命周期 |
| background task / workflow / plugin | 更新独立 atom、通知或管理面板 snapshot |
| AskUser / permission / OAuth | 注册 durable interaction owner，再发布对应交互 surface |

`session/update` 承载标准 ACP 流式内容、工具、计划和 usage；Peri 扩展只用于 ACP 标准面不能
表达的低频语义。扩展不得建立第二条语义不一致的 tool/turn 终态通道。

Root cache coverage 只使用当前 root turn 最近一次有效 observation；auxiliary agent usage 不得
覆盖父 turn。missing、zero 或不一致 observation 要显式清除旧样本。低覆盖提示在回合终态前
至多生成一次，不能由每个 model step 重复告警。

## 回合与 session 生命周期

Bridge 的 session phase 至少区分 idle、prompt running 与 history replay；渲染 variant 不等于
执行 phase。正常终态、取消、中断、挂起和执行失败必须有明确处理：

- 正常结束：归档 `CurrentTurn`，停止 loading，再按队列策略继续；
- 中断：按 request/session identity 拒绝 stale terminal；有输出时保留可审计内容，无输出时
  才恢复对应输入，不影响更新一代提交；
- 挂起：归档可见内容并停止 loading，但不把仍存活 Agent 误判为完成，也不提前 drain 输入；
- 失败：生成可见错误语义，清理与本回合绑定的 transient state；
- reset/load/rewind：递增 generation，失效旧 session 的 pending publication、interaction、
  selection、focus 与 hover 状态。

普通 session load 在 producer 入队边界取得 reservation，并持有到 load 提交或放弃；prompt
入口在选择 stable session 前等待 reservation 清零。该线性化要求防止延迟 load 替换首个 prompt
所需 session，详见 `ARC-SESSION-LOAD-001`。

Turn terminal 是唯一结束信号。每条终止路径都必须离开 loading；重复或 stale terminal 幂等，
不得删除新一代 turn 的内容。tool、SubAgent 和 interaction 的未终结状态应在对应 terminal
语义下收敛，而不是永久 spinner 或默认标记成功。

## Durable interaction

AskUser 和 permission request 走标准 ACP 交互通道。notification pump 在 forward 前从 request
提取非空 session identity，并由 interaction lifecycle 分配 semantic owner；bridge 发布前再次
验证 owner、session 和 projection。invalid、stale、已删除 session 或 transport terminal 必须
形成 typed cancel/terminal，而不是留下可提交 UI。

用户响应、cancel、session transition、prompt terminal 和 transport EOF 竞争同一 owner；每个
request 最多一次 response attempt 和一次 owner-aware local terminal。response 失败时保留可重试
surface；成功后 compare-and-clear exact owner，不能按相似 payload 或最新 popup 猜测。

Panel/Popup/Inline block 是同一 durable interaction 的不同投影，不是不同请求。headless client
可以 claim/respond，但不得触碰 TUI atom。审批的交互安全与目标 Workbench 见
[TUI Chat Workbench](tui-chat-workbench.md)。

## 焦点、后台与管理面

Panel、Popup、transcript 和 composer 的 focus/事件优先级由 TUI router 管理。组件只执行已有
semantic action；请求和副作用进入对应 consumer。Modal 处于前景时背景 inert；非模态 panel
是否允许 transcript 操作由区域 owner 决定。

SubAgent、后台 task、Cron、Workflow 和 plugin 数据不应混入 `CurrentTurn` 的文本语义：

- 与主时间轴有稳定 identity 的 Agent/tool activity 可投影为 transcript entry；
- service snapshot 或列表状态进入独立 atom/panel；
- 短暂完成提示进入 notification/background display；
- 需要长期审计的结果由 transcript 或服务端持久化提供，不能依赖会自动消失的 toast；
- polling 与 event 若同时存在，必须声明单一权威 reducer，避免两套状态互相覆盖。

## 稳定不变量

1. **ACP 边界**：TUI 不直接驱动 Agent runtime；所有运行时请求经 ACP。
2. **单一归约入口**：notification 先解码，再由 bridge 验证 session/owner 并写状态。
3. **单一 transcript 发布面**：消息区只消费 `VIEW_MODELS`；组件不旁路追加消息。
4. **身份优先**：session、request、message、tool 和 source identity 用于去重与 stale filtering；
   arrival order 或显示标题不是身份。
5. **终态完整**：完成、失败、取消、挂起和 transport terminal 均清理 loading 与 transient owner。
6. **live/replay 等价**：同一语义经相同 reducer 收敛；重放不恢复失效的可执行 interaction。
7. **前端隔离**：Agent/ACP 不依赖 `TuiRenderUnit`、atom、layout 或渲染缓存。
8. **安全显示**：协议 payload 按不可信输入处理；不显示 secret，不把任意结构化数据直接 dump
   到 transcript。
9. **增量渲染**：流式更新只失效变化 slot；viewport 外内容不做无界重复 wrap/clone。
10. **可验证变更**：跨层事件修改同步更新协议、mapper、TUI decoder/reducer、caps 和测试。

## 事实源

- TUI 入口与符号：[`docs/code-index/peri-tui.md`](../code-index/peri-tui.md)
- ACP host/session/event：[`docs/code-index/peri-acp.md`](../code-index/peri-acp.md)
- Agent loop/session：[`docs/code-index/peri-agent.md`](../code-index/peri-agent.md)
- ACP wire：[peri-acp-protocol.md](peri-acp-protocol.md)
- transcript 持久化：[message-transcript.md](message-transcript.md)
- TUI 工具与交互目标：[tui-chat-workbench.md](tui-chat-workbench.md)
- SubAgent 目标展示：[tui-subagent-activity.md](tui-subagent-activity.md)
- 跨模块契约：[architecture-contracts.md](../standards/architecture-contracts.md)
