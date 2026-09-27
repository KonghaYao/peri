# 对抗验证：TUI streaming publication 方案

本报告针对 2026-09-27 的 [issue 方案稿](../../spec/issues/2026-09-27-p0-tui-streaming-view-rebuild-cpu.md) S1–S4，以及审查开始时的生产源码。对照设计为 [现行性能设计](../design/tui-streaming-markdown-performance.md)。审查期间用户另外授权主 agent 实施修复；以下源码观察均描述修复前状态，不是修复后验收，也不推断并行修改已经解决这些问题。

**最新 disposition**：末节收尾审查为 **CONCLUSION_STANDS（限定为已收窄的机制与修复方向）**，未发现新的必须修复逻辑缺陷。先前攻击及首版问题的处理见末节；正式测试、lint 和现场性能证据由主 agent 的最终执行结果提供，本报告不替代它们。

## 裁定与证据

**CONCLUSION_WEAKENED**：三条缺陷机制成立，独立 publication 状态、主/子流统一调度和稳定历史折叠复用的方向成立；方案稿尚不足以直接作为完整实现契约。最强反例是折叠投影仍读墙钟，导致推荐缓存键缺少实质输入；其次是 UI 折叠与同步 session reset 对单一发布所有权的挑战。现场 CPU 的精确归因比例与修复收益仍未证实。

审查只写本报告，没有修改或挂载生产/测试源码。独立执行已有诊断二进制：

```text
target/debug/deps/peri_tui-a1f73702302d4cd8 cpu_review_ --nocapture --test-threads=1
exit 0; 3 passed; 0 failed; 1679 filtered out
```

命中 `cpu_review_main_chunk_projection_consumes_dirty`、`cpu_review_replay_fold_repeated_writes`、`cpu_review_subagent_bypasses_scheduler`。阅读了 `/tmp/peri-cpu-review-tests.rs`，确认它们断言当前缺陷，而非修复效果；既有二进制不是最终修复源码的构建证明。本轮未新增或执行下文提出的新反例测试，未运行 release benchmark 或重新现场采样。

## 攻击 1：历史折叠并非只依赖 revision、phase 与 override

- **目标论断**：S3 在 committed revision、phase、override revision 不变时，可复用历史折叠结果；失效后从 canonical 重建应与原 pass 等价。
- **具体反例 / 代码证据**：`acp_events/mod.rs::flush_current_turn` 在归档前调用 `CurrentTurn::deactivate`，后者只改 active 与 cache dirty，不冻结顶层 trailing bubble。`current_turn/projection.rs::sync_trailing` 的同长度缓存可继续保留 `started_at`；归档后的 canonical committed 因而仍可能带运行中 reasoning 和时钟起点。`render.rs::apply_fold_pass` 在非 PromptRunning phase 使用 `started_at.elapsed()` 冻结正文/推理，且只写临时快照。时间 t1 的终态缓存与 t2 的 override 失效重建将得到不同 duration；下个 PromptRunning phase 又可能从带 Running 的 canonical 恢复旧 reasoning 状态。缓存加入时钟会破坏稳定历史复用，忽略时钟则不满足逐次旧 pass 等价。
- **严重程度**：严重，影响最终用户可见时长、动画及缓存等价 oracle。
- **建议收敛**：把生命周期冻结与视觉 fold 分开。归档/终止时一次性形成稳定正文、reasoning duration/status；明确 LocalLoadingReset、挂起及仍运行 child 的行为。不要将旧 pass 对墙钟重复读取当作必须保持的正确行为，也不要将用户 override 回写 canonical。可以固定冻结投影，但必须说明失效重建如何保留该冻结事实。
- **可证伪验证**：真实 dispatch 链路输入 reasoning/text → TurnDone/Interrupted/Suspended，捕获 terminal duration/status；推进受控时间，添加/删除 override，启动下一轮并再次发布，旧历史完整字段须不变。覆盖尚有 running child 导致 flush 被跳过的分支。相同瞬时时间比较 cached/uncached 不足以排除此攻击，必须跨时间和 phase。

