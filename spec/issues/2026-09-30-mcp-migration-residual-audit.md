# MCP 大迁移残留审计：方向 B

日期：2026-09-30。状态：静态审计完成，以下任务尚未实施或运行验收。

## 1. 范围与判定依据

- 扫描 `peri-middlewares`、`peri-acp`、`mcp-packages` 的资源来源、插件装配、解析、工具注册及继承；跨仓库 Rust 符号搜索用于确认消费者，不改实现。
- 读取根 `CLAUDE.md`、标准索引、architecture-contracts、documentation、三个模块指引及对应 code-index；边界采用 `2026-09-29-workspace-mcp-resources-plan.md` 的 J3/J5、F11/F12、W4/W5，以及同日 decisions/acceptance 的已批准更新，不把旧波次计划当成当前实现。
- J3：SkillTool/DiscoverSkillsTool 保留宿主聚合。J5：资源扫描与技能正文归 MCP；插件 manifest/命名空间与宿主配置读取保留。W5：Agent 定义和项目指令消费走资源，冻结、执行、审批与可信来源判定仍留宿主。
- 证据为源码与消费者搜索；没有运行 Cargo、全量测试、产品、外部服务，没有读取个人配置或会话数据。下列验证均为建议，不是已通过结果。
- 起始 HEAD 为 `9df3cc53`。其他 worker 正在改 parser 与代码索引，行号是读取时定位，复查应同时使用所列符号。初始 `e2e/tui-tester` 子模块已脏，未触碰。
- 分类：① 确认存在、可删除/迁移的残留；② 重复解析需统一；③ 合理留宿主；④ 风险假设或边界歧义，待复现/裁决。优先级不是已复现故障严重性。

## 2. 优先任务清单

| 优先级 | 独立任务 | 分类 | 建议归属 | 风险 / 最小验证 |
| --- | --- | --- | --- | --- |
| P1 | B-01 收口插件 Agent 默认目录枚举，并校正 provider 根输入形态 | ①；漏发现结果④ | plugin loader + workspace resources owner | 根层级改变导致 agent 消失或 URI 改名；用默认目录、单文件、目录定义三个 fixture 走装配→资源发现→激活 |
| P2 | B-02 对照 skill frontmatter 纯语法重叠，确认共享边界 | ②；完全同语法及具体拒绝案例④ | MCP common + workspace + host activation owner | 潜在解析漂移，未复现拒绝；同一 fixture 比较 provider manifest 与 host activation，不移动审批/比较策略 |
| P2 | B-03 删除无生产消费者的 `all_agent_dirs` 聚合字段 | ① | plugin 契约/loader owner | 影响 DTO 构造者；全仓消费者检查，定向 loader/资源装配测试及相关 crates check |
| P2 | B-04 删除旧 `CommandProvider`/`PluginCommandProvider` 抽象及其 re-export | ① | plugin/command owner | public Rust API 使用方需确认；保留真正活跃的 `plugin_route_entries`，验证插件命令路由投影 |
| P2 | B-05 同步已落后的 W5 code-index 描述 | ①（文档残留） | documentation owner | 误导航而非运行故障；人工核对 Agent 根装配与 docs diff |

### B-01：插件默认 Agent 根仍由宿主扫描，输出层级与 provider 不同

**确认事实（①）**：`peri-middlewares/src/plugin/loader.rs:371` 的 `extract_agents_paths` 在默认分支检查 `agents/`、`.agents/`，于 `:393` 调用 `std::fs::read_dir`，只把其子目录放入结果（`:396`），不是仅根据 manifest 构造根路径。它不读取 Agent 正文，但确实仍承担来源枚举。

**活调用链**：`peri-acp/src/host/assemble.rs:58` 的只读加载入口 → loader 的共同装配逻辑（`peri-middlewares/src/plugin/loader.rs:687`）→ `LoadedPlugin.agents_dirs`（`:712`）→ `peri-acp/src/host/workspace_resources.rs:101` 按插件加标签 → 同文件 `:82` 注入 `ResourceRoot::plugin` → `mcp-packages/workspace/src/resources/agents.rs:250` 对根扫描。

**根形态的静态证据**：provider 的 `scan_root`（`mcp-packages/workspace/src/resources/agents.rs:99`）枚举根的孩子；接受 `{id}.md` 或 `{id}/agent.md`（`:132`、`:149`）。默认 loader 却把 `{plugin}/agents/{id}` 本身作为根：`{id}/agent.md` 会被再次按孩子解释，可能形成 id=`agent`，而 `{plugin}/agents/{id}.md` 根本不会进入默认 loader 结果。manifest 显式条目还允许任意存在的文件（loader `:379`），但 provider 调用的是 `read_dir`（agents `:107`）。这是确定存在的输入语义不对齐；尚未通过 fixture 确认具体用户可观察的漏发现/错误命名，故该结果归④，不宣称已经复现生产回归。

