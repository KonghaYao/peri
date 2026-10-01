# MetaHarness v4 收口：退役项清理、能力关闭闭包与资源覆盖一致性

状态：2026-10-01 三个 subagent 快速静态扫描完成；用户随后批准 channel 整体退役及删除已确认废弃项。退役删除与文档收口已实施，定向回归及隔离 workspace 编译通过；MH-01～MH-04 的行为修复仍待实施。本 issue 不授权删除仍有效的 builtin 策略键。

## 范围与依据

- 扫描按契约/配置键、运行时消费链、文档/测试三个方向并行进行；主 agent 复核关键调用链并去重。
- 证据来自当前工作树，汇总时 HEAD 为 `ac8430e6`；已有其他未提交修改未触碰，源码行号仅用于本次定位，实施时须同时核对符号。
- 初次扫描只新增此 issue，未改生产代码、未读取个人配置或会话数据、未联网、未运行 Cargo 或真实 provider 请求。下列源码行号是扫描快照；后续退役删除以本次 diff 与验证记录为准。
- 依据：[架构契约](../../docs/standards/architecture-contracts.md)的 ARC-FROZEN-001 / ARC-CAPABILITY-CLOSURE-001、[MetaHarness 现行设计](../../docs/design/meta-harness.md)、[9/29 已批准资源决策](2026-09-29-workspace-mcp-resources-decisions.md)的 X7/X8。
- 下列 P1/P2 是建议整改优先级，不是已复现生产事故定级。区分静态确认的调用路径与待 fixture 验证的用户结果。

## 结论与删除边界

v4 的主要问题不是“所有 Middleware 名都过时”，而是配置分类、跨链路关闭策略及说明未完全随 MCP 迁移收口。

| 对象 | 当前事实 | 本 issue 建议 |
| --- | --- | --- |
| `AgentDefineMiddleware`、`FilesystemMiddleware`、`TerminalMiddleware`、`GitWatchMiddleware` | 已非合法链槽位键；旧配置走 unknown-key warn 后忽略 | 删除现行文档、示例中的有效键描述；锁定退役行为，不恢复 shim |
| `WebMiddleware`、`ArtifactMiddleware`、`CronMiddleware`、`LspMiddleware`、`WorkspaceMiddleware` | 仍是有效 builtin 实例策略键，不是链槽位 | 保留并明确工具/资源关闭范围；不能仅因名字含 Middleware 而删除 |
| `LspSyncMiddleware` | 仍是 LSP 文档同步链槽位 | 保留，与 `LspMiddleware` 的实例工具关闭区分 |
| `AgentsMdMiddleware` | 冻结指令正文的宿主 adapter 仍有效 | 不因读取来源迁入 MCP 就删除消费 adapter |
| `BuiltInSubagents` | 独立 bool 策略，默认启用 | 保留并补手册，不与关闭整个 SubAgent 能力混同 |
| `15_channel` | 用户已批准退役 | 删除合法键、模板、gate 与 channel 专属运行时；旧配置 warn 后忽略，不留 shim |

旧 filesystem/terminal/git-watch 单项关闭与关闭整个 workspace **不是等价替换**；后者还会关闭其他工具和资源。不给用户配置自动写回，也不读取/清理个人 settings。

## MH-01 / P1：Meta 资源错误不应阻断可选覆盖

**事实**：`peri-middlewares/src/mcp/client.rs:378`、`:393` 的 `read_builtin_workspace_meta` 将 `resources/list` / `resources/read` 错误返回；`peri-acp/src/host/workspace.rs:418` 转为 ACP 错误；`peri-acp/src/host/requests/session_lifecycle.rs:238` 的创建路径排空环境、撤销创建。

**契约冲突**：X8 已批准覆盖读取失败 warn、保持内置、不阻塞创建、无磁盘兜底。扫描时设计同时写“不阻塞”与“读取失败走发布前补偿”，本轮已消除该文档矛盾，但没有修改运行时错误传播。不是重新裁决降级还是 fail-closed 的开放问题，应按已批准契约修复。必选 system 技能/指令输入的错误不由此一并吞掉。