## 攻击 2：单一发布所有权与同步 reset / UI 写入存在实际冲突

- **目标论断**：S1 bridge 持有 revision，统一函数成功写 VIEW_MODELS 后结算；折叠 UI 与 reset 接入同一边界即可。
- **具体反例 / 代码证据**：`message_area/entry_nav.rs::apply_fold_toggle` 同步改 `FOLD_OVERRIDES`、当前 snapshot 条目和 snapshot.generation；bridge 自己另持 generation。`session_boundary.rs::project_session_boundary` 则要求在 lifecycle operation gate 内同步清空 UI，并立即调用 `push_view_models_for_reset`。若折叠只变 atom、不发 bridge intent，idle 无新 ACP 事件时不会消费 revision；若把旧 slot 延迟排队，reset 后同 slot/同 FoldKey 可属于新 session。仅在 publication 前读一次 reset counter，无法排除“检查后、写快照前发生 reset”的交错。
- **严重程度**：严重，可丢交互或重新显示旧 session；影响所有权不变量。
- **建议收敛**：明确同步 reset 是受同一所有权协议约束的特殊写入，不能无理由改成异步清空。折叠命令携带 session epoch 与稳定 FoldKey，在最新 canonical 上解析；显式唤醒 bridge，并定义连续快速 toggle 是传目标态还是操作。publication 与 reset 的 ownership 检查/写入须有可解释的线性化点。generation 只有一个常规 writer；reset 的 epoch 与 generation 分开。
- **可证伪验证**：idle 点击无需后续 ACP 事件即生效；点击入队后 reset，再回放相同 ID，旧点击被拒绝；publication 在最后检查与写入之间遇 reset，旧 snapshot 不能复活；两次快速 toggle 的结果确定；同步 session boundary 返回时旧 UI 已清空。应通过真实 UI 命令/bridge 路径验证，不能仅调用 fold pass。

## 攻击 3：Streaming 热切 None 后已有 deadline 仍会发布

- **目标论断**：S2 None 不因 chunk 产生中间 publication，各模式共用 scheduler 且保持既有可见性。
- **具体反例 / 代码证据**：`panels/config.rs` 的 ROW_STREAMING 直接修改 `TUI_CONFIG_HANDLE` 并保存配置，没有通知 scheduler。`PublicationScheduler::fire_at` 到期无条件 `push_view_models`。在 S1 修复使 Deferred 生效后，Streaming 接收 chunk 建立 deadline，切 None 后再收 chunk，到期会发布后者。反方向 None → Streaming 也不等同于某类 canonical 文本为空：None 中可能已积累大量文本却从未可见。
- **严重程度**：严重，模式行为在修复后才真正暴露。
- **建议收敛**：定义配置变化是否是 publication policy epoch；切入 None 取消普通流式 pending，保留 canonical 未发布事实以供合法 barrier/终态；切入 Streaming 明确是否立即显示已有积累。不要不加区分地取消 replay 的 Deferred。None 必须解释为“不由 chunk 触发”，若工具/交互/终态 barrier 允许携带此前文本，要明确写出。
- **可证伪验证**：固定时钟，Streaming 建 pending → None → 新 chunk → 原 deadline；分别断言普通 publication 为零、canonical 完整、terminal 最终文本完整。再测 None → Streaming、Streaming → Block，以及同一 pending 中含 replay/流式输入时的策略。

## 攻击 4：首块、边界与模式的优先级仍欠定义

