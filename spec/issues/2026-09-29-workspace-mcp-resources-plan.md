# Workspace MCP resources、Skill 来源下沉与指令资源化主计划

> 日期：2026-09-29。状态：**已裁决（X 全 A，2026-09-29）；W1/W2 已实施并提交（0938942f / c9c5b02d / f511b7ae）；W3 已交付（W3a 两阶段准入与 frozen 同源 / W3b MetaHarness J6）；W4 已交付（W4a 资源面接线 711bf1d0 / W4b SkillTool 零 FS 切源 8c3d975a，见 §8.1 W4 行）；W5 已交付（Agent 定义与项目指令消费切换 f66bd251，见 §8.1 W5 行）**。
> 首版交付声明（仅此计划文件、不落代码、不 commit）已被波次实施取代：下文“新增 / 修改 / 删除”为实施范围记录，实际改动随各波提交。
> 2026-09-29 v2 变更：① 写入已裁决「system=摘要进 system prompt / 用户直接触发=全文」双路径语义（§5.4，含触发口径候选）；② 新增 §2.3「Skill 来源与收敛」调查结论；③「frozen 经资源读取后冻结」由待裁决移入已裁决（§7.0）；④ 原 D1–D8 收敛为 X1–X5（X1 触发口径、X2 收敛范围、X3 存量兼容、X4 关闭与指令 profile、X5 规范范围与失败语义）。
> 2026-09-29 v3 变更（用户补充裁决）：**`SkillTool` / `DiscoverSkillsTool` 不下沉**，保留宿主层做跨 MCP server 的 skill 聚合与把控；下沉的只是 **skill 来源**（workspace resources 承载）。工具位置与来源是两个独立维度，全文按此拆分改写（§5.6、§6.3、§8.1 W4、§9、§10）。
> 2026-09-29 v4 变更（用户新裁决，J5）：「Skill Tool 的能力收敛为只有读取 MCP 侧提供的源，Skill Tool 本身要解除对文件系统的依赖」。本版：§2.3 增补 FS 依赖点逐条处置表；§5.6 重写为新数据路径与改造前后对照；**原 X2（收敛范围）由 J5 关闭**；X3/X4/X5 连带更新；新增 X6（本地来源可信与批准策略）；§6/§8/§9/§10 同步。
> 2026-09-29 v5 变更（用户新裁决，J6）：「metaharness 也是可以挂入 workspace 里面的 resources 里面的，这个就比较简单了，扫描 resources list 即可」。本版：§5.2 增补第四类资源（段落覆盖文档）；§6.3 后新增 §6.4；新增裁决 J6 与待裁决 X7（来源白名单）/X8（关闭与失败语义）；§8.1 W3 增补本消费面、§8.2/§9/§10 同步。**J6 有硬前置 J2**（现状冻结先于 MCP pool 兴起，见 §6.4）。
> 2026-09-29 v6 rebase（W1 开工）：① 事实更正（据 `2026-09-29-workspace-mcp-resources-decisions.md` §8.1–8.2）：workspace 实例**已有** git ref resources 与订阅面（`accepted_subscription_filter` + `listen`），文中「workspace tools-only / dispatch 只转发 info/tools」的现状描述已过时——缺口是**新增内容域**（skills/agents/instructions）与 custom/templates 通路，不是全部 resources 通路；**既有订阅面必须保留**，「首期不声明 subscription」不适用于该既有 git ref 订阅（新增资源不自动继承订阅支持）。② 依据文档：`2026-09-29-workspace-mcp-resources-decisions.md`（X 全 A 裁决、规范锁定与阻塞项）与 `2026-09-29-workspace-mcp-resources-j2-feasibility.md`（J2 条件可行、B5 wire 验证方法）；W1 不依赖 J2。
> 三态口径：**现状**＝静态源码证据；**目标/建议**＝待实施方案；**运行时证据**＝本次未取得。遵循 `docs/design/mcp-adaptation-v4-part-1.md:7,182`，不把设计目标写成已经具备的能力。
> 行号是本次工作树读取时的定位信息，实施前须按符号复核。`assembly*`、builtin context 与 code-index 存在并行 LSP 下沉工作；本文只引用与本任务有关的既有边界，不评价 LSP 半成品、不以其暂态推导能力。

---

## §1 范围与成功标准

### 1.1 用户已确定的方向

1. `peri-mcp-workspace` / builtin `workspace` 提供符合 MCPP Skills 约定的 MCP resources。
2. Skills 继续经现有 MCP skills 体系投递；**已裁决（2026-09-29）**：system MCP 的 skill 以**摘要**注入 system prompt，**用户直接触发**时注入**全文**（触发口径候选与推荐见 §5.4 与 X1），非 system 保持发现/deferred 路径。
3. **已裁决（2026-09-29 补充）**：`SkillTool` / `DiscoverSkillsTool` **不下沉**，保留在宿主层（`peri-middlewares`）做**跨多个 MCP server 的 skill 聚合与把控**；下沉的只是 **skill 来源**（本地/内置技能、Agent 定义、项目指令改由 workspace resources 承载）。工具位置与来源维度分开表述，见 §5.6。
4. Agent 定义及 AGENTS.md / CLAUDE.md 项目指令由 resources 承载；上层不再直接读取这些文件后注入。
5. 本计划的验收对象是完整路径：资源提供 → MCP 发现 → 策略/校验 → 模型上下文或子 Agent；不是“有一个 resources/list handler”即完成。
6. **已裁决（2026-09-29）**：frozen 由「不启动 MCP 的准备阶段」调整为「经资源读取后冻结」，采纳原 D6 推荐时序方案（§7.0）。
7. **已裁决（2026-09-29 v4，J5）**：`SkillTool` 能力收敛为**只读 MCP 侧提供的来源**，**自身解除文件系统依赖**——宿主侧对 `~/.claude/skills`、`settings.json skillsDir`、项目 `.claude/skills`、builtin 内嵌资产的扫描与正文读取一律退出 SkillTool 数据源，文件系统读取职责整体归位 MCP 侧（§2.3 处置表、§5.6 数据路径）。
8. **已裁决（2026-09-29 v5，J6）**：MetaHarness 段落覆盖文档（`.peri/meta/*.md`）挂入 workspace resources（`peri-meta://workspace/{section_id}`），宿主冻结期改为「扫描 resources list + 按启用 section 读取」；**硬前置 J2**（现状冻结先于 MCP pool 兴起），归属 W3，见 §6.4。

### 1.2 非目标

不把 `Agent` 调度工具、模型运行、HITL、transport/pool/shutdown 下放；不改变七个文件/终端工具的任意路径行为；不新增 MCP server 实例或独立服务启动入口；不重构 LSP。依赖方向遵循 `mcp-packages/CLAUDE.md:5-17,33-40`，宿主语义保留的目标依据为 `docs/design/mcp-adaptation-v4-part-1.md:168-178`。

---

## §2 外部规范取证与解释

### 2.1 来源、完整性及边界

