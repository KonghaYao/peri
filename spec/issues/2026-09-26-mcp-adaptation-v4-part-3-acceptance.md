# MCP adaptation v4-part-3 验收记录（wave 2：Cron / LSP 实迁为 builtin MCP 实例）

> 2026-09-28 合并安全修订：本文 IF-D14 固定错误退化为迁移时点记录；workspace 已按当前 IF-D14 改为类型化安全原因及任务/日志/草稿恢复引用。真实边界与安全负例见 `peri-middlewares/src/mcp/builtin/workspace_recovery_test.rs`。


> 日期：2026-09-26（V-06 收口 2026-09-27）。状态：**就绪**——wave 2 生产迁移（cron / lsp 实迁为 builtin MCP 实例）已全部落地并提交（收口时 HEAD `f9e7b381`，工作树干净）；全量门禁四段绿（§6.1，16 个测试 target / 6565 passed / 0 failed / 15 ignored）；真实二进制正向验证已完成，print 路径与 TUI 路径两条通道均实测通过（§8）；§4–§6 已收口，§7 未闭合项与未落地 gap 均已逐条登记。
>
> **覆盖边界**：本记录的运行时证据只覆盖 `cron` / `lsp` 两个 builtin 实例；`workspace` 实例属 **wave 3**，尚未开始，不在本记录任何断言的范围内。
>
> 主计划：`spec/issues/2026-09-26-mcp-adaptation-v4-part-3-plan.md`（裁决与接口冻结的唯一事实源）。本文件只记录**现场证据**与三态判定，不回填设计裁决。
>
> 三态口径承接 part-1/part-2：「目标归属」（设计文档）≠「当前实现」（代码事实）≠「本次运行时证据」（命令 + exit + 计数）。绿色局部单测不得升级为整体迁移结论。
>
> 运行环境：worktree `/Users/konghayao/code/ai/peri-v4p3`，分支 `feat/mcp-adaptation-v4-part-3`，基线 `226fa2bc`（= part-2 HEAD）。
>
> **R29 回填（2026-09-27，HEAD `8f29aea7`）**：§3 第 5 行登记的两条 R29 具名用例**已落地并取证**（`mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`、`host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges`），命令与 `test result:` 见 §6.2（**H14** / **L06**），三态判定更新见 §3 第 5 行、§7.3、§7.6。本次回填**只**改动这两条涉及的行与本节日期注记，其余行的判定与证据未作任何改写。

## 1. 范围与证据口径

- 本波次范围（用户裁决 2026-09-26）：只做 `cron` 与 `lsp` 两个 builtin MCP 实例的实迁；`workspace` 单独成波（wave 3）。
- 迁移前基线（§2）必须在**任何生产改动之前**采集；§2 写入后不再改写，迁移后的对照重跑另行追加小节。
- 证据形式固定为：命令原文 + exit code + `test result:` 行 + 现场打印原文；夹具文件随提交可追溯。
- §2 采集时工作树只含测试夹具与验收记录，无生产源码改动（`git status --porcelain` 见 §2.5）。

## 2. 迁移前基线（W0）

### 2.1 仪器

| 项 | 内容 |
| --- | --- |
| 夹具文件 | `peri-acp/src/host/mcp_v4_wave2_baseline_test.rs`（新建；模块 `host::mcp_v4_wave2_baseline`，按既有 `#[path]` 写法挂载于 `peri-acp/src/host/mod.rs`） |
| 复用 | wave 1 的真实装配 + 真实 loader + 真实 stdio wire 夹具（`host::mcp_v4_wire_fixture`，**未改动该文件**） |
| 用例 1 | `wave2_baseline_first_request_and_deferred_summary`（无 LSP 配置） |
| 用例 2 | `wave2_baseline_lsp_tool_visible_when_server_configured`（夹具侧经生产函数 `load_merged_lsp_servers` + `create_session_lsp_pool` 注入最小 LSP 配置；不拉起 language server） |
| 观察量 1（直连面） | 首个 LLM 请求 `tools` 的名字集合（按到达顺序） |
| 观察量 2（摘要面） | 首个请求 system 文本中 `## Deferred Tools` 段的条目行 |
| 观察量 3（搜索面） | 脚本化调用 `SearchExtraTools(query=…)` 的工具结果文本与命中名单——**本波次的等价对照面**（原因见 §2.4） |

> **补记（W3 收口，不改写上面的 W0 证据）**：`§2` 首次采集时用例 2 的夹具经生产函数 `create_session_lsp_pool` 注入 LSP 配置；该函数已随 `21c6273a`（H-04 收口）删除，夹具相应改为 `create_host_lsp_pool`（同一 host 级唯一 pool 工厂）。**改动只涉及夹具的注入入口，不涉及 §2.3 的任何打印原文**；W0 采样时的生产路径以「基线」身份保留在本节，终态对照见 §4/§6。

### 2.2 命令与结果（逐字）

```
$ cargo test -p peri-acp --lib -- host::mcp_v4_wave2_baseline --nocapture     # EXIT=0
running 2 tests
test host::mcp_v4_wave2_baseline::wave2_baseline_first_request_and_deferred_summary ... ok
test host::mcp_v4_wave2_baseline::wave2_baseline_lsp_tool_visible_when_server_configured ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 719 filtered out; finished in 0.39s
```

```
$ cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture                 # EXIT=0（wave 1 夹具未被破坏）
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 718 filtered out; finished in 0.32s
```

```
$ bash scripts/check-layer-imports.sh                                        # EXIT=0
§0 依赖门：共 18 条规则，违规边 0
✅ 全边依赖门通过（Seam 3 完整版）
```

### 2.3 现场打印原文（关键行逐字）

用例 1（无 LSP 配置）：

```
[W2-V01 基线] 首个 LLM 请求工具数 = 18；工具名 = ["Agent", "AskUserQuestion", "Bash", "DiscoverSkillsTool", "Edit", "ExecuteExtraTool", "Glob", "Grep", "Read", "SearchExtraTools", "SkillTool", "TodoWrite", "Write", "folder_operations", "mcp__artifact__artifact", "mcp__web__WebFetch", "mcp__web__WebSearch", "mcp__wire_fixture__echo"]
[W2-V01 基线] deferred 条目名（`- <名字>:` 形态，共 7 个）= ["AgentResult", "DiscoverMCP", "RunPtcCode", "cron_list", "cron_register", "cron_remove", "mcp_read_resource"]
[W2-V01 基线] 命中行[27] 原文: - cron_list: List all registered cron/scheduled tasks with their status, next fire time, and prompt.
[W2-V01 基线] 命中行[28] 原文: - cron_register: Register a scheduled task that will automatically send a user message at the specified cron interval. The task runs in-memory only (lost on restart). Use standard 5-field cron expression.
[W2-V01 基线] 命中行[32] 原文: - cron_remove: Remove/delete a registered cron task by its ID.
[W2-V01 基线] 裸名 `cron_register`：总出现 = 2 次；deferred 条目行 = [28]；`"name":"cron_register"` 目录 JSON 行 = [12]
[W2-V01 基线] 裸名 `LSP`：总出现 = 1 次；deferred 条目行 = []；`"name":"LSP"` 目录 JSON 行 = []
[W2-V01 基线] 搜索面 cron：查询 = "cron"（max_results = 50）；tool 结果消息数 = 1；结果原文长度 = 3704 字符；命中工具名 = ["cron_remove", "cron_register", "cron_list", "mcp_read_resource", "AgentResult", "mcp__wire_fixture__glob", "RunPtcCode", "DiscoverMCP"]
[W2-V01 基线] 搜索面 cron：裸名 `cron_register` → 搜索结果命中 = true、直连工具表命中 = false；effective name `mcp__cron__cron_register` → 搜索结果命中 = false、直连工具表命中 = false
[W2-V01 基线] 搜索面 cron：裸名 `cron_list` → 搜索结果命中 = true、直连工具表命中 = false；effective name `mcp__cron__cron_list` → 搜索结果命中 = false、直连工具表命中 = false
[W2-V01 基线] 搜索面 cron：裸名 `cron_remove` → 搜索结果命中 = true、直连工具表命中 = false；effective name `mcp__cron__cron_remove` → 搜索结果命中 = false、直连工具表命中 = false
[W2-V01 基线] 搜索面 旁证：`mcp__wire_fixture__glob` 在摘要面总出现 = 1 次；其中 `- <名字>:` 条目行 = []；`"name":"…"` 形态（PTC 目录 JSON）行 = [12]
[W2-V01 基线] 搜索面 glob（旁证：mcp__ 前缀工具只在搜索面）：命中工具名 = ["mcp__wire_fixture__glob", "DiscoverMCP", "mcp_read_resource", "AgentResult", "cron_register", "RunPtcCode", "cron_remove", "cron_list"]
```

用例 2（注入最小 LSP 配置；含未注入对照组）：