**动作**：在可选覆盖消费边界分类降级，保留告警与实例身份校验；明确单段 read 失败对其他成功覆盖的处理，避免把可选错误传播成创建失败。

**验收**：真实消费链注入 list 失败、read 失败；会话仍成功、受影响段落保留内置、无本机读盘兜底。实例关闭、文档缺失、空正文和正常覆盖分别验收；必选输入失败仍按其契约拒绝创建并补偿。

## MH-02 / P1：启用段落读取与冻结必须使用同一会话配置

**事实**：`peri-acp/src/host/prepared.rs:254` 的 `resolve_configuration` 为目标 cwd 解析项目配置；`peri-acp/src/host/requests/session_lifecycle.rs:101` 的 `enabled_meta_sections` 却读取 host `peri_config`，创建阶段 `:235` 仍用它；`peri-acp/src/session/frozen.rs:81` 最终按传入会话配置构建覆盖状态。

**待复现影响**：启动目录 A 与创建目录 B 不同，B 独有的 `section=true` 可能不触发资源读取，随后误判为文档缺失；反向差异可能产生无用读取。尚未运行跨 cwd fixture。

**动作**：从本次 prepared 会话配置快照派生启用段落，读取与冻结同源；不再回读可变 host 配置。

**验收**：A 未启用、B 启用且有覆盖正文；B 的持久 frozen 与首个模型请求包含覆盖。反向配置不读取未启用资源；项目/global 逐 key 合并、创建后配置变化不改变当前会话。

## MH-03 / P1：Hook 关闭必须覆盖独立 SessionEnd 路径

**事实**：`peri-middlewares/src/assembly.rs:291` 跳过关闭的 Hook 链槽，但 `peri-acp/src/host/assemble.rs:698` 独立组装 `hook_groups`；`peri-acp/src/host/workspace.rs:514` 的 `finish_session_end` 直接筛选 SessionEnd，经 `peri-acp/src/host/assemble.rs:178` 的 standalone 入口执行，所查路径未消费冻结关闭集合。

**风险推断**：`HookMiddleware=false` 后仍可能运行终止脚本，产生进程/文件副作用；静态路径成立，但未运行哨兵 hook 复现。

**动作**：生命周期执行入口消费同一 session-local/frozen policy，不以“未进 Agent 链”推断全关闭。默认遵循能力闭包；若希望 SessionEnd 豁免，必须另获契约变更批准，不静默引入例外。

**验收**：配置关闭且存在 SessionEnd 哨兵；正常 close、重试 close 均不执行，启用时仍至多一次。关闭进行中取消/重试不重新开放已禁用 hook。

## MH-04 / P1：Plugin 关闭不能只跳过链槽

**事实**：`peri-middlewares/src/assembly.rs:236` 仅跳过 Plugin 槽位；`peri-acp/src/host/assemble.rs:682`、`:724` 的命令生成/注入和 `:689` 的插件 hooks 聚合独立进行；`peri-acp/src/session/construction.rs:55` 直接注册插件命令。`docs/design/meta-harness.md:255` 的现行契约却是“关闭插件整体注入”。

**边界**：已确认命令/hooks 的独立入口未按 Plugin 关闭位过滤；插件 MCP、技能和 Agent 根的完整泄漏范围仍需进一步验证，不宣称全部已复现。

**动作**：按插件来源和冻结策略关闭注入/路由/执行；审计 plugin MCP、skill、Agent 根与子链继承。不能修改共享全局配置或关闭非插件来源，也不将关闭策略等同物理卸载插件。

**验收**：插件各提供命令、hook、技能、Agent、MCP 工具；关闭后对应发现、客户端投影、执行与继承均不可用，非插件能力仍可用；两个不同策略的并发会话互不影响。

## MH-05 / P2：删除过时说明，补齐配置语义与测试定位

