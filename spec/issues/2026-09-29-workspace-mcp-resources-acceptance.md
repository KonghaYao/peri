# Workspace MCP resources 验收记录（W1–W6）

> 日期：2026-09-30（W6 收口 + L0 门禁修复）。仓库 `/Users/konghayao/code/ai/peri-v4p3`，分支 `feat/mcp-adaptation-v4-part-3`，基线 HEAD `0c5ecb40`（W6 提交）；L0 门禁存量修复与确定性改造见 §2 与 §6.4 第 11 项（提交 hash 待回填）。
> 口径：只记录**实际执行过**的命令与结果（命令 + EXIT + 关键计数）；静态证据给 `file:line`；没有运行证据的事项单列「未完成 / 未验证」，不写成已具备能力（遵循 `docs/standards/testing.md` TEST-EVIDENCE-001 与计划 §8.3）。
> 计划与裁决：`2026-09-29-workspace-mcp-resources-plan.md`（下称 plan）、`2026-09-29-workspace-mcp-resources-decisions.md`（下称 decisions）；本记录不重复两者全文，只写验收事实。

---

## 1. 波次与提交

| 波 | 提交 | 交付要点（一句话） |
| --- | --- | --- |
| W1 | `0938942f` | 冻结 `skill://` / `agent://` / `peri-instruction://` 契约与 `_meta`，workspace provider 资源面落地（scan/path/frontmatter/skills/agents/instructions/builtin），dispatch 完整转发；builtin SKILL.md ×7 迁入 workspace 包 |
| W2a | `c9c5b02d` | `McpSkillRegistry` 查询改 origin 感知（`SkillLookup` Found/Ambiguous/Missing），新增 `mcp::skill_activation` 统一激活（`resources/read` + digest + frontmatter 全字段校验）；三入口接入 |
| W2b | `f511b7ae` | 发现只发布 metadata（发现期 `resources/read` = 0、`skills/get` = 0 有 wire 用例），完整性校验全部归激活；歧义显式拒绝 |
| W3b-provider | `760a9e81` | `peri-meta://workspace/{section_id}` 段落覆盖资源域（一级 `.md` 扫描、读取失败跳过、symlink 跳过、未知 section -32002） |
| W3a | `6ad2a0af` + `8006224b` | 两阶段会话准入（`begin_initialization` → P3 activate → `commit_frozen` → 发布）；装配消费持久 frozen winner；fork 保持 write-once 删除语义 |
| W3b-consumer | `49058e8d` | 宿主段落覆盖来源由 FS 扫描改为 `peri-meta://` 读取；`peri-middlewares/src/meta_harness/` 删除；legacy 首次接纳保持接纳前构建（无执行环境 ⇒ 内置 + warn） |
| W4a | `711bf1d0` | 资源面输入经 `BuiltinInstanceContext.workspace_resources` 在 `run_initialize` 前注入，dispatch `workspace` arm 调 `WorkspaceMcpServer::with_resources` |
| W4b | `8c3d975a` | SkillTool 零 FS 切源：宿主三根扫描/正文读取全部删除，技能来源唯一为 MCP registry；`core:{skill}` 命令改发现管线投影；P4 冻结技能摘要 |
| W5 | `f66bd251` | Agent 定义与项目指令消费切换：`AgentSource::{Local,Remote}`、builtin agent 资产迁 provider、`peri-instruction://workspace/{main\|local}`、`AgentDefineMiddleware` 与 `ChainSlot::AgentDefine` 删除（蓝本 21→20） |
| W6 | `0c5ecb40` | 真实二进制链路验收（print/stdio + TUI）、preload 缺口报告强度收口、文档事实源收口与过程文档退役 |

---

## 2. 复核命令与结果（W6 收口时在本工作树重跑）