- **目标论断**：S2 “首个可见 text/reasoning 块”为 Immediate，身份按 occurrence/message 区分。
- **具体反例 / 代码证据**：现行设计主 Block 仅在 Markdown 边界发布，None 跳过中间发布，方案的无条件首块表述可被实现为两者也立即发布。`current_turn/subagents.rs::append_subagent_text/reasoning` 没接收 chunk.message_id，子流内部不能直接沿用主消息身份判定；`start_subagent` 对同一 active occurrence 重复 start 幂等，停止后同 ID resume 则新 occurrence。空 chunk 仍可进入主 append 并改 active/dirty，但不必对应可见块。
- **严重程度**：中等，容易让频率验收假通过或交互策略被静默改变。
- **建议收敛**：逐模式定义首可见资格，尤其主 Block 保留边界规则、None 不因首块触发；资格按 occurrence、text/reasoning 类别及可靠 message/segment 身份维护，缺 ID 有明确 fallback。空 chunk 不消耗首可见资格。BG 无组不应凭 source ID 创建主 transcript 的首块。
- **可证伪验证**：一个 child 持续 100 chunk，另一 child 中途首块；同 ID stop/resume；同 occurrence reasoning 后 text；工具边界后新段；缺/变化 message ID；空 chunk 后非空 chunk。按原因断言首块次数，并比较完整顺序。不能仅用 generation 增长数量代替首块分类。

## 攻击 5：revision 正确仍可能丢失 handler 副作用或终结语义

- **目标论断**：把 handler 直接发布移到 bridge 后，可维持现有 terminal/archive/交互行为。
- **具体反例 / 代码证据**：`turn.rs::handle_turn_done` 先发布状态，再触发 compact replay / drain_input_buffer；中断不同分支的 drain 顺序并不相同。`BridgeState::inject_system_note` 自带发布及 ACP_STATE 更新。`flush_current_turn` 遇 running child 会拒绝清空，而 TurnSuspended 直接归档/reset；`handle_loading_reset` 只切 phase。`flush_on_receiver_close` 只是最终投影，并不生成 terminal/deactivate，也不自动清 loading。把“receiver close 最终 flush”扩大为“所有 close 都退出 loading”是新增生命周期语义，不能由 revision 顺便实现。
- **严重程度**：严重，可能错误重排提交、归档或取消。
- **建议收敛**：列出每个 handler 的 canonical mutation、atom 副作用、后续发送和 publication barrier 顺序。统一发布时允许把后续副作用放到 publication 后执行，但不删除。receiver close 明确保留原语义还是升级为传输断开状态；shutdown 保持无 UI 写入。child stop 不消费整个 session 的 terminal ownership。
- **可证伪验证**：正常 terminal、零输出取消、stale cancel、挂起后 BG chunk、compact 后 replay、receiver close 与 reset 交错均经真实 bridge 运行。断言 INPUT_BUFFER 和 request 配对、note 恰一次、phase/loading、最终消息顺序和发布次数；不要用“没有 pending”替代这些断言。

## 攻击 6：override 删除不能只恢复 fold 而遗留 user_modified

- **目标论断**：S3 覆盖添加/移除后正确失效并恢复默认；历史缓存可重复利用。
- **具体反例 / 代码证据**：`render.rs::apply_fold_pass` 对 Tool/SubAgent/Interaction 使用 `override_fold.is_some() || vm.user_modified`。UI `apply_fold_override` 会把该标志设 true。若实现为了少重建而在上次 folded 缓存上删除 override，fold 虽回默认，user_modified 仍 true；分组免疫等行为可能继续保留。当前 canonical 与 UI snapshot 分离时此反例不必发生，因此这是约束新缓存实现的具体陷阱，不声称现行所有删除路径都有此 bug。
- **严重程度**：中等。
- **建议收敛**：失效重建从干净 canonical 开始，或显式恢复全部派生字段；不要把 folded 输出当下一次默认输入。明确“折叠成默认态”与“删除用户覆盖”是否两个动作；Group 与成员 Tool 的 key 不能互换。
- **可证伪验证**：两张可分组成功工具，展开其中一张后移除 override，检查 fold、user_modified、content_hash 和最终分组全部恢复；同样覆盖 SubAgent/Interaction，reset 后复用相同 ID。只断言 FoldPassWrites=0 无法证明这一点。