- **现行站点**：`peri-cool/src/content/docs/docs/features/meta-harness.mdx:69` 仍列退役 AgentDefine/Filesystem/Terminal 键，混列 builtin 与链槽，缺 Workspace/LspSync。改为分类说明与事实源链接；说明 workspace 同时关闭覆盖来源、LSP 工具与同步两键独立。
- **用户手册**：`docs/meta-harness.md:7`、`:139` 漏掉 `BuiltInSubagents` 及其合法键语义；补默认、冻结、仅控制内置定义、不关闭项目/plugin agents、fork/resume 和 Agent 工具的边界。
- **权威设计**：`docs/design/meta-harness.md:99` 的 known-key 伪代码只认两张表，`:137` 状态副本缺 `built_in_subagents_enabled`。优先删除易漂移结构/校验实现副本，链接契约符号；消除 MH-01 的相互矛盾失败说明。
- **类型注释**：`peri-acp-types/src/meta_harness.rs:3` 指向不存在的 `meta-harness-design.md`，`:28` 的 `disabled_middlewares` 注释未说明包含 builtin 策略键。修正文义即可，本轮没有依据要求大规模字段改名。
- **测试**：`peri-acp/src/session/mod_meta_harness_test.rs:73`、`:83` 使用 Web 策略键却称 middleware 卸载；调整语义命名，保留有效断言并分别验证链槽与实例。`:211` 的冻结不重读测试不等于 RPC 故障降级测试；在 `peri-acp/src/host/requests_meta_resources_test.rs` 补 MH-01/02 的真实消费链回归。
- **历史文章**：`peri-cool/src/content/posts/engineering/meta-harness.mdx` 是 2026-08-22 历史背景；加版本说明及现行 feature 导航，不全面重写成今日 inventory。

**验收**：合法键说明与 `SECTION_IDS`、`MIDDLEWARE_NAMES`、`BUILTIN_INSTANCE_POLICY_KEYS`、`BUILT_IN_SUBAGENTS_KEY` 一致；旧键 warn 后忽略，五个实例策略键仍可关闭，`BuiltInSubagents` 两个 bool 值均有效。同步受影响 code-index/模块指引入口，不复制新的动态 inventory。

## MH-06 / P2：channel 已批准退役，删除并验收

用户裁决（2026-10-01）：channel 退役，需要移除的删除。不保留等待未来装配的 dormant 实现。

已实施范围：删除 `15_channel` 合法键与模板；删除 `PromptFeatures`、无持有者数组、feature/section gate 及渲染参数；删除 ChannelBroker/Owner/State、channel 权限/消息契约、MCP handler/通知 sender/transport variant、host 参数透传和插件 manifest channel 类型；删除随 channel 失去生产消费者的 MultiplexBroker 及专属竞速测试。保留普通 tokio 通道、ACP 审批/问答、MCP elicitation、资源订阅与 Apps relay。

回归验证目标：旧 channel/退役 middleware 键的 true/false 均被忽略，而有效 workspace/段落键保留；段落声明全集只来自 middleware 持有者；冻结、主链、SubAgent、Workflow 的渲染与缓存 seam 保持一致；MCP Apps profile 的启用与禁用均有效。以下记录实际结果，不能以删除数量替代通过结论。

### 本次交付验证

工作树同时存在另一任务的 session/store 重构：一次 Agent 测试编译被 `peri-resources` 的未完成 API 改动阻断，未修补或回滚对方代码。后续采用 `f0ff1f0a` 的隔离快照，加本次 channel Rust 变更（含新测试模块），不纳入未提交的存储重构；没有创建分支或提交。

| 验证 | 结果 |
| --- | --- |
| `cargo test -p peri-acp-types --lib` | 工作树：516 passed |
| `cargo test -p peri-acp --lib -- prompt::` | 工作树：111 passed，含退役覆盖不创建段落与持有者全集回归 |
| `cargo test -p peri-acp --lib -- provider::config` | 工作树：37 passed，含退役键 true/false 均忽略 |
| `cargo test -p peri-acp --test prompt_cache_boundary` | 工作树：1 passed，wire 缓存 seam 与动态顺序保留 |
| `cargo test -p peri-agent --lib -- session::exec` | 隔离：76 passed |
| `cargo test -p peri-middlewares --lib -- mcp::` | 隔离：612 passed，含 Apps profile 开/关及 builtin 关闭矩阵 |
| `cargo check --workspace --all-targets` | 隔离：通过，含 TUI 与 integration targets |
| 受影响四个 crate 的 doc tests | 隔离：通过；无运行的 ignored 用例不计通过 |
| `bun test`（`peri-cool`） | 9 passed |
| 修改范围文件大小、本地相对文档链接、`git diff --check` | 通过；全库扫描仍有 15 个未触及存量超限文件，不宣称全库大小达标 |