- 首先抓取用户指定的 [mcp-skills.md GitHub 页面](https://github.com/KonghaYao/mcpp/blob/main/MCPP/mcp-skills.md)。渲染页正文中若干列表被抽取为零散代码 token，不能单独作为 MUST 条款的完整证据。
- 随后读取用户提供的 [raw 正文](https://raw.githubusercontent.com/KonghaYao/mcpp/main/MCPP/mcp-skills.md)，取得 §5.1–§5.10.4 完整条款。本计划以下以该章章节号引用；两处均已实际抓取成功。
- 来源为可变 `main`，**未锁定 commit/digest**；开工前须锁定审阅版本并记录版本差异。本文不宣称已经满足整套 MCPP：第 9 章扩展声明 wire 细节、第 7.3 章缓存与第 10 章批准的完整规则未展开核验，只采用本章明确重述的要求。
- 该章明确规定 Skills 与 MCP Agents，**没有定义 AGENTS.md / CLAUDE.md 专用 URI、索引或自动注入规则**。项目指令资源化必须标为 Peri 自有 profile，不能声称是该章标准。

### 2.2 关键定义

| 主题 | 规范要求及实施含义 |
| --- | --- |
| 承载 | §5.1：Skill 是目录，根为 UTF-8 `SKILL.md`，YAML frontmatter 至少 `name` / `description`；name 与父目录名一致；未知字段透传且不得因未知字段拒载。description 是 Discovery 选择依据。 |
| URI | §5.3：`skill://{org-prefix/}{skillName}/{relativePath}`；入口固定 `SKILL.md`，根 URI 无尾斜杠。authority 是命名空间，不做 DNS。附件逐文件暴露，不是独立 Skill。 |
| 清单 | §5.4：声明 `io.modelcontextprotocol/skills` 后必须实现 `skills/list`（分页）及 `skills/get`（按入口 URI，即使未枚举；未知 URI 为 `-32602`）。条目为 `{uri, frontmatter, resources:[{uri,digest}]}`，frontmatter 全量 JSON 化，digest 为逐字节 `sha256:{64hex}`。没有“一个正文资源就等于完整 skill 清单”的捷径。 |
| 标准 resources | §5.3–5.6：`resources/list` 是通用资源目录，`resources/read` 取正文/附件；`skills/list/get` 是技能身份及完整性清单，不替代 resources/read，也不是同名 tools/call。`resources/templates/list` 是兜底发现；目录导航 `resources/directory/read` 可选、须受 `directoryRead` 能力位门控。 |
| 发现 | §5.5：有扩展走 skills/list；否则由 resources/list + templates 找候选并标记“列表来源”。registry 仅收 name/description/origin/URI/可选 digest，不读取正文；空/局部列表不能证明不存在 Skill。§5.3 禁止只凭 scheme 认定/信任 Skill，须权威条目或校验通过的显式引用。 |
| 激活与注入 | §5.6：描述匹配/用户触发并获适用批准后读取，校验 digest 与 frontmatter 全字段一致性，失败不得使用；可经 skills/get 刷新重试。注入完整 SKILL.md 并携 origin，不等于在发现阶段把所有全文塞入 prompt。§5.9 命令与工具入口须共用加载/校验/批准路径。 |
| 附件安全 | §5.3：相对引用以 Skill 根解析，解析后 URI 必须在 resources 清单；拒绝隐藏段、node_modules、反斜杠、NUL、绝对路径、`..`、symlink，扫描和读取双重校验。任意目录/扩展名的普通附件可发现；内容字节决定 text/blob，扩展名仅辅助 MIME。scripts 可读不代表可执行。 |
| 预算/缓存 | §5.3 给出参考预算而非固定协议硬阈值：1 MiB/文件、8 MiB及128文件/Skill、32 MiB及1024文件/挂载、1024扫描项/Skill。§5.5–5.6.1：按 origin/授权域处理 ttlMs/cacheScope；清单/frontmatter/digest 变更使正文与附件 stale。§5.7.3 附件必须懒读，不能随正文全量注入。 |
| 编排 | §5.2/5.7：metadata 的 `io.mcpp/version/provider/depends_on/tools/context_budget` 是已登记扩展；未知扩展不能自占 `io.mcpp/`。编排为可选择消费的宿主策略；若消费依赖，须报告缺失/环并逐 Skill 校验批准，声明不授予权限。§5.7 总述的“可选”与 §5.7.1 的 MUST 措辞需在 X5 明确采用的 profile。 |
| Agents | §5.10：单 Markdown `agent://{org-prefix/}{agentName}/agent.md`，必填 name/description，标准 resources/list/read，无 agents/list/get。发现不读正文；首次远端激活显式批准，绑定 origin/URI/digest/有效能力；未知字段默认忽略，本地 hooks/permissionMode 等不得自动生效。有效能力是父可委派能力、宿主策略、用户批准、请求能力的交集或更小集合；非 fork 隔离上下文。 |

### 2.3 Skill 来源与收敛 / SkillTool 的 FS 依赖处置（2026-09-29 调查结论 + J5）

**问题（用户提问，已由 J5 部分回答）**：Skill 是否全部来源于 MCP？能否重构收敛为单一来源？

**来源盘点（现状，静态证据；调用点为生产路径，不含测试）**：

| 来源 | 位置与加载/注册路径 | 生产调用方 |
| --- | --- | --- |
| A. MCP skills | 发现：`peri-middlewares/src/mcp/skill_discovery.rs:1-19,85-125`（`skills/list` 或 legacy `resources/list` 扫描）→ 写 `McpSkillRegistry`（`peri-acp-types/src/mcp_skills.rs:147,247,267,294`；注册表条目含正文与 resources 清单，E6/E7） | 3 处：`peri-middlewares/src/skills/mod.rs:414-425`（合并进工具缓存）、`subagent/skill_preload.rs:144-157`（registry-first）、`mcp/skill_discovery.rs:300-345`（`/server:skill` 命令放行与 RPC 直返） |
| B. 文件系统 skills | 根解析 `peri-middlewares/src/skills/loader.rs:407-454`（User `~/.claude/skills`、Global `~/.peri/settings.json::skillsDir`、Project `{cwd}/.claude/skills`）；扫描 `peri-middlewares/src/skills/loader.rs:103-269`（深度/目录上限、symlink 防环）；元数据 `peri-middlewares/src/skills/loader.rs:65-90` | 4 处：`peri-middlewares/src/skills/mod.rs:404-412`（每轮 `before_agent` 重建）、`peri-middlewares/src/skills/mod.rs:271-286`（会话冻结摘要 `build_frozen_summary`）、`peri-middlewares/src/subagent/skill_preload.rs:158-165`（本地兜底扫描）、`peri-middlewares/src/host_ports.rs:427-436` → `peri-acp/src/session/construction.rs:64-87`（`core:{name}` 命令注册）。`find_skill_content`（`peri-middlewares/src/skills/loader.rs:464`）与 `list_skills`（`:387`）生产调用方为零（仅测试/文档标注为旧调用点） |
| C. 插件 skills | manifest `skills` 字段 → `peri-middlewares/src/plugin/loader.rs:325,685,710` → `PluginLoadResult.skills_roots`（`peri-acp-types/src/plugin.rs:577`）→ `peri-middlewares/src/skills/mod.rs:223`（`with_plugin_roots`）/ `peri-middlewares/src/subagent/skill_preload.rs:87`；命名空间 `plugin:{plugin}:{cmd}` 见 `peri-middlewares/src/plugin/loader.rs:272-295` | 与 B 的扫描调用方同源（插件根只是 SkillRoot 之一） |
| D. Builtin skills | 编译期嵌入 `peri-middlewares/src/skills/builtin/mod.rs:23-52`（7 项 `include_str!`）；扫描特判 `peri-middlewares/src/skills/loader.rs:139-146`；内容读取 `peri-middlewares/src/skills/content.rs:20-27`（虚拟 `<builtin>/<name>` 路径，不读盘）；关闭位 `disable_bundled`（`peri-middlewares/src/skills/loader.rs:444-451`） | 与 B 同源；`peri-middlewares/src/skills/builtin_test.rs` 校验 frontmatter |

**SkillTool 的 FS 依赖点与逐条处置（J5 裁决，2026-09-29）**：

> 事实更正：`skills/tools.rs` 自身**没有** `std::fs`/`std::path` 调用（全文件搜索零命中）；FS 依赖是**传递性**的——缓存的填充点（F2–F5）、`content::load` 的三条读取分支（F7–F9）。因此「解除 FS 依赖」= 切断这组传递来源，工具文件本身不动用文件系统 API。

| # | 依赖点（现状） | file:line | 处置 |
| --- | --- | --- | --- |
| F1 | `SkillTool::invoke` 读会话缓存 → `content::load` 取正文 | `peri-middlewares/src/skills/tools.rs:92-104`（缓存读取 :93-101、load :103） | **改造（工具保留宿主，J3）**：目录仍来自聚合缓存；正文改经 MCP `resources/read` + digest/frontmatter 校验（统一 activation） |
| F2 | 缓存填充①：`SkillsMiddleware::before_agent` 本地扫描 + MCP 合并 | `peri-middlewares/src/skills/mod.rs:404-431`（扫描 :406-412、合并 :414-425、写缓存 :427-431） | **搬到 MCP 侧**：删除本地扫描；缓存只由 MCP registry 投影（workspace + 外部 origin）填充 |
| F3 | 缓存填充②：冻结摘要一次性扫描 | `peri-middlewares/src/skills/mod.rs:271-282`（`build_frozen_summary`） | **搬到 MCP 侧**：经 J2 内容准入取 workspace/系统来源 manifest；宿主只做摘要渲染 |
| F4 | 缓存填充③：workflow agent 本地扫描并构造两工具 | `peri-middlewares/src/assembly/workflow.rs:174-195` | **搬到 MCP 侧**：改接同一 MCP registry 投影；删除本地 `scan_skill_roots` |
| F5 | 缓存填充④：`SkillPreloadMiddleware` 本地兜底扫描 | `peri-middlewares/src/subagent/skill_preload.rs:158-169` | **搬到 MCP 侧**：preload 只按名查 registry；未命中报缺口（不回落磁盘） |
| F6 | 命令注册同步扫描：`SkillsPort::available_skills` → `core:{name}` | `peri-middlewares/src/host_ports.rs:427-436`；`peri-acp/src/session/construction.rs:64-87` | **保留为宿主适配器（改只读投影）**：接口语义从「同步扫盘」改为「读 MCP registry 元数据」；理由——命令路由/UI 呈现属宿主协议面，但内容来源必须来自 MCP；时序随之异步（§5.6） |
| F7 | 正文读取分支：builtin 编译期嵌入 | `peri-middlewares/src/skills/content.rs:20-27`；`peri-middlewares/src/skills/builtin/mod.rs:23-52` | **搬到 MCP 侧**：`include_str!` 资产迁入 `peri-mcp-workspace`，经 resources 暴露 |
| F8 | 正文读取分支：MCP 发现期正文缓存 | `peri-middlewares/src/skills/content.rs:28-32` | **改造**：并入统一 activation（每次 read + 校验），不再信发现期缓存 |
| F9 | 正文读取分支：本地磁盘读 | `peri-middlewares/src/skills/content.rs:33-38` | **搬到 MCP 侧**：workspace 实例读盘；宿主删除该分支 |
| F10 | 查找 helper：`find_skill_in_list` / `find_skill_content`（内部调 `content::load`） | `peri-middlewares/src/skills/loader.rs:480-495`、`:464-473` | **删除**：`find_skill_content` 生产调用方为零；`find_skill_in_list` 仅剩 preload 兜底（F5 改造后无消费者） |
| F11 | 插件 manifest → 技能根列表（不读技能正文） | `peri-middlewares/src/plugin/loader.rs:325,685,710` → `peri-acp-types/src/plugin.rs:577` | **保留为宿主适配器**：插件安装目录与 manifest 属插件生命周期；只产出「根路径 + `plugin_name` 标签」交给 provider，**不读 SKILL.md**，故不属于 SkillTool 的 FS 依赖 |
| F12 | 配置读取：`settings.json` 的 `skillsDir` / `disableBundledSkills` | `peri-middlewares/src/skills/mod.rs:29-73` | **保留为宿主适配器**：读配置（非技能内容）并作为 provider 输入位（根列表 + 关闭位）；文档与测试需锁定 |

**结论（J5 后）**：

1. **不是全部来自 MCP**（事实不变）：MCP 只是四类来源之一；B/C/D 合计 4 个生产调用点，覆盖会话冻结摘要、技能工具、命令注册、子代理预载四条链。workspace builtin 当前不提供任何 skill（E1）。
2. **收敛问题已由 J5 关闭**：读盘与正文读取整体归位 MCP 侧；宿主保留的只是「不读技能内容的适配器」——插件 manifest 解析与 `plugin:{name}` 命名空间（F11）、配置读取（F12）、命令/UI 投影（F6）、SkillTool 聚合（F1）。原 X2 的「收敛到什么程度」不再开放；剩余问题是**收敛后的语义与信任口径**（X3 存量语义、X6 本地来源批准策略）。
3. 分层落点：**MCP 侧（`peri-mcp-workspace`）=** FS 三根扫描与正文读取（F2/F3/F5/F9）、builtin 资产（F7）、对外经 `resources/list|read` + `skills/list|get` 暴露；**外部 MCP server** 原生 MCP 通道；**宿主 =** F1/F6/F11/F12 与统一 activation、关闭集、命令面。第三方 `.claude/skills` 布局保持不变，兼容靠布局而非双读路径。
4. 代价与风险：目录扫描语义（symlink、深度/目录上限、同名先到先得、`:`→`-` 规范化）必须在 MCP 侧 provider 内重建，且**迁移期不得出现双读**（X3）；命令注册与冻结摘要的时序从「同步会话构建期」改为「MCP 内容准入后」，需要 §5.6 的 seam 与 J2 的时序（含 UI 上 `/skill` 出现的时点变化）；插件安装/卸载 → 资源清单失效链需接通（E18）；`disable_bundled` 关闭位要迁为 provider 输入。
5. 未确认：外部 MCP server 提供 skills 的现网规模；本地技能目录被 MCP 侧读取后，其存在性/名称会经 `resources/list` 与 registry 暴露给所有会话的可见性口径（受 session/origin 过滤约束，§7.3 第 3 条），本轮无运行时数据。

---

## §3 已复核现状（静态证据，不是运行通过声明）

| 编号 | 观察 | 证据 |
| --- | --- | --- |
| E1 | workspace 构造并包装 Read/Write/Edit/Glob/Grep/folder_operations/Bash；仅覆写 get_info/list_tools/call_tool，没有 resources handler。cwd 绑定不等于沙箱。 | `mcp-packages/workspace/src/workspace.rs:5-15,65-85,93-121` |
| E2 | common 的 server_info 仅开启 tools；未知工具是 invalid_params，工具执行失败投影成 CallToolResult error；安全错误依赖 typed allowlist，不能透传任意后端 detail。 | `mcp-packages/common/src/helpers.rs:13-17,40-66`；`failure.rs:1-17`；`result_mapping.rs:5-25`（后二者同目录） |
| E3 | builtin dispatch 枚举只转发 info/tools；仅改 workspace handler 不足以让 resources/custom methods 穿过 builtin 链路。runtime 持独立 duplex/task 和关闭，不持业务。 | `peri-middlewares/src/mcp/builtin/dispatch.rs:42-90`；`runtime.rs:1-10,24-36`（同目录） |
| E4 | WORKSPACE_TOOLS 七项均 direct；实例声明表同源驱动默认 system_mcp_tools。system 标识与必需工具名单是不同维度。 | `peri-acp-types/src/builtin_mcp.rs:123-195`；`peri-middlewares/src/mcp/builtin/mod.rs:215-228,267-268,327-329` |
| E5 | prepare_system_tools 只提升/校验工具；readiness 证据是当前代 initialize + live tools/list，不包括 skills/resources。StartupState 只能暂存/取出候选工具，不能写 prompt/transcript。 | `peri-middlewares/src/mcp/system_tools.rs:1-10,40-56`；`client/readiness.rs:1-19,66-79`（同 mcp 目录）；`peri-agent/src/middleware/capabilities.rs:158-168` |
| E6 | McpSkillRegistry 已是 session 级注册表，HandleToken 强引用+ptr identity 防 ABA，投影检测连接换代。注册内容是 SkillMetadata，仍可携正文。 | `peri-acp-types/src/mcp_skills.rs:1-25,65-106`；`peri-acp-types/src/skills.rs:42-71` |
| E7 | 现有 MCP skills 支持 extension 分流、分页 skills/list、digest/frontmatter 校验和 skills/get 恢复；但是发现会并发读取全文。legacy 扫描也读正文。 | `peri-middlewares/src/mcp/skill_discovery.rs:1-19,74-89,125`；`skill_discovery/skills_list.rs:25-79,102-103,204,327,672`；`skill_discovery/legacy_scan.rs:37-78`（均位于同 mcp 目录） |
| E8 | SkillsMiddleware 提供两项 direct 工具，before_agent 扫盘并合并 MCP 元数据；MCP 项明确不进入非 frozen prompt 摘要。MCP 正文从发现缓存取，缺失不读盘。 | `peri-middlewares/src/skills/tools.rs:29-47,83-103`；`skills/mod.rs:395-456`；`skills/content.rs:15-37` |
| E9 | 本地技能 roots 是 User→Global→Project→Plugin→Builtin；loader 会规范化 name、trim description，原扫描跟随 symlink。不能原样搬走即宣称规范一致。 | `peri-middlewares/src/skills/loader.rs:42-88,95,259-283,414-451`；`skills/mod.rs:138-149` |
| E10 | SkillPreload 以假 SkillTool tool-use/result 消息追加到用户输入后；registry-first，miss 后才本地扫描；RPC slash 路径直接读取 meta.content。 | `peri-middlewares/src/subagent/skill_preload.rs:40-64,133-175,191-209`；`peri-middlewares/src/mcp/skill_discovery.rs:281-290,300-339` |
| E11 | DiscoverMCP 是只读 deferred 快照查询，已接 agent_registry；mcp_read_resource 是通用资源入口，已有 skill 清单/digest 恢复校验，但入口无注册条目时不能据此宣称完成技能激活校验。 | `peri-middlewares/src/mcp/discover_tool.rs:1-7,30-58`；`resource_tool.rs:39-50,79-94,233-279`（同目录） |
| E12 | 已有 McpAgentRegistry：resources 快照→元数据，activate 才读、限256KiB、解析、digest；approval key 含 origin/URI/digest/tools/model/maxTurns；现阶段清空远端 skills 请求。entries 使用全池客户端，需补 session/关闭集审计。 | `peri-middlewares/src/mcp/agent_registry.rs:1-17,50-67,89-108,129-179,208-221`；`subagent/tool/mcp_activation.rs:13-65` |
| E13 | 本地 agents 有编译期 builtin 内容；加载路径先 project，再 builtin，再 plugin。AgentDefineMiddleware 仍有读盘 load_overrides，parser 含本地特有权限/hooks/memory 等字段。 | `peri-middlewares/src/subagent/built_in_agents.rs:19-58`；`subagent/tool/definitions.rs:40-88`；`agent_define/mod.rs:43-110`；`claude_agent_parser/mod.rs:19-87` |
| E14 | 项目指令 frozen 入口依次取 cwd 的 AGENTS.md/CLAUDE.md/.claude/AGENTS.md 首个存在文件，CLAUDE 支持深度3 import，另读 CLAUDE.local.md；普通 middleware 搜索还追加 user/extra，并支持 excludes。两条路径并非完全相同。 | `peri-middlewares/src/agents_md/mod.rs:78-124,127-158`；`docs/standards/documentation.md` DOC-LOADER-001 |
| E15 | ACP 构造 frozen 数据仍直接调用指令读盘与技能扫描；PreparedSessionInputs 明确准备阶段不启动 MCP 等执行资源，随后同步构建并编码 frozen。这里存在真实迁移时序缺口。 | `peri-acp/src/session/frozen.rs:55-74`；`peri-acp/src/host/prepared.rs:1-6,95-124` |
| E16 | SkillsPort 当前是同步扫描接口；SkillsProvider 调本地技能/agent 扫描。删 middleware loader 时不能漏掉 ACP 命令/可用 agents 投影。 | `peri-acp-types/src/ports.rs:462-470`；`peri-middlewares/src/host_ports.rs:427-445`；`peri-acp/src/session/construction.rs:67`；`peri-acp/src/prompt/mod.rs:449-451` |
| E17 | 关闭集工具投影有既有四面契约；装配的 open_builtin_bridges 只保留 direct。skill 发现驱动只传 session_id/cancel，connected 列表未在这里消费 builtin_closures。扩展资源关闭面不可只依赖工具过滤。 | `docs/standards/architecture-contracts.md` ARC-CAPABILITY-CLOSURE-001；`peri-middlewares/src/assembly.rs:391-407`；`peri-middlewares/src/mcp/middleware.rs:254-274` |
| E18 | 已有持久 resource cache、epoch/ticket 与通知失效；invalidate_resource_cache_origin 当前分别失效 read 或资源列表/模板域，不等价于 MCP skill registry、Agent 已激活缓存一并 stale。 | `peri-middlewares/src/mcp/resource_cache.rs:1-7,21-23,77-91`；`client/cache.rs:305-324`；`client/subscription.rs:106-116`（同 mcp 目录） |
| E19 | 全仓内容搜索未找到 `refresh_catalogs` 符号，不能把该线索当现有 API。确认的相近 seam 是 tool_catalog.refresh、run_before_reason_catalog 与 startup static-base 提交。 | 搜索结论＝未确认该命名；正向证据：`peri-agent/src/agent/stages/reason.rs:26-34`、`middleware_runner.rs:78-104` |
| E20 | **本地 skill 的用户显式触发链**：`/{skill}` 命中命令注册表 `core:{name}`（kind=Skill）→ `AgentPassthrough` 返回 `Inject(用户原文)` → 原文进 agent 管线 → `SkillPreloadMiddleware` 提取 `/name` token 并注入全文（fake SkillTool ToolUse/ToolResult 消息序列）。 | `peri-acp/src/session/construction.rs:47-88`；`peri-acp/src/session/command/mod.rs:98-117`；`peri-agent/src/session/exec/executor.rs:477-511`；`peri-middlewares/src/subagent/skill_preload.rs:23-38,111-135,186-210` |
| E21 | **MCP skill 的显式触发链**：`/{server}:{skill}` → `McpSkillReleaser`；交互式放行用户原文（仍由 E20 的 preload 注入全文），RPC 路径直返 skill 全文。 | `peri-middlewares/src/mcp/skill_discovery.rs:281-345` |
| E22 | **模型侧显式加载**：`SkillTool(skill_name)` 按 cached metadata 匹配后经 blocking 线程读正文，**工具结果即全文**；工具为 direct 且 prompt_declaration 存在。 | `peri-middlewares/src/skills/tools.rs:29-47,83-103`；`skills/mod.rs:395-402` |
| E23 | **子代理预加载**：`agent_def.frontmatter.skills` 驱动 `SkillPreloadMiddleware`，注入同一 fake SkillTool 序列（全文）。 | `peri-middlewares/src/subagent/tool/build_agent.rs:146`；`subagent/mod.rs:78`；`subagent/skill_preload.rs:111-135` |
| E24 | **Skill 来源与调用方盘点**：四类来源（FS 三根 / 插件 / builtin / MCP）各自加载路径与生产调用点数量，详见 §2.3；MCP 条目有 3 个消费点。 | `peri-middlewares/src/skills/loader.rs:103-269,407-454`；`skills/mod.rs:271-286,404-425`；`subagent/skill_preload.rs:144-165`；`host_ports.rs:427-436`；`peri-acp/src/session/construction.rs:64-87` |
| E25 | **SkillTool / DiscoverSkillsTool 自身零文件系统 API**：`tools.rs` 全文件无 `std::fs` / `std::path` 调用；正文来自 `content::load`，目录来自 `cached_skills`。FS 依赖是传递性的。 | `peri-middlewares/src/skills/tools.rs:92-104,168-183`（搜索零命中） |
| E26 | **宿主侧技能缓存/目录的全部填充点**（除 MCP registry 外）：before_agent 扫描、冻结摘要扫描、workflow agent 扫描、preload 兜底扫描、命令注册同步扫描。 | `peri-middlewares/src/skills/mod.rs:404-431,271-282`；`assembly/workflow.rs:174-195`；`subagent/skill_preload.rs:158-169`；`host_ports.rs:427-436` |
| E27 | **内容读取的三条分支与查找 helper**：builtin 嵌入 / MCP 缓存 / 本地读盘；`find_skill_in_list` 内部再调 `content::load`，`find_skill_content` 生产调用方为零。 | `peri-middlewares/src/skills/content.rs:18-40`；`skills/loader.rs:480-495,464-473` |

文档交叉校验：`docs/code-index/{mcp-packages,peri-middlewares,peri-acp-types}.md` 的 package 路由、skills/agent 注入及 builtin 两表章节已读，仅作导航；`docs/reference/mcp-ecosystem.md:544` 尚写“阶段二未做 skills/list/digest”，与 E7 不一致，应在实施文档收口时纠正，不能据此判断当前代码尚无该能力。MCP Apps relay 另有既定 envelope/权限边界，不是本次技能传输替代通道（`docs/design/mcp-multiplexing.md` §3、§9.7–9.9）。

---

## §4 规范对齐差距表

| 规范形态 | 仓库现状 | 差距 / 目标 | 影响面 |
| --- | --- | --- | --- |
| §5.3–5.4 文件 resources + skills 索引 | E1–E3：workspace tools-only | 新增资源 provider、list/read/templates、skills/list/get；声明能力并穿过 dispatch，不改其他 builtin 能力位 | `peri-mcp-workspace` workspace.rs + resources/；`peri-middlewares` builtin/dispatch.rs |
| 全量 frontmatter、name/path 一致 | E9：本地 loader 只取选定字段并改名 | 保留规范原值；CLI alias 是宿主路由字段，不能改 wire frontmatter。非法/历史名称如何处理待 X3 | workspace resources/skills；`peri-acp-types` skills.rs |
| Discovery 仅元数据 | E6–E8：发现时抓全文并缓存 | 分离 catalog 与 activation cache；复用 registry 连接状态，不再以 content 非空作为“已发现” | mcp/skill_discovery*、mcp_skills.rs、skills.rs |
| skills/get 支持未列出 URI；不能以 scheme 断 Skill | E7/E11：legacy 先按 scheme 筛选，通用读取与激活界限不统一 | 候选/权威状态显式化；显式 URI 走 get/验证；templates 不能当可枚举技能；未列附件不能绕过 manifest | mcp discovery/resource_tool；workspace resources |
| 激活统一校验与 origin | E8/E10：工具、preload、RPC依赖旧正文缓存 | 三入口共用异步 activation，不重复 loader；digest/frontmatter 不符停止使用，有限刷新重试 | skills/、subagent/skill_preload、mcp/skill_discovery |
| system 摘要 / 用户直接触发全文 / 普通延迟 | E4–E5/E8：只对工具做 direct，MCP skills 不注入摘要；本地 `/skill` 触发已注入全文（E20） | 已裁决双路径（§5.4）：system 技能**摘要**进 system prompt（覆盖旧「MCP 条目不进摘要」口径）、用户显式触发注入**全文**；不能往 system_mcp_tools 塞 URI；非 system 保持发现面 | mcp middleware/readiness；ACP frozen；Agent startup seam（条件） |
| 附件、路径、MIME、预算 | E1/E9：无对应 workspace resource 模块，旧技能扫描跟随 symlink | 资源公开面独立安全根，扫描+读取复验，逐文件清单，任意扩展名不误删；Bash/Read 原权限不借机修改 | workspace resources/path、skills、tests |
| 缓存与内容更新 | E6/E18：已有持久 cache/连接 token；资源失效不等价于所有激活状态失效 | 同 origin/session/auth/generation/resource revision 贯通；metadata 变更使正文、附件及批准 stale | mcp cache/subscription、两种 registry |
| §5.10 Agents | E12/E13：已有远端 Agent 消费，本地仍读盘/嵌入，远端 skills 清空 | 复用远端 registry，加本地资源 provider 与来源策略；补会话/关闭过滤、skill URI 激活；不新增 agents RPC | workspace resources/agents；mcp/agent_registry；subagent |
| 项目指令 | 该章无 profile；E14/E15 仍上层读盘 | 私有普通 resource + 宿主优先级/冻结；不得包装成 Skill 或 subagent 来自动执行 | workspace resources/instructions；ACP准备/frozen；agents_md |
| 段落覆盖（Peri 私有，J6） | 该章无 profile；E15/§6.4：冻结期宿主 `scan_harness_docs` 读盘 | 私有 resource（候选 `peri-meta://workspace/{section_id}`）：挂入 workspace resources，宿主冻结期按启用 section 读取；组合规则与契约类型不变，硬前置 J2 | workspace resources/meta；ACP `session/frozen.rs` |

---

## §5 目标方案与 seam（待批准）

### 5.1 提供方：扩展 workspace，不新建 crate/实例

**建议**在 `mcp-packages/workspace/src/resources/` 新增分域实现（skills / agents / instructions / meta 四类**来源**），与现有 `WorkspaceMcpServer` 合并成一个 handler；**不在包内新增技能工具**（工具保留宿主，§5.6）。理由：现行批准目标已指定 Workspace MCP 承载文件类能力（`docs/design/mcp-adaptation-v4-part-1.md:167-169`）；现有 package 职责表只缺资源行而非需要第六个能力实例（`mcp-packages/CLAUDE.md:21-31`）。

- server 侧只负责已授权文件/嵌入资产的解析、索引、快照和只读提供；不持模型上下文、HITL、外部 server pool。
- 宿主提供 roots/scope/plugin 标识、启用位、预算和作用域身份，包内扫描内容。不同 HOME/授权/工作区不复用无区分的实例 catalog。既有输入只有 Bash 的 task_manager/on_bg_complete（`mcp-packages/workspace/src/input.rs:5-15`）；**新增独立 Resource input DTO**，不得把空输入等同“技能不存在且已 ready”。
- input 在 initialize 前一次注入，遵守 AW3-11，不后置补 session 状态；不得用共享可变 registry 绕过一次性输入约束。host cwd 多作用域问题见 X3；提取时序见 §7.0 的 frozen 裁决。
- get_info 在 workspace 本地组合 resources + skills extension；不把 `common::server_info` 默认改成全实例支持 resources。不覆写 discover 的既有纪律保留（`workspace.rs:94`、`builtin/dispatch.rs:51`）。
- dispatch 必须转发 resource 方法和 SDK 承载 skills custom request 的入口；`rmcp` 根依赖为 3.1.4（`Cargo.toml:98`），具体 ServerHandler custom-method hook **未确认**，列为 W1 第一项 wire spike，禁止先猜签名施工。
- 工具失败沿 common failure/result_mapping；资源方法返回 MCP 方法级错误，不伪装 CallToolResult。未知 skills/get URI 按规范 `-32602`，其余错误码按 SDK/协议核实，安全文案不含主机绝对路径或正文。

### 5.2 URI、索引与版本/来源

以下是**建议 profile，非已有能力**：

| 类型 | URI / MIME | 发现与内容 |
| --- | --- | --- |
| Skill 根及附件 | `skill://{name}/SKILL.md` 或 `skill://{scope-prefix}/{name}/SKILL.md`；附件同根相对路径 | skills/list/get 为权威 manifest；resources/list 可列同批公开文件；SKILL.md text/markdown，附件按实际字节 text/blob |
| Agent 定义 | `agent://{scope-prefix}/{name}/agent.md`，text/markdown | resources/list 至少 uri/name/description/mimeType；read 返回完整单文档；不发明 agents/list/get |
| 项目指令 | 候选 `peri-instruction://workspace/main`、`peri-instruction://workspace/local`，text/markdown | list 只列存在且授权的文档；read 返回正文。可有 `peri-instruction://workspace/index` JSON manifest，标识候选顺序、选中项、scope、digest、import 依赖；此索引不是 MCP 核心原语 |
| 段落覆盖文档（MetaHarness，J6） | 候选 `peri-meta://workspace/{section_id}`（`{section_id}` = 文档 stem，消费面见 §6.4；scheme 名待 W1 冻结），text/markdown | list 只列 `{cwd}/.peri/meta/*.md` 实际存在的文档（stem = section_id）；**只在宿主冻结期按启用 section 读取**（配置 `true` 才 read，正文不预取）；read 返回全文，字节语义保留（不 trim、不解析 frontmatter）。来源白名单与失败语义见 X7/X8；`peri-meta://` 是**系统提示词覆盖面**，信任边界高于其余三类 |

Skill URI 的目录名与 frontmatter name 保持一致；scope-prefix 是稳定非秘密标识，不能把本机绝对路径塞进 URI，也不把 prefix 当 DNS。采用前缀时所有返回的入口/附件都使用一致的完整前缀，不同时伪造无前缀别名。本地同名选择及 shadow 项是否暴露由 X3 冻结。

origin 由宿主绑定的实例/连接赋值，不信任 server 文本自称来源。resource 的来源 scope、revision、选中路径标识等可用公开的 `io.peri/…` 私有 metadata（字段名待 W1 冻结），不得挤占 `io.mcpp/` 未登记项。区分四件事：包版本 `CARGO_PKG_VERSION`、MCPP 文档基线、Skill 可选 SemVer、内容 digest/revision；前三者都不能代替 digest。无 io.mcpp/version 时不自造版本。

推荐资源公开策略采用 §5.3 参考预算作为初始值，测试边界值；不按附件扩展名白名单过滤。拒绝 symlink 和扫描/读取间替换，读到完整字节后算 digest 再投影；路径解码一次、拒绝编码穿越和二次解码，不能仅 canonicalize 一次即声称解决 TOCTOU。内置 skill 用静态 registry 保持相同 manifest 形状，不把 `<builtin>/…` 当磁盘路径。

`resources/list`、skills/list 可以分页，但同一游标必须绑定 catalog revision，不能混合前后版本。首期不宣称 directoryRead/subscription 能力，除非对应 handler 和更新生命周期已实现。模板只描述可读路径范围，不能成为任意文件读取入口。

### 5.3 消费方：复用现有 MCP skills，不再建第二条扫描链

目标数据流：

```text
宿主授权 roots/来源配置 → workspace resource provider
  → skills/list|get + resources/list|read（既有 MCP transport）
  → McpSkillRegistry 元数据（origin + name + URI + frontmatter + manifest）
  → system 投递策略 / 普通发现策略
  → 同一 activation：权限 → read → digest/frontmatter → origin 标记
  → SkillTool 结果 / slash preload / RPC 返回 / 子 Agent preload
```

- 改造而非删除 `McpSkillRegistry` 的连接投影和 `HandleToken`；将正文移出发现条目，保留全量 frontmatter、manifest、权威/列表候选状态与 revision。opaque command/tool ID 是展示/查找键，不替代 `(origin,name,uri)`，跨来源同名必须可追溯并存。
- `skills/list` 成功只发布 metadata；不得先读正文“验证完才发现”。SDK/manifest结构错误作为发现错误，正文完整性延迟到激活。普通 MCP 仍不阻塞首 turn。
- activation 共用 `skills/get` 恢复与 verify 逻辑；加入 session/取消/权限/代际检查。发生内容漂移时，整份新 manifest 与正文一起校验并原子替换，不拼接新正文与旧摘要。frontmatter 不一致也是失败；是否允许一次 get 恢复须按规范和现有测试明确，不无界重试。
- `DiscoverMCP` 继续只读搜索 metadata（含 agent/resource），不在搜索中触发正文加载或批准。`mcp_read_resource` 继续通用读取，技能附件复用 manifest/digest 检查；读普通文本不能自动激活 Skill/Agent/项目规则。
- `SkillTool` 与 preload/slash/RPC 共用 activation；删除对 `meta.content` 的直接依赖。附件经 `mcp_read_resource(server_name, uri)`，不得让模型在 host 拼本地路径；未列出 URI 只可先 get 刷新 manifest，仍未列出即拒绝。
- 保留 legacy/模板/显式引用作为**对外协议互通**，但删除本地旧 loader 双轨。候选不等于已验证技能，空列表不等于无能力。缺 digest 的外部 Skill 按 X5/X6 确定的保守策略拒绝激活或要求额外批准，不静默伪造完整性。本地三来源的读取归位与宿主适配器边界见 §2.3。

### 5.4 投递机制：system 摘要、用户直接触发全文（已裁决）+ 非 system 发现

**已裁决（2026-09-29 用户原话）**：「system mcp 肯定是摘要在 system prompt 里面，但是如果是直接用户触发，则是全文」：

1. **system MCP 路径 = 摘要注入 system prompt**：system 实例的 skill 目录摘要（name / description / origin / URI）进入 system prompt（冻结面），无需模型先 `DiscoverMCP`。这覆盖既有「MCP 条目不进 prompt contribution」口径（`peri-middlewares/src/skills/mod.rs:443-445`，代码注释引「验收 9」；该验收出处未确认）：**system 实例**的 skills 摘要必须进入摘要注入面。摘要注入要求元数据在冻结前取得——与 §7.0 的 frozen 时序裁决同批落地。
2. **用户直接触发路径 = 注入全文**：用户显式触发时注入 SKILL.md 全文（经 §5.3 的统一 activation 校验、origin 标记与适用批准），附件仍懒加载。
3. `SkillTool` / `DiscoverSkillsTool` 是**宿主 direct 工具**（已裁决不下沉，§5.6），**不进入** workspace 声明表；`system_mcp_tools` 仍只控制 MCP server 的工具，不承载 URI 或资源选择。system 技能摘要进入 system prompt 是**上下文投递**，与两项工具是否可见互相独立。
4. 摘要与全文共用同一 discovery/activation 校验源；system 标识只影响投递时机与形态，**不提升权限**、不豁免 digest/frontmatter 校验。

**「用户直接触发」的判定口径（候选，待 X1 确认；不自行拍板产品语义）**：

| 候选 | 定义 | 对应既有 seam（E 编号） | 评价 |
| --- | --- | --- | --- |
| R1（推荐） | 用户 prompt 文本中的显式 token：本地 `/skill-name`（命令注册表 `core:{name}` 命中）与 MCP `/server:skill` | E20（construction.rs:47-88 → command/mod.rs:98-117 → executor.rs:477-511 → skill_preload.rs 全文注入）、E21（skill_discovery.rs:281-345） | 与「用户直接触发」直觉一致；有既有 seam 与测试；不依赖模型判断；本地与 MCP 两条 slash 链行为同构 |
| R2 | R1 + 模型侧 `SkillTool(skill_name)` 显式加载 | E22（tools.rs:83-103：工具结果即全文） | 该入口本身就是全文，纳入不改变执行语义，只影响术语与提示词文案（“触发者”是模型而非用户） |
| R3 | R2 + 子代理 frontmatter `skills:` 预加载 | E23（build_agent.rs:146 → skill_preload.rs:111-135：注入全文） | 属配置驱动的 activation，非用户动作；语义边界更远 |
| R4 | 一切进入上下文的入口都算（含 system 正文） | 全部 | 与已裁决「system=摘要」直接冲突，除非把 system 摘要定义为“prompt 内只注入摘要、正文一律全文”，会扩大首批注入量，不推荐 |

推荐 R1 作为「用户直接触发」的判定基准；R2/R3 保持既有全文语义但不称为「用户直接触发」（纳入与否只影响术语与验收断言范围）。R4 不推荐。

| 来源 | 目标首轮行为 | 后续路径 |
| --- | --- | --- |
| system MCP（含外部 system） | 有界等待 metadata；**技能摘要进入 system prompt**；用户显式 token 触发时注入全文；失败语义见 X5；能力未声明时正常为空/不适用，不凭空要求每台 server 支持 skills | 激活/附件读走共同路径；system 标识不提升权限 |
| 非 system MCP | 沿现有异步发现写 registry、命令和发现面，不阻塞首轮、不自动改 frozen 摘要（既有行为保留） | DiscoverMCP/技能查询 → activation；其工具继续 SearchExtraTools → ExecuteExtraTool |
| 关闭实例/关闭子能力 | 不投递、不发现、不激活；无法借 URI、slash、缓存、子 Agent/workflow 绕过 | 按统一 session policy 拒绝，不触发向关闭来源的 RPC |

**不能直接复用的部分**：StartupState 只有工具候选（E5）；before_react_start 又晚于首批 before_agent（`docs/standards/architecture-contracts.md` ARC-MIDDLEWARE-CAPABILITY-001）。不能让更早的 SkillsMiddleware 在未就绪时做“首轮保证”，也不能给 StartupState 偷加全状态写权限。

**frozen 时序（已裁决，见 §7.0）**：按采纳方案建立窄的 `ResourceCatalogSnapshot / ResourceContextSnapshot` seam（名称为提案）：受 lease 保护的内容准入阶段取得一次经过策略过滤的资源快照（含 system 技能摘要），frozen 渲染只消费该快照；RCRA 启动检查快照来源/代际与当前可用性。若进一步选择在 1R 暂存上下文候选，则新增 typed 暂存/提交接口，工具和上下文候选成功后统一提交，失败全丢弃；不得声称现有 StartupState 已支持。

### 5.5 seam 分类

| 分类 | seam | 计划处理 |
| --- | --- | --- |
| 复用 | system_mcp / system_mcp_tools、prepare_system_tools、typed bridge | 保留工具语义、原名 direct、first-wins 冲突和绑定来源审批；新增两项工具声明，不扩大到所有 system 工具 |
| 复用 | HandleToken、McpSkillRegistry 投影、MCP pool/read/cache、CommandRegistry | 连接代际/断连清理/命令投影沿既有链路，不另建宿主文件扫描 registry |
| 复用 | McpAgentRegistry、mcp_activation、subagent runtime | 资源定义消费与运行时仍分离；补边界而非新增 Agent runtime |
| 改造 | skills discovery/verify、metadata、preload、RPC命令 | 元数据发现/异步激活分离，共同校验；移除发现正文缓存假设 |
| 改造 | SkillsPort、SkillsProvider、ACP frozen/准备 | 从本地同步扫描端口改为资源快照读取；异步工作在明示生命周期阶段完成，不同步 block_on |
| 改造 | readiness/StartupState（条件）、frozen snapshot | X5 决定资源准入的证据范围与失败语义；frozen 时序**已裁决**（经资源读取后冻结，§7.0），保留 fail-closed 与原 frozen 字节复用 |
| 改造 | 闭包、缓存失效、Agent registry | session/auth/关闭集贯通，失效清理 metadata/activated/approval，不只清磁盘 cache |
| 新增 | workspace resources provider + Resource input | server 侧资源索引、预算、安全读取、静态内置资产；不依赖 peri-middlewares |
| 新增 | 项目指令 resource profile；宿主聚合侧可能需要窄的 origin 过滤/读取端口 | 来源收敛后（J5）聚合目录需按 session/关闭集收窄时才引入（关闭语义见 X4）；不把原始 pool 或 broker 塞进包里 |
| 未确认 | refresh_catalogs | 本次搜索无该符号；实施用已核实的 refresh/run_before_reason_catalog，若新增须作为新增接口评审 |

### 5.6 数据路径：SkillTool 只读 MCP 侧来源（J3 + J5，已裁决）

**已裁决两条（2026-09-29）**：
- （J3，用户原话「Skill Tool 还需要根据其他 mcp 的 skill 从而把控」）`SkillTool` / `DiscoverSkillsTool` **保留在宿主层**（`peri-middlewares`），职责是跨多个 MCP server 的 skill 聚合与把控；**不下沉**到 `mcp-packages/*`，workspace 包内不注册同名工具。
- （J5，用户原话「Skill Tool 的能力收敛为只有读取 MCP 侧提供的源，Skill Tool 本身要解除对文件系统的依赖」）**SkillTool 的数据源只有 MCP 侧**：路径中的文件系统读取（`~/.claude/skills`、`settings.json skillsDir`、项目 `.claude/skills`、builtin 内嵌资产）一律退出工具数据源，读盘职责整体归位 MCP 侧（§2.3 的 F1–F12 处置表）。

因此 v1 计划「两工具进入 workspace 包」与 X2 原「跨源边界」方案 A/B **均作废**；`docs/design/mcp-adaptation-v4-part-1.md:168`（「`SkillTool` / `DiscoverSkillsTool` 下放到 Workspace MCP 工具包」）与 J3 冲突，获批后须修订该行（§10）。

#### 5.6.1 谁读盘、经什么 seam 暴露

| 来源 | 读盘方 | 进程/边界 | 暴露 seam | 消费方 |
| --- | --- | --- | --- | --- |
| 本地技能三根（User/Global/Project）、插件技能根、builtin 资产 | **`peri-mcp-workspace`（`WorkspaceMcpServer`）内的资源 provider** | 进程内 builtin（AW3-01：独立 duplex/task/context，同一 OS 进程，独立 MCP 实例边界；未来换 stdio 形态不改协议） | `resources/list` + `resources/read`（附件同通道）、`skills/list` + `skills/get`（清单/完整性） | 宿主 SkillTool / DiscoverSkillsTool / preload / 命令面，全部经 **MCP client** |
| 外部 MCP server 的技能 | 各 server 自身 | 各自 transport（stdio/http/acp） | 同上（`skills/list\|get` 优先，legacy 走 `resources` 扫描） | 同上 |
| 插件 manifest 与配置（不读技能正文） | 宿主插件加载器 / 配置读取 | 宿主 | **不是技能来源**：只产出「根路径 + `plugin_name` 标签 + 关闭位」作为 workspace 实例的资源输入 DTO（§5.1） | workspace provider |

宿主的 `McpSkillRegistry`（`peri-acp-types/src/mcp_skills.rs:147,247,267,294`）继续是**唯一聚合目录**，但条目只来自 MCP 发现结果（本地 workspace 实例按普通 MCP server 注册，与第三方无区别）；`before_agent` 只做 registry → `cached_skills` 投影（改造 `peri-middlewares/src/skills/mod.rs:404-431`），不再做任何本地扫描或合并。

#### 5.6.2 聚合多个 MCP server（含第三方）的规则

- 条目键 `(origin, name, uri)`：`origin` = 宿主绑定的实例/连接名（workspace 与第三方一律平等），跨 origin 同名并存、不静默覆盖（`mcp_skills.rs:247-330` 现有查找需扩展为 origin 感知）。
- 工具入参保持 `skill_name` 字符串（兼容现有提示词与调用方）；解析顺序：唯一命中即用；多 origin 命中时按确定性规则消歧（建议：`{origin}:{name}` 显式形态，与 `/server:skill` 命令同构），歧义时返回候选列表而非任选。
- 全文读取：对命中 origin 发 `resources/read` + digest/frontmatter 校验（复用 F1 改造）+ origin 标记；附件同径按需读。
- `DiscoverSkillsTool` 输出聚合目录（含 origin 与 source 标签），覆盖 workspace 与外部来源。

#### 5.6.3 改造前后对照

| 维度 | 现状（E8/E25–E27） | 目标（J5 后） |
| --- | --- | --- |
| 技能目录来源 | 宿主本地扫描 + MCP registry 双源合并 | **已交付（W4b）**：仅 MCP registry（workspace 实例承担本地三根 + 插件根 + builtin；外部 MCP 原样）——`SkillsMiddleware` 零扫描，`cached_skills` 只由 registry 投影填充 |
| 正文读取 | `content::load`：builtin 嵌入 / MCP 发现期缓存 / `std::fs::read_to_string` | **已交付（W4b）**：统一 activation：`resources/read` + digest/frontmatter 校验；`content.rs` 与 builtin 嵌入注册表已删除，无本地分支 |
| 工具自身 FS | 无直接调用，但传递依赖磁盘 | 无直接调用，且**数据源中不存在磁盘路径**（可用静态断言 + 行为断言验证） |
| 命令 `/skill` 注册 | 会话构建期同步扫盘（`construction.rs:64-87`） | **已交付（W4b）**：MCP 发现完成后由 registry 投影注册（`project_core_skill_commands` + `CommandRegistry::reconcile`，异步时序，`kind = Skill` / `source = Core`）；构造期扫盘注册与 `SkillsPort::available_skills` 已删除 |
| 冻结摘要 | `build_frozen_summary` 本地扫描（mod.rs:271-282） | **已交付（W4b）**：经 J2 内容准入（P4）取 workspace 实例（system MCP）的 skills 清单快照，`render_frozen_summary` 渲染；system 摘要规则同 J1；读取失败 fail-closed（X5），面不适用/被关闭 ⇒ 空且不回落磁盘 |
| workflow agent 技能 | 本地扫 project skills 后构造两工具（workflow.rs:174-195） | **已交付（W4b）**：接同一 registry 投影（`WorkflowAgentContext.mcp_skill_registry` → `build_tools` / `build_middlewares`），与主链同一份目录（不再只看 project-level） |
| 子代理 preload | registry-first，miss 后本地扫描（skill_preload.rs:144-169） | **已交付（W4b）**：仅 registry（`lookup_exact` → `lookup_by_command` → 裸名 `lookup`）；未命中=缺口报告（warn），不回落磁盘 |
| 可用性 | 磁盘在即可用（与 MCP 连接无关） | **workspace 实例不可用/关闭 ⇒ 本地技能整体不可用**（**已交付（W4b）**：来源投影处剔除 + P4 快照为空；明确为空/缺口，不回落磁盘） |
| 完整性 | 本地来源无 digest 概念 | 本地来源同样有 manifest/digest（服务端计算），校验一致 |

#### 5.6.4 失败语义（SkillTool 视角）

| 情形 | 行为 |
| --- | --- |
| 某 server 未连接/初始化失败 | 该 origin 条目视为 stale：**不**出现在目录中（或标注不可用，按 X5 定）；按名调用返回明确错误（含 origin 与原因类别），**不回落磁盘** |
| 单次 `resources/read` 失败/超时 | 按现有读超时与取消契约返回错误；可经 `skills/get` 刷新重试一次（§5.3），不作无界重试 |
| digest/frontmatter 不匹配 | 拒绝使用（MUST NOT），报告校验失败；不注入未验证内容 |
| 部分 origin 可用 | 聚合目录仍工作（可用 origin 正常列出/加载），降级范围以 origin 为界并在发现面标注 |
| 全部 origin 不可用 | `DiscoverSkillsTool` 返回空目录 + 明确原因；`SkillTool` 报不可用；不得静默成功 |
| workspace 实例被关闭（策略/配置/环境） | 本地技能与两类工具可见性按关闭矩阵处理（§7.3）；不降级为宿主读盘 |

#### 5.6.5 与既有裁决的关系

- **「不互调」（part-1:81,178）不冲突**：`:81` 明确「由 Agent/Runtime 分别调用 MCP，并通过宿主端口接收结果」；`:178` 限定 5 个实例之间不互调。宿主 SkillTool 以 **MCP client** 身份向多个 server 发 `resources/read`，是被认可的宿主调用模式。唯一需修订的是 `:168` 的工具下放目标句（J3）。
- **权限与信任**：本地技能进入 MCP 通道后与外部来源共用同一 registry/read 路径；是否对「本机 workspace origin」沿用现有免逐技能批准策略，是新冲突，登记为 X6（§7.1）。
- **会话可见性**：本地技能目录进入 `resources/list` 后，文件存在性/名称经由 MCP 面暴露；受 session/origin 与关闭集过滤约束（§7.3 第 3 条），实现时必须验证不跨会话泄漏。

---

## §6 Agents 与项目指令资源化

### 6.1 项目指令：搬内容获取，不把文件内容升级为授权

目标分工：workspace 持文件公开、读取与 import 内容解析；宿主持选择规则、合并层级、权限与 frozen 生命周期。`AgentsMdMiddleware` 可以保留为**纯快照 prompt contribution adapter**，删除其中读盘、candidate_paths/find_file/read_frozen_content 和文件 import loader；不保留无 MCP 时的本地读取 fallback。该边界与批准目标一致（`docs/design/mcp-adaptation-v4-part-1.md:189`），不是要求把 prompt rendering 也移到 server。

- 按 E14 保留“主 frozen 只选 cwd 三候选首个匹配、不向父目录继承”及 `CLAUDE.local.md` 叠加；普通路径的 user/extra/excludes 不得无意变成所有主会话默认行为。选中的空文档是否继续查找按既有语义测试，不借迁移改变优先级。
- 方案建议：宿主向 provider 传解析后的**选择策略输入**；provider 返回选中资源、依赖 manifest 与内容，宿主校验并冻结，不重新扫描磁盘作第二次选择。实际磁盘定位由 server 保管。
- `@import` 从 server 的授权范围读取，保持深度3与循环防护（E14）；允许根外 import 的范围、symlink 收紧对存量的影响须 X3 决定。不能直接将 Skill 的“禁止隐藏路径”应用到 `.claude/AGENTS.md` 本身：Skill 目录内部规则与宿主已授权的资源源文件是不同边界。
- `CLAUDE.local.md`、user/extra 资源默认私有，resource list 也可能泄露文件存在性，必须在返回 metadata 之前过滤。来源标签在上下文中清晰可见；读取文本不能新增工具授权、禁用审批或改写宿主安全层。
- resources 内容更新只使活跃资源视图 stale，**不修改已经持久化的 frozen prefix**；同会话/子 Agent/fork 继续复用已选快照，新会话可看到新内容。恢复历史可显示旧快照，不代表取得当前关闭能力的执行授权（ARC-FROZEN-001、ARC-CAPABILITY-CLOSURE-001）。

### 6.2 Agent 定义：资源化三类来源，保留宿主运行时

- 将 builtin 定义 Markdown 及 `built_in_agents.rs` 的嵌入表迁到 workspace 的静态 Agent resource provider，删除上层嵌入副本和 get/list fallback。项目与插件定义也由 provider 读取、产出 `agent://…/agent.md`；宿主不再调用 `read_definition` 或 `scan_agents_detailed` 读文件（现入口 E13、E16）。
- 复用 `McpAgentRegistry` 与 `load_and_approve_mcp_agent`；先补元数据 session/关闭集过滤，再迁本地来源。不能把 server 自报 `scope=builtin` 视为可信内置来源。需要 host-assigned 来源描述，保留 project/plugin/builtin 的可追溯身份。
- E13 的实际 loader 优先级是 project→builtin→plugin；不要从 builtin 文件的“最低优先级”注释推断 plugin 一定覆盖 builtin。推荐先保持现有**本地**选择行为，同时跨 origin 同名并存、远端不覆盖本地。是否统一改优先级另列 X3，未裁决不改。
- parser 的纯 YAML/Markdown 解析若 server 与 host 都需要，提取为契约层纯解析模块（建议 `peri-acp-types/src/agents/parse.rs`）；不把文件 I/O 或 permission 策略搬入 types。`claude_agent_parser` 上层旧实现/重复导出在消费者迁完后删除，不留 shim。远端 v1 类型与 Peri 本地扩展显式区分，不能所有字段直接反序列化后执行。
- 已有远端规范化会清空 `permission_mode/hooks/memory/background/isolation/allowed_write_dirs/tone/proactiveness/prompt_mode/skills`（E12）。本地定义资源化若全部走此路径，会改变已有行为；X3 必须选择：严格 v1 丢弃本地扩展，或 host 针对受信本地 origin 明示支持选定扩展。两者都不允许 server 提权，远端未知字段仍默认忽略。
- Agent `skills` URI 请求进入同一 Skill activation，每项独立解析、校验、批准；不再简单清空，也不能因 Agent 已批准就跳过 Skill 校验。无法满足 preload 时报告缺口，不静默启动不完整配置。
- 只把候选描述投递主模型；Agent 全文在激活后作为子 Agent 候选 system prompt。若用户要求 system Agent 内容直注入主会话，须另行裁决，不能混同 system Skill 规则。非 fork 不自动继承主历史/凭据/批准；能力收敛按 §5.10.3 和宿主上限。

### 6.3 删除与保留清单

| 上层对象 | 目标处理 | 删除门槛 |
| --- | --- | --- |
| `SkillsMiddleware` | **保留两项技能工具（宿主跨 MCP 聚合，J3）**、prompt section、frozen 摘要渲染；删除本地 roots 扫描（`skills/mod.rs:404-412`）、`build_frozen_summary` 的本地扫描（`:271-282`）与「本地正文缓存权威」——聚合缓存只由 MCP registry 投影填充 | 聚合工具按名加载覆盖 workspace 与外部 origin；**`skills/` 无 `std::fs` 读调用**；摘要首轮与 slash/RPC 测试通过；不注册 workspace 同名工具 |
| `SkillPreloadMiddleware` | 保留输入提取、顺序、消息位置和取消；删除本地扫描兜底（`skill_preload.rs:158-169`）与 `content::load`，改为异步 activation（仅 registry） | 主 Agent、子 Agent、命令入口结果同源；registry 未命中时报告缺口而非回落磁盘；无假调用绕过批准 |
| `skills/tools.rs` | **保留在宿主**（J3）并改造：目录元数据仅来自 MCP registry 投影，正文改走统一 activation（J5）；删除 `cached_skills` 作为正文来源的旧假设 | 两工具仍为宿主 direct 工具；文件内无 `std::fs`/`std::path` 读路径；跨 origin（workspace 本地技能 + 外部 MCP 技能）按名加载与校验一致；**技能磁盘目录被移走后行为不变（经 MCP 读）** |
| `skills/loader.rs`、`skills/content.rs`、`skills/builtin/`、`find_skill_in_list`/`find_skill_content` | 整体删除：扫描与读盘迁 workspace provider（F2/F3/F5/F7–F10）；builtin 资产迁 `peri-mcp-workspace`；宿主只留按 origin 分发读取通道 | 全部 ACP/command/plugin/frozen 消费者已切换；不靠 re-export 维持旧路径 |
| `assembly/workflow.rs` 的本地技能扫描（`:174-195`） | 改为消费同一 MCP registry 投影；workflow agent 工具仍用宿主两工具 | workflow agent 的 skill 目录含 workspace 与外部 origin；本地扫描零残留 |
| `host_ports.rs` 的 `SkillsPort::available_skills`（`:427-436`） | 改为只读 MCP registry 投影（F6）；ACP 命令注册（`construction.rs:64-87`）改为发现完成后异步注册 | `/skill` 路由在 MCP 发现后出现；关闭 SkillsMiddleware/workspace 时路由不注册。**W4b 已交付（2026-09-29）**：`core:{skill}` 裸名命令改由 MCP 发现管线异步投影（`project_core_skill_commands` + `CommandRegistry::reconcile`，kind = Skill / source = Core），关闭位**两半**都已进投影判定——① **workspace 实例关闭**（`"WorkspaceMiddleware" ∈ disabled`）：A24 关闭集在来源投影处整体剔除，关闭实例不产生 `core:` 路由（既有交付）；② **`SkillsMiddleware` 链槽关闭**（`"SkillsMiddleware" ∈ disabled`，收口修复）：宿主装配从**同一份** `disabled_middlewares` 派生 `HostAssemblyInput::skills_face_closed` → `BuiltinInstanceContext.skills_face_closed`，发现管线一次读出并传投影——为真 ⇒ 目标集合置空、`reconcile` 同批撤下既有条目（不留幽灵路由），`{server}:{skill}` MCP 发现面与实例本身不受该位影响。**证据**：`cargo test -p peri-middlewares --lib`（新增 `mcp::skill_discovery::core_face_tests` 三条：投影函数级正例含 frontmatter `aliases` 派生 / 关闭位撤下既有条目且 mcp 面保留 / `before_agent` 管道级差分）、`-p peri-acp --lib`（新增 `host/requests_skill_resources_test.rs::skills_middleware_disabled_hides_core_commands_but_keeps_mcp_face`：默认会话发现后有 `core:{skill}`、`SkillsMiddleware=false` 会话无且 `workspace:{skill}` 仍在、13_skills 段与摘要不进冻结 prompt） |
| `AgentsMdMiddleware` | 保留纯 contribution adapter；删除全部读盘/搜索/import 行为 | 新建/legacy/read-only恢复/fork/frozen 回归均有证据 |
| `AgentDefineMiddleware` | 倾向删除空 hook/槽位，overrides 由资源激活结果交给既有 prompt renderer | 同批更新 production_blueprint、assembly、MetaHarness 名单和测试；不得顺带重排其余槽位 |
| `subagent/built_in_agents.rs` 与 `built-in/*.md` | 上层表和文件删除，包内静态资源替代 | catalog、定义加载、workflow、resume、builtin开关均已接资源/快照 |
| `claude_agent_parser/` | 纯解析迁契约层或按 X3 保留唯一 host parser；禁止复制一套 | 所有 parse_agent_file 消费者与本地扩展策略核对完毕 |
| `mcp/skill_discovery*`、`mcp/agent_registry.rs` | 保留 MCP client adapter/策略；改造发现与激活，不迁宿主 pool 到包内 | 按 §5、§7验收 |
| `SubAgentMiddleware`、HITL、runtime/context/transport | 保留生命周期与执行授权，仅更换内容来源输入 | 不以“resources 下沉”为由扩大本次范围 |

这里的“删除优于兼容”是删除上层重复的内部实现，不是删除外部协议所需的 legacy resource discovery，也不是无迁移声明地破坏历史 frozen snapshot（`CLAUDE.md:23`；ARC-FROZEN-001）。

### 6.4 段落覆盖文档（MetaHarness）：扫描 resources list，硬前置 J2

**用户裁决（J6）**：「MetaHarness 也是可以挂入 workspace 里面的 resources，扫描 resources list 即可」。本项是 §5.2 表的第四类资源，与 Skill / Agent / 项目指令三类并列，但信任边界更高（直接改写系统提示词段落），因此独立成节。

**现状（静态证据）**：

- 唯一 FS 读取点：`peri-acp/src/session/frozen.rs:79-82` 冻结期调用 `peri_middlewares::meta_harness::scan_harness_docs(cwd)`；
- scanner 语义（`peri-middlewares/src/meta_harness/mod.rs:23-74`）：仅 `{cwd}/.peri/meta/*.md` 一级、非递归、stem = key、读取失败 warn + 跳过、目录不存在 → 空 map；
- 组合规则（`frozen.rs:194-237`）：`SECTION_IDS` + `true` + 文档存在 → `section_overrides`（覆盖在 `PromptTemplate::new` 构造期生效）；`true` + 缺失 → warn + 保持内置；`false` → 静默；未知键已在解析期移除；
- 契约类型 `peri-acp-types/src/meta_harness.rs`（`SECTION_IDS` / `MIDDLEWARE_NAMES` / `BUILTIN_INSTANCE_POLICY_KEYS`）**不在本项范围**。

**硬前置（时序事实，J2）**：冻结发生在 `PreparedSessionInputs::prepare_new`（`peri-acp/src/host/prepared.rs:67,110` → `build_frozen_data_with_config_and_runtime`），而 MCP pool 的构造与 `run_initialize` 在其后的 `assemble_prepared` 内（`peri-acp/src/host/workspace.rs:59-60`）。**现状冻结期 MCP 池尚不存在**，因此「扫描 resources list」在 J2（内容准入 / 两阶段顺序，§7.0）落地前不可实现。本项是 J2 的**消费者**，不是 J2 的前置；W0/W1 不单独施工，禁止以「把 Mcp 槽位前挪」等违反 ARC-MIDDLEWARE-001 的方式抢跑（§7.2 同款边界）。

**目标分工**：

- workspace provider：扫描 `{cwd}/.peri/meta/*.md` 并以 `peri-meta://workspace/{section_id}` 暴露（§5.2 第四类；scanner 语义逐字保留：一级、`.md`、stem = key、读取失败跳过、空目录 = 无资源）；list 列出全部存在的 `.md` stem（含不在 `SECTION_IDS` 中的文件，逐字保留现今 scanner 行为）；宿主仅消费配置启用且 ∈ `SECTION_IDS` 的 key；该边界并入 W1 契约冻结时确认；
- 宿主（frozen 内容准入阶段）：`resources/list` 过滤出**配置启用的 section** → 对命中项 `resources/read` → 以同一 `build_meta_harness_state` 组合规则构建状态（组合函数与契约类型不变）；未启用文档不读取（正文不预取）；
- 冻结后不可变语义不变（ARC-FROZEN-001）：`section_overrides` 随冻结载体传播，会话内不重读。

**保留不改**：配置面（`meta_harness` map 仍走宿主 settings 链路）；`SECTION_IDS` 等契约常量；`PromptTemplate::new` 的覆盖生效时机；`.peri/meta/` 的文件位置与命名约定（`{section_id}.md`）。

**失败与关闭语义**：按 X8；来源白名单按 X7。

**实施记录（2026-09-29，W3b-consumer 交付）**：provider 半边已交付（`peri-mcp-workspace/src/resources/meta.rs`）：URI 形状冻结于契约 `MetaUri` / `meta_uri` / `parse_meta_uri`（`peri-acp-types/src/workspace_resources.rs`），authority = `workspace`、`_meta` scope = `project`；**symlink 按更严口径跳过**（W1「公开面禁 symlink」不变量优先于旧 scanner 的跟随行为，协调者裁定 A）；不设资源模板；空文件照列且 `read` 返回 `""`。宿主消费半边：`session/frozen.rs` 的 `build_frozen_data_with_config_and_runtime_and_docs` 只消费传入 docs（FS 扫描点删除），冻结期经真实 builtin `workspace` 实例读 `resources/list` → 过滤「配置启用且 ∈ `SECTION_IDS`」→ `resources/read`（`peri-middlewares/src/mcp/client.rs::read_builtin_workspace_meta`，X7 按 `ConfigSource::Builtin { instance: "workspace" }` 实例身份过滤、X8 关闭集命中返回空批）；组合仍用同一 `build_meta_harness_state`。构建时点：new 走 `prepare_new_deferred` → P1 草稿/lease → P2 装配 → P3 activate → P4 读资源并构建 frozen → P5 `commit_frozen`（发布前读取失败按 J2 补偿排空+撤销）；**legacy 首次接纳例外**（J2 §3.1/§6.3：接纳事务前无执行环境，存储层绑定先于执行所有权，legacy 保持「候选-构建一次 + adopt write-once + winner 重读」语义，覆盖不可得按 X8 保持内置并 warn，不回落磁盘）；冷 load/resume/fork 复用持久字节不变；`peri-middlewares/src/meta_harness/`（scanner 与测试）已删除，宿主侧 `.peri/meta` FS 读取点归零。

**未接线项已闭合（W4a，2026-09-29）**：生产装配点 `peri-middlewares/src/mcp/builtin/dispatch.rs` 的 `workspace` arm 现按 `ctx.workspace_resources` 装载资源输入（`Some` → `WorkspaceMcpServer::with_resources`，`None` → 资源面未接线）。输入由会话环境装配（`peri-acp/src/host/workspace.rs` 的 `assemble_with_frozen`）随 `HostAssemblyInput` / `BuiltinInstanceContext` **一次注入**（早于 `McpClientPool::run_initialize`，AW3-11 同一节奏；未引入共享可变 registry，也没有第二注入点），构造点不读配置。**只接 meta 面**：`WorkspaceResourcesInput::new().with_disable_bundled(true)`——不装 skill 根 / agent 目录（技能、代理来源的接线与消费切换同批归 W4b，先装会双源同名）；`disable_bundled = true` 是波次域隔离（provider 无域掩码，这是唯一能关掉 builtin 技能面的开关），W4b 必须以宿主配置的真实值替换。被动上线的域：provider 按 cwd 无条件列出的项目指令面（`peri-instruction://workspace/index`；main/local 仅在对应文件存在时列出）与既有 `workspace://git/ref`；宿主消费不切换（W4b/W5），行为不变。顶层三路径（TUI/print/stdio）不产生会话、无资源消费者，`workspace_resources: None` 保持未接线。
**端到端证据**：`peri-acp` 的 `requests_test::meta_resources::{new_session_meta_override_flows_through_builtin_workspace_resources, new_session_meta_override_unavailable_keeps_builtin_and_never_reads_disk}`（生产 `session/new` 路径，非测试专用构造器）；`peri-middlewares` 的 `mcp::builtin::dispatch::tests::dispatch_resource_wire_tests::w4a_*`（工厂装载语义）。provider 包文档（`mcp-packages/workspace/src/workspace.rs` 模块头与 `with_resources` 文档）仍写着「生产装配点不调用 `with_resources`」，本波禁改冻结包，登记为**过时文档**待包内维护。

**归属**：W3（与「system 摘要投递与 frozen 时序」同一冻结期消费面）；随后删除 `peri-middlewares/src/meta_harness/` scanner，测试语义迁到包内 provider 测试。

---

## §7 裁决：已定与待定

### 7.0 已裁决（2026-09-29，共六项 J1–J6）

**已裁决项不是建议，依赖它们的波次按此处口径施工；待裁决（X1、X3、X4、X5、X6、X7、X8）未定前不得先行施工。**

| # | 裁决 | 内容与采纳方案 | 涉及面 / 生效范围 |
| --- | --- | --- | --- |
| J1 | system 投递粒度（用户原话：「system mcp 肯定是摘要在 system prompt 里面，但是如果是直接用户触发，则是全文」） | system MCP 的 skill **摘要**注入 system prompt（冻结面）；**用户直接触发**注入**全文**。两路径共用同一校验与 origin 标记；system 身份不提升权限。触发口径候选 R1–R4 与推荐见 §5.4；术语范围待 X1 收敛，不影响执行语义 | §5.4；覆盖既有「MCP 条目不进 prompt contribution」口径（`peri-middlewares/src/skills/mod.rs:443-445`，注释引「验收 9」，出处未确认）——system 实例摘要**必须**进入摘要注入面。W3 验收 |
| J2 | frozen 时序（用户答复「是的」） | 采纳原 D6 方案：拆「只读配置准备」与「受 lease 保护的内容准入/冻结提交」——获得绑定/lease 后启动会话资源，read 完成才原子发布可执行 session 与 frozen；失败补偿。`prepared.rs` 的同步含义与 new/legacy write-once 时序随之调整 | `peri-acp/src/host/prepared.rs:1-6,95-124`、`peri-acp/src/session/frozen.rs:21-74`；`peri-resources` 会话存储事务能力**未确认**（W0 取证）。ARC-WORKSPACE-001 / ARC-FROZEN-001 语义保持：不改中途重读、不改冷恢复/fork 精确字节。W3/W5 前置 |
| J3 | SkillTool 归属（用户补充裁决：「Skill Tool 还需要根据其他 mcp 的 skill 从而把控」） | `SkillTool` / `DiscoverSkillsTool` **保留宿主**做跨 MCP 聚合，不下沉；下沉的是 skill **来源**。聚合 seam、改造点与「不互调」关系见 §5.6 | 作废 v1「工具进 workspace 包」与 X2 原跨源边界条目；需修订 `docs/design/mcp-adaptation-v4-part-1.md:168`（`:81,178` 的实例互不调用不受影响）。W4 验收 |
| J4 | Skill 来源盘点（用户提问的调查回答，非产品裁决） | **不是全部来自 MCP**：四类来源、4 个生产调用点；可收敛为单一内容通道，插件 manifest 与第三方目录保留宿主适配。见 §2.3 | 收敛范围由 J5 关闭；与工具位置（J3）互相独立 |
| J5 | SkillTool 数据源收敛（用户原话：「Skill Tool 的能力收敛为只有读取 MCP 侧提供的源，Skill Tool 本身要解除对文件系统的依赖」） | `SkillTool` / `DiscoverSkillsTool` 的**唯一数据源是 MCP 侧**：本地三根扫描、builtin 嵌入与 `content::load` 本地分支整体退出工具数据源，读盘归位 `peri-mcp-workspace`（§2.3 处置表 F1–F12、§5.6 数据路径与改造前后对照）。宿主保留的只是「不读技能内容的适配器」：插件 manifest 解析与命名空间（F11）、配置读取（F12）、命令/UI 投影（F6）、聚合工具（F1） | 关闭原 X2；新增 X6（本地来源信任/批准）；连带更新 X3（语义在 MCP 侧 provider 重建）、X4（关闭 workspace ⇒ 本地技能不可用、无 FS 兜底）、X5（按 origin 的失败语义）。W2/W4 验收；`docs/design/mcp-adaptation-v4-part-1.md:168` 修订范围扩为「工具不下放 + 来源全走 MCP」 |
| J6 | MetaHarness 覆盖文档资源化（用户原话：「metaharness 也是可以挂入 workspace 里面的 resources 里面的，这个就比较简单了，扫描 resources list 即可」） | `.peri/meta/*.md` 的扫描与正文读取从宿主迁为 workspace resources（`peri-meta://workspace/{section_id}`，scheme 名待 W1 冻结）；宿主冻结期改为「resources/list 过滤启用 section → 命中项 resources/read → 同一 `build_meta_harness_state` 组合」（§6.4）。**硬前置 J2**：现状冻结先于 MCP pool 兴起（`prepare_new` → `assemble_prepared`，`peri-acp/src/host/workspace.rs:59-60`），本项是 J2 两阶段顺序的消费者，归属 W3；W0/W1 不单独施工 | `peri-acp/src/session/frozen.rs:79-82,194-237`、`peri-middlewares/src/meta_harness/`（scanner 后删）；契约类型 `peri-acp-types/src/meta_harness.rs` 不变；新增 X7（来源白名单）/X8（关闭与失败语义）。关闭 workspace ⇒ 覆盖能力不可用（保持内置段落）、无磁盘兜底 |

### 7.1 待用户裁决（X1、X3、X4、X5、X6、X7、X8；原 X2 已由 J5 关闭）

**X1 用户直接触发的判定口径**
问题：「用户直接触发 → 注入全文」以哪个入口为准（本地 `/skill-name`、MCP `/server:skill`、模型 `SkillTool`、子代理 frontmatter 预载）？
选项：A（推荐，§5.4 R1）仅用户 prompt 文本中的显式 token；B 在 A 之上把模型侧 `SkillTool` 并入「直接触发」术语；C 在 B 之上并入子代理 `skills:` 预加载；D 一切进入上下文的入口都算（与 system 摘要冲突，不推荐）。
影响：只影响术语、提示词文案与验收断言范围（B/C 的执行行为已是全文，无执行差异）；D 会改变首批注入量。
若不定：默认按 A 执行；B/C 入口保持既有全文语义但不计作「用户直接触发」。

**X3 存量本地语义的兼容口径（技能根/symlink/同名/命名 + 本地 Agent 扩展字段）——J5 后范围更新**
问题：读盘归位 MCP 侧后，目录扫描语义（symlink 跟随、深度/目录上限、同名先到先得、`:`→`-` 规范化）与本地 Agent 扩展字段（permissionMode/hooks/memory 等）在新读取路径上如何处置？
选项：A（推荐）技能资源公开面禁 symlink（存量 symlink 技能失效需登记）、wire frontmatter 不改写（保持规范 name；`core:` 路由别名由宿主从 metadata 派生）；本地受信 origin 的 Agent 保留既定 host 扩展 profile，远端严格 v1；B 全保留现状语义（provider 内复刻 symlink 跟随与改名）；C 全收紧严格 v1（本地扩展失效、需迁移说明）。
影响：第三方 `.claude/skills` 生态与现有用户技能可用性、`core:{name}` 路由与 wire name 的一致性、本地 subagent 行为；实现落在 MCP 侧 provider（原 `skills/loader.rs:95,259-283` 的语义需在 provider 重建）。
若不定：默认按 A 执行。

**X4 关闭策略与项目指令 profile——J5 后影响更新**
问题：项目指令是否以私有 `peri-instruction://` resources（+索引）承载？Workspace/子能力关闭时资源、技能聚合来源与发现面如何联动（J5 后本地技能可用性完全依赖 workspace 实例）？
选项：A（推荐）私有普通 resources、不伪装 Skill；关闭 workspace = 本地技能与两类工具来源同时消失、纯历史快照可展示但无激活授权，且**任何组件不得回落磁盘读取**；B 项目指令继续宿主读盘（与 J5 的读取归位不一致，仅当用户希望项目指令例外）；C 关闭只隐藏工具、保留资源读取。
影响：ARC-CAPABILITY-CLOSURE-001 扩展面、DOC-LOADER-001 读取位置、跨宿主互操作（私有 scheme）；关闭后本地技能的可用性下降属 J5 的必然结果，需在关闭矩阵中显式断言「无 FS 兜底」。
若不定：默认按 A 执行（scheme 名待 W1 冻结；关闭采用策略键三入口一致矩阵）。

**X5 规范承诺范围与失败语义——J5 后扩为一处**
问题：本 profile 承诺到哪一级——是否声明 `directoryRead`、是否消费 `depends_on`/`tools`/`context_budget` 编排字段、§5.7.1 的 MUST 是否纳入；system 资源失败是否阻止首个模型请求；单个 MCP server 不可用/读取失败时（J5 后本地技能也在其中）降级语义如何界定？
选项：A（推荐）首期不声明 directoryRead、不实现自动依赖编排（明示不消费并记录）；system 已声明且被选中的投递失败 fail-closed；单 origin 失败 = 该 origin 条目 stale/不可用并按 §5.6.4 报告，其余 origin 继续；其余失败降级为空并告警；B 完整实现编排与依赖底线（含环检测）；C 全部失败都不阻塞（仅告警）。
影响：W2/W3 实现量与验收面、readiness 证据范围（E5）、能否宣称「完整规范对齐」；B 需先锁定规范 commit。
若不定：默认按 A 执行，且不得对外宣称已完整符合 MCPP。

**X6 本地来源的信任与批准策略（J5 新冲突）**
问题：本地技能改经 MCP 通道（进程内 workspace 实例）后，是否沿用既有「本机来源免逐技能批准」策略，还是与外部 MCP 来源统一走内容绑定批准（digest/frontmatter/内容变更重批）？
选项：A（推荐）**按 origin 分级**：workspace/本机受信 origin 沿用免逐技能批准（仍必需 digest/frontmatter 校验与 origin 标记），外部 origin 维持现有批准语义；B 全部 origin 统一逐技能批准（最严格，改变现有本地技能体验）；C 全部 origin 均免批准（不推荐，弱化外部来源安全）。
影响：HITL/Permission 语义、批准缓存键（origin/URI/digest/能力）、本地技能使用体验、`mcp/skill_discovery` 与 `subagent/tool/mcp_activation` 的批准路径；B/C 需同步修订 ARC-HITL-001 的适用面。
若不定：默认按 A 执行（本地来源仍校验内容完整性，但免逐技能批准弹窗）。

**X7 段落覆盖文档的来源白名单（J6 新裁决点）**
问题：`peri-meta://` 段落覆盖文档是否只允许 workspace（本机受信 origin）提供？外部 MCP server 若声明同名 scheme 的资源，是否一律不进入覆盖扫描？
选项：A（推荐）**只消费 workspace 实例提供**的 `peri-meta://` 资源；外部 origin 的同 scheme 资源不进入覆盖扫描（拒绝并记录，不静默）；B 允许任意已批准 MCP origin 提供（外部 server 可通过覆盖系统提示词段落注入任意指令，须另配内容批准/digest 绑定）；C 所有 origin 都不允许经资源提供覆盖（放弃段落覆盖能力，与 J6 方向相悖，不推荐）。
影响：prompt 注入面与信任边界——覆盖面比技能/Agent 更敏感（直接替换系统提示词段落），是把 X6 的 origin 分级原则推广到最高敏感面的判定；B/C 需同步修订 ARC-HITL-001 适用面与 `docs/meta-harness.md`。
若不定：默认按 A 执行（外部 origin 的 `peri-meta://` 资源不被消费；scheme 名待 W1 冻结）。

**X8 MetaHarness 覆盖的关闭与失败语义（J6 新裁决点）**
问题：workspace 关闭、`peri-meta://` 资源缺失或读取失败时，段落覆盖如何降级？是否阻塞会话创建？
选项：A（推荐）覆盖能力不可用即降级为**内置段落**（与现今「文档缺失 → warn + 保持内置」逐字同构）；不阻塞会话创建；**不回落磁盘**读 `.peri/meta`；关闭 workspace 的语义 = 「无法覆盖系统提示词段落」，进关闭矩阵（ARC-CAPABILITY-CLOSURE-001 面），与 J5 的「本地技能不可用」并列记录；B 配置为 `true` 但资源不可得时 fail-closed 拒绝创建会话（把段落定制失败当致命）；C 静默忽略（无告警，降低可观察性，不推荐）。
影响：关闭矩阵与 X5 失败语义交叉、DOC-LOADER-001、会话可用性；关闭态下 `meta_harness` 配置键的可观察语义变化须写入 `docs/meta-harness.md`；A 的告警口径沿用现今 warn（`frozen.rs:217-221` 同款文案面）。
若不定：默认按 A 执行。

### 7.2 明确保留、不覆盖的裁决

- part-4 AW3-01 同进程 builtin、AW3-11 initialize 前一次输入、AW3-04 七工具无新沙箱、runtime 的独立 transport/task 均保留；资源模块不创建新的 pool/runtime。
- `spec/issues/2026-09-28-system-mcp-direct-tool-names-plan.md` §已确认目标/本次范围：system_mcp_tools 选择、原名 direct、冲突 first-wins、非 system 前缀和审批绑定保留。**宿主两项技能工具（保留原位，J3）** 继续走同样的可观察目录准入；workspace 不注册同名工具、不建双注册。
- **part-1 目标条款的冲突与处置（不再列待裁决）**：`docs/design/mcp-adaptation-v4-part-1.md:168` 写「`SkillTool` / `DiscoverSkillsTool` 下放到 Workspace MCP 工具包」，与 J3 冲突；获批后按 J3 + J5 修订该行（**skill 来源全部经 MCP 侧读取/下放、工具保留宿主聚合且零 FS 依赖**）。`:81,178` 的「实例互不调用」不冲突（宿主以 MCP client 身份调用各 server，§5.6 已论证），无需修订。
- **两工具不得恢复任何宿主文件系统读取（J5）**：`skills/` 下不得保留 `std::fs`/`tokio::fs` 的技能读取路径；不得以「磁盘兜底」处理 MCP 不可用（§5.6.4）。宿主侧唯一可保留的 FS 相关职责是 F11/F12（插件 manifest 与配置读取，不读 SKILL.md）。
- ARC-MIDDLEWARE-001 不允许为了提前拿资源把 Mcp 槽位简单挪到 Skills/AgentsMd 前面。冻结准备用明确 seam（J2）；必要删除槽位同时改蓝本和装配，不重排其他槽位。
- part-4 AW3-10“不回填 part-1 施工进度”保留；本计划只有获批目标语义变化才更新 part-1 的对应目标条款，不把波次进展写入设计。DOC-DESIGN-001 同理。

### 7.3 权限、关闭集和生命周期

1. **关闭集扩展矩阵**：保留 direct tools、deferred index/resolver、parent_tools、workflow 四面，并新增 resources/list/read/templates、skills registry（含宿主聚合目录）、agent registry、**段落覆盖可用性（`peri-meta://`，J6：关闭 ⇒ 无覆盖来源；降级语义按 X8）**、slash/ACP/TUI completion、static prompt/examples、preload与运行时授权。关闭判断来自同一 frozen/session policy，不靠工具名前缀；关闭某实例必须使其 skill 条目不进入宿主聚合目录（J3），并断言技能加载**不回落磁盘**（J5：关闭 workspace 后本地技能不可用是预期行为，不是缺陷）。E17/E12 说明现有工具过滤不足以自动覆盖资源。
2. **read不等于grant**：resource读取与Skill/Agent激活分开；工具 schema direct 不等于免审批。scripts 只能作为内容读取，执行仍经原工具和 Permission/HITL；依赖/required tools 字段不授予能力。远端Agent批准必须内容/能力绑定，缓存命中也重新做权限收敛。
3. **隔离**：注册表/资源缓存含 host-assigned origin、授权域、session/workspace身份及连接generation；`workspace` 同名不代表所有会话同来源。私有本地文档先采用不跨会话持久复用的保守策略，只有有证据证明隔离后才扩大缓存。
4. **取消与关闭**：所有read/get/刷新/批准都有整体deadline和session cancel，旧handle返回不能回填新代。任务归宿主owner，关闭顺序不变；包内不以无主spawn延续会话。现读恢复的取消/换代限制已有注释（`peri-middlewares/src/mcp/resource_tool.rs:84-94`），本次activation重构要覆盖，不照搬缺口。
5. **错误/日志**：记录方法、大小、固定错误类、脱敏来源；不记录正文、绝对私有路径、env、token。资源清单/错误同样可能含敏感信息，沿 ARC-SECRET-001 和 common 安全错误范式。

### 7.4 性能与兼容/回滚

- server 生成 digest 必须读取字节，但这是**工具侧索引工作**，不等于 client Discovery 拉全文。采用每scope/revision单飞、分页、预算与有限并发；禁止每个列表请求无预算递归整个 HOME。内容变更由read复验与新manifest确认，不能用mtime或包版本代替完整性。
- 正文和附件按需取；大正文先完整校验再给明确“超预算/需选择”结果，不先截断再声称digest正确。首次system摘要/指令准备记录耗时、文件数和字节数，预算不足显式反馈；不注入全部附件。
- resource更新使activation/cache stale；frozen不随之重写。需要测“同会话旧摘要+新激活内容”的来源/revision可见性与安全性，避免无提示使用不同版本。
- 不保留本地/MCP双读、双注册或旧工具执行alias。发布按波次完整切换，故障回滚用完整波次版本回退；关闭workspace是“能力不可用”，不是回退旧middleware读盘。
- **J5 的时序与可得性变化**：`/skill` 命令注册与技能目录出现时点由「会话构建期同步」变为「MCP 发现完成后」（F6/§5.6.3）；冻结摘要依赖 J2 的内容准入。两处都需在 W3/W4 记录可观察行为变化，并验证无「先出现后消失」的竞态（发现重扫只做增量替换）。
- frozen持久格式若变化，须保留既有历史读取义务和未知版本fail-closed；新快照不由旧二进制原地覆盖。发布前冻结格式版本与降级策略；必要时只允许新会话启用新格式，不能为兼容偷偷重读当前磁盘替换历史内容。

---

## §8 分波实施、验收与测试

### 8.1 波次（每波独立验证，后波依赖前波的契约）

| 波次 | 工作及前置 | 独立验收 |
| --- | --- | --- |
| W0：冻结前置 | 确认 X1、X3、X4、X5、X6、X7、X8（J1–J6 不再复核）；锁定规范版本；复核并行LSP完成后的装配；核实 rmcp custom-method hook、会话准备/lease事务与frozen路径 | 裁决记录具备明确取舍/覆盖项（J1–J6 引用原话与影响面）；形成new/load/resume/fork/legacy顺序图，证明无准备期执行与无可执行半会话；无法证明则阻塞后续，不以架构猜测开工 |
| W1：资源提供与wire | X3/X4/X5；workspace Resource input、provider、manifest、list/read/templates、skills/list/get、Agent/指令资源、**本地三根+插件根+builtin 资产读取（J5 归位）**；dispatch完整转发；**不新增技能工具**、不切换上层消费；**段落覆盖资源（J6）按 §6.4 归属 W3，不在此波单独施工** | 独立包使用临时root+真实rmcp内存wire通过；能力位与实际方法一致；list每项可读；get未枚举合法项可读、未知为-32602；其他builtin不错误声明resources；workspace 工具表仍为七项（无 SkillTool）；七工具schema/顺序/取消不变；本地技能经 `skill://` 可按名读取（含 builtin 静态资产与插件根） |
| W2：发现/激活分离 | W1；改造McpSkillRegistry（origin 感知查询、歧义候选）、metadata、verify/cache、slash/preload共同activation；先用fixture验证再接默认workspace | skills/list发现阶段resources/read计数=0；主动激活才读取；digest/frontmatter失败不注入；资源模板/空列表/显式URI可发现；command和tool相同校验/批准；断连换代/取消晚响应不能复活旧条目；workspace 与外部 origin 同名可消歧 |
| W3：system摘要投递与frozen时序 | X5（失败语义）、X7/X8（J6）；按 J2 落实会话内容准入与snapshot；保留现工具readiness，把资源候选作为明确新增证据 | **system 实例技能摘要进入 system prompt**（首个模型请求可见、无需先 DiscoverMCP；本地技能经 workspace origin 同规则）；**用户显式 token 触发注入全文**（J1，口径按 X1 基准 R1）；非system延迟不阻塞；按 X5 失败语义终止或降级、无半提交；new/legacy失败补偿、cold load/resume/fork精确复用旧frozen；摘要与全文同源校验；**MetaHarness 覆盖改经 `peri-meta://`（J6）**：宿主 `scan_harness_docs` FS 读取点删除、仅对启用 section `resources/read`；缺文档 warn + 保持内置（逐字同构）；workspace 关闭时覆盖不可用且**无磁盘兜底**（X8）。**W3b-provider 已交付**（`peri-meta://` list/read、`MetaUri`/`meta_uri`/`parse_meta_uri`、`_meta` scope=project、symlink 更严口径跳过、无模板、空文件列出且 read 返回 `""`）；**W3b-consumer 已交付**：new 路径冻结构建点后移到 P3 activate 之后、`commit_frozen`（P5）之前，legacy 首次接纳按 J2 §3.1 保持接纳前构建（无执行环境 ⇒ 无 MCP 资源面，覆盖不可得按 X8 保持内置）；宿主 `.peri/meta` FS 读取点与 `peri-middlewares/src/meta_harness/` 一并删除；system 摘要投递（W3c）不在本波 |
| W4：SkillTool 零 FS 切源（工具保留宿主） | X1/X6；按 J3/J5 保留 `SkillTool`/`DiscoverSkillsTool` 于宿主；删除全部宿主技能 FS 读取点（F2–F5、F7–F10：`skills/mod.rs` 扫描、`build_frozen_summary` 扫描、workflow 扫描、preload 兜底、`content::load` 本地/builtin 分支、查找 helper）；`skills/tools.rs` 改读 MCP registry + 统一 activation；F6 命令注册改异步投影 | 宿主仍只有一个 SkillTool/DiscoverSkillsTool（workspace 不注册同名工具）；**`skills/` 无 `std::fs` 读调用**；移走技能磁盘目录后工具行为不变、workspace 不可用时不回落磁盘；按名加载覆盖 workspace 与外部 origin 且同名可消歧；`/skill` 路由随发现出现且关闭时不注册；主会话/slash/RPC/subagent/workflow 一致。**W4a 已交付**（2026-09-29，711bf1d0）：资源面输入经 `BuiltinInstanceContext.workspace_resources` 在 `run_initialize` 前一次注入，dispatch 的 `workspace` arm 按 `ctx.workspace_resources` 调 `WorkspaceMcpServer::with_resources`；当波只接 meta 面（`disable_bundled = true` 为波次域隔离，登记偏差 D2）。**W4b 已交付**（2026-09-29，8c3d975a）：① provider `get_info` 在资源面装配时声明 `capabilities.extensions["io.modelcontextprotocol/skills"]`（键的单一事实源 `peri_acp_types::skills::SKILLS_EXTENSION_ID`；未装配 provider 的实例不声明 = 声明即实现），客户端门闩 `peer_declares_skills` 因此由 builtin 链路命中；② 宿主装配（`peri-acp/src/host/workspace.rs`）按真实配置构造 `WorkspaceResourcesInput`——skill roots（User/Global/Project/插件根带 `plugin_name`）、`disable_bundled` = `settings.json` 真实值（D2 闭合，`disable_bundled(true)` 常量已删）、不装 agent 根；③ F2–F5/F7–F10 全部删除：`SkillsMiddleware` 零扫描（`before_agent` 只做 registry 投影）、`content.rs` 与 `skills/builtin/` 与 loader 扫描/查找 helper 删除、workflow 两工具与 subagent 预载改接同一 registry 投影、`SkillsPort::available_skills` 与构造期扫盘注册删除；④ F3：P4 内容准入期经 `read_workspace_skill_catalog`（builtin `workspace` 身份 + Connected + 未关闭，有界等待，不 block_on）取 system 来源技能快照 → `render_frozen_summary` 渲染冻结技能摘要（J1/W3c 投递半边），读取失败 fail-closed（X5）、面不适用/被关闭 ⇒ 空且不回落磁盘（X4/J5）；⑤ F6：`core:{skill}` 裸名命令改由 MCP 发现管线投影（`project_core_skill_commands` + `CommandRegistry::reconcile`，kind=Skill / source=Core，frontmatter `aliases` 派生别名），外部 origin 保持 `{server}:{skill}`；系统来源由连接事实标注（`McpSkillRegistry::mark_system_origins`，`ConfigSource::Builtin` + 未关闭），A24 关闭实例在来源投影处整体剔除（不可发现/不可激活）。**证据**：`cargo test -p peri-middlewares --lib`（1552 + 8 passed / 0 failed）、`-p peri-acp --lib`（771 passed / 0 failed，含新增 `host/requests_skill_resources_test.rs` 三条端到端：项目技能进冻结摘要 / `disableBundledSkills=true` 隐藏 builtin / 关闭实例 ⇒ 摘要为空且不回落磁盘）、`-p peri-mcp-workspace --lib`（367 passed / 0 failed）；静态证据 `grep -rn "std::fs" peri-middlewares/src/skills/` 零命中、`scan_skill_roots` 生产调用点为零。**F6 关闭位收口（2026-09-29，8c3d975a）**：补齐验收缺口「关闭 `SkillsMiddleware` 时路由不注册」——宿主装配（`peri-acp/src/host/workspace.rs`）从**同一份** `disabled_middlewares` 同批派生 `HostAssemblyInput::skills_face_closed`（`"SkillsMiddleware" ∈ disabled`），经 `BuiltinInstanceContext::with_skills_face_closed` 随上下文一次注入 pool；发现管线（`run_ensure_discovery` / `run_discovery_with_cache`）一次读出该位并传 `project_core_skill_commands`：为真 ⇒ 目标集合置空、`reconcile` 同批撤下既有 `core:` 条目（链槽关闭不留幽灵路由），`mark_system_origins`、`mcp_route_entries` / `finish_command_source`（`{server}:{skill}` 面）与实例装配均不消费该位。**本轮证据**：`cargo test -p peri-middlewares --lib` 全量（含新增 `mcp::skill_discovery::core_face_tests` 三条：投影函数级正例含 `aliases` 派生 / 关闭位撤下既有条目且 mcp 面保留 / `before_agent` 管道级差分）、`-p peri-acp --lib` 全量（新增 `host/requests_skill_resources_test.rs::skills_middleware_disabled_hides_core_commands_but_keeps_mcp_face`：默认会话有 `core:{skill}`、`SkillsMiddleware=false` 会话无且 `workspace:{skill}` 仍在、13_skills 段与摘要不进冻结 prompt）；修复前差分证据（同两用例红：`-p peri-middlewares` 编译期缺 `with_skills_face_closed` 与三参投影签名；`-p peri-acp` 断言失败，`snapshot` 含 `core:e2e-project-skill`）。**未完成/偏差**：外部 system MCP 的技能进冻结摘要不在本波（范围为 workspace origin）；legacy 首次接纳无执行环境 ⇒ 冻结摘要为空（J2 §3.1 语义，技能仍可经首轮 MCP 发现使用）；插件根技能的端到端用例未单独构造（provider 侧插件 scope 面由 W1 用例覆盖）。 |
| W5：Agent与项目指令消费切换 | X3/X4；先补registry会话/关闭过滤，再迁builtin/project/plugin Agent和项目指令；删除本地读取fallback及必要槽位 | 三来源定义可加载、同名不跨origin覆盖；未知字段默认忽略；Agent技能独立批准；指令优先级/import/local/excludes按profile；关闭矩阵、fork/非fork隔离与恢复满足契约 |
> **W5 交付事实（2026-09-29，提交 f66bd251）**
> **W5 补正（协调者审查裁决后，2026-09-29）**：① **F2**：workflow 面显式拒绝 `mcp__*` 远端 id（该面没有批准 seam；`resolve_agent_definition_via_registry`）。② **F3**：远端 agent 的 `skills` 恢复清空（技能级批准面不存在 —— W2b 结论 ⇒ 声明不构成隐式授权）；本地受信来源仍保留（X3-A）。③ **F4**：恢复 `{cwd}/agents` 为第二个 project 根（顺序在 `.claude/agents` 之后，与迁移前候选序一致）；登记扩张：`{cwd}/agents/*` 现在也进 `{{available_agents}}` 目录（迁移前只可显式加载）——「可激活即可发现」。④ **F5**：`peri-acp/src/host/workspace.rs` 拆出 `workspace_resources.rs`（1012 → 910 + 111 行，STD-SIZE-001 达标）；`session_lifecycle.rs`(1544，HEAD 1531)、`session_data.rs`(1278)、`peri-agent/src/agent/stages/mod.rs`(1011) 为**存量**超限（`check-file-size.sh` 报源码 3 / 测试 29），非本波引入，另立任务。⑤ **F6**：agent 名按来源拆分校验——本地走契约段校验（`_`/大写历史名可用，拒绝时 warn）+ 非 `mcp__` 前缀，远端沿用 HEAD 严格集。⑥ **F8**：关闭/断连的宿主绑定 builtin `workspace` 句柄**整体跳过**，不再投影成 `mcp__workspace__*` 远端条目。⑦ **F11**：DiscoverMCP 的 agent 投影绑定会话与 `SubAgentMiddleware` 关闭位（装配点从同一份 `meta_harness_disabled` 派生，单一来源）。⑧ **F13**：同 id 两形态优先级（目录形态先）显式化并加断言。⑨ **F14 纠错**：`plugin_agent_dirs` 在迁移前是**活管线**（`assemble.rs` → `frozen.rs` 喂 `scan_agents_detailed` 与 SubAgent loader），本波改为 provider 插件根输入后参数整体退场——不是「死管线」。⑩ 登记：symlink agent 定义不再可发现（X3 存量失效面，与技能侧并列）；指令链槽关闭不门控 P4 读取（与 W4b 技能面同构，行为保留）；legacy 首次接纳无执行环境 ⇒ 指令与技能摘要均不可得（J2 §3.1，新增 warn 信号）；preload 缺口报告强度属 W6。
> **W5 全量链与 flake 收口（协调者，2026-09-29）**：① acp 首轮全量抓到 1 例失败：`host::requests::tests::frozen_cases::test_session_load_cold_host_restores_original_frozen_prompt`（创建期 `claude_md()` 为 `None`）。根因经确定性实验钉死（`PERI_MCP_BUILTIN=off` 稳定复现）：`BuiltinInjectionPolicy::from_env()`（`peri-middlewares/src/mcp/config.rs`）在配置加载期读**进程级 env**，开关组用例在 `#[serial]` 临界区内置 `off`（`mcp_v4_builtin_test.rs:81`、`mcp_v4_wave2_test.rs:731`、`mcp_v4_startup_test.rs:142`），而 `serial_test` 只互斥标注者——读侧 4 个用例漏标 ⇒ 并行窗口内池无 `workspace` 句柄且 `initPhase` 已收口 `ready` ⇒ 资源面按 X4 静默缺席（**生产语义正确，缺陷在测试隔离**；owner 复跑另打中 `prepared_tests::new_session_persists_frozen_bytes_from_its_single_preparation`，同一形状）。修复：`prepared_test.rs` ×2、`requests_frozen_cases_test.rs` ×1、`requests_workspace_cases_test.rs` ×1 补 `#[serial]`（`TEST-HERMETIC-001` 读侧闭合；零断言放宽、零生产代码改动、未改 10s 上界；套件 74.4s → 77.4s）。此前记「已知 flake」的 `workspace_cases::worktree_new_resources_use_the_target_directory` 同根因、同批修复（升为已定缺陷）。② 最终链：`-p peri-acp --lib` **775 passed / 0 failed**（EXIT=0）；`-p peri-mcp-workspace --lib` **372 passed / 0 failed / 1 ignored**（EXIT=0）；`clippy --workspace --all-targets -- -D warnings`、`fmt --all --check`、`check-layer-imports.sh`（20 规则 / 0 违规）、`git diff --check` 全 EXIT=0；`check-file-size.sh` EXIT=1（源码 3 / 测试 29，均存量）。③ 存量负载敏感 flake 登记（非本波引入，未修复）：`mcp::workspace_recovery_tests::external_source_keeps_120_second_deadline_and_cancels_execution` —— `-p peri-middlewares --lib` 首轮 1539 passed / 1 failed（`assert_process_gone` 5s 上界超时，`workspace_recovery_test.rs:157`；文件自 `fb54f614` 起未改动、用例自身构造 ≥120s），首轮失败证据保留；单测复跑 1 passed（120.0s）、全量复跑 **1540 passed / 0 failed / 4 ignored**（EXIT=0）。④ 约定（`peri-acp/CLAUDE.md` Verify）：触碰 builtin `workspace` 资源面的用例必须与进程级开关组同键 `#[serial]`。
>
> **W5 交付事实（原始）**：① `McpAgentRegistry` 增会话可见性 + A24 关闭集 + host-assigned 来源（`AgentSource::{Local{scope,plugin},Remote}`，本地只认真实 builtin `workspace` 实例）；② builtin agent 资产迁 `mcp-packages/workspace/src/resources/builtin/agents/*.md`（`BUILTIN_AGENTS` 静态表，`agent://builtin/{id}/agent.md`），`peri-middlewares/src/subagent/built-in/*.md` 与 `built_in_agents.rs` 删除；③ 定义加载（`subagent/tool/definitions.rs`）与 workflow `resolve_agent_definition`（改 async，#[async_trait]）统一走 registry；④ `{{available_agents}}` 改由 `AgentCatalogPort::catalog`（`host_ports::AgentCatalogProvider` 绑定同一 registry，装配点 `preparation.rs` downcast）；⑤ 项目指令走 `peri-instruction://workspace/{main|local}`（P4 读取 → `FrozenInstructions` → 冻结构建），`AgentsMdMiddleware` 纯 adapter（无 `std::fs`），excludes 迁 provider 输入 `instruction_excludes`；⑥ 删除：`AgentDefineMiddleware` 模块与 `ChainSlot::AgentDefine`（蓝本槽位 21→20，AskUser/Permission 位置前移 1）、`scan_agents*`、`ReadDefinition`/candidate_paths、`SkillsPort`（→ `AgentCatalogPort`）、`plugin_agent_dirs`/`claude_md_excludes` 死管线。 证据：`cargo check --workspace --all-targets` 0 error/0 warning；`-p peri-mcp-workspace --lib` 368 passed；`-p peri-middlewares --lib subagent::tool` 107 passed；`mcp::agent_registry` 10 passed；`assembly::tests` 36 passed；fmt/layer-imports/diff-check 均 EXIT 0。未完成：验收缺口用例（关闭矩阵 E2E 已有 1 条、未知字段/ excludes 各 1 条）、全量 middlewares/acp 汇总行与 clippy（由协调者统一跑）、6 处文档的完整展开。
| W6：完整链路与文档收口 | 上述波次完成；串接print/stdio与TUI可观察行为；更新事实源并删除过程文档的退役副本 | 真实二进制fixture证明首请求、读取计数、批准/拒绝和关闭；原工具/资源/Apps不回归；文档不混淆目标/现状；输出命令、exit、实际测试数、阻塞平台与风险 |

W1可提供新接口而暂不启用宿主投递，不要求建立长期双提供面。W4/W5的“新消费者切换+旧实现删除+表/提示词同步”在各自波次内必须完整，不能以保留兼容层跨波规避红灯。W4 不删除技能工具（J3），只切换其来源与正文加载；**W4 是 J5 的闭环节点：宿主技能 FS 读取点必须清零，且不得保留任何「MCP 不可用时读磁盘」的兜底**。

### 8.2 必测矩阵

| 层 | 成功路径 | 失败、边界与生命周期 |
| --- | --- | --- |
| resource provider | 单Skill多层附件、未知扩展名文本、二进制blob、静态builtin、Agent单文档、项目指令 | missing、invalid、denied至少三类错误；隐藏/node_modules/编码穿越/NUL/绝对路径/symlink；列出后文件替换、大小/数量临界、UTF-8/NUL判断、嵌套SKILL.md只当附件 |
| skill协议 | 分页manifest、unknown字段透传、get局部列表外项、URI name一致 | 游标不前进/旧revision、伪scheme资源、name不匹配、digest漂移、frontmatter任一字段差异、动态缺manifest策略、跨origin同名与名称sanitize冲突 |
| activation | tool/slash/RPC/显式URI/Agent预载同路径、origin与工具发现提示；用户显式 token 触发 → 首轮全文注入（X1 基准 R1）；摘要不替代激活全文；**正文一律经 `resources/read`（含本地技能）** | 每入口拒绝批准均不注入；取消中断read/get/approval；stale刷新一次上限；依赖缺口/环（若X5采用）；附件未在manifest不能读取 |
| SkillTool 零 FS（J5） | 删除/移走技能磁盘目录后，`SkillTool`/`DiscoverSkillsTool` 行为不变（经 MCP 读）；workspace 与外部 origin 同名消歧；`skills/` 目录静态检查无 `std::fs`/`tokio::fs` 读取路径 | workspace 实例关闭/不可用时本地技能不可加载且**不回落磁盘**（报错/空目录含原因）；单 origin 读取失败不影响其他 origin；registry 未命中报缺口 |
| system及工具面 | **system 技能摘要出现在 system prompt**（含本地技能经 workspace origin）；用户显式触发注入全文；宿主两工具（SkillTool/DiscoverSkillsTool）direct、跨 origin 聚合；workspace 工具表仍七项 | 普通非system延迟；空required工具仍不自动提升其他工具；同名外部工具无builtin信任；工具冲突first-wins；无resources能力不是错误，声明支持后RPC失败不得当空成功；workspace 不注册同名技能工具 |
| instructions/frozen | 三候选优先级、local叠加、import深度/环、同session稳定 | 新建期间内容漂移、未授权import、空文件、缺文件和读取失败区分；write-once竞争winner；持久化失败补偿；关闭后不重读；冷恢复/fork不漂移 |
| 段落覆盖（J6） | workspace `peri-meta://workspace/{section_id}`：list 只列 `{cwd}/.peri/meta/*.md` 实际存在文档；启用 section 经 read 全文覆盖生效（`PromptTemplate::new` 构造期；字节语义不 trim） | 一级扫描限制（目录名为 `x.md` 不递归、非 UTF-8 名跳过）；列出后文件被移除/替换；`true`+缺文档 warn 且保持内置、不 panic；未启用文档 read 计数 = 0；workspace 关闭/读取失败 → 内置段落且**不读磁盘**（X8 基准 A）；外部 origin 同名 scheme 资源不被消费（X7 基准 A）；冻结后资源变化不重渲染（ARC-FROZEN-001） |
| Agent | project/builtin/plugin资源；远端首次批准及复用；skills URI独立激活 | permissions/hooks字段不提权、空tools与省略区分、未知工具保守删除、name/URI/MIME/大小、内容或有效能力变更重批、资源stale不得自动委派、跨会话不可见 |
| 闭包 | workspace与子能力独立启用 | 三关闭入口 × direct/deferred/parent/workflow + 资源/命令/ACP/TUI/prompt/preload/runtime；拒绝后RPC计数=0；缓存、显式URI、历史名称不能绕过；关闭 workspace 时本地技能不可用且无磁盘兜底（J5） |
| cache/shutdown | cold fetch→进程退出→warm hit→invalidate→回源（如启用持久缓存） | auth/scope/workspace/generation隔离、迟到回填、重连、同名新实例、cancel/EOF、owner drain；只清磁盘缓存而registry未stale的回归 |

### 8.3 验证规范与建议命令

遵循 `docs/standards/testing.md` §一、§二 P0/P1、§3.3、TEST-EVIDENCE-001 / TEST-HERMETIC-001 / TEST-LIFECYCLE-001、§8.1–8.2：测试文件≥30行用同目录 `_test.rs` 且挂载；真实内部链路不用mock绕过，外部网络用边界fixture；临时HOME/config/cache、固定时钟，环境修改串行；过滤测试必须非零，记录终态与计数。

**以下是实施后运行清单，本次没有执行，也不预报通过数量：**

```bash
cargo metadata --no-deps --format-version 1
cargo test -p peri-mcp-workspace --lib
cargo test -p peri-mcp-common --lib
cargo test -p peri-acp-types --lib
cargo test -p peri-middlewares --lib -- mcp::builtin_apply
cargo test -p peri-middlewares --lib -- mcp::builtin_runtime
cargo test -p peri-middlewares --lib -- mcp::tool_bridge
cargo test -p peri-middlewares --lib -- assembly::tests
cargo test -p peri-agent --doc
cargo test -p peri-agent --lib -- startup_gate
cargo test -p peri-acp --lib frozen_snapshot
cargo test -p peri-acp --lib test_session_load_cold_host_restores_original_frozen_prompt
cargo test -p peri-resources --lib frozen_snapshot
cargo test -p peri-middlewares --lib frozen_claude_md
cargo test --workspace
```

- W1/W2/W5新增测试精确模块过滤器须在实施挂载后确定并写入canonical路由；不得把想象的测试名称当验收证据。开发时先 `cargo test … -- --list` 确认目标命中，再运行并核对计数。
- lint/构建：按最终受影响包执行 `cargo clippy -p peri-mcp-workspace -p peri-acp-types -p peri-middlewares -p peri-agent -p peri-acp --all-targets -- -D warnings`；格式/文件规模遵守仓库标准。没有修改的并行LSP问题单独记录，不能归因本波或顺手修复。
- 真实print/stdio场景使用固定模型响应与临时fixture MCP，截取首个模型请求的tools、摘要/指令、resources/read计数；远端Agent批准后才启动。TUI按testing §8.2验证命令发现/关闭/拒绝，不以组件快照代替产品行为。测试不得读真实用户技能、项目秘密或访问真实外部API。
- 跨平台路径、symlink与进程关闭只在实际跑过的平台声明支持证据；cross-compile不代替runtime。资源持久缓存若首期不启用，明确标“不支持该优化”，不能用单进程unit测试宣称跨进程通过。

---

## §9 按 crate 的影响面（计划，不是本次修改）

新增路径为建议命名；以 W0 接口冻结为准。测试随实现同目录新增/修改；不创建新的MCP实例或crate。

| crate | 新增 | 修改 | 删除/迁出 |
| --- | --- | --- | --- |
| `peri-mcp-workspace` | `src/resources/{mod,skills,agents,instructions,meta,path}.rs` 与对应 `_test.rs`；迁入 builtin Skill/Agent Markdown 静态资产；**承接本地技能读取（J5）：三根扫描、插件根、builtin 资产、清单/digest 生成**；**承接 `.peri/meta` 覆盖文档扫描（J6，语义逐字保留）**。**不新增技能工具模块**（J3） | `Cargo.toml`、`src/{lib,workspace,input}.rs`：公开资源输入（根列表 + `plugin_name` 标签 + `disable_bundled` 关闭位）与 handler 合并；按实际需要使用根已有 serde_yaml/sha2 依赖（`Cargo.toml:53,110`） | 无原七工具删除 |
| `peri-mcp-common` | 默认不新增通用resource抽象；只有第二个真实复用点才提取 | 原则不改tools-only默认server_info；若资源错误确有共同范式才最小改helpers/failure/result_mapping与测试 | 无 |
| `peri-acp-types` | 窄资源快照/来源/activation端口类型；纯Agent parser模块（X3选择迁出时）；registry 的 origin 感知查询/歧义候选 | `src/{skills,mcp_skills,ports,builtin_mcp,meta_harness,agents}.rs`；`lib.rs`按新模块导出；frozen契约文件仅在需要存来源/revision时修改 | 不新增两技能工具的声明项（工具留宿主，J3）；不用旧路径re-export维持双实现 |
| `peri-middlewares` | 必要的 `src/mcp/skill_activation.rs`（统一激活）与context adapter测试；不新建本地扫描副本 | `src/mcp/{skill_discovery,resource_tool,discover_tool,agent_registry,middleware}.rs`、`skill_discovery/{skills_list,legacy_scan,verify}.rs`、`client/{readiness,cache,subscription}.rs`、`builtin/{dispatch,context,mod}.rs`；**`src/skills/tools.rs`（保留并改造正文加载，J3/J5）**、`src/skills/mod.rs`（删除本地扫描与合并）、`subagent/{mod,skill_preload}.rs`（删除本地兜底）、`subagent/tool/{definitions,mcp_activation}.rs`、`agents_md/mod.rs`、`host_ports.rs`（F6 只读投影）、`assembly/workflow.rs`（F4 接 registry）、`assembly.rs`与`assembly/{prompt,mcp,preparation}.rs`、`lib.rs`/manifest；permission/提示词/名字锁定测试按实际触点同步 | `src/skills/{loader,content}.rs`与`skills/builtin/`整体迁出（F7–F10）；**`src/meta_harness/`（J6：scanner 迁出，契约类型留在 `peri-acp-types`）**；`subagent/built_in_agents.rs`及`built-in/*.md`迁出；`agent_define`空middleware/读盘实现；`claude_agent_parser/`在X3提取后删除；**不删 `skills/tools.rs`（J3）但其中不得保留任何 FS 读取路径（J5）**；不删subagent运行时 |
| `peri-agent` | startup资源候选测试（仅X5/J2方案的窄seam需要） | `src/middleware/capabilities.rs`、`src/agent/stages/middleware_runner.rs`（条件），`src/session/factory.rs`及`session/exec/stage_builder/tools.rs`的槽位/工具所有权；tool_catalog/startup结构仅按窄seam需要改 | AgentDefine槽位若获批删除；不删通用MCP/SkillPreload槽位 |
| `peri-acp` | 会话资源准备/冻结生命周期集成测试；命令注册异步投影测试 | `src/host/{prepared,assemble,prompt,workflow_agent}.rs`、`src/session/{frozen,construction,frozen_snapshot}.rs`（`construction.rs:64-87` 命令注册改由 MCP 发现驱动）、`src/prompt/mod.rs`；new/load/resume/fork对应事务调用点在W0复核后列入，禁止只改 frozen.rs 忽略调用链 | 删除对middleware本地扫描/读盘入口的调用，不删版本化历史快照 |
| `peri-resources` | 仅必要的frozen事务/恢复回归 | 只有J2要求新的事务能力才改现有会话存储契约实现，确切文件**未确认**；不预先承诺重写store | 无预定删除 |
| `peri-tui` | 场景测试（如现有fixture不足） | 只在技能/agent名称、completion/权限展示契约受影响时改对应消费点；确切触点W0核实，不迁入扫描逻辑 | 无预定删除 |

`builtin/runtime.rs`只回归原生命周期，不预定业务改动。`assembly*`与code-index是并行热点，实施时必须串行合并，不覆盖LSP变更。影响表不包含本次以外的LSP源码工作。

---

## §10 文档路由与新增裁决记录

按 DOC-UPDATE-001（`docs/standards/documentation.md:27-31`）在**实现同波次**更新受影响事实源，本次一律不修改：

| 路由 | 目标更新 |
| --- | --- |
| `docs/code-index/mcp-packages.md` | workspace resources、资产、**本地技能读取（J5）**、wire测试的实际路径（**不含技能工具**，J3） |
| `docs/code-index/peri-middlewares.md` | host adapter 职责：保留两技能工具的聚合说明（J3）并注明**零 FS 读取（J5）**；删除旧 loader 导航；registry/activation/关闭集/seam入口 |
| `docs/code-index/peri-acp-types.md` | metadata/manifest/资源端口、origin 感知查询、身份/冻结契约（不新增两工具声明，J3） |
| `docs/code-index/{peri-agent,peri-acp}.md` | 若startup/冻结/链槽位改变，更新调用与canonical测试路由；命令注册时序改变（F6）同步；store变更再更新对应索引 |
| `docs/design/mcp-adaptation-v4-part-1.md` | 仅获批目标变化：system摘要投递与触发全文、**`:168` 的 SkillTool 归属修订（J3）与来源全部经 MCP 读取（J5）**、frozen 取得资源的时序（J2）；不回填波次进度 |
| `docs/design/middleware-system.md` | Skills/AgentsMd转context-only（技能工具保留宿主聚合）、AgentDefine槽位去留、内容获取与生命周期边界 |
| `docs/meta-harness.md` | **J6**：段落覆盖文档改由 workspace `peri-meta://` 资源提供、宿主按启用 section 读取（无 FS 扫描）；关闭语义（X8 结果）与来源白名单（X7 结果）；scanner 删除后的排查指引 |
| `docs/design/mcp-multiplexing.md` | 只在共享resource/cache边界实际改变时补说明；不把Skill通道映射为Apps信封，不重写既有relay设计 |
| `docs/reference/mcp-ecosystem.md` | resources/skills/Agents的实际能力与普通/system差别；修正§7旧“skills/list未做”描述，仍维持参考资料非权威定位 |
| `mcp-packages/CLAUDE.md` | workspace职责表加入 resources 与资源输入、**本地技能读取与清单/digest（J5）**、公开根/预算/安全读取及测试；明确**不含技能工具**（工具留宿主，J3）；依赖禁止反向不变 |
| `peri-middlewares/CLAUDE.md` | 文件扫描退出、技能工具保留宿主聚合且**零文件系统依赖（J5）**、host-only prompt/策略/生命周期的新路由 |
| `docs/standards/architecture-contracts.md` | 获批后精确更新ARC-TOOLS/CAPABILITY-CLOSURE/MIDDLEWARE；按X5/J2更新MIDDLEWARE-CAPABILITY/FROZEN/WORKSPACE实际变化，不以计划覆盖现行契约 |
| `docs/standards/documentation.md` | DOC-LOADER-001的读取位置与resource profile（含 `.peri/meta` 覆盖文档移至 MCP 侧读取，J6）；无父目录继承等未变语义保留 |
| `docs/standards/testing.md`及canonical测试索引 | 仅新增实际可执行的测试路由，不复制标准全文、不写动态固定测试数量 |
| prompt/内置技能文本/示例 | 消除旧本地路径读取指引；技能工具说明保留（宿主聚合不变，J3），只改来源标签与触发全文（J1） |

**批准后建议新增两份 active issue 记录（本次不创建）：**

1. `spec/issues/2026-09-29-workspace-mcp-resources-decisions.md`：J1–J6 与 X1、X3、X4、X5、X6、X7、X8 逐条结果（含 X2 由 J5 关闭的记录）、规范锁定版本、批准来源、明确覆盖的 part-1（含 `:168`）/AW3/架构契约条款、API形状、失败与回滚语义。
2. `spec/issues/2026-09-29-workspace-mcp-resources-acceptance.md`：W1–W6实际命令/exit/测试数、wire计数、首模型请求证据（摘要+触发全文）、**SkillTool 零 FS 断言证据**、关闭矩阵、平台限制与未完成项。

裁决获批才进入design，进度仍在spec/issues；完成关闭前稳定结论归还单一事实源，再删除过程文件，不复制历史到新归档目录（DOC-DESIGN-001、DOC-HISTORY-001）。

---

## §11 未确认项与审阅门槛

1. **协议**：raw正文已取到，但规范commit与第9/10章完整wire/批准规则未锁定；rmcp 3.1.4 server custom-method hook未查SDK源码/跑wire；不能承诺仅增两个trait方法就完成skills/list/get。
2. **生命周期**：frozen 时序方向**已裁决（J2）**，但具体 lease/事务实现尚未验证——资源读取是否能在现store接口下做到全失败补偿需要W0取证；`peri-resources` 的确切改动文件**未确认**。未完成不得先删除旧读盘路径。
3. **语义**：X1 触发口径（术语范围）、X3 存量兼容（J5 后范围更新）、X4 关闭与指令 profile、X5 规范范围与失败语义、X6 本地来源信任/批准、X7 段落覆盖来源白名单（J6）、X8 段落覆盖关闭与失败语义（J6） 均未裁决，本文没有自行拍板；J1–J6 已定（J4 是调查回答、**J5 关闭原 X2**、**J6 有硬前置 J2**），J3+J5 需同步修订 part-1 `:168`。
4. **刷新**：`refresh_catalogs`未找到；list/template/skill/Agent activated cache在真实通知与重连中的完整一致性未跑runtime，本计划只证明存在可复用组件与静态缺口，不宣称其整体已正确。
5. **J5 相关未验证项**：本地技能目录被 MCP 侧读取后，`resources/list` 的会话可见性/关闭过滤是否足以避免跨会话泄漏文件存在性，需 W1/W2 实测；命令注册改为发现驱动后的 `/skill` 出现时点与 UI 表现未验证；「零 FS」目前只是静态证据（E25），实施后需行为断言（移走目录行为不变、不可用不兜底）。
6. **并行工作**：LSP变更未评估；后续必须重读热点文件确认最终形状，本文行号可能漂移，不据暂态定论。
7. **本次验证范围**：只做外部规范抓取、源码/文档静态复核和单计划文件检查；不构建、不跑测试、不启动服务、不读个人配置/秘密。所有功能验收仍在后续实施波次。

**审阅通过标准**：X1、X3、X4、X5、X6、X7、X8 逐条作答（原 X2 已由 J5 关闭，如需推翻请直接指出）；J1–J6 如需调整请直接指出（尤其 J3/J5 对 part-1 `:168` 的覆盖范围、J6 对 `.peri/meta` 读取位置与冻结时序的覆盖范围）；否则可以继续补证据，但不开始依赖未定结论的代码修改。