## 攻击 7：无通知记账安全，但 effect 触发条件不能靠 incidental render

- **目标论断**：S4 仅减少哨兵无变化通知，submit/reset/resize/anchor 等可见行为不变。
- **具体反例 / 代码证据**：`run_auto_follow` 的 prev_total/prev_vis/prev_loading_epoch/prev_reset_counter/prev_items 等只是内部记账，改无通知有依据；scroll offset 与 follow_bottom 则被视图消费。`message_area/mod.rs` 虽订阅 LOADING_EPOCH、BRIDGE_RESET_COUNTER，use_effect 依赖元组只有 items_len、generation、loading、rows、height；若只有 epoch 改变而元组值相等，重新 render 不保证重新运行 effect。依赖此前的额外 wake 更不能保证它运行。
- **严重程度**：中等，不能以少了通知推导滚动行为完整。
- **建议收敛**：显式加入 effect 真正消费的生命周期依赖；内部记账无通知，实际视觉改变需通知。不要全局把 effect 的 write 改为 write_no_update。
- **可证伪验证**：原地相同快照/几何重复调用时无额外 wake；仅 loading/reset epoch 改变也消费哨兵；follow false 时新输出不抢滚动，真实 submit 恢复 follow，resize 与 interaction anchor 后实际 offset 变化产生通知。测试通知行为与纯滚动状态，渲染外观按仓库规范实测，不能只测试返回目标 offset。

## 攻击 8：统一 VIEW_MODELS cadence 不覆盖后台详情热路径

- **目标论断**：主/子统一调度解除逐事件 UI 放大，对 BG 同样改善整体成本。
- **具体反例 / 代码证据**：`streaming.rs` 在 BG 有组/无组两条路径均调用 `append_bg_text_chunk` / `append_bg_reasoning_chunk`，不受 None 限制。`bg_task_live.rs` 每次写 BG_LIVE_DETAIL，clone 当前 bubble、追加、全文 recompute_hash，再替换 nested_units；有组还同时维护 CurrentTurn。S1/S2 控制主 transcript publication，并不自动移除这条逐 chunk 的内容工作和 atom 通知。
- **严重程度**：中等，削弱全局 CPU 改善外推；不推翻针对 replay×subagent transcript 的机制。
- **建议收敛**：保留后台详情副作用，在报告中把它列为独立剩余成本；若测量显示主导再独立优化，不能为满足 20Hz 计数而丢 BG 内容。Deferred 次数上限仅适用于 scheduler 管理的 VIEW_MODELS 发布。
- **可证伪验证**：分别对 sync child、BG 有组、BG 无组测 VIEW_MODELS publication、BG_LIVE_DETAIL 更新和 hash 字节；固定输出总长度变化 chunk 大小，观察 BG 成本是否仍随事件率放大。打开/关闭后台详情 pane 分开记录可见渲染量。

## 攻击 9：成本计数与现场归因仍须保持有限结论

- **目标论断**：当前诊断与 S3 零历史折叠工作足以解释并解决现场 CPU。
- **具体反例 / 代码证据**：三诊断只证明合成输入上的 fold 写入、子流直推与主流 dirty 消费，未测 release 收益。已有 `FoldPassWrites` 不等于访问数，也不等于全文 hash 字节；稳定历史零写可仍全量扫描。当前计数器 publication reason 只按 phase 区分 Intermediate/Terminal，不能直接分离首块/边界/Deferred。分组 first_divergence、todo 定位、message_area prefix index 仍全量工作，BG 另有逐内容处理。
- **严重程度**：轻微至中等；方案稿已诚实收窄多数承诺，本攻击未推翻根因方向。
- **建议收敛**：正式验收新增真正的历史 fold visits、hash 调用/字节和 barrier reason 计数，区分 committed 与 live turn，预热后再测稳定区间；继续保留剩余 O(N) 声明。不用 `N=1000` 与 `N=100` 的耗时倍数证明 O(1)，不将基于旧源码的三测试通过写成修复通过。
- **可证伪验证**：回放/实时历史、override 稳态/失效、不同输出体积各自测量；真实 bridge 的时间推进与输出等价测试通过后，执行同类 release profile 与现场负载复测，按进程分别计数。原现场失配条目数和 replay 占比无法仅从调用栈样本反推。