**全量 ACP 不是全绿**：隔离 `cargo test -p peri-acp --lib` 为 778 passed / 5 failed。另用没有本次补丁的 `f0ff1f0a` 快照与独立构建缓存，运行 `host::requests::tests::workspace_cases::worktree_`：2 passed / 5 failed，五个失败名字及断言与补丁快照一致，确认并非 channel 删除新引入。未修复这些既有 worktree 绑定、执行租约及 SessionEnd 断言：

- `worktree_binding_hot_cold_resume_and_owner_are_consistent`
- `worktree_failed_assembly_retains_resources_and_lease_until_cleanup_retry`
- `worktree_missing_directory_history_is_read_only_and_load_is_rejected`
- `worktree_invalid_session_end_binding_skips_hook_but_drains_existing_resources`
- `worktree_session_end_retains_owner_and_joins_same_hook_on_retry`

MH-06 的退役删除与定向验收已完成；本 issue 因 MH-01～MH-04 等剩余项保持打开。现行管理边界已同步到 `docs/meta-harness.md` 的“MCP 为主的生态如何管理”与站点 feature 页，不把静态关闭契约写成所有链路均已验收。

## 已有任务关联，避免重复登记

- [9/29 resources decisions](2026-09-29-workspace-mcp-resources-decisions.md) X8 已规定 MH-01 目标；交付记录与已批准目标不一致，本 issue 跟踪修复，不作为新设计裁决。
- [9/29 acceptance](2026-09-29-workspace-mcp-resources-acceptance.md) 的剩余风险已部分覆盖站点 AgentDefine 遗留（MH-05）；本 issue 扩展到完整 MetaHarness 语义，不重复计作新发现。
- 同一 acceptance 已登记 `SkillsMiddleware=false` 与 `SkillPreloadMiddleware` 独立开关风险。沿用该开放项；真实 provider 是否拒绝假 `SkillTool` 历史调用未验证，不写成已证实故障，也不在此重复建任务。
- [9/30 MCP residual audit](2026-09-30-mcp-migration-residual-audit.md) B-01～B-05 主要涉及插件目录/语法/接口/code-index；不能视为上述关闭闭包和 Meta 覆盖行为已验收。

## 实施顺序与完成条件

1. 为 MH-01/02 的创建路径、MH-03 的终止 hook、MH-04 的插件注入建立失败回归，随后修复同源策略消费；按 MCP 生命周期测试规范保留隔离与同键 serial。
2. MH-05 已删除过时站点键表和设计结构/数组副本、补内置定义策略及 MCP 管理边界；测试命名等剩余项继续核对。MH-06 按用户裁决完成删除与定向验收。
3. 定向运行相关测试，检查首个模型请求、发现/执行、继承和关闭后的实际副作用，不能只断言 disabled 集合或缺席的 middleware。

建议命令（本轮未运行；实施时先核对各精确过滤器确实命中测试）：

```bash
cargo test -p peri-acp-types --lib -- meta_harness::tests
cargo test -p peri-acp --lib -- meta_harness
cargo test -p peri-acp --lib -- meta_resources
cargo test -p peri-middlewares --lib -- assembly::tests
cargo test -p peri-middlewares --lib -- mcp::builtin_runtime
git diff --check
```

实现时按新增回归的实际测试名补充定向命令；改 doc comment 后按仓库要求运行受影响 crate 的 doc tests。检查本地文档链接及 DOC-UPDATE-001 路由。仅在行为、文档与测试一致且遗留裁决闭合后关闭本 issue；不以扫描完成替代验收完成。