**建议的小任务**：先为默认 `agents/a.md`、`agents/b/agent.md`、manifest 显式单文件三类写装配到发现的差分用例；明确根与单定义的契约，然后由 provider 承担内容枚举。宿主保留 manifest 解释、插件身份标签与来源优先级；不把插件安装目录、配置读取一起迁走。单文件声明是否承诺支持需对照契约，不靠 `exists()` 猜测。

**最小验证**：loader fixture + `workspace_resources_input` fixture + provider list/read + registry 激活，断言 id、plugin scope、URI、正文与两种默认布局。关闭 workspace 后不回落 loader 读正文；同名优先与 symlink 规则保持已批准口径。不要仅测试 `extract_agents_paths` 返回非空。

### B-02：Provider 与宿主重复实现 skill frontmatter 解析

**确认事实（②）**：`mcp-packages/workspace/src/resources/frontmatter.rs:18` 自定义 delimiter/BOM 拆分，`:39` 用 `serde_yaml` 解析，`:72` 自定义 YAML→JSON；宿主 `peri-middlewares/src/mcp/skill_discovery/verify.rs:17` 则用 `gray_matter::Matter<YAML>` 和 `deserialize` 得到 JSON map。两条实现都在活路径，不是测试副本。

**真实重复到哪一层**：两端对同一 SKILL.md 都执行“取 YAML frontmatter → 转 JSON object，保留字段值”的纯语法操作，普通字符串键 mapping 的解析职责确实重叠；但不是两份可直接互换的相同实现。provider 显式定义 delimiter、key、tag 的映射规则，宿主委托 gray_matter；没有依赖内部实现/差分运行证据，不能认定 BOM、尾空白或 YAML 扩展语法完全相同。`frontmatter_maps_equal`、摘要构造、typed Agent profile 均不属于这份重复纯语法。故本项是共享原语机会，不是已经证实的运行缺陷，也不是要求删除 provider minimal frontmatter。

**活调用链**：provider `skills.rs` 建 manifest 的 `frontmatter` → MCP `skills/list` → host registry → `peri-middlewares/src/skills/tools.rs:123` 或 `peri-middlewares/src/subagent/skill_preload.rs:266` → `mcp::skill_activation::activate` → `peri-middlewares/src/mcp/skill_activation.rs:192` 重解析正文 → `:195` 与快照比较；旧资源发现路径 `mcp/skill_discovery/skills_list.rs:397` 也用宿主解析器。

**差异证据 / 局限**：provider 明确接受开头 BOM、delimiter 行尾空白（frontmatter `:16`），将数字/布尔 map key 字符串化、跳过不能表达的 key、剥掉 YAML tag（`:68`、`:95`）。宿主没有复用这些规则。仅凭源码不能断言 gray_matter 在所有这些输入上与 provider 不同；需复现才能认定某个合法 skill 被拒绝。重复实现本身已确认。

**建议的小任务**：先做两端差分 fixture，只有相同批准语法部分才考虑提取共用 primitive；保留 workspace provider 的 minimal frontmatter 职责，不复用 typed Agent parser 来改写资源快照。若差异是刻意 profile 区分，则记录契约，不为统一而统一。宿主继续拥有 `frontmatter_maps_equal` 的完整字段比对、digest 验证、stale 重试与可信来源/审批规则；不能以“provider 已解析”删掉宿主校验，也不能强行把 commands 的可选 frontmatter 语义并入 skill profile。

**最小验证**：共享表驱动用例覆盖普通 mapping、无 frontmatter、未闭合、BOM、CRLF、delimiter 尾空白、block scalar、tag、非字符串 key、数值；同文本经 provider list/read 与 host activate 成功/失败一致；篡改快照、正文 digest 与额外字段仍拒绝。现有 frontmatter/skill_activation 测试可作定向入口，未在本轮执行。

### B-03：`all_agent_dirs` 只生产、不被生产链消费

**确认事实（①）**：字段定义在 `peri-acp-types/src/plugin.rs:594`；loader `:898` 从每个插件复制聚合，`:970` 写结果；空配置也构造它（`:864`）。全仓 `rg -n 'all_agent_dirs' --glob '*.rs' .` 只命中字段定义、这些构造和 loader tests（`:929`、`:1747`），未发现生产读取。