## 最终评估

| 范围 | 裁定 |
| --- | --- |
| replay 终态 fold 残留导致重复历史变换 | 成立；已独立重跑诊断 |
| 子流 handler 直推绕过后置 scheduler | 成立；已独立重跑诊断 |
| 主流 projection 提前消耗 publication dirty | 成立；已独立重跑诊断 |
| S1–S4 总体方向 | 保留；需补齐时间、所有权、模式及副作用契约 |
| S3 推荐缓存键直接足够 | 被具体时长反例削弱 |
| 全 UI 20Hz / 全流程 O(增量) | 不成立，方案稿也不应作此承诺 |
| 修复正确性与 CPU 收益 | 本报告未验证 |

最终裁定为 **CONCLUSION_WEAKENED**。应将上述高风险反例收进实施契约及正式回归，随后针对最终源码重新验收；不得把本次对方案稿的审查当作修复完成证明。任务关闭时按 DOC-HISTORY-001 将稳定结论吸收进设计/代码索引并清理本过程报告。

## 修复后审查：首版实现的只读复核

此节针对主 agent 邀请复核时的首版工作区 diff。该版本已新增 `acp_events/fold.rs`，引入 `PublicationIntent::{Published,Streaming,Hidden}`、scheduler.unpublished、`deactivate → freeze_trailing`，正式测试仍在补充。审查未改生产代码，也未把此前二进制的三诊断结果用于证明此版修复。本节列出的源码模式用于界定审查版本；后续修改消除该模式后，应依据最终测试更新结论。

### R1：主 Agent Block 首块直接发布，偏离保留的契约

- **目标论断**：主 Block 继续仅在 Markdown 边界发布。
- **代码证据 / 具体轨迹**：`streaming.rs::stream_intent` 主 Block 分支使用 `if first || has_md_block_boundary_since(text, *pushed)`。空主 turn 收到普通 `"a"`，`starts_stream_block` 返回 true，结果 Immediate，而该文本没有 Markdown boundary。此前方案只建议改变子流 cadence，并未改变主 Block 首块策略。
- **严重程度**：中等，确定的策略偏差；不是推测性竞态。
- **建议收敛**：删除主 Block 的 first 例外，或取得并记录明确的主 Block 契约变更。子 Block 首块规则与主 Block 分开。
- **可证伪验证**：真实 dispatcher + scheduler，Block 主 text/reasoning 首个普通字符均不产生 publication，随后边界立即发布完整缓冲；Streaming 同输入首块立即显示，子 Block 按明确新策略验收。本审查没有执行此新增用例。

### R2：LocalLoadingReset 没有结算已建立的 deadline

- **目标论断**：取消与本地 loading 复位后，旧 pending 不会再次发布旧流状态。
- **代码证据 / 具体轨迹**：`turn.rs::handle_loading_reset` 仍只改 phase=Idle 并调用 push_acp_state，没有 `publish_barrier` 或独立 intent。dispatcher 因此返回 None，scheduler.accept 不清理同模式 pending。Streaming 首块已发布 → 第二块建立 pending → LocalLoadingReset → 原 deadline 到期，`fire_at` 仍会发布该 buffered 内容。LocalLoadingReset 用于 cancel、clear、prompt 失败兜底，不能假定随后一定有 TurnInterrupted 替它结算。
- **严重程度**：严重，属于已恢复正常的 scheduler 暴露出的生命周期缺口。
- **建议收敛**：明确本地 loading reset 是最终视觉 barrier 还是只隐藏 loading；若承担取消兜底，则显式结算 pending 并稳定该结束状态，保留它与归档/清除的区别。不能仅靠 current_turn.cache_dirty，因为任意 projection 读取可清掉它。
- **可证伪验证**：按上述事件轨迹推进固定时钟；断言该复位时的合法最终 snapshot、loading=false、旧 deadline 不再额外发布。再覆盖重复 LocalLoadingReset、随后仍有 child/BG 事件，以及真正 terminal 后再次 reset。本审查没有执行此新增用例。

