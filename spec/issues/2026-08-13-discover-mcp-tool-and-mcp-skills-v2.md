# DiscoverMCP 工具 + MCP Skills 命令注入（v2）

**状态**：Open
**优先级**：中
**类型**：功能
**创建日期**：2026-08-13
**来源**：2026-08-13 需求访谈（/interview 定案）+ `spec/issues/2026-08-13-mcp-skills-from-resources.md`（superseded）
**最后核查**：2026-08-13

## 任务定义

两部分：

1. **DiscoverMCP Tool**：给 agent 的只读 MCP 域查询工具（deferred）。agent 经它搜索 MCP server / tool / resource / skill、拉取 server 详情与清单。**不提供执行**——MCP 工具执行仍只走 `ExecuteExtraTool`，agent 自主选择路径，不做约定。
2. **MCP Skills 命令注入**：MCP server 以 `skill://` 资源暴露的技能，**异步发现**后注入技能缓存（被动可见）与**用户 commands 列表**（特殊颜色标记），**完全不主动进入 agent 上下文**。用户触发 command 后内容注入当前会话（带来源标注）。

现有 tool search 体系（`SearchExtraTools` / `ExecuteExtraTool` / deferred 面）能力**完全保留**。

## 现状盘点（2026-08-13 核查）

### tool search 体系

| 机制 | 现状 | 位置 |
| --- | --- | --- |
| shared_tools 注册面 | `BTreeMap<String, Arc<dyn BaseTool>>`，middleware `collect_tools` 返回的工具由上层 merge 进入 | `peri-middlewares/src/assembly.rs:216,455` |
| direct/deferred 分流 | `BaseTool::is_direct()` 默认 false；`before_agent` 按此分流，deferred 进 `ToolSearchIndex` | `tool_search/middleware.rs:68-107` |
| 发现 | `SearchExtraTools`（direct，meta 分组）搜 deferred 工具索引，返回完整 schema | `tool_search/search_tool.rs` |
| 执行 | `ExecuteExtraTool` 按 `{tool_name, params}` 执行 deferred 工具 | `tool_search/execute_tool.rs` |
| MCP 工具现状 | 桥接工具未覆写 `is_direct`（默认 deferred），已走"SearchExtraTools 发现 → ExecuteExtraTool 执行" | `mcp/tool_bridge.rs` |

**结论**：DiscoverMCP 只需在 `McpMiddleware::collect_tools` 返回、不覆写 `is_direct`，即自动进 deferred 面（可被发现、经 ExecuteExtraTool 执行）。零装配改动。

### skills 体系

- `SkillsMiddleware::before_agent` 每轮 `scan_skill_roots` **无条件覆盖** `cached_skills`（`skills/mod.rs:310-320`）——远端技能需**分源合并**（MCP 侧维护远端注册表，skills 侧合并），不能简单 append。
- `SkillMetadata { name, description, path: PathBuf, source, plugin_name }` 定义在契约层 `peri-acp-types/src/skills.rs:35`；`SkillSource` 无 Mcp 变体。
- `SkillTool` 按名加载（namespace 前缀匹配，`skills/tools.rs:213-238`）；Builtin source 从编译期常量取内容、其余读 `path` 磁盘——远端技能需新增内容来源分支。
- `DiscoverSkillsTool` 读 `cached_skills` 过滤返回——远端技能注册后**自然被动可见**。

### TUI commands 列表链路

```
peri-acp: SkillsPort::available_skills() 扫本地 skills
  → build_available_commands() → AvailableCommandsUpdate（含 meta.skillNames）
  → ACP 通知（peri-acp/src/host/stdio/commands.rs:15-59）
TUI: acp_notifier.rs:365-395 收通知 → AVAILABLE_SLASH_COMMANDS + SKILL_NAMES atoms → refresh_slash_items()
TUI: input_area.rs:1297 build_slash_items() 组装 SlashCompletionItem（按 SKILL_NAMES 区分 kind=Skill/Command）
TUI: slash_completion.rs:212 tier_color 按 kind 上色（Skill → semantic.status.warning）
选择后 insert_text "/name " 进输入框发送
```

**结论**：MCP skill 进 commands 列表的接入点是 ACP 通知的 `availableCommands` + `meta`；颜色标记在 `slash_completion.rs` 的 `tier_color` 按 `SlashActionKind` 分色——加 `McpSkill` 变体 + 静态色映射即可，渲染点现成。

### resource list/read 底层

