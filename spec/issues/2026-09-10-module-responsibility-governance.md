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
- [ ] **Middleware capability 接口**：继续现有
  [能力接口治理 issue](2026-07-25-middleware-capabilities-can-silently-no-op.md)，
  以调用点清单与编译期能力限制为验收依据；不能用移动文件替代 no-op API 消除。

## 全仓范围与完成条件

用户目标：彻底完成整个 Rust 仓库的模块结构与职责治理，逐步提交代码。
范围以 Cargo manifest 和实际源码为准：根 workspace 的 15 个 crate，以及
`side-projects/{git-stats,md-scan-matrix,image-spike}` 三个独立 Rust 项目。
不能以本轮已修改文件或 1000 行阈值替代全仓审查。

- [ ] 每个 crate 完成职责、状态所有权、依赖方向、宽接口与重复实现审查，记录实际修改
  或保持现状的理由；小而职责清晰的模块无需为统一文件尺寸而拆分。
- [ ] 下表高优先级入口完成职责拆分并验证 public/wire 路径与事务语义。
- [ ] Middleware 的失真/no-op 能力接口按关联 issue 完成迁移，hook 能力与生产实现一致。
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
| peri-middlewares | client、dynamic registry、assembly、staged_connection、subagent tool、middleware capability | client/registry 已提交；其余待审/治理 |
| peri-acp | host、session、event_sink、prompt、session lifecycle | host/session 已提交；事件与 prompt 待治理 |
| peri-resources | SQLite 连接、行映射、祖先上下文、compaction 事务 | SQLite 拆分已提交，65 项存储测试通过 |
| peri-agent | stage_builder、tool_dispatch、workflow agent、transcript、subagent factory、MiddlewareState | 已定位，待治理 |
| peri-acp-types | session 混合 runtime/错误/消息投递，event_v2 契约与映射；Agent queue_test 文件存在但未挂载 | 已定位，拆分及恢复测试挂载待完成 |
| peri-tui | client、ask_user、acp_notifier、current_turn、input_area、plugin panel | client 已提交；其他待审/治理 |
| peri-controller | Langfuse tracer registry、bridge 与控制面状态所有权 | 待审 |
| peri-model | protocol/runtime/transport/provider 分层清楚；测试专用 async SSE 重复路径需收敛 | 已审，保留总体结构，重复实现待处理 |
| peri-runtime | destroy 跨 await 后无条件 remove 可能误删替换后的 session；旧事件也可能借新 entry 补打 | 已审，登记实例所有权待复现/修复 |
| peri-workflow | runner、tool preflight、journal、progress | runner 已提交；其余待审 |
| peri-js-runtime | 分层可保留；artifact launch 未纳入 deadline/cancel、run_execute 错误提前返回可能跳过显式收尾 | 已审，生命周期路径待复现/修复 |
| peri-lsp | document sync/dispatcher 可分离；重复 readiness、请求 id 碰撞和进程任务 owner 需核查 | 已审，结构与生命周期待治理 |
| peri-theme | loader 混合查找/引用/构造；visited 跨根复用、Unicode hex切片、忽略 extends、重复默认主题 | 已审，解析边界待治理；缺少索引需补建 |
| peri-web-pty | handle_socket 混合协议/阻塞I/O/child监控；reader未join、末尾输出drain需实测 | 已审，连接owner与协议边界待治理 |
| langfuse-client | OTLP穷尽转换函数应按事件族拆分；batcher溢出策略/flush错误/shutdown语义需核查 | 已审，转换与batcher待治理 |
| side-projects/git-stats | 结构清楚；外层log sentinel碰撞、聚合展示名新旧次序需核查 | 已审，正确性修复/独立验证待完成 |
| side-projects/md-scan-matrix | 结构清楚；FAIL/前缀稳定性/collision结果只打印，不能据exit0宣称验收通过 | 已审，实验预期与退出码待治理 |
| side-projects/image-spike | 目标单一的TestBackend buffer/Kitty transmit实验，无需拆分 | 已审保留，独立验证待完成 |

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