**对照活调用链**：当前 W5 的 `plugin_agent_roots`（`peri-acp/src/host/workspace_resources.rs:92`）必须逐个 `data.plugins[].agents_dirs` 读取，并加 plugin name，未消费扁平聚合字段。因此可以删除扁平冗余字段，但不能删掉 `LoadedPlugin.agents_dirs` 或整个插件 Agent 管线。

**最小验证**：更新 DTO 构造与对应测试，定向验证 tagged plugin roots；相关 crates check 捕获其他构造者。结论限仓内消费者，未调查仓外使用这些 public Rust 类型的下游。

### B-04：旧 CommandProvider 抽象只有测试与 re-export

**确认事实（①）**：`peri-acp-types/src/plugin.rs:566` 的 `CommandProvider`，`peri-middlewares/src/plugin/loader.rs:812` 的 `PluginCommandProvider` 及 `:817` 的构造、`:823` 的实现，只有测试调用；全仓 Rust 搜索生产命中只剩定义和 `plugin/mod.rs:25`、`lib.rs:42`、`:112` 等 re-export。loader `:37` 的说明还明确称其为“保兼容”。

**活调用对照**：真实插件命令路径是 loader `extract_commands` → `PluginLoadResult.all_commands` → `peri-acp/src/host/assemble.rs:674` 调 `plugin_route_entries` → RouteEntry。`PluginCommandHandler`（loader `:259`）在这里仍被构造，是占位 fall-through 行为，不是无调用模块，不能随旧 provider 删除。

**建议 / 验证**：删除旧 trait、wrapper、相关 re-export 与仅保护旧抽象的测试，保留路由表契约。确认仓外 public API 义务后处理；内部不加 deprecated shim。用插件命令 namespace/provenance/投影测试验证，不把“尚无命令执行体”误当本轮 MCP 来源迁移 bug。

### B-05：代码索引仍停在 W4b 的 Agent 未接线口径

**确认事实（①，文档）**：`docs/code-index/mcp-packages.md:18` 的资源面行仍写“不装 agent 根（W5）”。当前 `peri-acp/src/host/workspace_resources.rs:74`、`:78` 注入两个 project agent 根，`:82` 注入插件根；`mcp-packages/workspace/src/resources/agents.rs:250` 消费它们，W5 acceptance 已登记切源。

**建议 / 验证**：仅更新索引的现状描述，不回写旧波次历史、不声称顶层三路径已接资源面；人工核对装配入口、provider list/read 与关闭边界即可。本轮未改该索引；并行 worker 也在维护索引，合并前再次核对是否已覆盖。

## 3. 合理留宿主：已核对，不登记为迁移 bug（③）

| 模块 / 定位 | 活路径与保留理由 |
| --- | --- |
| `settings.rs:33`、`:58`；plugin loader `:757`、`:758` | 宿主读取 skillsDir/关闭位、installed_plugins/settings；配置与插件生命周期输入，不是技能正文兜底。plan F11/F12 明确保留。 |
| plugin loader `:326`、`:338`、`:354`；skills loader `:33`、`:65` | 构造并检查插件 skill 根，`is_dir()` 是本机元数据检查，不是 SKILL.md 内容扫描。确有本地依赖，但不能仅凭 J5 误报正文双读；“宿主零 FS”文案若按所有 stat 理解则过宽。远端独立挂载根被提前过滤的风险另属④。 |
| plugin loader `:159`、`:217`、`:73` | 插件 commands 目录/Markdown 仍在宿主读取，活消费者为命令路由投影；J5 下沉的是 skill 来源，commands/hook 生命周期没有获批同批迁移。CommandFrontmatter 虽与 skill 都是 YAML，但 profile 不同，不直接认定重复业务规则。 |
| `agents_md/mod.rs:44`、`:71`；ACP `host/workspace.rs:149`、`:421` | 指令冻结正文注入、meta 组合与冻结留宿主；provider main/local 与 meta 读取经执行环境端口。此处没有本地内容兜底，也没有发现仍活跃的旧 instruction/import scanner。 |
| `mcp/skill_activation.rs:183`；`mcp/agent_registry.rs:442`、`:479`、`:607` | 内容绑定、digest/frontmatter 校验、本地/远端来源判定、批准缓存、远端 profile 规范化属可信边界。纯解析可共用，但校验和审批不能因 MCP 提供数据而删除。 |
| `subagent/tool/definitions.rs:145`；`subagent/fork.rs`；`subagent/mod.rs:445` | 父工具过滤、继承、能力推断及模型执行策略仍属编排；不应迁成 Workspace 资源逻辑。工具过滤消费父工具目录，不等于重复实现 MCP 文件工具。 |
| `mcp/builtin/{context,dispatch,runtime}.rs`；dispatch `:101` | 宿主实例装配、端口注入、transport、资源转发与 shutdown 合理留宿主；工具 handler/FS 执行在 packages，扫描未发现 packages 反向依赖 peri-middlewares 的 Cargo/source import。packages 依赖 peri-agent/shared contracts 本身不是违规。 |
| `at_mention/mod.rs:90`；`at_mention/file_reader.rs:44`、`:129` | `@path` 输入转换确实本地读文件/枚举目录；现行设计 part-1 `:191` 明确保留 AtMention，不等于 W4/W5 技能或指令旧来源未删。将任意输入文件读取进一步资源化需单独授权。 |
| `lsp/middleware.rs:81`、`:91` | 成功 Write/Edit 后向共享 LspPoolPort 发 didChange/didSave；ready_for 不成立时不读盘。文档同步是已批准宿主 seam，无 collect_tools，不能误报重复 LSP 工具/pool。 |
| `attribution/mod.rs:155`、`:185`、`:197`；assembly `:269` | GitAttribution hook 读取前后文件、git branch，仍是活本机 IO；part-4 `:33` 明确排除本波。part-1 未来查询下沉目标未完全实现，应视作未交付目标，不写成“本轮已迁移但旧实现未删”。 |