- 连接初始化已 `list_all_resources()` 缓存进 `McpClientHandle.resources`（`mcp/initialize.rs:175`）。
- `mcp_read_resource` 工具已存在（`mcp/resource_tool.rs`），120s 超时、2000 行截断。

## 设计

### A. DiscoverMCP Tool

- **注册**：`McpMiddleware::collect_tools` 返回，不覆写 `is_direct`（deferred）；`namespace() = Some("meta")`。
- **参数 schema**（JSON-RPC 风格，method + params 两字段）：

```json
{
  "type": "object",
  "properties": {
    "method": { "type": "string", "enum": ["search", "list", "detail"] },
    "params": { "type": "object" }
  },
  "required": ["method"]
}
```

- **method 语义**：

| method | params | 返回 |
| --- | --- | --- |
| `search` | `{ "query": string, "max_results"?: int }` | 全域子串匹配：server 名、tool 名/描述、resource uri、skill 名/描述；结果带 `type` 标注（server/tool/resource/skill）；**工具类结果带完整 JSON Schema**（供后续 ExecuteExtraTool 直接衔接） |
| `list` | `{ "server": string, "domain"?: "tools"\|"resources"\|"skills" }` | 指定 server 全量清单；`domain` 缺省返回三域摘要 |
| `detail` | `{ "server": string }` | server 级全量状态：连接状态、协议版本、capabilities、OAuth 状态、tools/resources/skills 清单摘要 |

- **错误**（轻量 JSON-RPC，无 id）：`{ "error": { "code": int, "message": string } }`。三类区分：未知 method（`-32601`）、搜索无结果（`0`，正常空结果非错误——返回空数组）、server 未连接/不存在（`-32000`）。query 缺失等参数错误 `-32602`。
- **边界**：只读。搜索结果中的 tool 调用提示 agent 走 `ExecuteExtraTool`；resource 读取走 `mcp_read_resource`；skill 加载走 `SkillTool`。DiscoverMCP 自身**不代理任何执行**。

### B. MCP Skills 异步发现与注入

#### 1. 发现（连接成功后异步）

```
initialize 成功（列表已缓存）→ 异步任务：
  过滤 resources 中 uri 以 "skill://" 开头、文件名 SKILL.md 的条目
  → 并发 resources/read 读全文
  → 解析 YAML frontmatter（name/description 必取）
  → 写入 MCP skill 远端注册表（Arc<RwLock<Vec<McpSkillEntry>>>）
```

- **入口约定**：`skill://<name>/SKILL.md` 即一个技能；同前缀附属资源不单独注册（agent 用 `mcp_read_resource` 按需读）。
- **异步语义**：不阻塞连接初始化与首 turn；发现完成静默（不注入 agent 上下文、无 TUI 提示音）；失败仅告警日志，不影响连接。
- **session 边界（硬约束，见 C 节）**：发现任务由 **session 装配链内的中间件**触发（首轮 `before_agent` 读 pool 的已连接状态做本 session 投影），**绝不从 app 级 `spawn_mcp_init` 触发**；注册表挂 session 级实例，**绝不挂 `McpClientPool` / 全局 atom**。
- **失效**：连接断开移除该 server 全部条目；重连重扫（复用 `reconnect.rs` 路径）。session 结束时注册表与发现任务随 middleware 实例销毁。
- **刷新**：本次不接 list_changed 订阅（阶段二随 SEP-2640 一起做）。

#### 2. 注入技能缓存（被动可见）

- `peri-acp-types/src/skills.rs`：`SkillSource` 增 `Mcp` 变体；`SkillMetadata` 增 `origin: Option<SkillOrigin>`（`SkillOrigin::Mcp { server, uri }`）与 `content: Option<String>`（MCP 来源存注册时读入的全文，本地为 None）。
- `SkillsMiddleware::before_agent`：本地扫描结果 + MCP 远端注册表**合并**写 `cached_skills`（本地扫描不再无条件覆盖）。
- **不主动进上下文**：不进 `frozen_summary`、不进 `build_summary` listing、不发 system-reminder。被动可见面 = `DiscoverSkillsTool` 搜索 + `SkillTool` 加载。
- `SkillTool` 加载路径：`SkillSource::Mcp` 时从 `content` 取（零 RPC，对齐 Builtin 模式）。

#### 3. 注入用户 commands 列表（颜色标记）