### R3：模式恢复只在后续 chunk 被观察，没有配置事件唤醒

- **目标论断**：模式热切换之后，对已接收未发布内容的处理确定。
- **代码证据 / 具体轨迹**：scheduler.fire_at 在 pending_mode 与当前配置不符时清 deadline 并保留 unpublished。Streaming pending → 切 None → deadline 到期被取消 → 切回 Streaming → 不再收到 chunk，此时 unpublished=true 且没有任何 pending。配置面板仍直接改配置，未唤醒该状态机，因此缓冲会一直等后续事件或 receiver close。`last_streaming_mode` 的 None→可见恢复分支仅由下一条 Streaming intent 触发；如果 None 期间没有 chunk，也未必记录过 None。
- **严重程度**：中等，取决于批准的模式恢复语义；不把它误报为所有 Streaming 停流都挂起。
- **建议收敛**：二选一明确写入设计：配置变化主动通知 scheduler 并处理已积累内容，或模式仅影响后续事件，并明确“切回模式本身不会 flush”。如果保留后一选择，不能宣称立即恢复此前不可见内容。
- **可证伪验证**：独立覆盖有/无新 chunk 的两种模式切换轨迹，读取 publication、pending 与 canonical 三者，不只断言模式值。

### R4：已排除的首轮高风险与仍需最终证据的边界

- **普通终态冻结**：`deactivate` 现在调用 freeze_trailing，后者保留已有 trailing_reasoning_frozen_ms；时钟起点清空后重复 deactivate 不再次覆盖冻结标记。结合 sync_trailing 的 pending_freeze 路径，源码上已消除普通归档后重算历史时长的首轮反例。仍需正式跨时间测试证明，不能用本次阅读替代。
- **历史结构失效**：核对本地 im 15.1.0 的 ptr_eq：Full 检查前后 chunks 与 middle 的共享身份。FoldedHistory 持有源引用，原地更改触发 COW；phase/override 值也参与失效。没有发现同长度 replacement 被该键静默漏过的具体路径。inline 分支值比较是明确例外，若非空 inline 可携大内容，其成本应单独承认。
- **terminal 副作用**：handler 将原同步 push 原位替换为 publish_barrier，再由 Published 告知 scheduler 结算，保留了原先 publish/drain/replay 的顺序。这个收敛解决了简单外提发布的重排风险。
- **UI 同步写入**：首版明确保留同步 fold/reset，并未实现首轮建议的异步命令迁移，因此不能再以异步 slot 污染作为本版新增 bug。原本跨线程 reset 与 publication 的最后检查/写入原子性，及 UI/bridge generation 双 writer 仍需单独实证；本轮未找到新证据证明这两个既有风险已经发生。
- **auto-follow**：内部哨兵改 write_no_update，实际 scroll/follow 保留通知，并把 loading/reset epoch 加入 effect 依赖。源码上回应了首轮遗漏，没有发现新增可见更新丢失路径；最终通知和滚动行为尚须验证。
- **计时口径**：历史 `folded_history.project` 位于 StageAssembleNs 区间，StageFoldNs 现在只含 live fold。若沿用旧字段名做前后耗时比较，必须解释该归属变化；不能把新 history cold fold 误认为 assemble 本身增长。

首版只读审查结论：**CONCLUSION_WEAKENED，等待 R1/R2 收敛和最终测试**。已发现的两项具体偏差不推翻整体修复方向；本节不构成正式测试、release 性能或现场体验通过证明。

## 收尾对抗审查与最终 disposition