### 本轮已由另一方向处理：Agent 纯解析和类型

`claude_agent_parser` 的纯解析/类型迁到 `peri-mcp-common::agent_definition`，以及无生产调用的 `format_agent_id` 删除，均由收敛 lane 本轮处理，不重复编辑、不再派任务。扫描期间已观察到 `peri-middlewares/src/mcp/agent_registry.rs:41` 改为导入 common。主 agent 本轮反馈：已完成，23 个 parser 单测通过，middlewares/workspace all-targets check 通过；这是主 agent 转述的验证结果，本 lane 未重跑。共享模块不新增扫描或授权，workspace provider minimal frontmatter 保留。本项与 B-02 的 skill verbatim frontmatter 不等价：typed profile 归位不能作为两端 skill JSON 语法完全相同的证明。旧 parser 归属引用由主 agent 同步 active spec，本报告不另改 spec。

## 4. 待复现 / 不扩大结论（④）

- **插件 Agent 用户可观察故障**：B-01 的根输入不对齐由源码确认；实际 installed plugin fixture 的命名/漏发现、manifest 是否支持单文件，需最小链路验证。不读个人插件目录验证，不将“可能为空”当成既有生产事故。
- **解析分歧导致拒绝**：B-02 已确认两条实现；BOM/tag/数字 key 只是差分候选，未执行 gray_matter/provider 对照，不能写成已复现 bug。
- **根在远端存在而宿主不存在**：skills loader `:65` 与 plugin loader `:338` 先本机 `is_dir()`，可能不适用于独立挂载执行环境；当前批准实现采用宿主绑定 builtin in-process 输入，不足以证明现行部署出错。需具备具体远端 provider 根映射用例后再定删除预过滤或增加边界契约。
- **public re-export**：只发现 re-export 不代表死接口；上述 B-04 有全仓消费者负证据，其他 skill/agent/shared-contract re-export 仍被宿主适配器或调用方使用，不批量删除。仓外 API 使用情况未调查。

## 5. 交付与扫描局限

- 本轮仓库唯一修改为本报告；未改 Cargo、源码、其他 Markdown，未 stage/commit，未运行全量或定向测试。
- 没有建立编译级 call graph，也没有验证宏生成或仓外下游使用；“无调用”仅指全仓源码搜索没有生产消费者。
- 关注资源迁移因果链而非逐文件完整审计；OAuth、hook 执行、数据库/凭据缓存、插件安装管理不是本轮故障验证面。已列的合理保留不承诺这些模块没有其他安全或生命周期缺陷。
- 并行工作改变 parser、索引和测试文件，报告按符号可重定位；提交/合并前应再次对 B-03/B-04 作消费者搜索、对 B-05 作文本核对。
- 审计读取时曾误生成临时 `/tmp/peri-b-fs-scan.txt`，已删除；这是唯一写入路径限制的执行偏差，不影响源码和仓库改动范围，后续扫描直接输出，不再落盘。