```
[W2-V01 基线] LSP 用例：settings 路径 = <临时 HOME>/.peri/settings.json；生产加载函数解析出 server 数 = 1；server 名 = ["wave2_probe"]
[W2-V01 基线] LSP 用例（对照/未注入）：deferred 条目含裸名 `LSP` = 否（条目名 = ["AgentResult", "DiscoverMCP", "RunPtcCode", "cron_list", "cron_register", "cron_remove", "mcp_read_resource"]）
[W2-V01 基线] LSP 用例：首个 LLM 请求工具数 = 18；工具名 = ["Agent", "AskUserQuestion", "Bash", "DiscoverSkillsTool", "Edit", "ExecuteExtraTool", "Glob", "Grep", "Read", "SearchExtraTools", "SkillTool", "TodoWrite", "Write", "folder_operations", "mcp__artifact__artifact", "mcp__web__WebFetch", "mcp__web__WebSearch", "mcp__wire_fixture__echo"]
[W2-V01 基线] LSP 用例：deferred 摘要行数 = 77；cron/lsp 命中行 = [… "#23=- LSP: Provides code intelligence via Language Server Protocol (LSP). …" "#46=- cron_list: …" "#47=- cron_register: …" "#51=- cron_remove: …"]（LSP 条目原文见下行）
[W2-V01 基线] LSP 用例：deferred 条目含裸名 `LSP` = 是；原文 = - LSP: Provides code intelligence via Language Server Protocol (LSP). Use this tool for navigating and understanding code — go to definitions, find references, get type information, view document symbols, and check diagnostics.
[W2-V01 基线] 搜索面 lsp：查询 = "lsp"（max_results = 50）；tool 结果消息数 = 1；结果原文长度 = 5185 字符；命中工具名 = ["LSP", "RunPtcCode", "cron_remove", "mcp_read_resource", "DiscoverMCP", "AgentResult", "mcp__wire_fixture__glob", "cron_list", "cron_register"]
[W2-V01 基线] 搜索面 lsp：裸名 `LSP` → 搜索结果命中 = true、直连工具表命中 = false；effective name `mcp__lsp__LSP` → 搜索结果命中 = false、直连工具表命中 = false
```

> 打印中的目标路径、临时 HOME 绝对路径已按「夹具不含真实环境信息」原则省略；夹具本身写临时 HOME，不读用户真实配置。

### 2.4 基线值与关键代码事实

**基线值（迁移前）**

| 面 | 值 |
| --- | --- |
| 直连工具表（首个 LLM 请求） | **18 项**（见 §2.3 首行；cron/lsp 工具均不在其中） |
| 摘要面条目 | 7 项：`AgentResult`、`DiscoverMCP`、`RunPtcCode`、`mcp_read_resource` + **cron 三裸名**；LSP 配置存在时额外含裸名 `LSP` |
| 搜索面 | cron 三裸名与裸名 `LSP`（配置存在时）可被 `SearchExtraTools` 命中；四个 effective name 均不命中 |

**关键代码事实（决定了对照面选择；均为代码事实，不是推断）**

- `peri-middlewares/src/tool_search/tool_index.rs:303-322`：`format_deferred_list` 在第 311 行按 `!name.starts_with("mcp__")` 过滤 ⇒ **`## Deferred Tools` 摘要面对 `mcp__` 前缀工具整体过滤**。
- 因此迁移后 `mcp__cron__*` / `mcp__lsp__LSP` **不会**以 effective name 出现在摘要面，而是**整体缺席**；摘要面只能作为「裸名消失」的时点证据，**不能**作为迁移前后的等价对照面。
- 等价对照面是**搜索面**（`ToolIndex` 收录 `mcp__` 前缀工具，不过滤）：迁移前命中裸名、迁移后应命中 effective name，二者**恰有其一**。旁证：`mcp__wire_fixture__glob`（wave 1 起就是 effective name 的 deferred MCP 工具）在摘要面条目行缺席、在搜索面可命中（§2.3 用例 1 最后两行）。
- `LspServerPool::has_servers()` 的语义是**配置表非空**（`peri-lsp/src/pool.rs:47` 惰性构造、`:223` = `!servers.is_empty()`），本次实测：注入配置但**未**拉起 language server 时，裸名 `LSP` 已出现在摘要面 ⇒ 工具面的门控谓词与「server 进程是否就绪」无关。此事实直接约束 LSP 实例的门控冻结（见主计划对应裁决）。

### 2.5 时点不变断言（迁移前后都必须绿）

| 断言 | 迁移前 | 迁移后 | 依据 |
| --- | --- | --- | --- |
| `SearchExtraTools("cron")` 命中 `cron_register` / `cron_list` / `cron_remove` 与 `mcp__cron__*` **恰有其一** | 裸名分支 | effective 分支 | 改名不并存；「都在」= 重复暴露、「都不在」= 能力净丢失，均判红 |
| `SearchExtraTools("lsp")`（配置存在时）命中 `LSP` 与 `mcp__lsp__LSP` **恰有其一** | 裸名分支 | effective 分支 | 同上 |
| `mcp__wire_fixture__glob` 摘要面**条目行**为空、搜索面可命中 | 成立 | 成立 | 摘要面过滤是入口条件，与改名无关 |
| 首个 LLM 请求恰 1 次模型调用、wire 上收到 `initialize` + `tools/list` | 成立 | 成立 | 夹具自检 |

### 2.6 `git status --porcelain`（§2 采集时）

```
 M peri-acp/src/host/mod.rs
?? peri-acp/src/host/mcp_v4_wave2_baseline_test.rs
```

`mod.rs` 的改动仅为 `#[cfg(test)]` 测试模块的 `#[path]` 挂载；**无生产源码改动**（该次 `cargo test -p peri-acp --lib` 通过，`check-layer-imports.sh` 违规边 0）。

## 3. 语义变更登记（本波次已知偏离）

> 施工规则 §9.9：语义变更必须逐条列出，禁止只写「行为等价」。状态列区分**已落地（有提交）**与**已验证（有命令证据，见 §6）**；两者不是同一层证据。

| # | 变更 | 影响面 | 已落地证据（commit / 代码符号） | 已验证证据（测试函数 → 命令 ID） |
| ---: | --- | --- | --- | --- |
| 1 | 模型面工具名改为 effective name（`mcp__cron__cron_register` / `mcp__cron__cron_list` / `mcp__cron__cron_remove` / `mcp__lsp__LSP`） | 搜索面、审批面、事件载荷、TUI 展示 | `151a03cc`（C-01 声明表四工具）、`8ed9ab6b`（L-01 lsp handler）、`2fc9da41`（H-05 dispatch 接线）、`abc5c775`（S-01 敏感清单显示名经注册表解析）；符号 `peri-acp-types/src/builtin_mcp.rs::CRON_TOOLS` / `LSP_TOOLS`、`peri-middlewares/src/permission/mod.rs::builtin_tool_effective_name` | `builtin_mcp::tests::original_tool_name_of_effective_hits_frozen_literals`（T01 同模块）、`mcp::builtin::tests::effective_tool_names_covers_registry`（M07）、`permission::tests::builtin_effective_names_match_original_name_policy`（S01）、`subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names`（S02）、`permission::tests::cron_sensitive_markdown_uses_registry_effective_name`、`kit::tool_display::tests::cron_lsp_effective_names_reuse_existing_display`（S03）、`truncate::tests::cron_lsp_effective_names_reuse_existing_summaries`（S04）、`host::mcp_v4_wave2::search_execution_uses_effective_names`（H11，**未落地**——全仓无此符号，见 §7.6；该面改由 H02 终态用例 + §8 真实二进制覆盖） |
| 2 | `## Deferred Tools` 摘要面不再列出这四个工具（`format_deferred_list` 的 `mcp__` 过滤先决效应） | 首个请求 system 文本 | 代码事实（迁移前即存在，非本波引入）：`peri-middlewares/src/tool_search/tool_index.rs::format_deferred_list` 过滤 `!name.starts_with("mcp__")` | 基线 `host::mcp_v4_wave2_baseline::wave2_baseline_first_request_and_deferred_summary`（§2.2）+ 终态 `host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary`（H02，同一函数内同时覆盖「有 LSP 配置 / 无 LSP 配置」两侧） |
| 3 | LSP pool 作用域由 per-session 变 per-host（A11/A22）；`root_uri` = host cwd；`session/delete` 不再关闭 language server | 多 cwd 部署的 root_uri；共享状态生命周期 | `6e72dcd1`（H-03 `create_host_lsp_pool` + 单 pool 门控）、`8f2e7331`（H-04 宿主装配重排、注入早于 initialize）、`21c6273a`（删过渡 session 池工厂）；符号 `peri-middlewares/src/assembly/lsp.rs::create_host_lsp_pool`、`peri-acp/src/host/shutdown.rs`（唯一 pool 有界关闭、按 `Arc::ptr_eq` 去重） | `host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown`（H08）、`host::requests::tests::lifecycle_cases::test_delete_active_session_does_not_shutdown_shared_host_lsp_pool`、`host::stdio::run_server_integration_tests::test_load_without_lsp_config_projects_empty_host_pool` |
| 4 | 工具错误文本经 IF-D14 唯一映射退化（主计划 §10 第 5 项 / R20） | cron/lsp 工具的业务错误文案 | `6b2c7557`（WP-MW 关闭矩阵用例）；符号 `peri-middlewares/src/mcp/builtin/web.rs::execution_failure_text`（唯一实现）、`cron.rs` / `lsp.rs` 经 `invoke_tool_call` 调用 | `mcp::builtin::cron::tests::call_tool_uses_if_d14_error_mapping`、`mcp::builtin::lsp::tests::call_tool_reuses_lsp_tool_and_shared_result_mapping`；`mcp::builtin::dispatch::tests::call_tool_error_text_is_fixed_and_redacted`（D06，**已落地**——命令与 `test result:` 见 §6.2；本列据此宣称已验证）；六类 LSP / 两类 Cron 文案清单见 §7.2 |
| 5 | 端到端超时上界新增外层 `McpToolBridge::TOOL_CALL_TIMEOUT = 120s`（`LspTool::timeout() == None`） | LSP 工具调用时延上界 | `peri-middlewares/src/mcp/tool_bridge.rs`（值未改；`6b2c7557` 仅放宽可见性并登记理由 R29） | **已落地（R29 回填，`8f29aea7`）**：`host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges`（F02，命令 **H14**；`ok. 1 passed; 0 failed; …; 731 filtered out; finished in 10.10s`）+ `mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`（命令 **L06**；模块同跑 `ok. 17 passed; 0 failed; …; 1951 filtered out; finished in 0.00s`）——命令原文与逐字 `test result:` 见 §6.2，两条都断言 120s **字面值** + typed `ToolCallError::Timeout` + 文案脱敏，并分别观测「到期只 drop 客户端等待、不取消 server 侧执行」与「显式 close 收敛」。**同一行登记的语义事实未变**：`LspTool::timeout()` 仍为 `None`，外层 120s 是 LSP 工具调用唯一上界（新证据见 §7.3） |