- **命名**：`mcp__<server>__<skill>`，与工具命名同构、天然去重。
- **ACP 侧**：`send_available_commands`（`peri-acp/src/host/stdio/commands.rs`）的 `availableCommands` 追加 mcp skill 条目；`meta` 增 `mcpSkillNames` 数组（与 `skillNames` 并列）。MCP skill **不进** `skillNames`（避免被当作本地 skill 上色/分类）。
- **TUI 侧**：
  - `acp_notifier.rs`：解析 `meta.mcpSkillNames` 写新 atom `MCP_SKILL_NAMES`；
  - `slash_completion.rs`：`SlashActionKind` 增 `McpSkill` 变体；`tier_color` 为其映射**静态约定色**（如 `semantic.status.info` 或主题中专门 token，**不进配置系统**）；
  - `input_area.rs:1323-1336`：按 `MCP_SKILL_NAMES` 归类 kind。
- **触发后行为**：用户选择 `/mcp__<server>__<skill>` 发送 → 复用现有 skill command 触发链路，SKILL.md 内容注入当前会话，**带来源标注**（注入文本前缀，如 `This skill is served by MCP server "<server>", uri: <uri>.`）。标注让 agent 知晓内容边界（提示注入防御）。
- **静默更新**：发现完成/变化时 commands 列表直接刷新（复用 `refresh_slash_items`），无 toast、无 system-reminder。

#### 4. 安全分层

- 来源标记：`source: Mcp` 贯穿缓存、`DiscoverSkillsTool` 结果（`source: "mcp"`）、注入标注。
- 权限类 frontmatter（如未来引入 `allowed-tools`）对 MCP 来源默认不生效。
- 加载只读：内容仅存内存缓存，不写盘。
- peri skills 无内联 shell 执行机制，无"禁 shell"需求；真实风险为提示注入，由来源标注 + 被动可见（不进 listing）缓解。

### C. Session 边界（架构约束，2026-08-13 定案）

**决策**：MCP 连接池**不下沉 session**（维持 app 级共享：stdio 子进程、OAuth 凭证、面板直读均不动）；本方案新增的一切派生数据**严格 session 级**。

**三条硬约束**：

1. 远端 skill 注册表挂 **session 装配链**（middleware 实例级，随 session 生灭自清理）——**绝不挂 `McpClientPool` / `kit::atoms::MCP_PANEL_POOL` 等 app 级全局**。
2. 异步发现由 **session 中间件**触发（首轮 `before_agent` 读 pool 已连接状态做本 session 投影，用 session 取消令牌）——**绝不从 `spawn_mcp_init` 触发**。
3. commands 下发数据源走 session 链路：`available_commands_update` 本就是 per-session transport 通知，skills 输入 = 本地扫描 + 本 session 注册表合并，**不新增全局通道**。

**评估摘要**（不池下沉的好处/坏处）：
- 好处：改动面最小，零连接管理回归风险；进程级共享对子进程/OAuth 合理；单活动 session 下语义等价；挂点做错影响面限于本方案功能。
- 坏处：session 注册表需从 app 级 pool **投影**连接状态（连接/断开/重连事件 → 注册表增删），多 session 并发时每 session 重复扫描 RPC；pool 比 session 长寿，发现任务须随 session 取消；面板（app 级视图）与 session 内 skill 可见性可能有时序差；多 session 并发时不同 cwd 的 `.mcp.json` 无法隔离（属池下沉议题，非本方案引入）。
- 缓解：投影只读（只从 pool 读快照，不回写 pool）；面板/session 时序差声明为已知边界，不追求强一致。
- 迁移注：若未来 MCP 池下沉 session（对齐 `create_session_lsp_pool` 模式），本方案数据挂点随之迁移、对外接口不变。

## 验收标准

### DiscoverMCP

1. **注册契约**：`DiscoverMCP` 为 deferred（`is_direct == false`），`SearchExtraTools` 可搜到、`ExecuteExtraTool` 可执行；namespace 为 meta。
2. **search**：mock 池含 server/tool/resource/skill 四类条目 → 关键词命中全域；结果带 `type` 标注；工具类结果含完整 JSON Schema；无结果返回空数组非错误。
3. **list**：指定 server 按 domain 返回清单；缺省返回三域摘要；server 不存在返回 `-32000` 错误对象。
4. **detail**：返回连接状态/协议版本/capabilities/OAuth 状态/清单摘要；未连接 server 状态如实反映。
5. **错误契约**：未知 method `-32601`；参数缺失 `-32602`；错误响应结构 `{error:{code,message}}` 无 id。
6. **只读断言**：DiscoverMCP 无任何 MCP tools/call 路径（代码级或行为测试锁定）。

### MCP Skills