| 命令 | EXIT | 结果（关键计数） |
| --- | --- | --- |
| `cargo test -p peri-mcp-workspace --lib` | 0 | 372 passed / 0 failed / 1 ignored |
| `cargo test -p peri-acp-types --lib` | 0 | 514 passed / 0 failed |
| `cargo test -p peri-middlewares --lib` | 0 | 1545 passed / 0 failed / 4 ignored |
| `cargo test -p peri-acp --lib` | 0 | 775 passed / 0 failed |
| `cargo test --workspace` | 0 | 70 个 test binary 汇总行：7218 passed / 0 failed / 41 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | 无 warning（`Finished dev profile ... in 17.99s`） |
| `cargo fmt --all --check` | 0 | 无输出（无未格式化文件） |
| `scripts/check-layer-imports.sh` | 0 | 20 条规则 / 违规边 0 |
| `scripts/check-file-size.sh` | 1 | 扫描 1785 个文件，超阈值 32（源码 3 / 测试 29）——**全部为存量**，本波改动文件均 ≤1000 行（见 §6.4 第 1 项） |
| `git diff --check` | 0 | 无输出（无空白错误） |
| `cd e2e && npm run e2e -- --file tests/scenarios/workspace-mcp-resources.test.ts --serial --retry 0` | 0 | 1 文件 / 1 通过 / 0 失败（20s；print/stdio 场景） |
| `cd e2e && npm run e2e -- --file tests/scenarios/workspace-mcp-resources-tui.test.ts --serial --retry 0` | 0 | 1 文件 / 1 通过 / 0 失败（17s；TUI 场景） |
| `cd e2e && npm run e2e:l0` | 1 | （W6 收口时）12 文件：✅ 6 通过 / ❌ 6 失败（10m26s，并发 1，重试 0；flake 门禁未过，含本波注册的 print/stdio 场景文件）。6 个首轮失败经 HEAD 基线对照（§6.4 第 11 项）判定 5 个为存量、1 个为真实模型用时波动，**非本波回归** |
| `cd e2e && npm run e2e:l0`（存量修复后，2026-09-30） | 0 | 11 文件：✅ 11 通过 / ❌ 0 失败（4m08s，并发 1，重试 0，首轮 11/11 全过）。`header-suffix-and-error` 移出 L0；`viewport-40x8` / `first-tool-stuck-running` / `edit-diff` 改由本地假 model server 重放（`helpers/replay-model.ts`），L0 不再依赖真实模型/凭据/网络（TEST-HERMETIC-001，见 §6.4 第 11 项） |

上述命令由同一条证据链脚本顺序执行（每条命令单独落日志 + 逐条 EXIT），测试计数取自各命令日志的 `test result:` 汇总行；`check-file-size.sh` 的 EXIT=1 由存量超限产生，不等于本波回归（理由见 §6.4 第 1 项）。

---

## 3. 真实链路证据

驱动方式：真实二进制 `target/debug/peri`（`cargo build -p peri-tui --bin peri`），隔离世界（临时 `HOME` + 临时 cwd + `env -i` 风格环境），模型侧为本地假 model server（记录每个请求 body，`isMain` = 含 tools 的主请求），MCP 侧为临时 fixture server（每条 JSON-RPC 先落 `#recv` wire 日志）。夹具：`e2e/helpers/workspace-mcp-fixture.ts`、`e2e/fixtures/mcp-skill-fixture.mjs`；场景：`e2e/tests/scenarios/workspace-mcp-resources.test.ts`（print/stdio）、`...-tui.test.ts`（TUI/tmux）。不消耗外部 API，不读真实用户配置。

### 3.1 首个模型请求（print，`-p "hello w6" --output-format stream-json --permission-mode bypass`）

取证 run 事实（`exit=0`，工具 17 项）：

- **tools 面**：`Agent, AskUserQuestion, Bash, DiscoverSkillsTool, Edit, ExecuteExtraTool, Glob, Grep, Read, SearchExtraTools, SkillTool, TodoWrite, WebFetch, WebSearch, Write, artifact, folder_operations` —— 七工具（`Read/Write/Edit/Glob/Grep/folder_operations/Bash`）全在，`SkillTool`/`DiscoverSkillsTool` 由宿主提供，**workspace 不注册同名技能工具**（`mcp__workspace__*` 中无 skill 工具）。
- **system 段落（bytes=97222）含冻结技能摘要**，来源标签如实：`[builtin]`（provider 内嵌资产）与 `[project]`（`{cwd}/.claude/skills` 经 workspace 实例读取）。摘录：

  ```text
  你可以使用以下 Skills（专项能力），在需要时提及其名称：

  - **mcp__workspace__cron** [builtin]
  - **mcp__workspace__goal** [builtin]
  - **mcp__workspace__multitask** [builtin]
  - **mcp__workspace__probe-skill** [project]
  - **mcp__workspace__programmatic-tool-calling** [builtin]
  - **mcp__workspace__ultra-adlc** [builtin]
  - **mcp__workspace__ultracode** [builtin]
  - **mcp__workspace__use-artifacts** [builtin]
  ```

  （摘要只列名称与来源；此形态沿用迁移前渲染，不是本波引入；正文不进入 system。）
