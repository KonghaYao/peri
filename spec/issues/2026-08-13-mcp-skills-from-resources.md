# MCP Skills：从 server resources 发现并加载技能

**状态**：Superseded（由 `spec/issues/2026-08-13-discover-mcp-tool-and-mcp-skills-v2.md` 取代）
**优先级**：中
**类型**：功能
**创建日期**：2026-08-13
**来源**：`docs/design/mcp-connector-guide-v2.md` §7（skills 发现与加载）+ MCP SEP-2640 Skills Extension 草案研究
**最后核查**：2026-08-13

## 取代说明（2026-08-13）

2026-08-13 需求访谈（/interview）定案的新方向与本 issue 设计有重大差异：新增 DiscoverMCP Tool（deferred 只读查询）、MCP skills **不主动进 agent 上下文**（不进 listing/frozen catalog）、改走**用户 commands 列表**（特殊颜色标记、触发后注入会话）。设计、验收标准与落点均在新 issue。本 issue 保留 SEP-2640 阶段二路线与 resource 前缀方案的事实基础，供新 issue 参考。

## 任务定义

让 peri 具备「从已连接的 MCP server 发现并加载技能」的能力：server 以 `skill://` scheme 的资源暴露 `SKILL.md`，peri 在连接时自动发现、注册进现有 skills 系统，agent 经 `SkillTool` 按需加载全文。分两阶段：

- **阶段一（本次实施）**：兼容 Claude Code 生态的 resources 前缀方案（`resources/list` → 过滤 `skill://` 前缀 → `resources/read` → frontmatter 解析 → 注册）。零协议扩展，今天即可互操作。
- **阶段二（SEP-2640 定稿后）**：升级到 `skills/list` 原语 + digest 内容完整性校验 + `list_changed` 订阅热更新。

## 现状盘点（2026-08-13 核查）

**已具备的底层能力**（回答「能否 list / 能否下载 resource」）：

| 能力 | 现状 | 位置 |
| --- | --- | --- |
| 连接时 list resources | ✅ 已做。initialize 成功后调 `list_all_resources()`，结果缓存进 `ServerInfo` | `peri-middlewares/src/mcp/initialize.rs:175,395` |
| 资源摘要注入工具面 | ✅ 已做。`resource_summary()` 把资源 URI 列表写进 `mcp_read_resource` 的 description | `peri-middlewares/src/mcp/client.rs:668`、`resource_tool.rs:37-52` |
| 读单个 resource | ✅ 已做。`mcp_read_resource` 工具按 `server_name` + `uri` 裸读，120s 超时，2000 行截断 | `peri-middlewares/src/mcp/resource_tool.rs` |
| 本地 skills 系统 | ✅ 已做。`SkillsMiddleware` 预扫描缓存 `Arc<RwLock<Option<Vec<SkillMetadata>>>>`；`SkillTool` 按名加载全文（支持 namespace 前缀如 `ecc:plan`）；`DiscoverSkillsTool` 搜索 | `peri-middlewares/src/skills/tools.rs`、`loader.rs` |

**缺失的是 skill 语义层**——list/read 是通用资源原语，当前没有任何「资源 = 技能」的映射：

1. 无 `skill://` 前缀过滤与 frontmatter 解析，资源不会注册进 skills 系统（`cached_skills` 数据源只有本地目录 / builtin / 插件）。
2. `SkillTool` 只按名称在本地缓存中查找，无经 `resources/read` 的 URI 加载路径。
3. `DiscoverSkillsTool` 搜索范围不含远端技能。
4. 无来源标记（origin-tag）：加载的技能无法区分本地 / MCP 来源，事件链无追溯。
5. 无安全分层：MCP 来源技能与本地技能同权限。
6. client 侧 initialize 未显式声明 `capabilities.resources`（`serve_client_auto` 用默认 handler，能力声明依赖 rmcp 默认值，需确认并显式化）。
7. **`cached_skills` 每轮覆盖问题**：`SkillsMiddleware::before_agent` 每轮无条件 `scan_skill_roots` 覆盖缓存（`skills/mod.rs:310-320`），远端技能直接 append 会被下一轮扫描冲掉——需要分源合并（本地扫描结果 + 远端注册表），而非简单注册。