7. **发现契约**：mock server 暴露 `skill://demo/SKILL.md` + 附属资源 + 非 skill 资源 → 仅 `mcp__<server>__<skill>` 注册；断连移除；重连重扫。
8. **分源合并**：MCP 连接后 `cached_skills` 含远端技能；下一轮 `before_agent` 本地扫描后仍在（不被覆盖）。
9. **不主动进上下文**：远端技能不出现在 prompt contribution / frozen summary / system-reminder（断言贡献文本不含其 name/description）。
10. **被动可见**：`DiscoverSkillsTool` 返回含 `source: "mcp"` 条目；`SkillTool` 按 `mcp__<server>__<skill>`（及 `<server>:<skill>`）加载返回缓存内容；本地技能回归不变。
11. **commands 列表**：ACP `available_commands_update` 含 mcp 条目且 `meta.mcpSkillNames` 正确；TUI 该条目 kind=McpSkill 且颜色为约定色（渲染/单元测试锁定）。
12. **触发注入**：选择 command 后注入文本带来源标注（含 server 名与 uri）。
13. **静默**：发现完成不产生 Info 消息/system-reminder（事件流断言）。
14. **session 边界断言**：远端注册表/发现任务不引用 `McpClientPool`/全局 atom（代码级断言：注册表类型无 `'static` 全局挂点；发现任务由 session 中间件 spawn、持 session 取消令牌）；session 销毁后注册表随之释放（单测模拟 assemble → drop）。

## 风险与取舍

- **deferred 悖论**：DiscoverMCP 自身 deferred，需经 SearchExtraTools 发现——agent 需先知道"有个搜 MCP 的工具"。缓解：SearchExtraTools description 已列 Core 工具与发现用途，DiscoverMCP 的 name/description 进 deferred 索引后自然可见；后续可观察触发率决定是否升 direct。
- **search 结果体积**：工具类带完整 schema 可能让返回很大。缓解：`max_results` 上限（默认 5、上限 20）+ 结果截断策略复用既有输出截断。
- **commands 列表膨胀**：每个 MCP skill 一条，恶意/低质 server 可注入大量条目。缓解：命名带 server 前缀可识别；后续可按 server 折叠。
- **内容新鲜度**：注册时读入缓存的 SKILL.md 不随 server 侧变化刷新（本次无 list_changed 订阅）——重连时重扫兜底；阶段二接订阅。
- **旧 issue 演进**：本 issue 取代 `2026-08-13-mcp-skills-from-resources.md`（superseded），保留其 SEP-2640 阶段二路线与 resource 前缀方案事实基础。

## 非目标

- 不在 DiscoverMCP 提供 MCP 工具执行（执行仅 `ExecuteExtraTool`）。
- 不改 tool search 体系现有行为（SearchExtraTools/ExecuteExtraTool 保留原语义）。
- 不做 SEP-2640 `skills/list` 原语、digest 校验、list_changed 订阅热更新（阶段二）。
- 不做 MCP scope 语义与企业策略。
- 颜色标记不进配置系统。
- 不做 MCP skill 的发现完成通知（toast/system-reminder）。

## 涉及文件

- `peri-middlewares/src/mcp/` — DiscoverMCP 工具实现（新增）、异步发现任务、远端注册表；`middleware.rs` collect_tools 注册。
- `peri-acp-types/src/skills.rs` — `SkillSource::Mcp`、`SkillMetadata.origin/content`。
- `peri-middlewares/src/skills/mod.rs` — `before_agent` 分源合并。
- `peri-middlewares/src/skills/tools.rs` — `SkillTool` Mcp 来源加载分支、`DiscoverSkillsTool` source 输出。
- `peri-acp/src/host/stdio/commands.rs` — `availableCommands` 追加 mcp 条目 + `meta.mcpSkillNames`。
- `peri-tui/src/kit/slash_completion.rs`、`input_area.rs`、`acp_notifier.rs`、`atoms.rs` — `SlashActionKind::McpSkill`、颜色映射、atom 与归类逻辑。
- `spec/issues/2026-08-13-mcp-skills-from-resources.md` — superseded。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-13 | — | Open | agent | 需求访谈定案后创建；supersede 旧 issue |
| 2026-08-13 | Open | Open | agent | 新增 C 节 session 边界三硬约束（MCP 池不下沉定案）+ 验收 14 |
| 2026-08-13 | Open | Open | agent | 8 Slice 全部落地；组 A-F + 最终集成评审 APPROVED；修复 HIGH-1（TUI `_meta` wire key）与 LOW-1（commands 合并去重）；验收 1-14 全闭合，待提交 |