- **项目指令**：`{cwd}/AGENTS.md` 哨兵出现在 system（经 `peri-instruction://workspace/main`，W5）。
- **发现期不读正文**：同一 run 的 fixture wire 计数 = `initialize 1 / tools/list 1 / resources/list 1 / skills/list 1 / resources/read 0 / skills/get 0`。

### 3.2 显式触发注入全文（X1 = R1）

| 路径 | 事实（同一假 model server 捕获） |
| --- | --- |
| 本地 `/probe-skill please apply` | 首轮 messages 含 `ToolUse{SkillTool, {skill_name:"probe-skill"}}` 与其 Result，正文哨兵在 **messages** 中；system 不含正文；messages 条数 4 |
| 外部 `/skillfx:demo-skill please apply` | fixture wire 出现 `resources/read` **1 次**，目标 `skill://skillfx/demo-skill/SKILL.md`；外部正文哨兵进入首轮 messages |

### 3.3 批准 / 拒绝（stdio ACP，远端 Agent 激活）

驱动：`peri acp --cwd <work>`；`initialize → session/new → session/prompt`；`session/request_permission` 由测试按 `allow_once` / `reject_once` 裁决。

- 批准：审批面到达客户端（`toolCall.title = "MCP Agent activation"`，`rawInput.uri` 指向 `agent://remote/beta/agent.md`）；fixture 收到 agent 定义 `resources/read` ≥1；批准后子代理产生独立请求（带任务文本），父会话收到成功回执（`child_thread_id` 与模型回复）；批准前 fixture 零 `tools/call`。
- 拒绝：审批面同样到达；拒绝后**无**子代理请求、无成功回执；拒绝理由以 `is_error` tool_result 回传父会话；fixture 零 `tools/call`。

### 3.4 关闭矩阵（`meta_harness.WorkspaceMiddleware=false`）

- print：七工具全部**不注册**、`mcp__workspace__*` 无技能工具、system 无技能摘要与项目指令；磁盘上技能文件存在也不注入（正文哨兵不进任何请求），fixture `resources/read = 0`。
- stdio：`/probe-skill` 不可激活（无 SkillTool 调用、无正文注入）。
- 关闭 `SkillsMiddleware`（链槽关闭）面：`core:{skill}` 命令不注册且同批撤下、`{server}:{skill}` 面保留（`peri-acp` 集成用例，见 §5 路由）。

### 3.5 回归

- 首个请求的 system 中，deferred 资源清单仍含既有 `workspace://git/ref`（既有 git ref 资源与订阅面未被新内容域挤掉）；七工具表与 §3.1 同口径。
- TUI（`workspace-mcp-resources-tui.test.ts`）：`/probe` 面板列出 `core` 投影 `/probe-skill` 与 server 面 `/workspace:probe-skill`；提交后首轮出现 `SkillTool(skill_name=probe-skill)` 且正文进入 messages；关闭态不可激活；远端 Agent 审批弹窗 Esc 拒绝后不启动子代理、理由回传。

---

## 4. SkillTool 零 FS 证据

- 静态：`rg -n 'std::fs' peri-middlewares/src/skills/` **零命中**（仅 `skills/loader.rs:19` 保留 `use std::path::PathBuf` —— 根解析适配器，不读技能内容）；`rg -n 'std::fs' peri-middlewares/src/agents_md/` 零命中。
- 行为：§3.1/§3.4 的真实链路显示技能正文只经 MCP `resources/read` 进入（发现期计数 0、激活期 ≥1），关闭 workspace 后磁盘技能不注入、无回落；外部 origin 与 workspace origin 同规则。
- 唯一来源：宿主技能根解析剩余职责为「路径 + scope 标签 + 关闭位」输入（F11/F12），插件 manifest 解析与配置读取不读技能正文。