## 4. 三面工具面断言（V-02 / V-03）

**已收口（V-06，2026-09-27）**。三行断言与命令 ID、exit、`test result:` 行、实际命中名单如下（命令原文亦见 §6.2 台账；所有命令在收口 HEAD 上真实执行）。

| 面 | 观察量 | 承担函数 | 状态 |
| --- | --- | --- | --- |
| 直连面（首个 LLM 请求 `tools`） | 不含任何 `mcp__cron__*` / `mcp__lsp__*`（实测 17 项） | `host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary` | 已落地（已取证）+ **H02** |
| 摘要面（`## Deferred Tools`） | 四个 effective name 整体缺席（`mcp__` 前缀过滤的先决效应）；裸名消失 | 同上用例内的「deferred 目录面」——按 §2.4 判定，该面的等价对照**只能取搜索面**；`## Deferred Tools` 段本身另由 **§8** 真实二进制证据承担 | 已落地（已取证）+ **H02** / §8 |
| 搜索/执行面 | 逐工具裸名 XOR effective name（恰有其一） | `host::mcp_v4_wave2::search_execution_uses_effective_names` | **未落地（登记为 gap）** |

> 搜索/执行面承担函数经只读核查**全仓无此符号**：`grep -rn "search_execution_uses_effective_names" --include=*.rs .`（排除 `./target`）**零命中**——该名字只出现在 `spec/issues/**` 的计划文本（主计划 §8 第 24 行、sub-plan C §8、sub-plan V 第 16/24 行与 §6 台账、本记录）中，**代码侧从无定义，也无注释交叉引用**（`mcp_v4_wave2_baseline_test.rs` 的两处 `super::mcp_v4_wave2::…` 交叉引用指向的是 `wave2_final_first_request_and_deferred_summary`，非本行函数）。同批复核零命中的，连同本行共 §7.6 四例（该节 R29 两例已于 `8f29aea7` 回填落地并取证）。**不删除本行**：该面已由**真实二进制正向验证**覆盖——§8 print 路径 A 组实测 `SearchExtraTools(query="cron")` 命中三个 `mcp__cron__*`，并由 `ExecuteExtraTool` 以 `mcp__cron__cron_register` 真实执行；TUI 路径 A/B 两组同效。故不阻塞首版可用。

### 4.1 H02：终态首个请求 + 目录面（命令原文与逐字打印）

```
$ cargo test -p peri-acp --lib -- host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary --exact --nocapture     # EXIT=0
running 1 test
[W2-V03 终态] cron+lsp（配置非空）：查询 = "cron lsp"（max_results = 50）；首个请求直连工具 = ["Agent", "AskUserQuestion", "Bash", "DiscoverSkillsTool", "Edit", "ExecuteExtraTool", "Glob", "Grep", "Read", "SearchExtraTools", "SkillTool", "TodoWrite", "Write", "folder_operations", "mcp__artifact__artifact", "mcp__web__WebFetch", "mcp__web__WebSearch"]；搜索面命中工具 = ["mcp__cron__cron_remove", "mcp__cron__cron_register", "mcp__cron__cron_list", "mcp__lsp__LSP", "DiscoverMCP", "mcp_read_resource", "RunPtcCode", "AgentResult"]
[W2-V03 终态] cron（无 LSP 配置）：查询 = "cron lsp"（max_results = 50）；首个请求直连工具 = ["Agent", "AskUserQuestion", "Bash", "DiscoverSkillsTool", "Edit", "ExecuteExtraTool", "Glob", "Grep", "Read", "SearchExtraTools", "SkillTool", "TodoWrite", "Write", "folder_operations", "mcp__artifact__artifact", "mcp__web__WebFetch", "mcp__web__WebSearch"]；搜索面命中工具 = ["mcp__cron__cron_remove", "mcp__cron__cron_register", "mcp__cron__cron_list", "AgentResult", "DiscoverMCP", "RunPtcCode", "mcp_read_resource"]
test host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 730 filtered out; finished in 0.14s
```

打印中的两条命中行与 §2.3 基线**可直接逐字对照**（基线用 `"cron"` / `"lsp"` 两次单查询，终态用一次 `"cron lsp"` 查询；只比较「工具是否在命中名单内」，不比较 score 排序）：

| 观察面 | §2.3 基线（迁移前，逐字） | §4.1 终态（`--nocapture` 逐字） | 判定 |
| --- | --- | --- | --- |
| 搜索面 `"cron"`（无 LSP 配置） | `["cron_remove", "cron_register", "cron_list", "mcp_read_resource", "AgentResult", "mcp__wire_fixture__glob", "RunPtcCode", "DiscoverMCP"]` | `["mcp__cron__cron_remove", "mcp__cron__cron_register", "mcp__cron__cron_list", "AgentResult", "DiscoverMCP", "RunPtcCode", "mcp_read_resource"]` | 裸名 → effective name，**恰有其一** ✓ |
| 搜索面 `"lsp"`（LSP 配置非空） | `["LSP", "RunPtcCode", "cron_remove", "mcp_read_resource", "DiscoverMCP", "AgentResult", "mcp__wire_fixture__glob", "cron_list", "cron_register"]` | `["mcp__cron__cron_remove", "mcp__cron__cron_register", "mcp__cron__cron_list", "mcp__lsp__LSP", "DiscoverMCP", "mcp_read_resource", "RunPtcCode", "AgentResult"]` | 同上 ✓ |
| 直连面（首个请求 `tools`） | 18 项（基线夹具额外挂了 wave 1 的真实 stdio wire 夹具服务，故多一项 `mcp__wire_fixture__echo`） | 17 项（终态用例的夹具不含该 wire 服务） | 两侧均**无** `mcp__cron__*` / `mcp__lsp__*` ✓ |

> 两列**不作字面集合相等**：基线夹具额外挂了一个真实 stdio wire 夹具（`mcp__wire_fixture__glob` / `mcp__wire_fixture__echo`），终态用例的夹具不含该服务（差 1 项直连、1 项搜索命中）。判定按 §2.5 的 XOR 口径逐工具进行。另：终态用例的正控制为 `mcp__web__WebSearch`（wave 1 恒注入且 direct）——两侧 17 项列表内均可见，故「不含 cron/lsp」不是断言面空洞。
>
> 脱敏：本用例打印只有工具名清单与查询字符串，**不含**临时 HOME 或绝对路径，符合 §2.3 既有省略原则（§8 的证据文件亦按同原则只引相对路径）。

### 4.2 H02 所在模块十例（`--exact` 用于模块前缀会命中 0 例，须记）

```
# ① 按「模块前缀 + --exact」的字面形态执行：libtest 的 --exact 只匹配**完整用例名**，故 0 例命中
$ cargo test -p peri-acp --lib -- host::mcp_v4_wave2 --exact     # EXIT=0
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 731 filtered out; finished in 0.00s
```

```
# ② 修正形态：尾随 `::` 过滤即可精确圈定本模块（`host::mcp_v4_wave2_baseline::*`
#    不含子串 `host::mcp_v4_wave2::`，不串味），模块十例全绿
$ cargo test -p peri-acp --lib -- 'host::mcp_v4_wave2::'     # EXIT=0
running 10 tests
test host::mcp_v4_wave2::context_injected_before_initialize_and_rejects_duplicate ... ok
test host::mcp_v4_wave2::off_has_zero_builtin_injection ... ok
test host::mcp_v4_wave2::wave1_compatibility_with_wave2 ... ok
test host::mcp_v4_wave2::lsp_handler_constructed_after_config_merge ... ok
test host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown ... ok
test host::mcp_v4_wave2::reconnect_has_single_tick_driver ... ok
test host::mcp_v4_wave2::lifecycle_state_matrix ... ok
test host::mcp_v4_wave2::cron_register_tick_approval_continuation ... ok
test host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary ... ok
test host::mcp_v4_wave2::wave2_ready_gate_matrix ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 721 filtered out; finished in 33.54s
```

