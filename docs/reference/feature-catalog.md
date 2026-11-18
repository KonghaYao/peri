# 功能目录

> 状态：当前源码盘点；评级为本次 #446 提案，待核对未随工单附送的旧裁决。
> 本表是功能登记入口，不是运行时 owner/开关表，不覆盖所链接契约。

分级规则只在 [功能登记标准](../standards/features.md) 维护；实现和验证状态见 [验收映射](../verification/feature-acceptance.md)。稳定 ID 不因重排或等级改变而改变；退役 ID 不复用。每行可见字符包含 ID、等级、功能、边界和链接标签，不含 URL 与表格分隔符，最多 100 字。

本次从 Cargo workspace、MCP handlers、middleware 模块、ACP 请求、TUI 面板、SDK/Rivet 入口交叉盘点；源码位置见验收表。内部辅助函数、参数组合、重复入口不各算能力。159 项旧草表未附送，不能虚构逐项迁移对应；旧表对账仍是明确缺口。此表范围为 Peri 产品能力和直接支持的 SDK/插件；side-projects 实验不视为已发布能力。

| ID | 等级 | 功能与边界 | 权威来源 |
| --- | --- | --- | --- |
| F-CORE-001 | L1 | RCRA 循环；Receive 决定正常队列耗尽退出 | [契约](../design/architecture.md) |
| F-CORE-002 | L1 | 模型推理；工具调用与文本输出进入同一执行链 | [契约](../design/architecture.md) |
| F-CORE-003 | L1 | 工具分派；结果按调用身份回填 | [契约](../design/architecture.md) |
| F-CORE-004 | L1 | 停止执行；取消信号向阶段与工具传播 | [契约](../design/architecture.md) |
| F-CORE-005 | L1 | 会话冻结；已有会话不因配置变化扩权 | [契约](../design/meta-harness.md) |
| F-CORE-006 | L2 | 执行预算；迭代和持久工作预算各自有界 | [契约](../design/architecture.md) |
| F-CORE-007 | L2 | 执行失败；原因链保真并产生可观察终态 | [契约](../design/architecture.md) |
| F-CORE-008 | L2 | 事件顺序；交付水位先于相关回答发布 | [契约](../design/architecture.md) |
| F-CORE-009 | L2 | 资源收尾；未结清子作用域不得伪装完成 | [契约](../design/architecture.md) |
| F-CTRL-001 | L1 | 能力装配；MetaHarness 经 Middleware 控制 Harness | [契约](../design/middleware-system.md) |
| F-CTRL-002 | L1 | 能力关闭；发现、解析、执行与注入同步收敛 | [契约](../design/middleware-system.md) |
| F-CTRL-003 | L1 | 派生控制；子链、fork、workflow 继承冻结事实 | [契约](../design/middleware-system.md) |
| F-CTRL-004 | L2 | 中间件生命周期；执行 hooks 遵循链序 | [契约](../design/middleware-system.md) |
| F-CTRL-005 | L2 | 声明诊断；重复身份、未知依赖和冲突显式失败 | [契约](../design/middleware-system.md) |
| F-CTRL-006 | L2 | 宿主注入；端口实现不成为第二能力权威 | [契约](../design/middleware-system.md) |
| F-PROMPT-001 | L2 | 静态段落；只收集已装配能力声明 | [契约](../design/system-prompt.md) |
| F-PROMPT-002 | L2 | 请求期贡献；动态注入服从同源能力事实 | [契约](../design/system-prompt.md) |
| F-PROMPT-003 | L2 | 段落覆盖；不能复活缺席能力 | [契约](../design/meta-harness.md) |
| F-PROMPT-004 | L2 | 缓存前缀；动态信息不得进入静态缓存 | [契约](../design/system-prompt.md) |
| F-PROMPT-005 | L2 | Provider 投影；内部边界标记不泄漏 wire | [契约](../design/system-prompt.md) |
| F-PROMPT-006 | L3 | Persona 与语言；基础文本来源独立可追踪 | [契约](../design/system-prompt.md) |
| F-PROMPT-007 | L3 | 环境上下文；日期平台与 Git 信息按冻结契约注入 | [契约](../design/system-prompt.md) |
| F-COMPACT-001 | L2 | Micro compact；预算触发上下文投影压缩 | [契约](../design/micro-compact.md) |
| F-COMPACT-002 | L2 | Full compact；摘要提交与原始历史分离 | [契约](../design/micro-compact.md) |
| F-COMPACT-003 | L2 | 原文回查；压缩后可按引用读取原始内容 | [契约](../design/micro-compact.md) |
| F-COMPACT-004 | L2 | 压缩失败；不提交半成品历史 | [契约](../design/micro-compact.md) |
| F-COMPACT-005 | L2 | 工具配对修补；只改模型视图不重跑工具 | [契约](../design/micro-compact.md) |
| F-COMPACT-006 | L3 | Token 预算；估计使用同一请求投影 | [契约](../design/micro-compact.md) |
| F-COMPACT-007 | L3 | fork 历史；继承视图与压缩生命周期一致 | [契约](../design/micro-compact.md) |
| F-COMPACT-008 | L3 | rewind 历史；失效投影不跨回退复用 | [契约](../design/micro-compact.md) |
| F-SESSION-001 | L1 | 创建会话；身份与执行目录分离 | [契约](../design/session-id-environment.md) |
| F-SESSION-002 | L1 | 加载会话；读取持久历史并保留冻结上下文 | [契约](../design/session-id-environment.md) |
| F-SESSION-003 | L1 | 关闭会话；取消与结果交接完成后释放资源 | [契约](../design/session-id-environment.md) |
| F-SESSION-004 | L2 | 切换会话；先关闭旧执行再发布新身份 | [契约](../design/session-id-environment.md) |
| F-SESSION-005 | L2 | 会话发现；机器工作区绑定不等于持有权 | [契约](../design/session-id-environment.md) |
| F-SESSION-006 | L2 | 工作区重定位；身份不随路径改名漂移 | [契约](../design/session-id-environment.md) |
| F-SESSION-007 | L2 | 跨工作区续作；新环境使用新会话 | [契约](../design/session-id-environment.md) |
| F-SESSION-008 | L3 | 会话归档；列表位置不等于删除与停止 | [契约](../design/session-id-environment.md) |
| F-SESSION-009 | L3 | 会话删除；遵守持久记录与运行资源边界 | [契约](../design/session-id-environment.md) |
| F-MQ-001 | L1 | 持久收件箱；消息是待处理输入事实源 | [契约](../design/thread-mq.md) |
| F-MQ-002 | L1 | 原子 Receive；接纳与交付记录保持一致 | [契约](../design/thread-mq.md) |
| F-MQ-003 | L1 | 子任务结果；可靠交接后才结算责任 | [契约](../design/thread-mq.md) |
| F-MQ-004 | L2 | 用户队列；投递与取回不重放历史 | [契约](../design/thread-mq.md) |
| F-MQ-005 | L2 | 晚到消息；按所属会话激活消费 | [契约](../design/thread-mq.md) |
| F-MQ-006 | L2 | 终态幂等；重复交付不生成重复结果 | [契约](../design/thread-mq.md) |
| F-MQ-007 | L2 | 恢复边界；加载持久消息不恢复旧进程执行 | [契约](../design/thread-mq.md) |
| F-HISTORY-001 | L2 | 标准历史回放；未协商扩展时遵循 ACP | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-002 | L2 | History V1；V2 beta 关闭时保留回滚路径 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-003 | L2 | History V2 协商；请求后未选中须显式失败 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-004 | L2 | 历史 initial；加载响应带窗口初始快照 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-005 | L2 | 历史分页；游标与窗口身份隔离 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-006 | L2 | 历史 seek；定位后保留合法 live 尾部 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-007 | L2 | 历史正文；按内容版本和预算加载 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-008 | L2 | 历史失效；迟到响应不能污染新窗口 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-009 | L2 | 继承历史索引；未就绪不退化为无限扫描 | [契约](../design/peri-acp-protocol.md) |
| F-HISTORY-010 | L3 | 只读历史；展示不夺取执行所有权 | [契约](../design/peri-acp-protocol.md) |
| F-TASK-001 | L2 | 子 Agent 委派；恒后台返回任务回执 | [契约](../design/session-async-tasks.md) |
| F-TASK-002 | L2 | 并发子任务；身份与结果归属相互隔离 | [契约](../design/session-async-tasks.md) |
| F-TASK-003 | L2 | 子任务 follow-up；活动中输入进入同一任务 | [契约](../design/session-async-tasks.md) |
| F-TASK-004 | L2 | 子任务 resume；非活动执行重新建立运行资源 | [契约](../design/session-async-tasks.md) |
| F-TASK-005 | L2 | 子任务取消；清理完成才发布取消终态 | [契约](../design/session-async-tasks.md) |
| F-TASK-006 | L2 | 子任务失败；启动失败不退回同步执行 | [契约](../design/session-async-tasks.md) |
| F-TASK-007 | L2 | 子任务完成；父链消费可靠终态结果 | [契约](../design/session-async-tasks.md) |
| F-TASK-008 | L2 | 子任务 Shell；晚到结果仍有消费责任 | [契约](../design/session-async-tasks.md) |
| F-TASK-009 | L3 | 子任务配置；定义、模型层级与工具范围受限 | [契约](../design/session-async-tasks.md) |
| F-TASK-010 | L3 | 旧参数兼容；run_in_background 不恢复同步模式 | [契约](../design/session-async-tasks.md) |
| F-INTERACT-001 | L1 | 工具权限；真实目标执行受审批约束 | [契约](../design/interaction-brokers.md) |
| F-INTERACT-002 | L1 | 交互归属；审批不能跨会话或运行身份回填 | [契约](../design/interaction-brokers.md) |
| F-INTERACT-003 | L2 | 权限模式；自动分类与显式批准边界可诊断 | [契约](../design/interaction-brokers.md) |
| F-INTERACT-004 | L2 | 用户提问；选项与自由输入回到原请求 | [契约](../design/interaction-brokers.md) |
| F-INTERACT-005 | L2 | 交互取消；关闭与迟到回复不得悬挂执行 | [契约](../design/interaction-brokers.md) |
| F-MCP-001 | L2 | MCP 连接；stdio 与 HTTP 由统一 client 消费 | [契约](../design/tool-system.md) |
| F-MCP-002 | L2 | 工具发现；direct 与 deferred 来自同一事实 | [契约](../design/tool-system.md) |
| F-MCP-003 | L2 | 工具搜索；SearchExtraTools 返回可用 schema | [契约](../design/tool-system.md) |
| F-MCP-004 | L2 | 间接执行；ExecuteExtraTool 保留真实目标权限 | [契约](../design/tool-system.md) |
| F-MCP-005 | L2 | MCP 资源；list、read 与模板由 provider 提供 | [契约](../design/tool-system.md) |
| F-MCP-006 | L2 | MCP 订阅；通知按 session/task 归属消费 | [契约](../design/tool-system.md) |
| F-MCP-007 | L2 | MCP 任务；get、cancel 与终态独立于工具响应 | [契约](../design/tool-system.md) |
| F-MCP-008 | L2 | MCP 重连；重建目录不复活已关闭能力 | [契约](../design/tool-system.md) |
| F-MCP-009 | L2 | MCP OAuth；鉴权结果按连接身份交付 | [契约](../design/tool-system.md) |
| F-MCP-010 | L2 | MCP 单出口；builtin 能力由 peri router 汇聚 | [契约](../design/mcp-v5-single-exit.md) |
| F-MCP-011 | L2 | MCP 启动诊断；不可用能力显式报错 | [契约](../design/tool-system.md) |
| F-MCP-012 | L3 | 动态注册；ACP 控制面注册不等于懒加载 | [契约](../design/acp-mcp-control.md) |
| F-MCP-013 | L3 | MCP 缓存；响应复用不替代当前能力目录 | [契约](../design/mcp-cache.md) |
| F-MCP-014 | L3 | MCP 独立服务；mcp-start 提供标准传输 | [契约](../design/tool-system.md) |
| F-MCP-015 | L3 | ToolSearch 别名；目标接口尚在独立 PR | [契约](../design/tool-system.md) |
| F-MCP-016 | L3 | Dynamic 懒加载；显式 Loader 前不得启动目标 | [契约](../design/tool-system.md) |
| F-WORKSPACE-001 | L2 | 文件读取；范围与媒体投影由 workspace 提供 | [契约](../design/tool-system.md) |
| F-WORKSPACE-002 | L2 | 文件写入；失败不伪造成功副作用 | [契约](../design/tool-system.md) |
| F-WORKSPACE-003 | L2 | 文件编辑；匹配条件满足才应用修改 | [契约](../design/tool-system.md) |
| F-WORKSPACE-004 | L2 | 文件搜索；Glob 与 Grep 校验模式再遍历 | [契约](../design/tool-system.md) |
| F-WORKSPACE-005 | L3 | 目录操作；路径解析与错误诊断在 provider | [契约](../design/tool-system.md) |
| F-WORKSPACE-006 | L2 | Bash 执行；进程组状态决定终态 | [契约](../design/tool-system.md) |
| F-WORKSPACE-007 | L2 | 后台 Shell；任务回执与输出持续可读 | [契约](../design/tool-system.md) |
| F-WORKSPACE-008 | L2 | Shell 取消；进程资源收尾后确认取消 | [契约](../design/tool-system.md) |
| F-WORKSPACE-009 | L2 | 文件回退；校验 pre-image 与变更集再应用 | [契约](../design/tool-system.md) |
| F-WORKSPACE-010 | L3 | 项目指令；workspace 资源提供展开后的正文 | [契约](../design/tool-system.md) |
| F-WORKSPACE-011 | L3 | 技能发现；User、Project、Plugin 来源聚合 | [契约](../design/tool-system.md) |
| F-WORKSPACE-012 | L3 | 技能加载；渐进读取不复制提供方扫描逻辑 | [契约](../design/tool-system.md) |
| F-WORKSPACE-013 | L3 | Agent 目录；内置与插件定义由资源提供 | [契约](../design/tool-system.md) |
| F-WORKSPACE-014 | L3 | Git 观察；有订阅者才采样 ref 变化 | [契约](../design/git-watch-middleware.md) |
| F-WORKSPACE-015 | L4 | 路径诊断；候选建议不掩盖原始失败 | [契约](../design/tool-system.md) |
| F-WORKSPACE-016 | L3 | 大输出存档；正文截断后保留可回查引用 | [契约](../design/tool-system.md) |
| F-EXT-001 | L3 | Web 搜索；搜索能力由 web MCP 提供 | [契约](../design/tool-system.md) |
| F-EXT-002 | L3 | Web 抓取；页面结果走统一工具边界 | [契约](../design/tool-system.md) |
| F-EXT-003 | L3 | Artifact；成果读写与资源访问独立于执行 | [契约](../design/tool-system.md) |
| F-EXT-004 | L3 | Cron；触发事实经会话控制路径处理 | [契约](../design/session-async-tasks.md) |
| F-EXT-005 | L3 | Monitor 启动；command source 返回标准任务 | [契约](../design/monitor-mcp.md) |
| F-EXT-006 | L3 | Monitor 读取；有界事件环显式报告丢弃 | [契约](../design/monitor-mcp.md) |
| F-EXT-007 | L3 | Monitor 提醒；运行命中与终态经订阅投递 | [契约](../design/monitor-mcp.md) |
| F-EXT-008 | L3 | Monitor 停止；终态仍允许有界结果读取 | [契约](../design/monitor-mcp.md) |
| F-EXT-009 | L3 | 配置资源；读取有效配置而非重复合并 | [契约](../design/configuration-authority.md) |
| F-EXT-010 | L3 | 凭据资源；密钥访问保留专属能力边界 | [契约](../design/acp-mcp-control.md) |
| F-WORKFLOW-001 | L2 | Workflow 执行；节点按 runner 契约推进 | [契约](../design/workflow.md) |
| F-WORKFLOW-002 | L2 | Workflow 委派；Agent 节点使用内部子任务机制 | [契约](../design/workflow.md) |
| F-WORKFLOW-003 | L2 | Workflow 状态；journal 与结果按运行身份关联 | [契约](../design/workflow.md) |
| F-WORKFLOW-004 | L2 | Workflow 取消；清理与完成确认不互相冒充 | [契约](../design/workflow.md) |
| F-WORKFLOW-005 | L3 | Workflow 恢复；遵循任务边界而非重放全部历史 | [契约](../design/workflow.md) |
| F-WORKFLOW-006 | L3 | Workflow 进度；资源投影供宿主与 TUI 消费 | [契约](../design/workflow.md) |
| F-WORKFLOW-007 | L3 | Workflow MCP；模型工具与宿主回填信道分离 | [契约](../design/workflow.md) |
| F-WORKFLOW-008 | L4 | Ultra-ADLC；大交付按文件协议编排 | [契约](../design/workflow.md) |
| F-CONFIG-001 | L2 | 配置解析；typed resolver 产生有效快照 | [契约](../design/configuration-authority.md) |
| F-CONFIG-002 | L2 | 配置作用域；目录与 settings 路径决定视图 | [契约](../design/configuration-authority.md) |
| F-CONFIG-003 | L2 | 配置保存；expected revision 保护编辑基线 | [契约](../design/configuration-authority.md) |
| F-CONFIG-004 | L2 | Provider 配置；端点、模型与凭据按来源校验 | [契约](../design/configuration-authority.md) |
| F-CONFIG-005 | L3 | 配置重载；旧快照不隐式改变既有会话 | [契约](../design/configuration-authority.md) |
| F-CONFIG-006 | L3 | Beta 开关；启动期请求与连接协商分离 | [契约](../design/configuration-authority.md) |
| F-CONFIG-007 | L3 | 插件安装；作用域与来源准入保持明确 | [契约](../design/configuration-authority.md) |
| F-CONFIG-008 | L3 | 插件市场；目录刷新不替代安装事实 | [契约](../design/configuration-authority.md) |
| F-CONFIG-009 | L3 | 命令扩展；插件命令通过统一入口执行 | [契约](../design/configuration-authority.md) |
| F-MODEL-001 | L1 | Anthropic 请求；消息与工具转换遵守 wire 契约 | [契约](../design/model-adapters.md) |
| F-MODEL-002 | L2 | OpenAI Compatible；兼容接口保留适配差异 | [契约](../design/model-adapters.md) |
| F-MODEL-003 | L2 | OpenAI Responses；响应事件转换为统一流 | [契约](../design/model-adapters.md) |
| F-MODEL-004 | L2 | 模型流解析；增量与终态不相互覆盖 | [契约](../design/model-adapters.md) |
| F-MODEL-005 | L2 | 请求取消；传输中断回到执行控制链 | [契约](../design/model-adapters.md) |
| F-MODEL-006 | L2 | 模型重试；保留错误分类与预算 | [契约](../design/model-adapters.md) |
| F-MODEL-007 | L3 | 推理预算；thinking 与 effort 按 provider 投影 | [契约](../design/model-adapters.md) |
| F-MODEL-008 | L3 | Token 用量；模型计量与本地估计明确区分 | [契约](../design/model-adapters.md) |
| F-TUI-001 | L2 | 输入提交；绑定提交时的会话身份 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-002 | L2 | 流式消息；ACP 事件投影为稳定展示状态 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-003 | L2 | 工具活动；调用身份连接开始、输出和终态 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-004 | L2 | 待发送队列；取回与投递有明确用户反馈 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-005 | L2 | 历史浏览；窗口翻页不阻塞 live 尾部 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-006 | L3 | 子 Agent 面板；活动和终态保留所属身份 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-007 | L3 | 任务面板；查询取消与输出引用可追踪 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-008 | L3 | 审批面板；原交互取消后不接受迟到操作 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-009 | L3 | 问答面板；选择与自由输入反馈同一请求 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-010 | L3 | 配置面板；保存后发布 accepted snapshot | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-011 | L3 | 模型面板；选择与设置作用域可见 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-012 | L3 | 插件面板；安装状态与错误可见 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-013 | L3 | MCP 面板；连接状态、鉴权与错误可见 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-014 | L3 | Workflow 面板；运行与节点状态分开投影 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-015 | L3 | Cron 面板；任务管理消费同一服务事实 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-016 | L3 | 会话浏览器；列表选择不等于执行所有权 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-017 | L4 | 主题切换；展示配色不改变业务状态 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-018 | L4 | Markdown 渲染；流式增量保持文本语义 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-019 | L4 | 鼠标交互；命中范围与绘制范围一致 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-020 | L4 | 输入编辑；Unicode、光标与滚动正确 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-021 | L4 | GenUI 展示；结构化内容按协议投影 | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-022 | L5 | 滚动条样式；纯展示细节不单设 E2E | [契约](../design/tui-acp-data-flow.md) |
| F-TUI-023 | L5 | 提示文案；不承担权限与状态判定 | [契约](../design/tui-acp-data-flow.md) |
| F-SDK-001 | L2 | ChatManager；管理 Chat 句柄与生命周期 | [契约](../design/peri-sdk-chat.md) |
| F-SDK-002 | L2 | ChatService；业务动作进入同一执行入口 | [契约](../design/peri-sdk-chat.md) |
| F-SDK-003 | L2 | ChatView；只读事实投影不成为控制面 | [契约](../design/peri-sdk-chat.md) |
| F-SDK-004 | L2 | ChatReplica；同步展示与服务事实分离 | [契约](../design/peri-sdk-chat.md) |
| F-SDK-005 | L1 | 会话执行归属；SDK 管理唯一执行者 | [契约](../design/peri-sdk-chat.md) |
| F-SDK-006 | L2 | Runtime 适配；ACP 后端可替换而不复制规则 | [契约](../design/peri-sdk-chat.md) |
| F-SDK-007 | L3 | SDK 数据库；持久事实与运行句柄分离 | [契约](../design/peri-sdk-chat.md) |
| F-SDK-008 | L3 | 专用装配；agent-builder 组合通用 SDK 能力 | [契约](../design/peri-sdk-chat.md) |
| F-RIVET-001 | L2 | ChatActor；Rivet action 接入 SDK Chat | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-RIVET-002 | L2 | SandboxActor；工作区资源生命周期独立 | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-RIVET-003 | L2 | 活动账本；内部任务与待交付结果阻止误休眠 | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-RIVET-004 | L2 | KeepAwake；未知活动不能判定空闲 | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-RIVET-005 | L3 | 资源查询；只读查询不新增调度状态 | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-RIVET-006 | L3 | Sandbox MCP；HTTP descriptor 绑定资源引用 | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-RIVET-007 | L3 | 扩容与 drain；活跃执行连续性待真实验收 | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-RIVET-008 | L3 | 长时运行；24h 连续性须真实时间证据 | [契约](../../npm-packages/peri-rivet/DESIGN.md) |
| F-PLATFORM-001 | L3 | WASM 运行；复用同一 Harness 与协议语义 | [契约](../design/serverless-execution-identity.md) |
| F-PLATFORM-002 | L3 | 执行环境身份；宿主实例与会话身份分离 | [契约](../design/serverless-execution-identity.md) |
| F-PLATFORM-003 | L3 | Sandbox 启动；显式工作区和运行策略 | [契约](../design/serverless-execution-identity.md) |
| F-PLATFORM-004 | L3 | 时间适配；wall 与 monotonic 边界分离 | [契约](../design/time-runtime.md) |
| F-PLATFORM-005 | L3 | JS Runtime；脚本能力不直接持有业务权威 | [契约](../design/serverless-execution-identity.md) |
| F-PLATFORM-006 | L3 | 遥测；Langfuse 消费观测事实不控制执行 | [契约](../design/architecture.md) |
| F-PLATFORM-007 | L4 | 内存治理；预算释放与长时证据独立追踪 | [契约](../../spec/issues/2026-11-18-v5-memory-447.md) |
| F-PLATFORM-008 | L4 | 组件消费；TUI 业务与纯组件 crate 分离 | [契约](../standards/tui.md) |
| F-PLATFORM-009 | L4 | Memory 插件；MCP 与 CLI 提供独立记忆能力 | [契约](../../npm-packages/memory-plugin/DESIGN.md) |
| F-DURABLE-001 | L1 | 持久历史；canonical 与模型投影分离 | [契约](../design/message-transcript.md) |
| F-DURABLE-002 | L2 | 进入会话恢复；work_state 决定新运行准入 | [契约](../design/session-durability.md) |
| F-DURABLE-003 | L2 | 恢复子树；子结果路由与父绑定重新校验 | [契约](../design/session-durability.md) |
| F-DURABLE-004 | L2 | 恢复快照；不复活旧进程或扩大冻结能力 | [契约](../design/session-durability.md) |
| F-APPS-001 | L3 | MCP Apps 附件；绑定可信记录与连接身份 | [契约](../design/mcp-multiplexing.md) |
| F-APPS-002 | L3 | MCP Apps relay；请求多路隔离不串应用 | [契约](../design/mcp-multiplexing.md) |
| F-APPS-003 | L3 | MCP Apps 清理；idle 与关闭释放连接资源 | [契约](../design/mcp-multiplexing.md) |
| F-EXT-011 | L3 | Hook 扩展；生命周期调用受装配控制 | [契约](../design/middleware-system.md) |
| F-EXT-012 | L3 | At mention；显式引用转换为请求上下文 | [契约](../design/system-prompt.md) |
| F-EXT-013 | L3 | Goal 提醒；目标提示不另建执行控制面 | [契约](../design/system-reminder.md) |
| F-EXT-014 | L3 | Todo 管理；任务清单不成为执行调度器 | [契约](../design/tool-system.md) |
| F-EXT-015 | L3 | 图像输入；媒体转换遵循模型协议边界 | [契约](../design/model-adapters.md) |
| F-CLI-001 | L3 | 非交互输出；print 模式复用 Agent 执行链 | [契约](../design/architecture.md) |
| F-CLI-002 | L3 | 身份登录；认证结果供模型连接使用 | [契约](../design/configuration-authority.md) |
| F-CLI-003 | L4 | 版本更新；升级检查与运行会话分离 | [契约](../design/architecture.md) |
| F-CLI-004 | L3 | 元数据查询；只读查询不更改执行事实 | [契约](../design/meta-control.md) |
| F-CLI-005 | L3 | Workflow CLI；运行与分发共用工作流语义 | [契约](../design/workflow.md) |
| F-CLI-006 | L3 | 启动配置；显式参数进入统一配置路径 | [契约](../design/configuration-authority.md) |