---

## 5. 重点测试路由（测试数取自本轮实际运行，见 §2）

```bash
cargo test -p peri-acp-types --lib -- workspace_resources
cargo test -p peri-mcp-workspace --lib -- resources
cargo test -p peri-middlewares --lib -- mcp::skill_activation
cargo test -p peri-middlewares --lib -- mcp::skill_discovery::core_face_tests
cargo test -p peri-middlewares --lib -- mcp::agent_registry
cargo test -p peri-acp --lib -- host::requests::tests::skill_resources
cargo test -p peri-acp --lib -- host::requests::tests::meta_resources
```

（路由已写入 `docs/standards/testing.md`；`--list` 命中数：workspace_resources 18、skill_resources 5、meta_resources 2、mcp::agent_registry 12、mcp::skill_activation 11、core_face_tests 3。）

---

## 6. 未完成 / 未验证 / 风险

### 6.1 decisions §9 阻塞项状态（W6 收口时）

| ID | 状态 | 说明 |
| --- | --- | --- |
| B1 | 部分（设计已闭合，断点注入未做） | 两阶段准入与补偿由 `peri-resources` / `peri-acp` 用例覆盖正常与补偿路径；**未做**进程中断 / 断电式崩溃注入的观测实验 |
| B2 | 部分 | 冷恢复/未来版本由 `requests_frozen_cases_test.rs` 等用例覆盖；**未做**手工「创建后改关闭配置再冷恢复」的 blob 对比实验 |
| B3 | 部分 | 首个模型请求计数已由 §3.1/§3.4 覆盖（tools/system/摘要/指令）；hook/cron/RPC 全序计数未做 |
| B4 | **开放** | Skills 规范第 9/10/7.3 章同版本 digest 未锁定；受限 profile（不声明 `directoryRead`、不消费编排字段）已按 X5 落地并登记 |
| B5 | 已收敛 | W1/W2 的 rmcp 内存 wire 用例 + §3 真实链路 wire 计数；未知方法 -32601、未知 URI -32602 有 wire 用例 |
| B6 | 部分 | session/关闭过滤与「关闭不回落磁盘」在 §3.4；**未做**双会话跨会话可见性、同名伪 origin 的 e2e |
| B7 | **开放** | 提前资源启动后的 owner drain / lease 释放无运行证据；现有 git 采样 task 的生命周期未证明 |
| B8 | 部分 | 冻结 V1 round-trip 与未来版本 fail-closed 由用例覆盖；「移走技能目录」的 e2e 未单独构造（单测覆盖 provider 侧） |

### 6.2 本波登记的行为变更与偏差

1. **W5 行为变更（登记）**：用户级 `~/.claude/AGENTS.md` 与调用方 extra-path 的指令查找消失——项目指令只剩 provider 三候选（`AGENTS.md` / `CLAUDE.md` / `.claude/AGENTS.md`）+ `CLAUDE.local.md` 叠加；`~/.claude/AGENTS.md` 不再被读取。
2. **W5 登记**：symlink agent 定义不再可发现（X3 存量失效面，与技能侧同口径）；`{cwd}/agents/*` 现在也进 `{{available_agents}}` 目录（迁移前只可显式加载）。
3. **W5 登记**：指令链槽关闭不门控 P4 读取（与技能面同构，行为保留）；legacy 首次接纳无执行环境 ⇒ 指令与技能摘要均不可得（J2 §3.1，warn 信号）。
4. **W3b-consumer 登记**：legacy 首次接纳的段落覆盖保持内置 + warn（无 MCP 资源面），冷恢复沿用持久 blob。
5. **W6 收口**：preload 缺口报告强度（decisions ⑩ 登记项）按 §6.3 处理。
6. **W6 补正（协调者审查后）**：`SkillTool` 四类失败文案原在 `skills/tools.rs` 与 preload 各有一份副本，且 activation 失败回执只注入裸 `error.reason()`——与「回执文案与工具面同名失败串逐字一致」的自述不变量不符。收口为单一派生点（`skills/mod.rs` 四个 `skill_*_message`），两条调用链共同使用，activation 失败回执改为完整产品串 `SkillTool: cannot activate '<name>' (<reason>)`，单测断言同步（`skill_preload_test.rs`）。
7. **W6 补正（独立评审后）**：preload 路径判据原名 `subagent_path`、注释只写「子代理路径」，但判据实际是「宿主显式传入名单非空」——**workflow agent 链同样命中**（`assembly/workflow.rs` 无条件构造 `SkillPreloadMiddleware::new(skill_names)`，生产可达），披露不完整。改名 `explicit_list_path`、注释与单测名（`test_explicit_list_path_*`）同步；行为零变化（不引入 `GapPolicy` 开关——两个调用点语义相同，加开关是多余抽象）。

