# ToolSearch 核心工具声明与当前会话工具视图不一致

**状态**：已修复（2026-08-22）
**优先级**：高
**类型**：缺陷
**创建日期**：2026-08-22
**来源**：用户报告 + 静态代码核查

## 问题

`SearchExtraTools` 与 `ExecuteExtraTool` 的工具描述通过 `CORE_TOOL_NAMES` 生成固定的 14 项“Core tools”清单，并无条件声明这些工具“always available”。但该清单不是当前 session 实际可调用工具的事实源。

实际工具集在每个 session/turn 中由中间件链收集，应用 disabled middleware、agent allowlist/disallowlist 等过滤，再通过 `BaseTool::is_direct()` 筛选后发送给模型。因此某个 Core 工具未装配、被禁用或被过滤时，模型仍会从 ToolSearch 元工具描述中获知它“始终可用”，造成能力声明与运行时注册表冲突。

`--bare` 仅跳过 plugins、MCP、settings hooks、LSP 等初始化；它不应移除文件、终端或 Web 基础工具。若 bare 会话只看到元工具，需另查 agent tool filter 或 MetaHarness 配置；但无论触发条件是什么，静态能力声明的缺陷均成立。

## 根因

- `peri-middlewares/src/tool_search/core_tools.rs` 维护静态 `CORE_TOOL_NAMES`。
- `SearchExtraTools::new` 与 `ExecuteExtraTool::new` 在构造时把该清单写入 description。
- `ToolSearchMiddleware::before_agent` 已能取得本轮实际 `direct_arcs`，却只将其用于 prompt declarations，没有用于更新元工具的能力说明。
- Stage builder 的本地工具视图才是 session 内工具可见性的事实源；模型请求中进一步以 `is_direct()` 过滤。

## 影响

- 模型可能声称拥有未挂载的文件、Shell、Web 或其他核心工具。
- 模型可能因元工具文字而跳过 `SearchExtraTools`，却在实际调用时才发现工具不存在。
- 用户无法区分工具未注册、被 session filter 移除，还是工具服务故障。

## 修复目标

1. 直接调用工具的能力说明必须依据当前 session 的实际 direct tool 集合生成。
2. 静态 Core 工具名只能作为分类、兼容或测试基线，不能作为运行时能力声明。
3. Core 工具未装配、被禁用或被 agent filter 排除时，元工具描述不得称其为 `always available`。
4. 为至少一个缺少部分 Core 工具的会话工具集添加回归测试，断言元工具声明只列出实际 direct tools。
5. 保持 deferred tool 的 `SearchExtraTools → ExecuteExtraTool` 调用路径不变。

## 相关但独立的问题：MCP 计数与健康状态

- `MCP connected, 9 tools` 表示单个 MCP handle 在初始化时获得的远端工具数量。
- `SearchExtraTools.total_available = 10` 表示本 session 全部 deferred 工具的索引数量，可能额外包含 artifact、cron、workflow、LSP、goal 等非 MCP 工具；这不是仅凭 9 vs 10 即可判定的计数错误。
- 返回值缺少来源或 namespace 分类，容易误导。后续可增加按来源的计数说明。
- `ClientStatus::Connected` 表示 MCP transport/握手成功，不保证每个工具依赖的下游服务持续可用。需要时另行设计 `backend reachable` 或 `last tool call` 健康状态，不能将其混同为 transport 连接状态。

## 验收标准

- [x] 直接工具说明由当前实际 direct tools 生成。
- [x] 被过滤或未装配的 Core 工具不会被描述为可直接调用或始终可用。
- [x] 默认完整工具集仍正确声明其实际 direct tools。
- [x] deferred 工具搜索与 `ExecuteExtraTool` 代理执行行为不回归。
- [ ] 针对性测试通过，且 `cargo fmt --check` 与 `git diff --check` 通过。

## 文档与代码核验（2026-08-22）

代码核验确认 `ToolSearchMiddleware::before_agent` 从每 turn 的 local tool view 收集 direct/deferred 集合，并刷新 `SearchExtraTools` / `ExecuteExtraTool` 描述；`build_session_tool_view` 仍是 disabled/filter 后能力集合的事实源。对应稳定约束已同步到 `ARC-TOOLS-001` 与 `docs/code-index/{peri-agent,peri-middlewares}.md`。最后一项保留到本次文档维护批次实际运行测试和格式检查后关闭。