> 10 例的实际命中名单如上（顺序即 libtest 打印顺序，非源码顺序）：`context_injected_before_initialize_and_rejects_duplicate`、`off_has_zero_builtin_injection`、`wave1_compatibility_with_wave2`、`lsp_handler_constructed_after_config_merge`、`multi_cwd_degradation_and_host_shutdown`（**H08**）、`reconnect_has_single_tick_driver`、`lifecycle_state_matrix`、`cron_register_tick_approval_continuation`（**H12**）、`wave2_final_first_request_and_deferred_summary`（**H02**）、`wave2_ready_gate_matrix`。故 H08 与 H12 的命令证据在同一模块运行中一并取得。

## 5. 端到端链路（V-03）

**已收口（V-06，2026-09-27）**。两条链路的承担函数、实际状态与等价证据：

| 链路 | 断言 | 承担函数 | 状态 |
| --- | --- | --- | --- |
| cron：effective register → tick → 触发 → 审批 → `QueuedMessage(MessageSource::CronTrigger)` → continuation | 审批调用计数 + continuation 落地 + 无重复 tick | `host::mcp_v4_wave2::cron_register_tick_approval_continuation`（`peri-acp/src/host/mcp_v4_wave2_test.rs:2133`） | 已落地（已取证）+ **H12** |
| LSP：Write → `didChange` → `didSave`，且不改写工具结果 | 替身计数（ready / change / save 各 1）+ 工具结果逐字不变 | `host::mcp_v4_wave2::lsp_sync_does_not_change_tool_result` | **未落地（登记为 gap）** |

### 5.1 H12：cron 端到端链路（命令原文与逐字打印）

```
$ cargo test -p peri-acp --lib -- host::mcp_v4_wave2::cron_register_tick_approval_continuation --exact --nocapture     # EXIT=0
running 1 test
[W2 ready] instance=web transport=peer(未关闭)/协议初始化=server_info(0.2.0)/能力协商=tools/工具面=["WebSearch", "WebFetch"]
[W2 ready] instance=artifact transport=peer(未关闭)/协议初始化=server_info(0.2.0)/能力协商=tools/工具面=["artifact"]
[W2 ready] instance=cron transport=peer(未关闭)/协议初始化=server_info(0.2.0)/能力协商=tools/工具面=["cron_register", "cron_list", "cron_remove"]
[W2 ready] instance=lsp transport=peer(未关闭)/协议初始化=server_info(0.2.0)/能力协商=tools/工具面=["LSP"]
[W2 cron-e2e] register_approval=1 name=mcp__cron__cron_register input=Object {"expression": String("*/5 * * * *"), "prompt": String("w2-cron-e2e-prompt")} → tick_enabled=true 的一代 mounted；组合根任务 task_id=01a0e0c3-e529-7382-b290-d349dec1f584
[W2 cron-e2e] trigger_approval=1(reject) task_id=01a0e0c3-e529-7382-b290-d349dec1f584 → queue=0 len=0 continuation=0（审批调用计数可观测）
[W2 cron-e2e] trigger_approval=2(approve) → queue=QueuedMessage(MessageSource::CronTrigger)×1（dispatch 被闸门卡住时读出；len=1）
[W2 cron-e2e] dispatch=真实 continuation turn（model_calls=1，请求含 cron reminder 正文 source=cron）；wire 收到 session/update + peri/agent_event_done；会话 history=Some(1)；queue 已消费=空
test host::mcp_v4_wave2::cron_register_tick_approval_continuation ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 730 filtered out; finished in 6.09s
```

命中的四行 `[W2 ready]` 同时给出 4 个实例的**工具面原始名**（cron 三裸名 / lsp 单裸名）= 注册表声明面；模型面名字在 `register_approval=1` 行以 `mcp__cron__cron_register` 出现（审批面调用计数可观测）。`task_id` 为本次运行的真实 UUID（每次运行不同）。

### 5.2 LSP 链路：承担函数未落地，等价证据两层

`host::mcp_v4_wave2::lsp_sync_does_not_change_tool_result` 经只读核查**全仓无此符号**（`grep -rn` 零命中；同批 `lsp_sync_close_cross_matrix` 亦零命中，见 §7.6）。**不删除本行**：该链路的等价证据由两层构成——

**（1）单元层：同步的执行序与门控分支（L03，本次真实执行）**

```
$ cargo test -p peri-middlewares --lib -- lsp::middleware::tests     # EXIT=0
running 8 tests
test lsp::middleware::tests::read_failure_degrades_to_ok ... ok
test lsp::middleware::tests::not_ready_skips_read_and_notifications ... ok
test lsp::middleware::tests::non_write_tools_ignored ... ok
test lsp::middleware::tests::change_error_still_attempts_save ... ok
test lsp::middleware::tests::not_ready_with_missing_path_skips_file_access ... ok
test lsp::middleware::tests::relative_path_resolved_against_state_cwd ... ok
test lsp::middleware::tests::write_sync_orders_change_then_save ... ok
test lsp::middleware::tests::missing_or_non_string_file_path_skips_port ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 1959 filtered out; finished in 0.02s
```

其中 `write_sync_orders_change_then_save` = 断言序列「ready → didChange → didSave」；`read_failure_degrades_to_ok` = 读盘失败仍以 `Ok` 返回、不改写工具结果。**但这两条是替身（counter）证据，不是「工具结果文本逐字不变」的证据**——后者由下一层承担。

**（2）运行时层：真实二进制三组（§8 的 S2a / S2b / S2c）**

同一 `Write` 工具结果文本在「无同步（S2a：0 条 LSP 通知）」与「有同步（S2b/S2c：真实 `didOpen`/`didChange`/`didSave`）」下**逐字相同**——三组均为 `Wrote 1 line src/main.rs`（取证据见 §8.1 结论 3，文件 `S2{a,b,c}/reqs/req-*.json` 的 `tool_result` 原文）。这就是本行「不改写工具结果」的等价证据；缺的是「替身计数（ready / change / save 各 1）」这一**形态**的断言，故仍登记为 gap（§7.6）。

## 6. 全量门禁与命令台账（V-06）

**已收口（V-06，2026-09-27）**。§6.1 四条门禁**采用既有采集结果，本次收口未重跑**（耗时原因）；§6.2 为具名命令台账，其中「本次执行」的行在收口 HEAD 上真实跑过并逐字抄录。

### 6.1 全量门禁（四段，采集日志逐字核对）

| # | 命令原文 | 判定 | 采集日志 |
| ---: | --- | --- | --- |
| G01 | `cargo build --workspace` | EXIT=0 | `/tmp/w3_gate.log` |
| G02 | `cargo test --workspace --lib` | EXIT=0；**16 个 target / 6565 passed / 0 failed / 15 ignored** | `/tmp/w3_gate2.log` |
| G03 | `cargo clippy --workspace --all-targets -- -D warnings` | EXIT=0；零告警（`Finished dev profile ... in 32.68s`，无 warning 行） | `/tmp/w3_gate2.log` |
| G04 | `bash scripts/check-layer-imports.sh` | EXIT=0；`§0 依赖门：共 18 条规则，违规边 0` | `/tmp/w3_gate2.log` |

G02 的 16 个 target 逐字（顺序即日志顺序；合计与上表一致）：

```
test result: ok. 97 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.01s
test result: ok. 731 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 39.89s
test result: ok. 456 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 875 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.02s
test result: ok. 131 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 50 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.01s
test result: ok. 98 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.14s
test result: ok. 1962 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 31.91s
test result: ok. 156 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 156 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.20s
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1679 passed; 0 failed; 7 ignored; 0 measured; 0 filtered out; finished in 15.34s
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s
test result: ok. 108 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 0.40s
```

三条口径注记（收口时只读核对日志所得，不含推断）：

1. **EXIT 行的来源**：两份日志内只有段完成标记（`GATE_BUILD_DONE` / `GATE_TEST_DONE` / `GATE_CLIPPY_DONE` / `GATE DONE`）与零 `error`/`FAILED` 行，**没有** `EXIT=n` 行——EXIT=0 是采集时外层 runner 的判定。G02 的 16 行全部 `ok`、G03 无 warning 行、G04 打印两条 ✅，与 EXIT=0 自洽。
2. **G01 段有 1 条告警**：`warning: \`peri-tui\` (bin "peri") generated 1 warning` + `= note: \`#[warn(linker_messages)]\` on by default`——来自链接器消息，**G03（clippy）不报**，也不影响 EXIT=0 判定。这是本次收口发现的唯一「门禁叙事与日志字面」的差异，登记在此不作结论。
3. **G02 的 15 ignored** 分布在三个 target（5 + 7 + 3），与 §7.4 的未取证声明无关（是既有 `#[ignore]` 用例）。

### 6.2 具名命令台账

> 「来源」列区分**本次执行**（收口会话在 HEAD `f9e7b381` 上真实执行、逐字抄录）与**无命令**（用例未落地）。本波次早期会话留下的日志只作对照注记（S03/S04 两行），不当作本次证据；也未把先前会话的结果写成本次执行。
>
> R29 回填两行（**H14** / **L06**）的来源标为「R29 回填」，在同一分支 HEAD `8f29aea7` 上真实执行并逐字抄录；其余各行**未重跑、未改写**。