### 6.3 preload 缺口报告强度（W6 决定项）

**语义（按路径分流，`peri-middlewares/src/subagent/skill_preload.rs`）**：

- **宿主显式名单路径**（子代理与 workflow agent 的定义显式声明 `skills:`，`skill_names` 非空；两条装配链共用 `SkillPreloadMiddleware::new`，workflow 侧见 `peri-middlewares/src/assembly/workflow.rs`）：每个**无法预载**的声明技能注入一对「假 `SkillTool` ToolUse + 失败回执（`tool_error`，`is_error = true`）」，与成功项同处一条 Ai 消息、按声明顺序与 `tool_call_id` 一一配对（`inject_skill_tool_sequence`）。缺口从「只有宿主 warn 日志」提升为**子代理自己上下文里的模型可见事实**——这才是 plan §5.6「报告缺口，不静默启动不完整配置」的受众；报告不阻断（子代理照常启动，可自行改用 `DiscoverSkillsTool`）。代码内路径判据名 `explicit_list_path`（不是「子代理专用」；见 §6.2 第 7 项）。
- **副作用（登记）**：假调用与回执随 `transcript.append` 持久化，并经会话回放投影为 SkillTool 失败卡片——配置缺口会让客户端看到用户未发起的红色卡片，且随会话长期存在。这是「模型必须看到同一份 transcript」的必然代价；W6 不在投影层对假调用打标记。
- **主 Agent 路径**（`skill_names` 为空，从最后一条 Human 消息启发式提取 `/token`）：缺口**零注入**。理由是提取本身是启发式的——用户文本里的路径片段（如 `/tmp`）会命中——凭空注入失败回执会制造用户从未发起的工具错误；原始 `/token` 文本仍在 Human 消息里可见，warn 日志保留。
- **文案单一权威**：四类缺口（registry 未装配 / 未命中 / 跨 origin 歧义 / 激活失败）逐字复用真实 `SkillTool` 的失败串，构造点唯一（`peri-middlewares/src/skills/mod.rs` 的 `skill_registry_unwired_message` / `skill_not_found_message` / `skill_ambiguous_message` / `skill_activation_failed_message`；`skills/tools.rs` 与 preload 共同调用，歧义清单派生自同一 `candidate_list`）。

**测试证据**：

- 单测（新增 5 条，含配对、顺序、`is_error` 分类、文案）：`cargo test -p peri-middlewares --lib -- subagent::skill_preload` EXIT 0，**23 passed / 0 failed**；混合用例断言 `[命中, 缺口, 命中]` 三结果按声明顺序配对且仅缺口项 `is_error = true`。
- e2e 用例 ⑥（真实二进制 print + 假 model server，`workspace-mcp-resources.test.ts`）：项目本地 agent 定义声明 `missing-skill-x` ⇒ 子代理首轮请求 messages = 3（任务文本 + Ai[ToolUse{SkillTool}] + Tool[is_error]），回执 `tool_use_id` 与 ToolUse `id` 精确配对；请求定位锚在子代理独有事实（`messages[0]` 恰为子代理任务文本），缺口文案断言为精确产品串 `Skill 'missing-skill-x' not found` 且不得出现 `registry is not wired`（收紧后可发现「子代理链丢装配 registry」的回归）；父会话仍收到子代理成功回执（`child_thread_id`）与 ≥2 轮主请求——**父会话不因缺口中止**。
- 主路径零注入由既有 3 条用例锁定（`test_missing_registry_reports_gap_without_injection` / `test_registry_miss_injects_nothing` / `test_preload_ambiguous_origin_rejects_injection` 期望未改，仍绿）。

