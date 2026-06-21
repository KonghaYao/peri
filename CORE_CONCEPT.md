1. Agent Loop: 不断循环"Model 思考 → Tool 执行 → Model 再思考"直到 LLM 给出最终回答。

2. Model: 分层封装的统一 LLM 调用层。
   - BaseModel trait：OpenAI / Anthropic provider 适配
   - BaseModelReactLLM：在 BaseModel 之上实现 ReAct 协议
   - ReactLLM：对外暴露的高层接口
   - RetryableLLM：外层包裹，提供指数退避重试

3. Tool: 三层工具系统。
   - CoreTools：12 个核心工具，始终对 LLM 可见
   - ToolSearch：SearchExtraTools / ExecuteExtraTool，按需发现并执行 Deferred 工具
   - DeferredTools：Cron / MCP / LSP 等延迟加载工具，对 LLM 不可见

4. Middleware: 固定顺序的中间件链，通过生命周期钩子织入 ReAct 循环。
   - collect_tools：向 agent 注入工具，Agent 启动前合并到工具表
   - before_agent：Agent 启动前的初始化，如注入 system prompt、冻结会话数据
   - before_model：调用 LLM 前的预处理，如上下文压缩、目标提醒
   - after_model：LLM 返回后的后处理，如解析响应内容
   - before_tools_batch：批量工具并发执行前的处理，如 HITL 批量审批
   - before_tool：单个工具执行前的拦截，如 HITL 审批
   - after_tool：单个工具执行后的处理，如 Git 归因追踪
   - after_tools_batch：一批工具全部写入 state 后的聚合处理
   - after_agent：Agent 结束后的收尾，如清理临时状态、归档会话
   - on_error：执行失败的统一兜底处理

5. LLM Messages: 统一的消息表示层，支持多角色和多模态内容。
   - BaseMessage：统一消息类型，包含角色、ID、内容和工具调用列表
   - ContentBlock：标准内容块枚举，支持 Text/Image/Document/ToolUse/ToolResult 等类型
   - MessageContent：消息内容包装，支持 Text/Blocks/Raw 三种形式

6. AgentEvent: Agent 执行过程中的增量事件流，驱动 TUI 渲染和持久化。
   - AiReasoning：AI 推理内容（思考过程）
   - TextChunk：LLM 输出增量文字
   - ToolStart：工具调用开始
   - ToolEnd：工具调用结束
   - BgToolStep：后台 agent 工具调用进度
   - StateSnapshot：完整消息历史快照
   - MessageAdded：增量消息
   - LlmCallStart：LLM 调用开始
   - LlmCallEnd：LLM 调用结束
   - LlmRetrying：LLM 调用重试中
   - ContextWarning：上下文窗口使用警告
   - SubagentStarted：子 agent 开始执行
   - SubagentStopped：子 agent 执行完成
   - BackgroundTaskCompleted：后台 agent 任务完成
   - CompactStarted：上下文压缩开始
   - CompactCompleted：上下文压缩完成
   - CompactError：上下文压缩失败
   - RewindCompleted：对话回退完成
   - TodoUpdate：Todo 列表更新
   - LspDiagnostics：LSP 诊断更新
   - AgentExecutionFailed：Agent 执行失败

7. Token 用量管理: 跟踪会话 token 消耗并触发压缩机制，防止上下文窗口溢出，数值在 TUI 状态栏中持续更新。
   - TokenTracker：累计输入/输出/缓存 token 的会话级追踪器
   - ContextBudget：定义 compact 触发阈值和警告阈值的上下文预算

8. Compact: 通过摘要和内容裁剪控制上下文大小，支持自动触发和手动调用。
   - full_compact：通过 LLM 生成结构化摘要
   - micro_compact_enhanced：原地压缩图片、文档、工具结果为占位符
   - re_inject：按需将压缩的文件信息重注入上下文

9. Frozen Data 机制: 会话级不可变数据，保证系统提示词稳定性和 prompt cache 效率。
   - FrozenSessionData：会话创建时一次性捕获 system_prompt 和 claude_md 等
   - SubAgent 透传：通过 with_frozen_data 传递，保证 SubAgent 与 Main Agent 看到相同内容

10. SessionManager 会话管理器: 管理会话生命周期、状态和运行时 Agent 实例。
    - AcpSession：会话状态记录，包含消息历史、provider 设置和权限模式
    - AgentRuntime：运行时 Agent 实例包装，携带 cancel_token 和取消策略

11. Slash Commands 斜杠命令: 在 executor 入口拦截的特殊命令，按执行方式分类。
    - Immediate：直接执行不构建 Agent，如 /compact 和 /rewind
    - Passthrough：透传到正常 Agent 管线
    - Transform：变换 prompt 内容后传给 Agent

12. 遥测系统: 基于 Langfuse 的可观测性追踪，记录 LLM 调用和工具执行。
    - LangfuseTracer：每轮对话的追踪器实例，记录完整生命周期
    - SubAgent 栈：支持嵌套 SubAgent 调用的层级追踪

13. Claude Code Hook 系统: 兼容 Claude Code 的插件钩子，在 Agent 生命周期关键点触发外部脚本或评估，通过 hooks.json 配置。
    - HookEvent：生命周期事件类型
      - 工具相关：PreToolUse / PostToolUse / PostToolUseFailure / PostToolBatch
      - 会话相关：SessionStart / SessionEnd / UserPromptSubmit / Notification
      - Agent 相关：Stop / StopFailure / SubagentStart / SubagentStop
      - Compact 相关：PreCompact / PostCompact
      - 权限：PermissionRequest
    - HookType：四种执行方式
      - Command：Shell 命令执行
      - Prompt：LLM 提示词评估
      - Http：HTTP POST 请求
      - Agent：子 Agent 完整循环
    - HookAction：八种返回动作
      - Allow：允许继续
      - Block：阻止操作
      - ModifyInput：修改工具输入
      - PermissionOverride：覆盖权限决策
      - PreventContinuation：阻止 agent 继续
      - SystemMessage：注入系统消息
      - AdditionalContext：追加上下文
      - InitialUserMessage：追加初始消息

14. SubAgent 系统: 子 Agent 执行与任务委托机制，支持多种运行模式。
    - SubAgent 工具：通过 Agent 工具按 subagent_type 派发子任务
    - Fork 模式：继承父 agent 全部对话历史和工具集，在现有上下文中继续工作
    - Background 模式：通过 /bg 或 run_in_background 后台独立运行，完成后通知主 agent
    - BackgroundTaskRegistry：管理最多 3 个并发后台任务，提供状态查询和取消
    - Built-in Agents：预定义的子 agent 类型，如 explorer / coder / plan

15. Skills 系统: 专项能力扫描与注入系统，加载 SKILL.md 定义并生成摘要注入系统提示词。
    - SkillsMiddleware：在 before_agent 阶段扫描 skills 目录，生成摘要前插到消息历史
    - SkillSource：五种来源优先级，User → Global → Project → Plugin → Builtin
    - Builtin Skills：随二进制编译期嵌入的内置 skill，最低优先级可被同名覆盖
    - 触发方式：用户 /skill-name 触发，加载完整 SKILL.md 内容
    - Frozen Summary：session/new 时一次性冻结摘要，会话内不再重新扫描

16. LSP 集成: 语言服务器协议集成，在文件编辑后自动同步获取代码诊断。
    - LspMiddleware：通过 after_tool 钩子在 Write/Edit 后同步文件内容
    - LspServerPool：管理多个 LSP 服务器实例的生命周期