## 阶段一设计（resources 前缀方案）

与 Claude Code `mcpSkills.ts` 机制对齐，纯用现有 resources 原语：

### 1. 发现与注册（client 侧，连接后）

```
initialize 成功 → list_all_resources()
→ 过滤 uri 以 "skill://" 开头的资源
→ 并发 resources/read 每个 skill 的入口资源（SKILL.md）
→ 解析 YAML frontmatter（name / description 必取，其余透传）
→ 注册进 skills 系统缓存（SkillMetadata 增加 origin 字段）
```

- **入口资源约定**：每个 `skill://<name>/` 前缀目录下以 `SKILL.md` 为入口。`resources/list` 返回中 `skill://<name>/SKILL.md` 即一个技能（mimeType `text/markdown`）。同一前缀下的附属资源（如 `skill://<name>/references/*.md`）归属同一技能，不单独注册。
- **命名**：`mcp__<server>__<skill>`，与工具命名同构。`SkillTool` 匹配逻辑已支持 namespace 前缀，远端技能以 server 名为 namespace。
- **失效**：连接断开时移除该 server 的全部技能；重连后重扫（复用 `reconnect.rs` 路径）。

### 2. 加载（SkillTool 扩展）

- `SkillMetadata` 增加 `origin` 枚举（`Local` / `Mcp { server, uri }`）+ 可选 `content: Option<String>`（远端技能注册时已读入，本地为 `None`）。
- **注册时读取、加载零 RPC**：发现阶段已 `resources/read` 拿到 SKILL.md 全文，直接存入 `content`——对齐 Builtin source 编译期存内容的既有模式（`builtin/mod.rs`），`SkillTool` 加载路径统一从缓存取，无断连/超时失败面。订阅 `resources_list_changed` 时重读刷新（阶段二）。
- 附属资源（references 等）按需读：技能正文引用相对路径时，agent 用 `mcp_read_resource` 工具自行读取（该工具已存在，无需新工具）。

### 3. 统一发现（DiscoverSkillsTool 扩展，零新工具）

- `cached_skills` 已是本地 + 远端的聚合缓存，`DiscoverSkillsTool` 搜索范围自然覆盖两者——**不新增任何搜索工具**，避免 N 个 server 产生 N 个碎片搜索入口（`mcp-connector-guide-v2.md` §7.4 已定）。
- **分源合并**：McpMiddleware 维护独立的远端注册表（`Arc<RwLock<Vec<SkillMetadata>>>`，连接/断开/重连时增删）；`SkillsMiddleware::before_agent` 在本地扫描结果之上合并该注册表再写缓存——本地扫描不再无条件覆盖远端。
- **与 ToolSearch 的关系（重叠审查结论）**：skill 不是工具，**不进 ToolSearch 面**（`SearchExtraTools` 只搜 deferred 工具，MCP 桥接工具仍按现状经 tool search 发现，两者语义不冲突）。发现/加载完全复用 `DiscoverSkillsTool` + `SkillTool` + `mcp_read_resource` 三个既有工具，本方案**新增工具数为零**。
- server 自带的 skill 搜索类工具**保持既有桥接行为，不搞排除特例**——它作为普通 `mcp__` 工具经 tool search 可见是工具桥接的统一契约，与 skills 系统无关。

### 4. 安全分层