### 6.4 存量与外部事项

1. **STD-SIZE-001 存量超限**（非本波引入）：`check-file-size.sh` 报告源码 3 / 测试 29 项超限，其中源码为 `peri-acp/src/host/requests/session_lifecycle.rs`（1544）、`peri-acp/src/host/requests/session_data.rs`（1278）、`peri-agent/src/agent/stages/mod.rs`（1011）。W5 已拆 `host/workspace.rs` → `workspace_resources.rs`（910 + 111），未动上述存量。
2. **远程 Turso 路径仅编译验证**：cloud 用例 32 处 `#[ignore]`，本机无运行证据（W3a/W3b 已登记）。
3. **已注册 flake**：`mcp::workspace_recovery_tests::external_source_keeps_120_second_deadline_and_cancels_execution`（W5 收口判定为既有时序问题，单测隔离绿；非本波引入）。
4. **`peri-cool` 子模块（文档站）仍描述 `AgentDefineMiddleware` 与用户级 `~/.claude/AGENTS.md`**：写权限外，需在文档站侧单独跟进。
5. **模块 `CLAUDE.md` 携带长波次交付块**：与 DOC-MODULE-001 的精简要求存在张力，建议后续折叠为 code-index/验收记录引用（本波未动，避免破坏已审阅文本）。
6. **part-1..4 plan/acceptance/sub-plan 系列保留**：这些记录仍有开放的 UNVERIFIED / 缺口项，按 DOC-HISTORY-001 不整体退役；后续按各自缺口关闭情况逐项处置。
7. **未验证推断（评审提出，W6 未处理）**：`SkillsMiddleware` 与 `SkillPreloadMiddleware` 在 `meta_harness` 中是**相互独立**的开关；若关闭前者而保留后者，注入的假 `SkillTool` tool_use 会出现在工具表不含该工具的请求里。真实 provider 对未知工具名的 tool_use 是否报错未验证（静态阅读无法证明），本波不加开关依赖闭包校验；登记为开放风险。
8. **`skills:` 声明只接受技能名**：agent 定义 frontmatter 的 `skills:` 元素按名解析（全名 `mcp__{server}__{skill}` / `{server}:{skill}` / 裸名三形态），**不接受 URI 形态**（如 `skill://review/SKILL.md`）——按 URI 声明会落入 not-found 缺口回执。当前生产 agent（builtin 6 + 本仓 `.claude/agents` 2）均未声明 `skills:`，无现实回归；未做 URI 归一化，已在 `skill_preload.rs` 文档注明。
9. **e2e 夹具失败路径**：`world.close()` 不持有 fixture MCP 子进程 pid（该进程由 SUT 持有，SUT 退出即收敛）；`runStdio` 已加 finally 兜底 kill peri 子进程，异常场景下仍可能有残余 node 进程，登记为已知限制。
10. **e2e 用例 ① 的证据强度**：标题宣称「发现阶段零正文读取」，可观测证据为外部 fixture `resources/read = 0`（同一 run 的 `skills/list ≥ 1` 为机制正对照）+ 正文哨兵不出现在任何模型请求；workspace 本地来源是进程内 provider，「读取但不使用」不会被该证据发现。接受现状（评审 nit，未改标题）。
11. **L0 PR 冒烟门禁失败归因（W6 收口，HEAD 基线对照实验）**：`npm run e2e:l0` 首轮 12 文件 6 通过 / 6 失败（EXIT=1，见 §2 第 13 行）。为判定是否本波引入，把本波 4 个运行时文件（`peri-acp/prompts/sections/13_skills.md`、`peri-middlewares/src/skills/{mod,tools}.rs`、`subagent/skill_preload.rs`）临时还原到 W6 前 HEAD `440fad70`（备份 `/tmp/w6-runtime-backup`，`cmp` 校验通过），重建二进制（`BUILD_EXIT=0`）后用同一命令复跑 6 个失败文件（`--serial --retry 0`，EXIT=1，4m33s：1 通过 / 5 失败），随后原样恢复 4 文件（`cmp` 校验 `restore verified`）并重建（`REBUILD_EXIT=0`）。对照结论：

    | 文件 | W6 二进制（L0） | HEAD 基线复跑 | W6 单跑复验 | 结论 |
    | --- | --- | --- | --- | --- |
    | `legacy-history-upgrade` | ❌ 0s（套件加载：`schema.rs 未声明 PRAGMA user_version`） | ❌ 同 | — | **存量**：`schema.rs:258` 自 `ecaded2f`（2026-09-20）起为格式串 `PRAGMA user_version = {CURRENT_SCHEMA_VERSION}`，测试正则 `/PRAGMA user_version = (\d+)/` 失配；与本波无因果 |
    | `workspace-no-git` | ❌ `:222:35`（messages 多一个 role 组） | ❌ 同位置同断言 | — | **存量**（与本波无因果）。多出 role 的候选解释为 `system_reminder`（会话内「System reminder · Capability · mcp · Info」条目）；未直接读取失败 run 的 messages 行，标注为**未坐实** |
    | `workspace-slow-git` | ❌ `:331:20`（同 shape） | ❌ 同 | — | **存量** |
    | `plugin-uninstall-no-freeze` | ❌ `:74:7`（`Text "已安装" not found`） | ❌ 同 | — | **存量** |
    | `header-suffix-and-error` | ❌ `:57:5`（模型 turn 等待 120s 超时） | ❌ `:198:9`（错误卡点击展开 5s 超时） | ❌ `:198:9`（1m51s） | **存量**：确定性失败点为 `:198:9`（W6 单跑与 HEAD 基线一致）；L0 的 `:57:5` 为长跑期间真实模型用时波动 |
    | `edit-diff` | ❌ `:60:7`（Write 完成态 120s 超时） | ✅ 通过 | ✅ 通过（1m32s） | **非回归**：失败为真实模型用时波动（flake） |

    两点交付侧事实：① L0 tier 的 12 个文件中有 **7 个经 `launchPeri`（`dev.sh` + `.env`）驱动真实模型**，与本 tier 自述「偏确定性用例」存在张力——模型端用时波动可越过用例的 120s 等待窗（`edit-diff`、`header-suffix` 在 L0 同批超时，单跑通过/推进到更后阶段）。② 6 个失败中 5 个在 W6 前 HEAD 上原样复现或推进到同一确定性失败点，全部与本波 4 个运行时文件无因果；L0 过门所需修复登记为存量任务（`schema` 正则失配、`system_reminder` 计数期望、错误卡点击展开、真实模型用例的确定性改造）。**该批存量修复已于 2026-09-30 完成并验证（下表）。**

    **L0 存量修复与确定性改造（2026-09-30，W6 后续；全部修复均先单跑、后经全量 L0 复验）**：

    | # | 修复 | 验证证据 |
    | --- | --- | --- |
    | 1 | `legacy-history-upgrade`：版本正则从 `PRAGMA user_version`（`schema.rs` 自 `ecaded2f` 起为格式串，永久失配）改瞄 `CURRENT_SCHEMA_VERSION` 常量 | 单跑 EXIT=0（1 文件 / 1 通过，48s） |
    | 2 | `workspace-no-git` ×2 处 / `workspace-slow-git` ×1 处：role 计数期望补 `system_reminder`（首轮 MCP 能力概览按 canonical 契约持久化——原「未坐实」推测由失败 run 实测坐实） | 单跑均 EXIT=0（18s / 13s） |
    | 3 | `plugin-uninstall-no-freeze`：产品修复 + 夹具修正。**产品**：空 slash 补全弹窗 Confirm 吞 Enter（`/plugin` 打不开面板）→ `SlashCompletion` 新增 `on_submit` 回调，无候选 Confirm 走 `submit::commit_input` 统一提交落点（同层单趟分发下「放行给输入区」不可行，见代码注释与 TRAP 说明）；**夹具**：补 `.claude-plugin/plugin.json`、`scope`/`origin` 改用 serde PascalCase 枚举名（原值使 `installed_plugins.json` 解析失败） | `cargo test -p peri-tui --lib` **1706 passed / 0 failed / 7 ignored**（EXIT=0，含新回归测试 `test_empty_slash_popup_enter_submits_and_opens_plugin_panel`）；L0 全量复验 |
    | 4 | `header-suffix-and-error`：展开详情断言改用 MCP 桥接脱敏文本（单一常量 `EXPANDED_ERROR_RE`，waitFor 谓词与终态断言同源；旧期望 `Error: File not found at /nonexistent` 只存于私有 `ToolFailure.detail`、永不上屏） | 单跑 EXIT=0（1 通过，4m01s）；collapsed / expanded 快照离线核验正则非空洞（false / true） |
    | 5 | L0 确定性改造（TEST-HERMETIC-001）：新增 `e2e/helpers/replay-model.ts`（本地假 model server，Anthropic SSE 剧本重放；`env -i` + 死端口代理护栏 + `misses()` 偏航契约）；`viewport-40x8` / `first-tool-stuck-running` / `edit-diff` 改隔离 HOME + 重放驱动（**既有断言零改动**，文件末新增 `misses()==0` 守卫）；`header-suffix-and-error` 移出 L0（留 L1/L2）；`tiers.mjs` L0 = 11 文件、描述更新；`e2e/CLAUDE.md` 增 TEST-HERMETIC-001 不变量与路由行 | 3 文件单跑均 EXIT=0（9.8s / 50.7s / 37.4s；基线分别 167.5s / 51.3s / 92–115s）；**L0 全量 11/11 全绿（4m08s，首轮全过，零 flake）**，见 §2 第 14 行 |

    修复期间的过程观察（供参考）：viewport 首轮单跑由新增的 `misses()` 守卫暴露「peri 首请求末尾追加 user 角色 `<system-reminder>`（MCP connection_summary）」的匹配偏航，匹配逻辑由「最后一条 user 文本」改为**全部 user 消息文本拼接**（`replay-model.ts` `userText`，TRAP 注释）；`plugin-uninstall-no-freeze` 与 `legacy-history-upgrade` 不经假 model server（前者无模型请求、后者死端口配置），同样满足不依赖真实凭据/网络，tier 描述按此事实措辞。

