# 大文件与模块职责治理

**状态**：Open（后续边界治理待实施）
**类型**：重构
**创建日期**：2026-09-10

## 分析与取舍

文件行数用于发现候选，实际拆分依据是状态所有权、变化原因和契约测试。
单个 owner 可以有多个私有实现模块；不能为缩短文件而复制状态、引入第二套
生命周期、扩大公共 API，或把一个原子事务拆成多个独立加锁的步骤。

本轮选择三个有现成回归测试、可以在 crate 内保留 API 的入口：

| 入口 | 拆分前行数 | 集中的职责 | 拆分方向 |
| --- | ---: | --- | --- |
| `peri-middlewares/src/mcp/client.rs` | 1729 | service、资源缓存、OAuth、关闭事务、状态通知 | pool 保留状态所有权，操作按职责进入私有子模块 |
| `peri-tui/src/acp_client/client.rs` | 1477 | 通知泵、reverse request、session reservation、RPC、UI settlement | client 保留共享 owner，wire、session、请求和交互分别实现 |
| `peri-workflow/src/runner.rs` | 1369 | artifact 准备、握手、domain RPC、agent 调度、终态持久化 | runner 保留执行编排，准备、协议、消息循环和收尾分别实现 |

这些是入口职责整理，不等同于完成跨 crate 依赖或能力接口收敛。
实现后的导航只维护在 [MCP 索引](../../docs/code-index/peri-middlewares.md)、
[TUI 索引](../../docs/code-index/peri-tui.md) 和
[Workflow 索引](../../docs/code-index/peri-workflow.md)。

## 后续候选与验收边界

- [x] **Dynamic MCP registry**：`peri-middlewares/src/mcp/dynamic/registry.rs`
  同时承担 connector 装配、load/unload operation、capability 发布、session close
  与 projection lease。先从 `ProductionDynamicMcpConnector` 和
  `CheckedSessionMcpProjection` 定位可分离边界；`RegistryState` 仍为单一 owner，
  `load_commit_allowed` 与 capability 发布必须保持同一锁内裁决。验收覆盖 stale
  generation、并发 load/unload、session close 和失效 projection。
- [x] **ACP host**：`peri-acp/src/host/mod.rs` 的 `run_acp_server_inner` 同时处理
  transport 消息、OAuth 转发和 deployment teardown。先分离消息路由与关闭事务，
  保持 `HostTaskOwner` 单一持有、prompt 后台执行及 response/notification 顺序；
  验收复用 stdio/MPSC wire、prompt error 和 EOF drain 测试。
- [x] **ACP session manager**：`peri-acp/src/session/mod.rs` 包含
  `SessionDynamicMcpNotificationSink`、session 装配、caps 注册和 frozen 构建。
  notification sink 与 caps 操作可先独立定位；不得把 frozen 数据读取移到每次
  prompt，也不得复制 session/caps 注册表。验收覆盖 new/load/resume/fork、冷恢复
  frozen snapshot、caps 门控与 cancel。