| 命令 ID | 命令原文 | 命中函数（关键） | `test result:`（逐字） | 来源 / 备注 |
| --- | --- | --- | --- | --- |
| **T01** | `cargo test -p peri-acp-types --lib -- builtin_mcp::tests` | `builtin_mcp::tests::original_tool_name_of_effective_hits_frozen_literals`（共 11 例） | `ok. 11 passed; 0 failed; 0 ignored; 0 measured; 445 filtered out; finished in 0.01s` | 本次执行 |
| **M07** | `cargo test -p peri-middlewares --lib -- mcp::builtin::tests` | `mcp::builtin::tests::effective_tool_names_covers_registry` 等（共 23 例） | `ok. 23 passed; 0 failed; 0 ignored; 0 measured; 1944 filtered out; finished in 0.00s` | 本次执行 |
| **D06** | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests` | `mcp::builtin::dispatch::tests::call_tool_error_text_is_fixed_and_redacted` 等（共 6 例） | `ok. 6 passed; 0 failed; 0 ignored; 0 measured; 1961 filtered out; finished in 0.06s` | 本次执行；§3 第 4 行据此由「拟新增」转「已落地」 |
| **S01** | `cargo test -p peri-middlewares --lib -- permission::tests` | `permission::tests::builtin_effective_names_match_original_name_policy`、`cron_sensitive_markdown_uses_registry_effective_name`（共 33 例） | `ok. 33 passed; 0 failed; 0 ignored; 0 measured; 1934 filtered out; finished in 0.50s` | 本次执行 |
| **S02** | `cargo test -p peri-middlewares --lib -- subagent::tests` | `subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names`（共 32 例） | `ok. 32 passed; 0 failed; 0 ignored; 0 measured; 1935 filtered out; finished in 0.00s` | 本次执行 |
| **S03** | `cargo test -p peri-tui --lib -- kit::tool_display::tests` | `kit::tool_display::tests::cron_lsp_effective_names_reuse_existing_display`（共 15 例） | `ok. 15 passed; 0 failed; 0 ignored; 0 measured; 1671 filtered out; finished in 0.01s` | 本次执行；先前会话日志用更宽的 `kit::tool_display` 过滤得 14 例，本记录以本次逐字值为准 |
| **S04** | `cargo test -p peri-tui --lib -- truncate::tests` | `truncate::tests::cron_lsp_effective_names_reuse_existing_summaries`（共 38 例） | `ok. 38 passed; 0 failed; 0 ignored; 0 measured; 1648 filtered out; finished in 0.00s` | 本次执行；先前会话日志的 `truncate` 宽过滤得 56 例（跨模块命中），范围不同 |
| **H02** | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary --exact --nocapture` | `host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary` | `ok. 1 passed; 0 failed; 0 ignored; 0 measured; 730 filtered out; finished in 0.14s` | 本次执行；打印见 §4.1 |
| **H08** | `cargo test -p peri-acp --lib -- 'host::mcp_v4_wave2::'`（模块十例同跑） | `host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown` | `ok. 10 passed; 0 failed; 0 ignored; 0 measured; 721 filtered out; finished in 33.54s` | 本次执行；清单见 §4.2 |
| **H11** | ——（无命令：用例未落地） | `host::mcp_v4_wave2::search_execution_uses_effective_names` | —— | **未落地**：全仓无此符号（§7.6）；等价证据 = H02 搜索面打印 + §8 真实二进制 |
| **H12** | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::cron_register_tick_approval_continuation --exact --nocapture` | `host::mcp_v4_wave2::cron_register_tick_approval_continuation` | `ok. 1 passed; 0 failed; 0 ignored; 0 measured; 730 filtered out; finished in 6.09s` | 本次执行；打印见 §5.1 |
| **L02** | `cargo test -p peri-lsp --lib -- pool::tests` | `pool::tests`（共 16 例，含 `test_shutdown_kills_child_process`、`port_ready_for_reflects_routed_server_state`） | `ok. 16 passed; 0 failed; 0 ignored; 0 measured; 82 filtered out; finished in 0.02s` | 本次执行；输出夹带 `kill: <pid>: No such process`（关闭路径用例的固有噪声，非失败） |
| **L03** | `cargo test -p peri-middlewares --lib -- lsp::middleware::tests` | `lsp::middleware::tests::write_sync_orders_change_then_save`、`read_failure_degrades_to_ok`（共 8 例） | `ok. 8 passed; 0 failed; 0 ignored; 0 measured; 1959 filtered out; finished in 0.02s` | 本次执行；清单见 §5.2 |
| **C03** | `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests` | `mcp::builtin::cron::tests::call_tool_uses_if_d14_error_mapping`（共 9 例） | `ok. 9 passed; 0 failed; 0 ignored; 0 measured; 1958 filtered out; finished in 8.01s` | 本次执行 |
| **I01** | `cargo test -p peri-middlewares --test mcp_isolation_contract` | 集成 target 5 例（`instances_have_independent_transport_task_and_state` 等） | `ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.26s` | 本次执行；**包是 `peri-middlewares`**：`cargo test -p peri-acp --test mcp_isolation_contract` 逐字报 `error: no test target named \`mcp_isolation_contract\` in \`peri-acp\` package`（EXIT=101，libtest 提示可用 target 在 `peri-middlewares`） |
| **L06** | ① `cargo test -p peri-middlewares --lib -- mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline --exact --nocapture --test-threads=1`（EXIT=0）<br>② 模块全量：`cargo test -p peri-middlewares --lib -- 'mcp::tool_bridge::' --nocapture`（EXIT=0） | ① `mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`<br>② 模块 17 例命中（含 `direct_flag_tests` 两例、`typed_bridges_apply_declared_direct_only_for_builtin_instances` 等） | ① `ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1967 filtered out; finished in 0.01s`<br>② `ok. 17 passed; 0 failed; 0 ignored; 0 measured; 1951 filtered out; finished in 0.00s` | **R29 回填**（`8f29aea7`）；用例名逐字来自子计划 L §6.2 的命令台账行（V 台账未给该命令分配 ID，**L06 为本次回填新增编号**，取 L 系列下一空位）；打印：`[R29 bridge] elapsed=120.001s timeout=true timeout_secs=120 text="MCP 服务器 \"lsp\" 工具 \"LSP\" 调用超时 (120s)" entered=2 in_flight_at_deadline=true server_side_dropped_at_deadline=0 replay=0 after_deadline_call=ok client_exit=Some(Cancelled) server_task=Quit(Closed)`（虚拟耗时 120.001s，真实 `finished in 0.01s`） |
| **H14**（= F02） | ① `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges --exact --nocapture --test-threads=1`（EXIT=0）<br>② 模块全量：`cargo test -p peri-acp --lib -- 'host::mcp_v4_wave2::'`（EXIT=0） | ① `host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges`<br>② 模块 11 例命中（模块原 10 例 + 本用例；清单见 §4.2 与下行打印） | ① `ok. 1 passed; 0 failed; 0 ignored; 0 measured; 731 filtered out; finished in 10.10s`<br>② `ok. 11 passed; 0 failed; 0 ignored; 0 measured; 721 filtered out; finished in 43.38s` | **R29 回填**（`8f29aea7`）；打印：`[W2 timeout] elapsed=120.001s timeout=true timeout_secs=120 text="MCP 服务器 \"lsp\" 工具 \"LSP\" 调用超时 (120s)" client_await_dropped=true server_side_cancelled=false in_flight_at_deadline=true(pid=<真实 PID> alive + lsp_state=Running) close_elapsed=6.007683375s host_shutdown=Complete { host: Complete, dynamic_mcp: Complete, mcp_pool: Complete { settled_services: 4, failed_services: 0 }, session_close_failures: 0 } child_reaped=true converged=true`；输出夹带一行 `kill: <pid>: No such process`（收尾存活探针的固有噪声，与 L02 同源，非失败） |

**宿主侧过滤口径（本次修正）**：因 `host::mcp_v4_wave2` 与 `host::mcp_v4_wave2_baseline` 互为子串，宿主侧单例断言一律用 `--exact` + **完整用例名**；而**模块级**清单不能用 `--exact`（模块前缀不是用例名，实测 0 例命中，见 §4.2），应改用尾随 `::` 的子串过滤 `'host::mcp_v4_wave2::'`。

**R29 回填的模块级命中名单（逐字，顺序即 libtest 打印顺序）**

L06②（`'mcp::tool_bridge::'`，17 例）：`test_format_content_mixed`、`test_format_content_text_only`、`direct_flag_tests::test_system_direct_flag_preserves_bridge_identity`、`test_new_creates_correct_full_name`、`test_new_empty_description`、`test_new_sanitizes_colons_in_names`、`test_new_creates_correct_description`、`test_new_sanitizes_dots_in_names`、`test_new_preserves_input_schema`、`test_invoke_not_connected`、`app_allowed_tools_intersects_resource_visibility_and_canonical_catalog`、`direct_flag_tests::test_build_tool_bridges_keeps_deferred_default_and_matches_typed`、`test_build_tool_bridges_filters_app_only_tool_from_model_catalog`、`typed_and_deferred_builders_differ_only_in_direct_flag`、`typed_bridges_apply_declared_direct_only_for_builtin_instances`、`test_build_tool_bridges_empty_pool`、`builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`（前缀均为 `mcp::tool_bridge::tests::`，除 `direct_flag_tests::` 两例为 `mcp::tool_bridge::direct_flag_tests::`）。