### 6.5 平台限制

- 全部运行证据取自 **macOS（本机）**；Linux/Windows 未运行。
- TUI 场景依赖 tmux（本机 3.6a）与本机 Node；CI 环境需具备同等前置。
- 本计划验收链的模型侧为本地假 model server（Anthropic SSE 形状）；未接真实 provider 做端到端（本计划不依赖）。2026-09-30 起 L0 tier 不再依赖真实模型/凭据/网络（见 §6.4 第 11 项）；**L1/L2 仍有真实依赖**——如 `thread-switch.test.ts` 的 LLM judge 需 `OPENAI_API_KEY`（无 key 时 `helpers/judge.ts:37-39` 显式 throw），本轮回归抽查中其 UI 断言全过、失败点仅在 judge 初始化（运行环境未加载 `.env` 凭据，非回归）；L1/L2 全面 hermetic 化不在本轮范围。
- Windows 路径穿越/保留名等平台语义未验证（provider 侧按 X5 受限 profile 拒绝 `..`、绝对路径、反斜杠、NUL）。

### 6.6 相邻未决（不在本计划范围，未处理）

- 图像 P1（`Q3` URI 冻结前置、`Q1/Q2/Q4/Q8` 待用户裁决）与 MCP Apps relay 边界：见 `docs/design/mcp-multiplexing.md` 与对应裁决记录，本计划只引用不比改。
- LSP 下沉并行工作：本计划只引用其落地后的装配形状，不评价其中间态。

---

## 7. 过程文档处置

- `2026-09-29-workspace-mcp-resources-j2-feasibility.md` **已退役**（DOC-HISTORY-001），历史见 Git；全仓仅存一处标注为历史参考的引用（decisions `:17`）。
- plan / decisions 保留为 active issue 记录（含未完成项与偏差登记）；文档事实源同步见 plan §10 表（W6 完成）。
