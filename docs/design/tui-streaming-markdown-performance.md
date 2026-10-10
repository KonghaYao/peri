# TUI 流式 Markdown 性能设计

> 状态：现行设计

## Scope

本文定义 `peri-tui` 从 ACP 流式事件到消息区 Markdown 渲染的性能与正确性边界。ACP transport、Agent runtime 和 Markdown library 本身不在此设计范围。

## 数据接收与视觉发布

ACP chunk 必须立即、完整、有序地写入 `BridgeState::current_turn` 的 canonical state。视觉 publication 是派生行为，由 bridge-local scheduler 控制，不能反向限制接收。

默认 `Streaming` 模式采用单 pending、固定 50 ms cadence：主 Agent 与各子流 occurrence/segment 的首个非空 text/reasoning chunk 立即发布，后续 chunk 共用已有 deadline，新的 chunk 不延后该 deadline。主 Agent `Block` 沿用 Markdown boundary 判定（含初始首块），子流 `Block` 不检测 Markdown boundary，按相同 50 ms cadence 合帧。`None` 不因 chunk 发布中间内容。首块、工具/消息边界、交互和终态是即时 barrier，因此 20/s 不是总发布次数上限。

Scheduler 与 `BridgeState` 由同一 bridge task 持有。reset、session transition、terminal、receiver close 和 shutdown 都必须使 pending deadline 失效；receiver close 可发布已接收但尚未投影的最终 canonical state，shutdown 不再写 UI。publication 前必须完成 session/reset 所有权检查，旧 deadline 不得覆盖新 session 或 terminal snapshot。

Tool 与 SubAgent boundary 保持消息顺序和既有可见性。需要在 drain/replay 等副作用之前发布的 handler 使用同步 `publish_barrier` 并显式返回 `Published`；scheduler 结算 pending，不重复发布。同步 session/reset 与 UI 折叠路径保留原有 owner 检查和顺序，不迁移成异步 bridge 请求。

发布状态独立于 projection cache dirty：明确 projection 读取不能吞掉待发布事实；`push_acp_state` 的条目计数不物化 VM。scheduler 区分 replay 的无条件 pending 与受模式约束的 streaming pending。每次 chunk 重读模式，deadline 执行也校验模式：切到 `None` 取消流式 pending，保留 canonical 积累；切回 `Streaming` 在下个非空 chunk 恢复，主 `Block` 仍等既有边界。单独改配置而无新事件不保证即时 publication；replay、terminal、receiver close 等 barrier 不受 `None` 抑制。

## Lazy ViewModel projection

`CurrentTurn` mutation 只更新 canonical state、rolling hash 和 dirty 标记。Owned ViewModel projection 只在真实 publication、tool/SubAgent/message freeze、terminal correctness barrier 或明确读取时同步。单次 publication 只取得一次 current-turn projection并复用。

Terminal handler 保持各事件既有 archive、reset、note、hash、segment order 和 loading 退出语义。合帧不得伪造 terminal，也不得让旧 pending publication 在 terminal 后再次出现。

## 稳定历史折叠

Replay 工具终态使用 `fold_for_status` 生成基础 fold。`BridgeState::folded_history` 以 committed 结构身份、phase 和折叠覆盖表缓存派生历史；持有 canonical 共享节点使同长度修改也能通过 COW 身份变化失效。im 小向量无共享指针时使用固定容量分支的值比较。覆盖表独立于 canonical，添加/移除覆盖从 canonical 重建，避免残留 `user_modified`。

主回合归档、deactivate 或离开 PromptRunning 时先冻结 canonical 的 trailing 时长，重复冷重建不能重新读取已结束计时器。稳定历史不再遍历折叠或格式化/哈希工具正文；active turn 继续按当前状态折叠。后续 todo/group 与消息区全 slots 扫描仍可能为 O(N)，BG_LIVE_DETAIL 独立的逐 chunk 明细更新不在本 scheduler 范围。

Auto-follow effect 将纯内部记账用无通知写入，真实滚动/follow 变化仍通知；依赖包含 loading epoch 与 bridge reset，不能只靠 generation 变化。

## 增量 Markdown 与最终 oracle

