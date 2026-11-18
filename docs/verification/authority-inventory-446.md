# #446 权威文档盘点与处置

状态：本轮静态审计记录，不是能力运行验收或新增规范。
基线：`cee019f34337e420178be0c2671c98f8c23c4083`（2026-11-18）。

范围：基线跟踪的根 Markdown、全部 docs Markdown、npm 包文档、两层内 crate/MCP README 与构建指引；
包含声明权威的设计，也登记参考、代码索引、证据以防其被误用为权威。
旧过程 `spec/issues/` 由活跃工单与 Git 历史管理，不将所有历史叙事提升成契约。
本轮不移动有效设计、不复制包内设计。保留表示实际保留原位置；改写表示已修改正文。
没有为凑处置类型删除有效设计。新功能表与验收映射不属于基线旧文档清单。

契约唯一位置：长期不变量归 standards，各领域具体方案归 design/包内 DESIGN；
根、README、code-index 只导航，verification 只记证据。此表不新增运行时 owner/启用映射。
重叠说明仅作为有链接的摘要；发生矛盾按 STD-INDEX-002 分类，不能用表中“保留”覆盖正确契约。

| 文档 | 处置 | 理由与结果 |
| --- | --- | --- |
| [AGENTS.md](../../AGENTS.md) | 保留 | 符号链接指向 CLAUDE.md；同一物理正文，无第二套根指令。 |
| [CHANGELOG.md](../../CHANGELOG.md) | 保留 | 发布历史，不是现行契约或本轮验证。 |
| [CLAUDE.md](../../CLAUDE.md) | 改写 | 收敛项目理念与导航，去掉重复分层清单和工程细则，满足根预算。 |
| [CONTEXT.md](../../CONTEXT.md) | 保留 | 领域术语单一词表；不持有运行调度或能力启用事实。 |
| [README.md](../../README.md) | 保留 | 「Peri Code」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [docs/code-index/git-stats.md](../code-index/git-stats.md) | 保留 | 「git-stats 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/image-spike.md](../code-index/image-spike.md) | 保留 | 「image-spike 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/langfuse-client.md](../code-index/langfuse-client.md) | 保留 | 「langfuse-client 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/mcp-packages.md](../code-index/mcp-packages.md) | 保留 | 「MCP package 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/md-scan-matrix.md](../code-index/md-scan-matrix.md) | 保留 | 「md-scan-matrix 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-acp-types.md](../code-index/peri-acp-types.md) | 保留 | 「peri-acp-types 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-acp.md](../code-index/peri-acp.md) | 保留 | 「peri-acp 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-agent.md](../code-index/peri-agent.md) | 保留 | 「peri-agent 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-analysis.md](../code-index/peri-analysis.md) | 保留 | 「Peri 数据分析代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-config.md](../code-index/peri-config.md) | 保留 | 「peri-config 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-controller.md](../code-index/peri-controller.md) | 保留 | 「peri-controller 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-js-runtime.md](../code-index/peri-js-runtime.md) | 保留 | 「peri-js-runtime 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-middlewares.md](../code-index/peri-middlewares.md) | 保留 | 「peri-middlewares 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-model.md](../code-index/peri-model.md) | 保留 | 「peri-model 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-openui.md](../code-index/peri-openui.md) | 保留 | 「OpenUI 代码索引（@peri-code/sdk）」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-process.md](../code-index/peri-process.md) | 保留 | 「peri-process 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-resources.md](../code-index/peri-resources.md) | 保留 | 「peri-resources 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-runtime.md](../code-index/peri-runtime.md) | 保留 | 「peri-runtime 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-theme.md](../code-index/peri-theme.md) | 保留 | 「peri-theme 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-time.md](../code-index/peri-time.md) | 保留 | 「peri-time 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-ts-sdk.md](../code-index/peri-ts-sdk.md) | 保留 | 「Peri TS SDK 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-tui.md](../code-index/peri-tui.md) | 保留 | 「peri-tui 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-wasm.md](../code-index/peri-wasm.md) | 保留 | 「peri-wasm 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/code-index/peri-workflow.md](../code-index/peri-workflow.md) | 保留 | 「peri-workflow 代码索引」仅定位实现入口，不授予新增控制权威。 |
| [docs/design/README.md](../design/README.md) | 改写 | 补 Session Durability 与专用装配路由，限定旧执行恢复边界。 |
| [docs/design/acp-mcp-control.md](../design/acp-mcp-control.md) | 保留 | 保留「ACP MCP 运行控制」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/architecture.md](../design/architecture.md) | 保留 | 保留「Perihelion 总体架构」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/beta-flags.md](../design/beta-flags.md) | 保留 | 保留「Beta Flag 系统」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/command-system.md](../design/command-system.md) | 保留 | 保留「Command 系统权威架构」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/configuration-authority.md](../design/configuration-authority.md) | 保留 | 保留「配置权威面」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/git-watch-middleware.md](../design/git-watch-middleware.md) | 保留 | 保留「Git Watch（git ref 变化提醒）」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/interaction-brokers.md](../design/interaction-brokers.md) | 保留 | 保留「交互 Broker 与审批、问答设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/mcp-adaptation-v4-part-1.md](../design/mcp-adaptation-v4-part-1.md) | 保留 | 保留「MCP 适配清单（v4-part-1）」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/mcp-cache.md](../design/mcp-cache.md) | 保留 | 保留「MCP 响应缓存策略」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/mcp-multiplexing.md](../design/mcp-multiplexing.md) | 保留 | 保留「MCP 透传信道设计（单 ACP 连接上的多路数据分离）· 定稿」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/mcp-v5-single-exit.md](../design/mcp-v5-single-exit.md) | 保留 | 保留「MCP v5：builtin 单出口目标设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/message-transcript.md](../design/message-transcript.md) | 保留 | 保留「会话消息与 Transcript 架构设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/meta-control.md](../design/meta-control.md) | 保留 | 保留「Peri Meta 数据访问」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/meta-harness.md](../design/meta-harness.md) | 保留 | 保留「MetaHarness 设计 — 提示词段落替换与 middleware 卸载」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/micro-compact.md](../design/micro-compact.md) | 保留 | 保留「Micro Compact：可回查的模型视图投影」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/middleware-system.md](../design/middleware-system.md) | 保留 | 保留「Middleware 系统设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/model-adapters.md](../design/model-adapters.md) | 保留 | 保留「Model Adapter 架构设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/monitor-mcp.md](../design/monitor-mcp.md) | 保留 | 保留「MonitorMCP（command source 监控）」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/peri-acp-protocol.md](../design/peri-acp-protocol.md) | 保留 | 保留「peri-acp 协议设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/peri-sdk-chat.md](../design/peri-sdk-chat.md) | 保留 | 保留「Peri SDK Chat 架构」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/rcra-message-activation.md](../design/rcra-message-activation.md) | 保留 | 保留「RCRA 消息、执行与历史会话加载」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/serverless-execution-identity.md](../design/serverless-execution-identity.md) | 保留 | 保留「Serverless 执行环境身份」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/session-async-tasks.md](../design/session-async-tasks.md) | 改写 | 限定不恢复旧任务目录，回链工作意图新运行，纠正 async-only 来源。 |
| [docs/design/session-durability.md](../design/session-durability.md) | 改写 | 前置 MQ 改仓库链接；sync 限历史迁移；实施/仿真记录分离至 active 验收追踪。 |
| [docs/design/session-id-environment.md](../design/session-id-environment.md) | 保留 | 保留「会话身份、工作区归属与恢复」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/storage-v2-machine-workspace-session.md](../design/storage-v2-machine-workspace-session.md) | 保留 | 保留「存储 v2：Machine → Workspace → Session」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/system-prompt.md](../design/system-prompt.md) | 保留 | 保留「System Prompt 构建系统设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/system-reminder.md](../design/system-reminder.md) | 保留 | 保留「System Reminder 协议设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/thread-mq.md](../design/thread-mq.md) | 保留 | 保留「Thread MQ：统一持久收件箱与 Rust 操作接口」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/time-runtime.md](../design/time-runtime.md) | 保留 | 保留「时间能力边界」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/tool-system.md](../design/tool-system.md) | 保留 | 保留「工具系统架构设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/tui-acp-data-flow.md](../design/tui-acp-data-flow.md) | 保留 | 保留「TUI 与 ACP 数据流」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/tui-chat-workbench.md](../design/tui-chat-workbench.md) | 保留 | 保留「TUI Chat 与 Tool Activity Workbench 目标设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/tui-panel-architecture.md](../design/tui-panel-architecture.md) | 改写 | 修正组件拆分后的代码落点，保留面板业务边界。 |
| [docs/design/tui-streaming-markdown-performance.md](../design/tui-streaming-markdown-performance.md) | 保留 | 保留「TUI 流式 Markdown 性能设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/tui-subagent-activity.md](../design/tui-subagent-activity.md) | 保留 | 保留「TUI SubAgent 活动行设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/ultra-adlc.md](../design/ultra-adlc.md) | 保留 | 保留「Ultra-ADLC 设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/user-input-queue.md](../design/user-input-queue.md) | 保留 | 保留「用户待发送队列」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/workflow-mcp.md](../design/workflow-mcp.md) | 保留 | 保留「Workflow as MCP 设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/design/workflow.md](../design/workflow.md) | 保留 | 保留「Workflow 系统设计」领域设计；状态与未落地边界不由登记改成已验证。 |
| [docs/meta-harness.md](../meta-harness.md) | 保留 | 「MetaHarness：提示词与能力配置」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [docs/reference/README.md](../reference/README.md) | 改写 | 补功能目录、验收映射与文档处置证据导航。 |
| [docs/reference/artifact-remote-storage.md](../reference/artifact-remote-storage.md) | 保留 | 「Artifact 远程存储配置」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/dev-build-cache.md](../reference/dev-build-cache.md) | 保留 | 「本机编译缓存（sccache）使用说明」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/i386-static-build.md](../reference/i386-static-build.md) | 保留 | 「32 位 x86 Linux 静态构建」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/langfuse-data-integrity.md](../reference/langfuse-data-integrity.md) | 保留 | 「Langfuse 监控数据结构检查手册」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/loongarch64-build.md](../reference/loongarch64-build.md) | 保留 | 「64 位 LoongArch Linux 静态构建」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/mcp-ecosystem.md](../reference/mcp-ecosystem.md) | 保留 | 「MCP 生态定位与互通指南（Connector 视角）」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/openai-responses.md](../reference/openai-responses.md) | 保留 | 「OpenAI Responses 与 ChatGPT 计划登录」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/oxidizer-fetch-upstream-research.md](../reference/oxidizer-fetch-upstream-research.md) | 改写 | 修复 RequestRouter 拆分后的路径； 「Oxidizer `fetch` 上游核查」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/time-platform-research.md](../reference/time-platform-research.md) | 保留 | 「时间库与 WASM 平台资料」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/tui-manual-verification.md](../reference/tui-manual-verification.md) | 保留 | 「peri-tui 手工验证 Checklist」是参考与操作资料，不覆盖 standard/design。 |
| [docs/reference/wasm-host-ecosystem.md](../reference/wasm-host-ecosystem.md) | 保留 | 「Peri WASM 生态接入调查」是参考与操作资料，不覆盖 standard/design。 |
| [docs/standards/architecture-contracts.md](../standards/architecture-contracts.md) | 改写 | 去掉当前 sync/background 双模式表述，限定旧执行恢复移除范围。 |
| [docs/standards/documentation.md](../standards/documentation.md) | 保留 | 保留「CLAUDE.md 文档规则」长期规则及 Verify；功能登记不复制该契约。 |
| [docs/standards/environment-variables.md](../standards/environment-variables.md) | 保留 | 保留「Peri 环境变量控制表」长期规则及 Verify；功能登记不复制该契约。 |
| [docs/standards/git.md](../standards/git.md) | 保留 | 保留「Git 分支与上游规则」长期规则及 Verify；功能登记不复制该契约。 |
| [docs/standards/index.md](../standards/index.md) | 改写 | 补功能登记标准的唯一路由。 |
| [docs/standards/rust.md](../standards/rust.md) | 保留 | 保留「Rust 仓库规则」长期规则及 Verify；功能登记不复制该契约。 |
| [docs/standards/testing.md](../standards/testing.md) | 保留 | 保留「Perihelion 测试规范」长期规则及 Verify；功能登记不复制该契约。 |
| [docs/standards/tui-environment-variables.md](../standards/tui-environment-variables.md) | 保留 | 保留「TUI 环境变量控制表」长期规则及 Verify；功能登记不复制该契约。 |
| [docs/standards/tui.md](../standards/tui.md) | 保留 | 保留「TUI 规则」长期规则及 Verify；功能登记不复制该契约。 |
| [docs/verification/benchmarks/README.md](benchmarks/README.md) | 保留 | 「验证基准」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/benchmarks/sdk-wasm-capacity.md](benchmarks/sdk-wasm-capacity.md) | 保留 | 「SDK WASM Agent 容量基准（1 GiB 宿主 RSS）」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/history-v2-acceptance.md](history-v2-acceptance.md) | 保留 | 「History V2 验收与发布说明」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/01-session.md](tui-integration-cases/01-session.md) | 保留 | 「启动与会话」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/02-prompt-order.md](tui-integration-cases/02-prompt-order.md) | 保留 | 「输入与消息时序」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/03-stream-tools.md](tui-integration-cases/03-stream-tools.md) | 保留 | 「流式回答与工具」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/04-subagent.md](tui-integration-cases/04-subagent.md) | 保留 | 「Subagent 与后台任务」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/05-interactions.md](tui-integration-cases/05-interactions.md) | 保留 | 「审批与问答」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/06-transcript-ui.md](tui-integration-cases/06-transcript-ui.md) | 保留 | 「消息区与覆盖层」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/07-execution-boundaries.md](tui-integration-cases/07-execution-boundaries.md) | 保留 | 「执行、历史与故障边界」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/08-background-lifecycle.md](tui-integration-cases/08-background-lifecycle.md) | 保留 | 「后台任务生命周期」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/09-performance.md](tui-integration-cases/09-performance.md) | 保留 | 「长会话与资源稳定性」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/10-configuration.md](tui-integration-cases/10-configuration.md) | 保留 | 「配置与能力开关」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/11-transcript-display.md](tui-integration-cases/11-transcript-display.md) | 保留 | 「消息内容显示」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/12-composer-status.md](tui-integration-cases/12-composer-status.md) | 保留 | 「输入区、状态栏与后台任务栏」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/13-panel-display.md](tui-integration-cases/13-panel-display.md) | 保留 | 「面板与详情显示」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/14-popup-display.md](tui-integration-cases/14-popup-display.md) | 保留 | 「阻塞交互与弹出层显示」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/15-browse-layout.md](tui-integration-cases/15-browse-layout.md) | 保留 | 「浏览态与整体布局」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/16-clipboard-transport.md](tui-integration-cases/16-clipboard-transport.md) | 保留 | 「复制通道与远程剪贴板」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/17-setup-wizard.md](tui-integration-cases/17-setup-wizard.md) | 保留 | 「首次配置向导」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/README.md](tui-integration-cases/README.md) | 保留 | 「TUI 集成测试用例」是限定基线的证据/用例，不作为新能力规范。 |
| [docs/verification/tui-integration-cases/display-coverage.md](tui-integration-cases/display-coverage.md) | 保留 | 「TUI 显示覆盖矩阵」是限定基线的证据/用例，不作为新能力规范。 |
| [examples/README.md](../../examples/README.md) | 保留 | 「Examples」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [examples/mcp-use-showcase/README.md](../../examples/mcp-use-showcase/README.md) | 保留 | 「mcp-use Showcase」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [examples/minimal/README.md](../../examples/minimal/README.md) | 保留 | 「Minimal」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [examples/parallel-search/README.md](../../examples/parallel-search/README.md) | 保留 | 「Parallel Search MCP」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/@peri-sdk/OWNERSHIP-MODEL.md](../../npm-packages/@peri-sdk/OWNERSHIP-MODEL.md) | 保留 | 「User、Workspace、SessionDB、Workspace 环境与 Chat 运行实例」在原包维护专属边界，不复制到中央控制表。 |
| [npm-packages/@peri-sdk/README.md](../../npm-packages/@peri-sdk/README.md) | 保留 | 「@peri-code/sdk」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/@peri-workflow/DESIGN.md](../../npm-packages/@peri-workflow/DESIGN.md) | 保留 | 「@peri-code/workflow — Design & Integration Contract」在原包维护专属边界，不复制到中央控制表。 |
| [npm-packages/@peri-workflow/README.md](../../npm-packages/@peri-workflow/README.md) | 保留 | 「@peri-code/workflow」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/agent-builder/README.md](../../npm-packages/agent-builder/README.md) | 保留 | 「Peri Agent Builder」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/memory-plugin/DESIGN.md](../../npm-packages/memory-plugin/DESIGN.md) | 保留 | 「Peri Memory MCP：Hindsight 后端与独立消息导入器」在原包维护专属边界，不复制到中央控制表。 |
| [npm-packages/memory-plugin/README.md](../../npm-packages/memory-plugin/README.md) | 保留 | 「@peri/memory-plugin」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/memory-plugin/VALIDATION.md](../../npm-packages/memory-plugin/VALIDATION.md) | 保留 | 「Memory MCP 0.1.0 验证记录」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/peri-rivet/DESIGN.md](../../npm-packages/peri-rivet/DESIGN.md) | 改写 | 补当前设计状态、SDK 唯一路由与真实运行验收缺口。 |
| [npm-packages/peri-rivet/README.md](../../npm-packages/peri-rivet/README.md) | 保留 | 「@peri-code/rivet」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/peri-rivet/docs/custom-runtime.md](../../npm-packages/peri-rivet/docs/custom-runtime.md) | 保留 | 「自定义 transport runtime」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/peri-sandbox/COMPATIBILITY.md](../../npm-packages/peri-sandbox/COMPATIBILITY.md) | 保留 | 「Compatibility and validation ledger」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/peri-sandbox/MCP-TOOL-TEST-ISSUES.md](../../npm-packages/peri-sandbox/MCP-TOOL-TEST-ISSUES.md) | 保留 | 「peri-sandbox MCP 工具测试问题报告」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/peri-sandbox/README.md](../../npm-packages/peri-sandbox/README.md) | 保留 | 「peri-sandbox」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [npm-packages/peri-sandbox/peri-sandbox-design.md](../../npm-packages/peri-sandbox/peri-sandbox-design.md) | 保留 | 「peri-sandbox 设计（单实例）」在原包维护专属边界，不复制到中央控制表。 |
| [patches/README.md](../../patches/README.md) | 保留 | 「固定第三方补丁」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [peri-agent/README.md](../../peri-agent/README.md) | 保留 | 「peri-agent」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [peri-tui-components/README.md](../../peri-tui-components/README.md) | 保留 | 「Terminal components」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [peri-wasm/README.md](../../peri-wasm/README.md) | 保留 | 「peri-wasm」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [side-projects/agent-defect-analyzer/README.md](../../side-projects/agent-defect-analyzer/README.md) | 保留 | 「Agent Defect Analyzer」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [side-projects/daytona/README.md](../../side-projects/daytona/README.md) | 保留 | 「peri-sandbox」是使用/开发入口；实现事实由其指向的代码与设计核对。 |
| [side-projects/peri-db-viewer/README.md](../../side-projects/peri-db-viewer/README.md) | 保留 | 「Peri DB Viewer」是使用/开发入口；实现事实由其指向的代码与设计核对。 |

补充处置（原盘点范围外、因导航修复实际修改的 active spec）：

| 文档 | 处置 | 理由与结果 |
| --- | --- | --- |
| [spec/issues/2026-11-01-thread-mq-implementation.md](../../spec/issues/2026-11-01-thread-mq-implementation.md) | 改写 | 修复两处 Thread MQ 相对链接，不改变实施状态或验收声明。 |

新增 `spec/issues/2026-11-18-session-durability-verification.md` 接收设计中的实施顺序与历史 SQL 仿真记录；这些历史验证本轮未重跑。