再次阅读了最新 TUI 生产 diff、`publication_test.rs` 及其真实模块挂载入口。遵照主 agent 要求，没有编译、没有启动测试，没有修改生产代码或其他文档。workspace 同时存在非本任务 ACP/Agent 修改，本审查不对那些改动作结论。

### 已处理与判断更正

1. **更正 R1 的初始反例**：此前只检查 `stream_intent`，漏看既有 `has_md_block_boundary_since` 明确规定 `since_chars == 0` 返回 true。因此“主 Block 首个普通字符发布是新增偏差”的论断不准确，撤回这一部分。当前去掉 `first ||` 的实际作用是防止后续 message/segment 首块额外绕过原 boundary helper，保留了现有首次推送与增量边界规则。`test_publication_block_and_empty_chunks_respect_boundaries` 中首次普通文本后 generation=1 与既有规则一致。文档不能把 Block 简化成“首次字符绝不发布”。
2. **R2 已在代码中处理**：`handle_loading_reset` 在 PromptRunning→Idle 时调用同步 publish_barrier；Published 让 scheduler 清除 unpublished 与 deadline。`push_view_models` 在非 PromptRunning 时先 freeze_trailing，避免只有临时快照冻结。对应回归通过真实 dispatch→scheduler 路径断言已收推理完整、loading=false、pending 清理、时钟推进无重复发布、重复复位幂等。这里确认的是测试源码覆盖，未声称测试已运行通过。
3. **R3 已明确收窄**：模式由 chunk/deadline 读取；只改配置且无后续事件不保证立即发布。None→Streaming 在后续 chunk 恢复；主 Block 仍遵循原边界函数，不能笼统说“切入任意可见模式，下个 chunk 总会立即显示”。deadline 模式检查取消纯流式 pending，replay Deferred 不受 None 抑制。该选择不再视为未解决代码缺陷。
4. **历史冻结与计时已落实**：deactivate 冻结顶层 trailing，非 PromptRunning publication 冻结尚未归档的顶层 trailing；原空 reasoning placeholder 在终态保留 Completed 形态，重算 hash。历史 project 已计入 StageFoldNs。正式回归源码断言终态 canonical 无 started_at、reasoning 非运行态，并跨 phase/override/cold rebuild 比较完整条目。
5. **S1 所有权方案已合理缩小**：handler 以 Published 显式确认同步 barrier，保留原 drain/replay 副作用顺序；UI fold/reset 维持原同步路径。此次修复不声称实现了全面异步 UI 命令所有权迁移，也不宣称消除所有既有 session 并发竞态。

### 最终代码与测试源码观察

未发现可据当前源码列为新增阻塞项的反例。独立 pending 标记不再被显式 projection 读取消费；稳定 committed 的结构 identity 与 override 值校验有明确失效路径；冷投影仍从 canonical 重建，覆盖状态不会反向污染基础卡片。新增正式测试由 `acp_bridge.rs` 挂载，覆盖主/子 text/reasoning 合帧、None 切换、replay、close、loading reset、Block、两子流与 resume、BG 无组、100/1000 历史稳态访问与 hash 操作计数、同长度工具更新/缩短以及归档时长字段稳定。

这些测试主要验证真实 handler→scheduler 的同步链路；不能据此宣称真实 async loop 所有 session/reset 交错、全部模式组合或终端体验已穷尽。仍需主 agent 以最终退出结果确认完整 suite；审查不重复运行以免与正在执行的全套验证争用资源。

**最终裁定：CONCLUSION_STANDS。** 此裁定限于三条机制已证实、修复按收窄契约正确处理对应发布/折叠路径、当前只读复核未发现新增阻塞逻辑缺陷。它不证明全 TUI 达到 20Hz 上限、不证明全部 publication 为 O(增量)，也不证明 release 或现场 CPU 已降低到某个水平。BG_LIVE_DETAIL 的逐 chunk 工作、全 slots/分组扫描和最终测试运行结果仍应在交付中如实区分。