H14②（`'host::mcp_v4_wave2::'`，11 例）：`cron_register_tick_approval_continuation`、`wave1_compatibility_with_wave2`、`wave2_final_first_request_and_deferred_summary`、`lsp_bridge_timeout_cancels_and_converges`（**本次新增**）、`reconnect_has_single_tick_driver`、`context_injected_before_initialize_and_rejects_duplicate`、`lifecycle_state_matrix`、`lsp_handler_constructed_after_config_merge`、`off_has_zero_builtin_injection`、`wave2_ready_gate_matrix`、`multi_cwd_degradation_and_host_shutdown`（前缀均为 `host::mcp_v4_wave2::`）。该 11 例顺序与 §4.2 的 10 例顺序不同（libtest 打印顺序非源码顺序），**以本次逐字值为准**。

**R29 回填的受影响包回归（同一 HEAD `8f29aea7`，逐字）**

```
$ cargo test -p peri-middlewares --lib                                                  # EXIT=0
test result: ok. 1963 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 31.96s

$ cargo test -p peri-acp --lib                                                          # EXIT=0
test result: ok. 732 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 51.30s

$ cargo clippy -p peri-middlewares -p peri-acp --all-targets -- -D warnings             # EXIT=0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.52s（零 warning）

$ cargo fmt -p peri-middlewares -p peri-acp -- --check                                  # EXIT=0（无输出）

$ bash scripts/check-layer-imports.sh                                                   # EXIT=0
§0 依赖门：共 18 条规则，违规边 0
✅ 全边依赖门通过（Seam 3 完整版）
```

> 计数口径注记：两条本波次新增用例使包级计数相对 §6.1 G02 各 +1；§6.1 的四段门禁**未重跑**，本小节的包级数字是同分支 `8f29aea7` 上的另一次真实执行，不替代 G02。


## 7. 未闭合项与三态判定

> §7.1–7.3 登记**代码事实**（文件 + 行号，只读核查）与三态判定；其中 §7.3（R29）已带运行时证据（§6.2 的 H14 / L06 逐字 `test result:`）。§7.4–7.6 为收口后的未取证声明、既有现象登记与未落地 gap 登记；§7.6 六个具名用例中 R29 两例已落地并取证，其余四例仍零命中。命令与计数证据见 §4–§6；真实二进制证据见 §8。

### 7.1 本波不修的既有缺陷三件（主 plan §8 第 14 行 / R22）

| # | 缺陷 | 代码事实（证据位置） | 三态判定 |
| ---: | --- | --- | --- |
| 1 | TUI 私有 scheduler + 私有 1s tick 与宿主装配份**并存** | 私有构造 `peri-tui/src/app/cron_state.rs:14-23`；私有 tick `:25-34`（`spawn_tick_task` 无限循环）；构造 + spawn `peri-tui/src/app/mod.rs:97-99`；面板数据源 `peri-tui/src/kit/entry.rs:628` → `peri-tui/src/kit/service_snapshot.rs:54` → `peri-tui/src/kit/panels/cron.rs:3-4`。对照宿主侧：`peri-acp/src/host/assemble.rs:307-312`（新建 scheduler）+ `:364-370`（`tick_enabled`）+ `peri-tui/src/launch.rs:188`（`drive_cron_tick: true`） | 目标 = 实例内 tick（`A32` 后由 pool 唯一 spawn 点的代监督者持有）；当前实现 = 宿主侧已内化，但 TUI 私有份仍在，二者**不是同一 `Arc`**（无 `Arc::ptr_eq`），面板与模型工具面各看一份；运行时证据 = 本波显式排除（主 plan §8 第 7 行禁止用全仓 grep 零 tick 作验收条件） |
| 2 | print / stdio 路径 `drive_cron_tick: false`；workspace 端口与工具面 scheduler 不同源 | `peri-acp/src/host/stdio/mod.rs:148-151`、`peri-acp/src/host/workspace.rs:71-75`、`peri-tui/src/cli_print.rs:156-158` 三处 `false`；`peri-acp/src/host/workspace.rs:84-86` 将 `cfg.cron_scheduler` 换成宿主份 | 行为零变化（迁移前后一致）；登记为遗留，不修 |
| 3 | LSP 同步覆盖缺口：仅精确名 `Write` / `Edit` 触发 | `peri-middlewares/src/lsp/middleware.rs:64`（`tool_call.name != TOOL_WRITE && != TOOL_EDIT ⇒ return Ok(())`；常量 `peri-middlewares/src/tool_search/core_tools.rs:17-18`）；门控顺序（`ready_for` 先于读盘）`:74-79`、读盘 `:81-87`、change→save + 双错误 debug 降级 `:89-97` | 目标 = 仅 Write/Edit（本波冻结，不做工具面扩张）；当前实现 = 精确名匹配，故 `Bash` 落盘、`folder_operations`、插件工具与 `mcp__*` 文件类工具均不触发同步；证据 = `lsp::middleware::tests` 8 例（含 `non_write_tools_ignored`） |

### 7.2 工具错误文本退化清单（R20）

**cron：迁移前逐工具可见文案（`CronError` 仅两个变体，`peri-middlewares/src/cron/mod.rs:21-27`）**

| 来源 | 迁移前文案 | 产生点 |
| --- | --- | --- |
| `CronError::InvalidExpression` | `cron 表达式无效: {croner 解析原文}` | `cron/mod.rs:75-76` |
| `CronError::TaskLimitReached` | `已达到定时任务上限（20）`（`MAX_CRON_TASKS` 见 `cron/mod.rs:15`） | `cron/mod.rs:79-80` |
| `CronRegisterTool` 本地串 | `missing expression field` / `missing prompt field` / `prompt 不能为空` | `cron/tools.rs:52-61` |
| `CronRegisterTool` 成功文本 | `已注册定时任务 {id}（{expression}），prompt: {prompt}`（回显用户 prompt） | `cron/tools.rs:65-71` |
| `CronListTool` | 无 `Err` 分支 | `cron/tools.rs:102-131` |
| `CronRemoveTool` 本地串 | `missing id field` / `Scheduled task {id} not found`（回显 id） | `cron/tools.rs:172-182` |

**LSP：`LspToolError` 六变体（`peri-middlewares/src/lsp/tool.rs:13-30`）**

| # | 变体 | Display 原文 | 泄漏面 |
| ---: | --- | --- | --- |
| 1 | `MissingParam` | `缺少参数: {0}` | 参数名 |
| 2 | `InvalidOperation` | `无效的 operation: {0}` | 模型给的 operation 原文 |
| 3 | `RequestFailed` | `LSP 请求失败: {0}` | 上游错误文本（可能含 URI / 路径） |
| 4 | `NotReady` | `LSP 服务器未就绪` | 无 |
| 5 | `InvalidPosition` | `行号和列号必须 >= 1` | 无 |
| 6 | `NoServerForExtension` | `无 LSP 服务器可处理文件: {file_path} (扩展名: {extension})` | **路径 + 扩展名（本波前最重的泄漏面）** |

**迁移后（cron / lsp / web / artifact 同一路径）**：业务 `Err` 一律替换为固定规则文本

```
tool `{tool}` failed to execute; the failure detail is withheld by policy (no paths, environment values, or credentials are exposed). Verify the input and retry.
```

- 唯一实现 `peri-middlewares/src/mcp/builtin/web.rs:37-44`（`execution_failure_text`，只内插**原始工具名**）；唯一拼接点 `:104-113`，其中 `Err(_error)` 下划线绑定即「原始错误不入模型面」的结构性证据；
- cron / lsp handler 同经 `invoke_tool_call` 调用（`peri-middlewares/src/mcp/builtin/cron.rs:80-83`、`lsp.rs:106-107`、`web.rs:169-170`）；
- 外层桥接再加一层**同样脱敏**的包装：`peri-middlewares/src/mcp/tool_bridge.rs:292-309`（`is_error` → `ToolCallError::CallFailed`），模型最终看到 `MCP 服务器 "{server}" 工具 "{tool}" 调用失败: <固定脱敏文本>`（模板见 `:18`）。

### 7.3 超时上界不对称（R29）

`LspTool::timeout() == None`（`peri-middlewares/src/lsp/tool.rs:266-268`）与 bridge 的 `TOOL_CALL_TIMEOUT = 120s`（`peri-middlewares/src/mcp/tool_bridge.rs:60`；发射点 `:277-284`；文案 `:24-29`，含 server/tool 原始名与秒数、**不含**路径/参数）。三态：值未改（`6b2c7557` 仅放宽可见性并登记理由）；行为影响 = LSP 工具迁移后新增 120 s 外层期限；运行时验证 = **已落地并取证（R29 回填，`8f29aea7`）**：`host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges`（`peri-acp/src/host/mcp_v4_wave2_test.rs:3219`，命令 **H14**）与 `mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`（`peri-middlewares/src/mcp/tool_bridge_test.rs:455`，命令 **L06**）——两条都断言 120 s **字面值** + typed `ToolCallError::Timeout` + 文案脱敏，逐字 `test result:` 见 §6.2，判定见 §3 第 5 行与 §7.6。

### 7.4 未执行 / 未取证声明