流式 Assistant bubble 使用既有 `MarkdownRenderCache` 的保守稳定前缀：只冻结明确闭合且不含高风险结构的 block，保存 immutable rendered chunks；table、image/reference-like、list-like 与未配对 fence 保留在 mutable tail。追加时只 parse/materialize mutable tail 和新稳定区域。

`VmCacheSlot` 复用稳定 chunk 的 `Line` 与 slot-local wrap map，并以稳定 chunk identity 和确定 offset 跳过不变 chunk 的 materialize/wrap。slot 内保留轻量 placeholder 以维持既有 logical index，`SlotLines` 以 chrome/tail slice 与 stable chunk `Arc` 组合，viewport/selection 按 part offset 取行，不再把稳定 `Line` 克隆回 contiguous slot。宽度或主题改变时允许重做 conversion/layout，但复用 retained parsed blocks；source replacement 完整失效。

冻结或 terminal bubble 必须通过完整输入 parse/render 形成 correctness barrier。最终结果应与相同完整输入的一次性 parse 在结构上等价；不得以中间帧优化牺牲 table、image、fence、list、CJK、长无空格行或 syntax highlighting fidelity。

## Transcript slot index

消息区维护持久 logical/visual 树索引；publication 提供带前后 generation 的变化后缀提示，
连续版本只更新变化 slot。版本不连续、reset、历史结构变化或全局布局失效允许完整重建，
不能把不可靠提示用于跳过正确性失效。无变化帧复用历史索引，活动动画仅访问可见集合；
本地折叠与 bridge 发布在同一 VIEW_MODELS 写锁内递增已发布版本；bridge 不得仅递增
私有计数而与本地版本碰撞。构建期间前驱发生变化时，变化提示不匹配则完整失效。
Global visual 或 logical 坐标先定位 slot，再查询 slot-local 行高映射。
production 不构建 transcript-wide aggregate wrap map。

Selection、semantic copy、hover/focus、entry click、scroll、anchor、auto-follow、viewport、footer 与 image 坐标均消费同一 slot index。计数使用 `usize` 和饱和边界；文本映射保持 Unicode 字符边界与终端显示宽度语义。

轻量索引保留每 logical line 的视觉前缀和不可变 canonical 来源，不强持有可淘汰的 Lines。
旧 frame/handler 保留旧布局版本，冷区选择与复制按 slot 临时重建，完成即释放。
冷加载、全局失效及轻量索引容量仍随历史内容增长，不是整会话 O(1) 内存承诺。

## 共享 entry 渲染与缓存预算

主消息与 SubAgent 详情使用同一 `EntryRenderCache`，以内容、完整 GridSpec、主题 identity、
语言、occurrence 与 surface 管理失效；解析、布局和动画装饰分别更新。
详情以真实视口绘制，完整 `usize` 高度与虚拟滚动目标参与既有面板鼠标仲裁，不创建全历史
ScrollView buffer，也不通过截断内容伪造视口优化。

主消息可淘汰派生缓存预算为 16 MiB，详情为 8 MiB；计量包括解析块、owned 文本、
Lines 与 wrap map 的 retained capacity，同缓存共享 Arc 去重，跨缓存保守计入。
这是一致的预算估算而非 allocator/RSS 实测；canonical 历史、轻量高度索引、全局高亮缓存、
可见超大工作集及复制暂态单列。淘汰清除整个重型 entry，不改变高度、锚点或完整内容。

## 条件策略

当前设计保留 `String` canonical storage、terminal full parse 与完整 intermediate syntax highlighting。只有 release profile 证明对应路径成为主要热点，并具备独立正确性证据时，才可引入 rope/storage、移除 terminal full parse 或增加中间高亮 fallback。

## 验证

- `cargo test -p peri-tui --lib`
- `cargo check -p peri-tui`
- `cargo clippy -p peri-tui --all-targets -- -D warnings`
- `cargo test --workspace --doc`
- release synthetic matrix：长度、chunk size、Markdown shape、history slots 与 terminal oracle
- `cargo fmt --all --check`
- `git diff --check`

行为入口以 `docs/code-index/peri-tui.md` 和源码为准；实施/profile 状态保留在对应 active issue。