- [x] **Middleware capability 接口**：已移除失真能力并按 hook 收窄接口；
  生产 runner 回归及编译期能力限制已验证，稳定入口见
  [ARC-MIDDLEWARE-CAPABILITY-001](../../docs/standards/architecture-contracts.md#arc-middleware-capability-001)。

## 全仓范围与完成条件

用户目标：彻底完成整个 Rust 仓库的模块结构与职责治理，逐步提交代码。
范围以 Cargo manifest 和实际源码为准：根 workspace 的 15 个 crate，以及
`side-projects/{git-stats,md-scan-matrix,image-spike}` 三个独立 Rust 项目。
不能以本轮已修改文件或 1000 行阈值替代全仓审查。

- [ ] 每个 crate 完成职责、状态所有权、依赖方向、宽接口与重复实现审查，记录实际修改
  或保持现状的理由；小而职责清晰的模块无需为统一文件尺寸而拆分。
- [ ] 下表高优先级入口完成职责拆分并验证 public/wire 路径与事务语义。
- [x] Middleware 的失真/no-op 能力接口完成迁移，hook 能力与生产实现一致。
- [ ] 复核低于阈值但职责交织的长函数，包括 TUI 大型交互组件、Agent 工具派发及
  Controller 观测注册表；判断依据为行为边界，不能仅移动文件。
- [ ] 根 workspace 完整 build/test/doc-test/clippy、依赖方向与格式检查完成；独立
  Rust 项目按各自 manifest 验证。真实平台/网络依赖及 ignored 项需单列证据，
  不能用 root workspace 的通过状态替代。
- [ ] 每一组已验证改动形成独立本地提交；代码索引/契约导航与源码一致；工作树无
  遗漏修改。目标全部验收后删除本过程文档，当前入口仍保留在代码索引。

## 审查队列

以下静态发现须通过针对性测试确认，再按职责修复；不能把风险判断当作已复现缺陷。

| Crate / 项目 | 重点入口与职责问题 | 当前状态 |
| --- | --- | --- |
| peri-middlewares | client、dynamic registry、assembly、staged_connection、subagent tool、middleware capability | client/registry/assembly 已提交；capability A/B/C 已验证；subagent tool 待治理 |
| peri-acp | host、session、event_sink、prompt、session lifecycle | host/session 已提交；event_sink 已拆分验证；prompt 模型/观测/stage 装配已拆分；canonical 旧测试副本已移除，真实 transcript/SQLite 冷重载覆盖保留；部署关闭接线已实现 |
| peri-resources | SQLite 连接、行映射、祖先上下文、compaction 事务 | SQLite 拆分已提交，65 项存储测试通过 |
| peri-agent | stage_builder、tool_dispatch、workflow agent、transcript、subagent factory、MiddlewareState | stage_builder、capability、tool_dispatch 已拆分验证；transcript writer 已分离并修复失败态空转；workflow agent 观察/结果投影已分离，subagent factory 的 context/spawn/resume/claim 已分离，恢复状态取消owner已实现 |
| peri-acp-types | session 混合 runtime/错误/消息投递，event_v2 契约与映射；Agent queue_test 文件存在但未挂载 | session 拆分已验证，Agent queue 7 项测试恢复挂载；event_v2 契约/通道/纯映射已拆分，53项测试迁回定义crate |
| peri-tui | client、ask_user、acp_notifier、current_turn、input_area、plugin panel | client 已提交；ask_user/notifier/current_turn 已拆分验证；input_area 主体保留清晰边界；plugin 搜索状态与请求 owner 已拆分，真实桥接回归通过 |
| peri-controller | Langfuse tracer registry、bridge 与控制面状态所有权 | 已拆分；漏关观测回归2红→全套130+5绿；移除6个仅装配阶段使用的锁 |
| peri-model | protocol/runtime/transport/provider 分层清楚；测试专用 async SSE 重复路径需收敛 | 已审保留总体结构；测试专用 SSE 整链已删除，生产路径147项测试通过 |
| peri-runtime | destroy 跨 await 后无条件 remove 可能误删替换后的 session；旧事件也可能借新 entry 补打 | 已修复并提交：4项旧实现回归失败；16项Runtime及16项Controller关联测试通过 |
| peri-workflow | runner、tool preflight、journal、progress | runner 已提交；progress 按 run 共持投影/执行标记、journal Git/输出职责已分离，2红→91绿；tool 完成发布与preflight进程生命周期已分离并修复取消窗口，94项全绿 |
| peri-js-runtime | 分层可保留；artifact launch 未纳入 deadline/cancel、run_execute 错误提前返回可能跳过显式收尾 | preparation/install/invocation owner 已修复；4红到42全绿，含实际Unix进程回收；Windows runtime 未验证 |
| peri-lsp | document sync/dispatcher 可分离；重复 readiness、请求 id 碰撞和进程任务 owner 需核查 | dispatcher 已拆分，双向ID/畸形响应/close pending 三回归由红到72全绿；client/pool readiness 与请求跨重启归属已修复；document sync 已归属单次连接，缓存与 writer 准入同步提交；92项通过 |
| peri-theme | loader 混合查找/引用/构造；visited 跨根复用、Unicode hex切片、忽略 extends、重复默认主题 | 已拆分并修复引用链/Unicode hex/extends；29项单元与集成测试通过，索引已补建 |
| peri-web-pty | handle_socket 混合协议/阻塞I/O/child监控；reader未join、末尾输出drain需实测 | Unix 连接 owner、非阻塞 I/O 与协议边界已拆分；24单元+7真实WS测试通过，Windows阻塞读取收尾仍有平台缺口 |
| langfuse-client | OTLP穷尽转换函数应按事件族拆分；batcher溢出策略/flush错误/shutdown语义需核查 | 事件族转换已拆分，73项测试通过；batcher flush 失败确认已修复，77项通过；shutdown owner已验证83项；宿主关闭权限/任务join/Incomplete重试已接线；backpressure与非法配置待治理 |
| side-projects/git-stats | 结构清楚；外层log sentinel碰撞、聚合展示名新旧次序需核查 | NUL framing与最新展示名修复已验证：28项测试含真实Git fixture，独立Clippy通过 |
| side-projects/md-scan-matrix | 结构清楚；FAIL/前缀稳定性/collision结果只打印，不能据exit0宣称验收通过 | 评估/报告已分离；6项回归、45矩阵+33补充checks及独立Clippy通过 |
| side-projects/image-spike | 目标单一的TestBackend buffer/Kitty transmit实验，无需拆分 | 已审保留；本地2项buffer行为测试及all-targets Clippy通过；不等于真实终端验证 |

## 每批验证

1. 对照移动前函数体核查锁作用域、await 顺序、Drop、取消与终态；内部 helper
   使用最小可见性，public 导出路径保持兼容。
2. 运行所改子系统的目标测试，记录真实退出码和非零用例数；必要时运行关联
   ACP/TUI 链路及 doc tests。测试范围以
   [testing standard](../../docs/standards/testing.md) 为准。
3. 运行 workspace clippy、格式与依赖方向检查；文件规模扫描只作为待治理清单，
   不通过放宽阈值或修改豁免来消除提示。
4. 按 [文档维护规范](../../docs/standards/documentation.md) 更新受影响代码索引和
   canonical 路由。后续事项完成后删除此过程文档，稳定入口保留在代码索引。


## 第三批验证记录

- Runtime：新增4个并发场景在旧实现全部失败；修复后16项通过（另含取消重试、旧persist失败），Controller关联16项通过。
- session拆分：types 10项、Agent session 215项（含恢复挂载queue 7项）、ACP event_sink 14项通过。
- 装配：Agent exec 59项、中间件assembly 33项、ACP executor flow 19项通过；54个ChainSlot arm及disabled guard顺序逐项核对。
- Theme：24项单元+4项builtin集成+1项隔离HOME loader集成通过；保留root路径与dotfiles symlink兼容性。
- Model：147项通过；测试改走生产SSE reader，删除仅供测试的async decoder分支。
- 本批涉及的6个crate doc tests：1项通过、3项原有ignored；其余无可执行用例。Theme doc tests为0项。
- workspace all-targets Clippy（`-D warnings`）、格式、16条依赖门与diff检查通过。
- image-spike独立manifest：2项行为测试及all-targets Clippy通过。

- md-scan-matrix独立manifest：6项回归、45矩阵+33补充checks及all-targets Clippy通过；预期碰撞精确标记，未预期失败返回非零。

上述结果不替代尚未执行的全仓完整测试，也不覆盖下一批正在修改的模块。


## 第四批验证记录

- 首次完整workspace测试（允许本机测试端口）：45个测试目标，5481通过、37失败、12 ignored。失败分布在JS fixture未构建与middleware进程环境污染；不能据此宣称全仓通过。
- 按本地源码构建PTC的两个JS fixture后，JS原有33项通过。新加4项生命周期回归在原实现全部失败，修复仍在进行。
- MCP staged_connection的PATH/sentinel测试迁为子进程，父进程不再临时改PATH。修复后middleware全套1612通过、4项原有ignored；原workflow快速失败诊断亦通过，无需修改其生产逻辑。
- capability A：Agent全套736项通过，middleware上述全套覆盖真实Image回写；仅删除失真能力和收紧消息替换，hook分阶段接口B尚未落地。
- Controller：原登记生命周期回归1通过2失败；修复后130单元+5集成通过。事件转换函数体核对相同，保持flush/relock顺序。
- git-stats独立manifest：28项测试与all-targets Clippy通过；真实Git fixture覆盖消息/路径marker及新旧展示名。
- Agent/Middleware/Controller doc tests与workspace all-targets Clippy（`-D warnings`）通过；分组提交均通过hooks。

后续修改完成后仍需重新完成workspace总体验证、build、独立项目与平台边界复核。

## 第五批验证记录

- JS runtime：准备阶段取消/时限和结果失败的4项回归先在旧实现失败；单次 invocation 与安装进程 owner 修复后42项单元测试通过，涵盖握手取消不隔离健康缓存、staging 清理、锁等待取消和真实 Unix 安装进程回收。外部 future Drop 仍仅尽力清理；Windows runtime 未验证。
- LSP：传输管道与分发 owner 分离，服务器请求先按 method 分类，避免双向同 ID 消费错误 pending；close 自行拒绝 pending 并等待自有任务。3项回归先红，修复后72项单元测试通过；尚未收敛 client/pool 就绪状态与请求跨重启归属。
- capability B/C：7个 compile-fail 与3个生产 runner 回归通过；Agent739单元+4集成、ACP611单元+8集成通过。所有生产 hook 迁移到阶段能力接口，稳定规则转入 ARC-MIDDLEWARE-CAPABILITY-001，关闭并删除能力治理过程 issue。
- middleware：沙箱内1608通过、4失败（本机监听和进程状态权限），允许本机测试操作后1612单元+8集成通过，4项原有 ignored。没有以放宽断言处理环境失败。
- TUI：notifier 解码、CurrentTurn 累积/投影与 AskUser 表单职责分离；1466项单元通过，2项原有 ignored。保持原增量缓存、owner 切换与通知发布顺序。
- workspace doc tests：8项通过、3项原有 ignored；workspace all-targets Clippy（`-D warnings`）、格式与16条依赖方向门通过。首次 Clippy 的两处 unused import 已删除后重新通过。
- 规范漂移：按源码/契约测试优先级同步 cache 字段缺省、显式零与逐 sample 提示语义；删除已 Fixed 的 #113，#114 保留现场验收待办。仓库 Markdown 本地文件链接扫描86项，无失效目标；分组提交均通过 hooks。

上述分组验证不替代最后的 workspace 完整 build/test，也不关闭仍需现场验收的 cache 提示问题或平台测试缺口。


## 第六批验证记录

- LSP：client 的注册/握手/请求职责分离；只在完整握手后就绪，request guard 绑定原 dispatcher，pool 移除重复 readiness。单一 bounded writer 保证已接纳帧完整写入，关闭拒绝 pending 并保留可重试 join/reap 的 owner。旧实现3项 readiness/取消回归失败；当前87项单元测试通过，包含真实进程退出、满队列关闭、同名 pool 替换。document sync 的全局缓存仍需下一批收敛，未宣称整个 LSP 生命周期完成。
- Langfuse：OTLP 转换按 trace/observation/generation/metadata 分离，原事件族字段映射与顺序保持；9项新增生产转换用例覆盖12类事件，全套73项通过。batcher 的 flush 确认、资源关闭与背压语义仍待处理。
- WebPTY：Unix 连接统一持有 PTY、非阻塞读写与退出清理；协议与平台适配分离，输出先 drain 再发送 exit。旧实现回归使用独立线程 watchdog 复核为3通过2失败，避免阻塞 runtime 令 Tokio timeout 假通过。当前24项单元与7项真实 WebSocket 测试通过；取消显式 shutdown 后可继续等待同一 cleanup task。Windows runtime 未验证，阻塞读取线程收尾缺口保留。
- Plugin：DiscoverState 统一查询/选择/结果，search ticket 绑定 generation、session 与桥接 reset；真实 RPC 结果驱动展示和所选详情。2项原实现回归先失败，最终10项目标回归通过，涵盖重复查询乱序、临时渲染借用、关闭取消。测试恢复全局 atoms 后，全量 TUI 1476项通过、2项原有 ignored。
- 完整 `cargo test --offline --workspace --no-fail-fast`：46个测试目标共5596项通过、0失败、12项原有 ignored；包含8项实际执行的 doc tests。先前 fixture/PATH 环境问题不再导致失败。测试允许本机端口和子进程操作，没有外部服务调用。
- 完整 workspace build、all-targets Clippy（`-D warnings`）、格式、typos 与16条依赖方向检查通过。各模块代码索引同步到当前职责与 owner；本批分组提交通过 hooks。

此 checkpoint 不能替代后续改动的验证，也不关闭独立项目/平台或现场验收缺口。


## 第七批验证记录

- 工具派发：审批后错误把已声明 `path` 字段改为 `file_path` 的2项生产回归先红，修复统一使用 `tools::normalize_params`。删除测试专用 resolver，其消费者契约迁到真实 resolver 的6项测试。tool_dispatch 根从912行降至298行，执行管线与 PTC 适配器私有分离；13个保留函数体逐体核对相同。新增快工具即时事件先于批次原子提交、PTC pinned target 与内层不结算外层批次的回归。
- Langfuse flush：3项旧实现回归全部失败；私有失败水位只在公开 flush 调用者实际观察结果后确认，取消等待不清错，迟到确认不能清除新失败。新增并发确认不倒退保护，旧吞错测试更新为精确错误摘要、下一空确认与实际发送恢复。77项单元通过；shutdown/backpressure 尚未修复。
- LSP 文档同步：真实握手 gate 与16槽满队列回归先2红2绿；RegisteredConnection 同时拥有 dispatcher 和文档缓存，先等待 writer permit，再同步规划版本/准入/提交缓存，锁外等待实际写入确认。已准入后取消保留完整帧与缓存，旧连接不能触及新缓存；关闭拒绝旧 permit。92项单元通过。
- ACP prompt：模型工厂、turn 观测与 stage/compact hook 装配拆为私有模块，公开 hook 路径保留；借用既有 AcpServerConfig 将调用参数从27项收敛为7项。配置快照时点、池 owner、原回调与 await 顺序保持。依赖门仅随已存在装配职责移动到精确新文件，不扩大目录豁免；模块指引修正链序蓝本与具体装配位置。
- 关联完整验证：Agent742单元+4集成、ACP611单元+8集成、Langfuse77单元、Controller130单元+5集成通过；对应 doc tests 8项通过。middleware1612单元+8集成通过，单元4项与doc1项原有 ignored。
- workspace all-targets Clippy（`-D warnings`）、格式与16条依赖方向检查通过。代码索引已同步；分组提交须通过 hooks。本批结果不替代后续修改的最终 workspace 验证。


## 第八批验证记录

- 事件契约：event_v2 根18行保留全部公共 re-export，私有 types/bus/executor_mapping 管类型身份、通道、纯映射；三段生产代码去注释/空白核对完全相同。53项原测试迁回 types，Agent 新增公共路径身份保护。同步实际有界 broadcast、Render 完成事件与测试导航；已有事件饱和 Open issue 保持未关闭。
- Transcript：唯一生产持久化 writer 提取到私有 persistence 模块，内存/canonical compaction 仍由 Transcript 持有。真实 writer +虚拟时钟/计数Waker回归先红（无新op仍被失败batch deadline唤醒），修复后 terminal failure 只等待新操作；sticky barrier、未落盘payload和Shutdown顺序保持。
- Workflow progress：新回归先证实完成保留期中phase token从20变0；TrackedRun 同时拥有公开投影与本次执行标记，删除两把锁的逆序获取路径。保留期内统计不丢、cache-hit历史用量不混入，到期 run/marker 同时释放；时点注入验证精确过期边界。
- Workflow journal：Git baseline/postcondition、JSON 输出提取与磁盘记录拆开，public 路径保留。真实目录冲突回归先证实写失败仍把原文替换为无效引用；修复只在文件写成功后提交引用，失败保留原文。Workflow 全套91项通过、3项原有 ignored。
- Langfuse shutdown：admission、worker、join owner 分离；独立关闭信号不受队列满阻塞，只排空已提交命令，保留JoinHandle跨取消/重试，缓存固定终态。6项新增回归覆盖本机HTTP gate、满队列、未轮询生产者、并发重复关闭及真实abort/panic；83项全包通过，Controller130+5通过。新API没有旧实现，未把编译缺失或模拟shutdown当作旧行为红灯；宿主/print接线仍需下一批完成。
- 分层验证：types384单元+3集成、Agent691单元+4集成、ACP611单元+8集成通过；Agent实际doc tests 8项通过，types2项原有ignored。
- 完整 workspace 测试：46个目标共5618通过、0失败、12项原有ignored。初次strictClippy发现journal拆分后两处unused import，删除后workspace all-targets Clippy（`-D warnings`）、完整build与格式检查均以exit0通过；本批分组提交通过hooks。Markdown本地链接86项无失效，16条依赖方向门与typos通过。

当前仍需处理子Agent恢复取消回滚/事件排空、workflow tool完成observer、workflow agent装配，以及Langfuse部署关闭和背压配置；本批不宣称全仓治理完成。

## 第九批验证记录

- Workflow tool：真实 Node 回归先确认调用在快速窗口取消会丢通知、快速 kill 被另一套投影改成 Failed；单一 RunCompletion 任务等待 runner 收尾并发布一次投影，快速响应仅观察。preflight 独立进程回归先确认取消后进程与目录均残留；改为 TempDir 与 kill-on-drop 进程所有权，正常路径等待回收，取消 Drop 仅尽力回收。Unix 实测修复后三项回归均通过，全 crate 94通过、3项原有ignored；all-targets严格Clippy通过，doc target执行0项，不能计为行为证明。
- 子 Agent 恢复：真实 dispatch 取消回归先失败；factory 按 claim/context/spawn/resume 分离，唯一 worker 顺序持有 active、回滚及恢复的同步终态写入。准备取消恢复旧状态，运行取消写 cancelled；同步 Start/Stop 顺序保持，后台成功注册后才移交。原中断返回契约在首轮全仓暴露回归后恢复，23项Agent与18项middleware恢复用例均通过；原有契约断言未改。
- 动态 MCP 关闭：真实 registration 的首次 Incomplete/取消后重试两回归先红；修复仅实际 Complete 后缓存完成。ACP 部署保留原 host task 及实际待关闭 AcpSession，取消等待/Incomplete可重试，完整drain后才关闭新部署拥有的Langfuse；外部共享会话不提前关闭。print正常/业务错误统一close与join，资源Incomplete/TaskFailed可见，遥测HTTP失败旁路报告。最终ACP610单元+8集成、middleware1614单元+8集成、TUI1478单元及部署回归通过；middleware4项、TUI2项原有ignored保留。

- 首轮完整workspace：46个目标5634通过、2失败、12项原有ignored；失败为恢复中断返回兼容与既有TUI reminder测试未串行化却清空共享atom。前者按既有跨crate契约修复，后者仅给三个使用共享atom的测试补串行锁，不改生产快照/断言。首次编译另修正新transcript测试使用专用payload编码API，不为测试添加生产Serialize实现。

- Workflow agent：观察/进度与结果投影进入私有模块，单一RunStats owner保持loop→close bus→join→stats/result→terminal次序；子Agent转发器不再因observe关闭提前丢弃其他通道的缓冲事件，真实回归先红后绿。
- ACP canonical清理：删除两个tests-only持久化投影替身、七项依赖替身测试与三项无消费计算；新真实transcript→writer→compaction→SQLite冷重载回归覆盖reminder在摘要前/后的身份、类型、顺序与单次投影，既有失败原子性/staging测试保留。
- 最终完整workspace测试：46个目标共5637通过、0失败、12项原有ignored，包含8项实际doc tests；Agent704单元+4集成、Workflow94单元通过。workspace all-targets Clippy（`-D warnings`）、完整build、格式均exit0，16条依赖方向门与typos通过；六组代码提交均通过hooks。Markdown本地文件链接86项无失效，索引与架构关闭契约同步。

尚余SubAgent tool的定义/审批/意图装配边界，以及Langfuse背压与非法配置；保持本治理issue为Open。现有平台与独立后台任务边界不因本批验证被扩大。