- 7.1–7.3 基本为只读代码事实（文件 + 行号）；**例外**为 §7.3 的 R29 两条具名用例——已随回填（`8f29aea7`）落地并取证（§6.2 的 H14 / L06 逐字 `test result:`），不属本清单。命令与计数证据已收口于 §4–§6，真实二进制证据见 §8。
- **本次收口移出 UNVERIFIED 的项**（先前列在未取证清单内，现由 §8 覆盖）：TUI 路径本身（tmux + 真实 `peri` 两轮 run：bypass 组与 default+Enter 批准组）；审批 **Approve** 分支（`tui-hitl` 组 `Enter` 批准后 continuation 完成、模型面收到 `tool_result`）。
- **仍为 `UNVERIFIED`**（本波的运行时证据覆盖不到，逐条留待后续波次或另行取证）：
  1. **cron tick 真实到点触发**——本波所有 tick 证据均由替身 / 夹具驱动（H12 的 `trigger_approval` 计数亦为脚本化触发），**未**等真实 cron 分钟边界到点；
  2. `PERI_MCP_BUILTIN=off` 的**真机运行**（现有证据只有单测 / 集成夹具，见 T01、I01）；
  3. **真实 rust-analyzer**（§8 的 LSP 侧全部走本地假 LSP 替身 `fake-lsp.js`）；
  4. **外部（非 builtin）MCP 实例**（本波运行时证据只覆盖 web / artifact / cron / lsp 四个 builtin）；
  5. 审批 **Deny / Esc** 分支与 **Auto** 分类器判定（本波只实测 Approve）；
  6. `session/delete` 与 host shutdown 的**真实进程**资源观测（现有 = 代码事实 + 夹具用例 H08 / `peri-lsp` pool 用例）；
  7. **Windows 路径形态**（宿主侧终态用例带 `#[cfg(not(windows))]`，macOS 上不构成证据）；
  8. 用户级 / 插件级 skill 文本是否仍有裸名调用残留（不在工作树内，无法只读核对）；
  9. `docs/design/mcp-adaptation-v4-part-1.md` 是否随本波更新（属历史目标冻结文档，本波只登记不改）；
  10. TUI cron 面板是否存在经 ACP 命令面的第二取数路径。

### 7.5 真实运行观察到的既有现象（非本波引入，不修）

以下三条在 §8 的真实二进制运行中被观察到，均为**既有现象登记**（含代码事实定位），**不作为本波产品缺陷结论**：

1. **`connection_summary` system-reminder 注入非确定性**：同配置、同脚本、同 HOME 连跑两次，首个 LLM 请求的 `messages.length` 分别为 **2**（含 `kind=connection_summary` 的 system-reminder 块，长度 607）与 **1**（无该块）。证据：`/tmp/w3_smoke/A/reqs/req-1.json` 与 `/tmp/w3_smoke/A-run1/reqs/req-1.json`（两轮 `scenario.json` 逐字相同、退出码均 0）。影响面 = 首个请求的 token 计数与提示形态在两次运行间不稳定；本波不修，登记备查。
2. **`rootUri` 与 `textDocument.uri` 的路径规范化来源不同**：`rootUri` 走 `canonicalize()`（`file:///private/tmp/...`，macOS 上解析 symlink），`textDocument.uri` 走 `std::path::absolute`（`file:///tmp/...`）。对严格按 URI 前缀匹配工作区的 language server，两者可能被视作**不同工作区**。证据：`/tmp/w3_smoke/S2b/lsp-notifications.log`（`initialize.params.rootUri` 与 `didOpen.params.textDocument.uri` 同一文件路径两种前缀）。
3. **`--max-turns` 在 print 路径被显式忽略**：`peri-tui/src/cli_print.rs:104` 的 `let _ = (effort_override, max_turns, allowed_tools, disallowed_tools);`——四个 CLI 参数在 print 路径上被丢弃（值绑定到 `_`）。登记为既有事实，本波不修。

### 7.6 未落地具名用例（登记为 gap，不阻塞首版可用）

以下六个具名用例**在收口时源码中不存在**（`grep -rn "<函数名>" --include=*.rs .` 零命中），按用户裁决「正向验证优于补测试代码」降级为 gap 登记。其中 R29 两例已于回填（`8f29aea7`）落地并取证（见下行），其余四例仍零命中（2026-09-27 复核一致）。逐条给出所属验收面、未落地原因、**已有什么等价证据**（对不上就直说「无等价证据」）：

| 用例 | 所属验收面 | 未落地原因 | 等价证据 |
| --- | --- | --- | --- |
| `host::mcp_v4_wave2::search_execution_uses_effective_names` | §4 搜索/执行面（R21 / A20 XOR 判据） | 降级：该面已由正向验证覆盖，补测试代码收益低 | **有**：H02 终态打印（搜索面命中三个 `mcp__cron__*` + `mcp__lsp__LSP`，且四个裸名全不在名单内，§4.1）；§8 print 路径 A 组与 TUI 两组的 `SearchExtraTools`→`ExecuteExtraTool` 真实往返 |
| `host::mcp_v4_wave2::lsp_sync_does_not_change_tool_result` | §5 LSP 端到端链路（A8 / A30） | 降级：运行时正向验证已覆盖「结果文本不变」 | **部分**：§8 S2a/S2b/S2c 三组 `Write` 结果文本逐字相同（`Wrote 1 line src/main.rs`）；L03 的 `read_failure_degrades_to_ok`。**缺**「替身计数 ready/change/save 各 1」形态的断言 |
| `host::mcp_v4_wave2::lsp_sync_close_cross_matrix` | LSP 同步 × 关闭（`session/delete` / pool 关闭）交叉矩阵（A11/A22） | 降级：同属收口期 gap 登记 | **部分**：H08（`multi_cwd_degradation_and_host_shutdown`，随模块十例实跑）+ `peri-lsp` `pool::tests` 的 `test_shutdown_kills_child_process` / `test_shutdown_then_ensure_respawns`。**缺**同步 × close 的交叉矩阵本身（`peri-middlewares/src/lsp/middleware.rs` 内无 `didClose`/关闭分支可测） |
| `host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges` | R29 超时上界（§7.3） | **已落地（R29 回填，`8f29aea7`）**：`peri-acp/src/host/mcp_v4_wave2_test.rs:3219` | **有**：§6.2 **H14**（`ok. 1 passed; 0 failed; 0 ignored; 0 measured; 731 filtered out; finished in 10.10s`）+ 打印 `elapsed=120.001s timeout=true timeout_secs=120 … server_side_cancelled=false in_flight_at_deadline=true close_elapsed=6.007683375s … child_reaped=true converged=true`——120 s 外层期限与「到期只 drop 客户端等待、显式 close 后收敛」已在运行时观测（虚拟耗时 120 s，真实耗时 10.10s） |
| `mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline` | R29 超时文案面（bridge 报错模板 `peri-middlewares/src/mcp/tool_bridge.rs:24-29`） | **已落地（R29 回填，`8f29aea7`）**：`peri-middlewares/src/mcp/tool_bridge_test.rs:455` | **有**：§6.2 **L06**（`ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1967 filtered out; finished in 0.01s`，模块同跑 `ok. 17 passed`）+ 打印 `[R29 bridge] elapsed=120.001s timeout=true timeout_secs=120 text="MCP 服务器 \"lsp\" 工具 \"LSP\" 调用超时 (120s)" … client_exit=Some(Cancelled) server_task=Quit(Closed)` |
| `mcp::builtin::lsp::tests::delayed_fixture_converges_without_orphan_on_cancel` | LSP 取消后无孤儿进程（A30 / A22） | 同上 | **部分**：`mcp::builtin::lsp::tests` 现有 4 例（`server_info_declares_tools_only`、`handler_snapshots_tools_after_config_merge`、`list_tools_follows_configured_server_set`、`call_tool_reuses_lsp_tool_and_shared_result_mapping` 全绿）覆盖工具面与错误映射；`peri-lsp` pool 侧覆盖关闭/重启。**缺**「取消 → 收敛 → 无孤儿」的延迟夹具用例本身 |

**边界声明**：上表「有 / 部分」项的等价证据**只支持其文字所述的那一点**，不得外推为原用例的形态断言（尤其 H02 的搜索面打印不能替代 R21 的逐工具 XOR 矩阵）。**R29 两例（`lsp_bridge_timeout_cancels_and_converges`、`builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`）已于 R29 回填（`8f29aea7`）落地并取证**（§6.2 H14 / L06），不再是收口遗留缺口；上表其余四例仍为「无 / 部分等价证据」。

## 8. 真实二进制正向验证（首版可用证据）

> **来源声明**：本节证据由用户在本波收口前用真实 `target/debug/peri` 二进制采集（2026-09-27），**本次文档收口未重跑**，只做只读核对（逐文件读原文，凡写入本节的数字与字符串均为文件内字面值）。证据目录：`/tmp/w3_smoke/`（print 路径）与 `/tmp/w3_tui_smoke/`（TUI 路径）；下文引用一律用相对路径。
>
> 仪器：LLM 侧接**本地假 model server**（`ANTHROPIC_BASE_URL=http://127.0.0.1:<port>`、`ANTHROPIC_API_KEY=test`，见各组 `cmdline.txt`），LSP 侧接**本地假 LSP**（`fake-lsp.js`，逐条通知落 `lsp-notifications.log`）。真实件 = `peri` 二进制本身（含真实装配、真实 builtin MCP 实例、真实 ACP 会话循环）。

### 8.1 print 路径（`peri -p "..." --output-format stream-json`）