- **来源标记是核心**：`SkillSource` 增加 `Mcp` 变体，`DiscoverSkillsTool` 结果带 `source: "mcp"`，agent 与用户可见技能出处；事件链记录加载来源供追溯。
- **提示注入防御**：peri skills 无内联 shell 执行机制（当前 SKILL.md 是纯 markdown 指令，无 `!`command`` 动态上下文注入），因此"禁 shell"是空约束；真实风险是远端内容诱导 agent 越权行为——缓解：远端技能正文由 server 单方提供，加载时在结果中标注来源提示（如 "This skill is served by MCP server X"），让 agent 知晓内容边界。
- 权限类 frontmatter 字段（若未来引入 `allowed-tools` 等）对 MCP 来源**默认不生效**，需显式信任。
- 加载行为只读：注册/加载不写盘，内容仅存内存缓存。

### 5. client capabilities 声明

- initialize 时显式声明 `capabilities.resources`（含 `listChanged: true` 以接收 list_changed 通知）。当前 `serve_client_auto` 用 `()` 默认 handler，需检查 rmcp 默认是否已声明；未声明则显式构造。

## 阶段二（SEP-2640 定稿后，本次不实施）

- 升级 `skills/list` 原语（Extension id `io.modelcontextprotocol/skills`），取代前缀过滤。
- 按资源清单 digest（sha256）做内容完整性校验，校验失败拒绝加载并告警。
- 订阅 `notifications/resources/list_changed` 热更新技能注册表（`McpSubscriptionsConfig` 已有 `resources_list_changed` 开关，可复用）。

## 验收标准

1. **发现契约测试**：mock server 暴露 `skill://demo/SKILL.md` + 附属资源 + 一个非 skill 资源 → 连接后仅 `mcp__demo__<name>` 技能注册，非 skill 资源不注册；断开后技能移除。
2. **加载测试**：`SkillTool` 按 `mcp__demo__<name>` 或 `demo:<name>` 加载，返回注册时缓存的 SKILL.md 内容（与 mock 的 `resources/read` 一致）；本地技能加载行为回归不变。
3. **搜索测试**：`DiscoverSkillsTool` 结果同时含本地与远端技能（`source: "mcp"` 可见）；对同一关键词的排序/去重有确定行为（契约测试锁定）。
4. **安全测试**：MCP 来源技能前端权限类字段被忽略（若引入）；来源标记在 `DiscoverSkillsTool` 输出与事件链中可观测。
5. **分源合并测试**：MCP 连接成功后 `cached_skills` 含远端技能；下一轮 `before_agent` 本地扫描后远端技能仍在（不被覆盖）；断开后移除。
6. **capabilities 断言**：initialize 请求中 `capabilities.resources.listChanged == true`（transport 测试可捕获）。

## 风险与取舍

- **上下文预算**：远端技能 name + description 进入 skills listing（与本地技能共用预算），恶意/低质 server 可注入大量条目挤占预算。缓解：listing 排序把远端技能放在本地之后；后续可按 server 限配额。
- **命名冲突**：`mcp__<server>__<skill>` 前缀天然隔离 server 间冲突；与本地技能同名时两者共存（SkillTool 大小写不敏感匹配按 namespace 前缀消歧）。
- **拉取成本**：连接时对每个 skill 入口并发 `resources/read` 增加连接耗时。缓解：并发 + 单技能读取超时（复用 120s 常量，考虑降为 10s 失败跳过）；连接成功后技能注册可后置异步完成（首 turn 概览只报「N skills 加载中」）。
- **方案生命周期**：阶段一是 Claude Code 事实标准，但 SEP-2640 定稿后需迁移。`skill://` 前缀过滤逻辑隔离在单一模块内，迁移成本可控。

## 非目标

- 不实现 MCP scope 语义与 `allowedMcpServers` / `deniedMcpServers` 企业策略（独立议题）。
- 不实现 skill 对工具依赖的 `dependencies` 声明（SEP-2640 未决议题）。
- 不实现 registry `skills.json` 组织级技能库分发。
- 不为 server 侧提供 skill 搜索聚合服务（peri 是 client，不是 registry）。