5 组，证据目录 `/tmp/w3_smoke/{A,B,S2a,S2b,S2c}/`，每组含 `exit-code`、`run.stdout`、`reqs/req-<n>.json`（每个 LLM 请求体，含 `tools` / `system` / `messages`）、`lsp-notifications.log`、`cmdline.txt`：

- 退出码：五组 `exit-code` 逐字均为 `0`；
- 末行事件：五组 `run.stdout` 末行均以 `{"type":"result","stop_reason":"end_turn","status":"completed","is_error":false,` 开头（后接 `usage` 与 `total_cost_usd`，逐组数值不同）；
- 场景：`A` = 无 LSP 配置（`SearchExtraTools("cron")` → `ExecuteExtraTool(mcp__cron__cron_register)` → `SearchExtraTools("lsp")`）；`B` = 有 LSP 配置（查询 `lsp` 与 `cron`）；`A-run1` = `A` 的同参数重跑（用于 §7.5 观察 1）；`S2a`/`S2b`/`S2c` = LSP 写后同步三组（见结论 3）。

**结论 1（cron 链路成立）**：`A/reqs/req-2.json` 的 `SearchExtraTools(query="cron")` 工具结果原文中，`results` 前三项即三个 builtin 工具 `mcp__cron__cron_remove` / `mcp__cron__cron_register` / `mcp__cron__cron_list`（描述前缀 `[MCP:cron]`）；`A/reqs/req-3.json` 中模型以 `ExecuteExtraTool` + `tool_name="mcp__cron__cron_register"`（`params` = `{"expression":"*/5 * * * *","prompt":"wave2 smoke"}`）真实执行，`tool_result` 原文为 `已注册定时任务 01a0e0b9-212c-75f1-91f3-512bfd2fb210（*/5 * * * *），prompt: wave2 smoke`（UUID 每次运行不同），`is_error=false`。

**结论 2（LSP 工具面门控成立）**：无配置组 `A` 的每个请求 system 文本内均含 `lsp (connected, 0 tools)`，且 `A/reqs/req-4.json` 的 `SearchExtraTools(query="lsp")` 命中名单内**无** `mcp__lsp__*`；有配置组 `B` 为 `lsp (connected, 1 tools)`，且 `B/reqs/req-2.json` 的 `SearchExtraTools(query="lsp")` **首条命中即 `mcp__lsp__LSP`**。两组同刻的另三实例均一致（`cron (connected, 3 tools)`、`web (connected, 2 tools)`、`artifact (connected, 1 tools)`）——门控差异只落在 lsp 一档。

**结论 3（LSP 写后同步成立，带前提）**：

- `S2a`（仅 `Write`，无 LSP 工具调用）→ `lsp-notifications.log` **0 行**（0 字节）。这是**惰性池 + `ready_for` 前置判定**的设计后果（未拉起 server 即无同步），不是同步失效。
- `S2b`（`Write` → `ExecuteExtraTool(mcp__lsp__LSP, diagnostics)` 拉起 server → `Write`）→ `lsp-notifications.log` 真实收到 6 行，方法序列逐字为 `spawned` → `initialize` → `initialized` → `textDocument/didOpen` → `textDocument/didChange` → `textDocument/didSave`；`didChange` 的 `contentChanges[0].text` 即第二次写入的正文。
- **两次 `Write` 的工具结果文本与 S2a（无同步）逐字相同**：三组均为 `Wrote 1 line src/main.rs`（取自各组 `reqs/req-*.json` 的 `tool_result`；S2b/S2c 的 `t1` 与 `t3` 两份均同）。
- 旁证 `S2c`（`Write` → `ExecuteExtraTool(mcp__lsp__LSP, workspaceSymbol)` → `Write`）→ 同为 6 行，序列为 `spawned` → `initialize` → `initialized` → `workspace/symbol` → `didOpen`（正文已是第二版）→ `didSave`：冷启动路径下 `didOpen` 直接带上当前盘面内容、**不产生** `didChange`，与 `S2b` 的「open → change → save」是两条不同序。

**结论 4（直连面 / 摘要面排除成立）**：五组（`A`/`B`/`S2a`/`S2b`/`S2c`）首个请求（`reqs/req-1.json`）的 `tools` 逐组均为 **17 项**（`Agent`、`AskUserQuestion`、`Bash`、`DiscoverSkillsTool`、`Edit`、`ExecuteExtraTool`、`Glob`、`Grep`、`Read`、`SearchExtraTools`、`SkillTool`、`TodoWrite`、`Write`、`folder_operations`、`mcp__artifact__artifact`、`mcp__web__WebFetch`、`mcp__web__WebSearch`），其中 `mcp__cron__*` / `mcp__lsp__*` 命中数逐组为 **0**；首个请求 system 文本的 `## Deferred Tools` 段逐组为**条目行 12 项**（`AgentResult`、`DiscoverMCP`、`DynamicMCP`、`RunPtcCode`、`Workflow` 及其 `goal`/`create`/`complete`/`block`/`clear`/`get`、`mcp_read_resource`），段内 `mcp__cron` / `mcp__lsp` 命中数 **0**、裸名（`cron_register` / `cron_list` / `cron_remove` / `LSP`）条目行 **0**。

### 8.2 TUI 路径（tmux + 真实 `peri`）

两轮独立 run 均通过，证据目录 `/tmp/w3_tui_smoke/tui/`（`--permission-mode bypass`）与 `/tmp/w3_tui_smoke/tui-hitl/`（`--permission-mode default`，Enter 批准）：

- **runner 判定**：runner 为 `/tmp/w3_tui_smoke/tui-run.sh`，其成功路径以 `exit 0` 收尾、失败路径为 `exit 3` / `exit 4`（脚本内 `:82` / `:161`）。两轮 `commands.log` 均完整走到成功路径末步（末行 `# 会话请求：3 个；辅助请求：0 个`，位于 `exit 0` 前一步），且收尾残留检查逐字为「无 tmux server（`tmux ls` → `no server running ...`）、无残留 `peri` 进程、无残留 harness 进程」（`tmux-ls.txt`、`residual-peri.txt`、`residual-harness.txt` 均为空/零命中）。
- **工具结果原文（bypass 组）**：`tui/screens/06-expanded-result.txt` 屏上逐字 `已注册定时任务 01a0e0c0-2289-7472-a2f6-1e4215b94d13（*/5 * * * *），prompt: tui wave2 smoke`；`tui/reqs/req-3.json` 的 `tool_result`（`tool_use_id=t2`）回传模型的文本与之**同一 UUID、逐字相同**。
- **MCP 面板**：`tui/screens/07-mcp-panel.txt` 逐字 `MCP Pool:ready4/4 connected`，四实例 `transport: builtin`，工具数依次为 `artifact: 1`、`cron: 3`、`lsp: 0`、`web: 2`（`/mcp` 斜杠命令取屏）。
- **HITL 组**：`tui-hitl/screens/09-approval.txt` 逐字出现 `mcp__cron__cron_register wants to run: {"expression":"*/5 * * * *","prompt":"tui wave2 smoke"}`（其上一行为 `Allowed once`）；`tui-hitl/screens/06-expanded-result.txt` 为批准后的结果卡（`▾ ExecuteExtraTool expression: */5 * * * *` + 本组 UUID `01a0e0c0-cd75-7840-99d0-2ec5bcae377f` 的成功文本）；`tui-hitl/reqs/req-3.json` 同 UUID 回传。逐命令记录见两组的 `commands.log`。

**三条观察（作为已知现象登记，不作产品缺陷结论）**：

1. 首轮（`tui1/`）手动阶段用 **Ctrl+X 未取得 MCP 面板**（`tui1/screens-manual/m05-mcp-panel.txt` 屏上无面板），改用 `/mcp` 斜杠命令后成功（`tui1/screens-manual/m07-mcp-panel.txt` 与顶层 `screens-mcp-panel-salvage.txt` 均出现 `MCP Pool:ready4/4 connected`）。属按键绑定与取屏时序的**测法差异**，本波未深究，故最终 runner 统一走 `/mcp`。
2. **Default 模式的工具卡呈现外层名**：卡片标题是 `ExecuteExtraTool` + 参数摘要 `expression: */5 * * * *`，目标名 `mcp__cron__cron_register` 只在 **HITL 审批文案**里直接可见（`wants to run: ...`）。因此**屏幕单独不足以证明目标名**，须与 `reqs/req-*.json` 的 `tool_use` / `tool_result` 联合判断（本次两组均已联合核对）。
3. HITL 组状态行显示 `Don't Ask`，而该轮权限模式为 `default`：这是 `peri-tui/src/kit/status_bar.rs:582` 的既有 catch-all 文案（`_ => i18n::tr("statusbar-permission-dont-ask")`，`"default"` 落到 `_` 分支），**非本波引入**，不在本波范围内修改。

### 8.3 本节证据的边界

以上证据全部来自 `ANTHROPIC_API_KEY=test` + 本地假 model server / 本地假 LSP 的受控环境：**未接触任何真实凭据**、**未验证真实 Anthropic API 兼容性**（含流式事件形状、错误码、限流与重试行为）、**未跑 e2e 套件**。因此 §8 只支持「首版可用」层面的正向结论（真实二进制内，cron/lsp 两个 builtin 实例的工具面、检索/执行往返、门控与写后同步按设计工作），不构成对真实服务端行为或全量回归的结论——后者以 §6.1 的四段门禁与 §6.2 的台账为准。

